//! BLE HID keyboard — the transport behind the macropad app.
//!
//! esp-radio's [`BleConnector`] is an HCI transport (`bt_hci::transport`), so
//! the host side is `trouble-host` over `ExternalController`. This module owns
//! the whole radio side: the GATT table (HID over GATT plus the battery service
//! hosts expect), advertising, and the connection loop that turns queued
//! reports into notifications.
//!
//! Apps never see any of this. They push a [`Report`] onto [`KEYS`] and the
//! task here delivers it — the same split as the buzzer, where the app asks and
//! the firmware owns the hardware.
//!
//! Structure and the fiddly protocol details follow `ref/dualkey-provisioning`,
//! a working BLE HID keyboard on this same esp-radio + trouble-host stack. The
//! details that are easy to get wrong — and silent when wrong — are commented
//! where they appear.

use bt_hci::controller::ExternalController;
use embassy_sync::blocking_mutex::raw::{CriticalSectionRawMutex, NoopRawMutex};
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Timer};
use enc_state::AppState;
use esp_hal::efuse::{InterfaceMacAddress, interface_mac_address};
use esp_hal::peripherals::BT;
use esp_hal::rng::Trng;
use esp_radio::ble::controller::BleConnector;
use trouble_host::attribute::{
    AttPermissions, AttributeTable, CharacteristicProp, PermissionLevel, Service,
};
use trouble_host::prelude::*;
use trouble_host::types::uuid::Uuid;

/// One HID keyboard report: a modifier bitmap, a reserved byte, then up to six
/// simultaneous keycodes. The boot-protocol layout, which every host accepts.
pub type Report = [u8; 8];

/// Depth of the queue from the UI task to the radio. Four is two full
/// keypresses (press then release) with slack; a macropad cannot outrun it by
/// hand, and dropping beats blocking the UI loop.
const KEY_QUEUE: usize = 4;

/// Reports waiting to go out. The app pushes, [`task`] delivers.
pub static KEYS: Channel<CriticalSectionRawMutex, Report, KEY_QUEUE> = Channel::new();

/// Advertised name; also the GAP device name.
const DEVICE_NAME: &str = "T-Encoder Macropad";

/// 0x03C1 = Keyboard, little-endian on the wire.
const APPEARANCE_KEYBOARD: [u8; 2] = [0xC1, 0x03];

/// HID service (0x1812), little-endian on the wire.
const HID_SERVICE_UUID: [u8; 2] = [0x12, 0x18];

/// Standard boot-keyboard report descriptor: 8 modifier bits, a reserved byte,
/// then six key slots. Byte-for-byte the reference's, including the second
/// `05 07` (Usage Page: Keyboard) before the key array — redundant in principle
/// since that page is already selected, but hosts parse this with their own
/// quirks and it is not worth diverging over.
const REPORT_MAP: &[u8] = &[
    0x05, 0x01, 0x09, 0x06, 0xA1, 0x01, 0x05, 0x07, 0x19, 0xE0, 0x29, 0xE7, 0x15, 0x00, 0x25, 0x01,
    0x75, 0x01, 0x95, 0x08, 0x81, 0x02, 0x95, 0x01, 0x75, 0x08, 0x81, 0x01, 0x95, 0x06, 0x75, 0x08,
    0x15, 0x00, 0x25, 0x65, 0x05, 0x07, 0x19, 0x00, 0x29, 0x65, 0x81, 0x00, 0xC0,
];

/// HID Information: bcdHID 1.11, country 0, remote-wake + normally-connectable.
const HID_INFORMATION: [u8; 4] = [0x11, 0x01, 0x00, 0x03];

/// Vendor id source 2 (USB), a placeholder VID/PID, product version 1. A host
/// uses this to choose a quirk table; Apple's guidelines want it present.
const PNP_ID: [u8; 7] = [0x02, 0xE5, 0x02, 0xA0, 0x00, 0x01, 0x00];

/// Reported battery level. The board has no fuel gauge; hosts show something
/// sane rather than nothing.
const BATTERY_LEVEL: u8 = 100;

/// HCI ACL buffers held by the controller.
const ACL_SLOTS: usize = 6;
/// Concurrent connections. A keyboard talks to one host at a time.
const CONNS: usize = 1;
/// L2CAP channels: ATT and the security manager's, with headroom.
const CHANNELS: usize = 4;
/// Attribute table capacity — services, characteristics and descriptors.
const ATT_MAX: usize = 40;
/// Client characteristic configuration descriptors tracked per peer.
const CCCD_MAX: usize = 6;
/// Peers the attribute server keeps CCCD state for.
const CONN_MAX: usize = 2;

/// Advertising interval while waiting for a host.
const ADV_INTERVAL_MIN: Duration = Duration::from_millis(30);
const ADV_INTERVAL_MAX: Duration = Duration::from_millis(60);

/// Pause before retrying after an error, so a failing stack cannot spin.
const ERROR_BACKOFF: Duration = Duration::from_secs(2);

/// The attribute server this module builds.
type HidServer<'a> =
    AttributeServer<'a, NoopRawMutex, DefaultPacketPool, ATT_MAX, CCCD_MAX, CONN_MAX>;

/// Builds the GATT table by hand rather than with `#[gatt_service]`.
///
/// The macro cannot attach a Report Reference descriptor to the input report,
/// which HID over GATT requires; the reference builds its table this way for
/// the same reason.
fn build_server<'a>(
    report_store: &'a mut [u8; 8],
    boot_store: &'a mut [u8; 8],
    protocol_mode: &'a mut [u8; 1],
    control_point: &'a mut [u8; 1],
    leds: &'a mut [u8; 1],
) -> (
    HidServer<'a>,
    Characteristic<[u8; 8]>,
    Characteristic<[u8; 8]>,
) {
    let mut table: AttributeTable<'a, NoopRawMutex, ATT_MAX> = AttributeTable::new();

    // Battery service first. A host that bonds while this exists caches the
    // service list, so removing it later breaks reconnection.
    let mut battery = table.add_service(Service {
        uuid: Uuid::Uuid16(0x180Fu16.to_le_bytes()),
    });
    let _ = battery
        .add_characteristic_small(
            Uuid::Uuid16(0x2A19u16.to_le_bytes()),
            [CharacteristicProp::Read],
            [BATTERY_LEVEL],
        )
        .build();
    let _ = battery.build();

    // Device Information. Apple's accessory guidelines require this of a HID
    // accessory; the PnP id is how a host picks its quirk table.
    let mut device_info = table.add_service(Service {
        uuid: Uuid::Uuid16(0x180Au16.to_le_bytes()),
    });
    let _ = device_info
        .add_characteristic_ro(Uuid::Uuid16(0x2A29u16.to_le_bytes()), b"LilyGo")
        .build();
    let _ = device_info
        .add_characteristic_ro(Uuid::Uuid16(0x2A50u16.to_le_bytes()), &PNP_ID)
        .build();
    let _ = device_info.build();

    let mut hid = table.add_service(Service {
        uuid: Uuid::Uuid16(0x1812u16.to_le_bytes()),
    });

    let _ = hid
        .add_characteristic_small(
            Uuid::Uuid16(0x2A4Au16.to_le_bytes()),
            [CharacteristicProp::Read],
            HID_INFORMATION,
        )
        .build();

    let _ = hid
        .add_characteristic_ro(Uuid::Uuid16(0x2A4Bu16.to_le_bytes()), REPORT_MAP)
        .build();

    // Protocol Mode: 1 = Report (the default); a host may write 0 for Boot.
    let _ = hid
        .add_characteristic(
            Uuid::Uuid16(0x2A4Eu16.to_le_bytes()),
            [
                CharacteristicProp::Read,
                CharacteristicProp::WriteWithoutResponse,
            ],
            [1u8],
            &mut protocol_mode[..],
        )
        .build();

    // The input report, plus the Report Reference descriptor that binds these
    // bytes to a report id and direction. Without it a host cannot relate the
    // notifications to the descriptor map.
    let mut report = hid.add_characteristic(
        Uuid::Uuid16(0x2A4Du16.to_le_bytes()),
        [CharacteristicProp::Read, CharacteristicProp::Notify],
        [0u8; 8],
        &mut report_store[..],
    );
    let _ = report.add_descriptor_small(
        Uuid::Uuid16(0x2908u16.to_le_bytes()),
        AttPermissions {
            read: PermissionLevel::Allowed,
            write: PermissionLevel::NotAllowed,
        },
        [0u8, 1u8],
    );
    let input_report = report.build();

    // Boot Keyboard Input/Output Report. Exposing Protocol Mode *declares*
    // Boot Protocol Mode support, which makes these mandatory — and a host that
    // switches to boot mode subscribes here instead of to the report above. We
    // promised boot mode without providing it, so those subscriptions had
    // nowhere to land and no key ever arrived.
    let boot = hid.add_characteristic(
        Uuid::Uuid16(0x2A22u16.to_le_bytes()),
        [CharacteristicProp::Read, CharacteristicProp::Notify],
        [0u8; 8],
        &mut boot_store[..],
    );
    let boot_report = boot.build();

    // Output report: the host writes keyboard LED state here. We have no LEDs,
    // but a host that cannot write them may refuse to finish setup.
    let _ = hid
        .add_characteristic(
            Uuid::Uuid16(0x2A32u16.to_le_bytes()),
            [
                CharacteristicProp::Read,
                CharacteristicProp::Write,
                CharacteristicProp::WriteWithoutResponse,
            ],
            [0u8],
            &mut leds[..],
        )
        .build();

    // HID Control Point: the host writes suspend / exit-suspend here.
    let _ = hid
        .add_characteristic(
            Uuid::Uuid16(0x2A4Cu16.to_le_bytes()),
            [CharacteristicProp::WriteWithoutResponse],
            [0u8],
            &mut control_point[..],
        )
        .build();

    let _ = hid.build();
    (AttributeServer::new(table), input_report, boot_report)
}

/// Owns the radio for the life of the program: advertises, accepts one host at
/// a time, and forwards queued reports as notifications.
#[embassy_executor::task]
pub async fn task(bt: BT<'static>, state: &'static AppState) {
    let connector = match BleConnector::new(bt, esp_radio::ble::Config::default()) {
        Ok(connector) => connector,
        Err(e) => {
            log::error!("ble: connector init failed: {e:?}");
            return;
        }
    };
    let controller: ExternalController<_, ACL_SLOTS> = ExternalController::new(connector);

    // The security manager refuses to build without a cryptographically secure
    // seed — `Rng` is deliberately not `CryptoRng`, so this must be the `Trng`,
    // which in turn needs main's `TrngSource` to be alive.
    let mut trng = match Trng::try_new() {
        Ok(trng) => trng,
        Err(e) => {
            log::error!("ble: no TRNG entropy source: {e:?}");
            return;
        }
    };

    // A stable identity, from the chip's own Bluetooth MAC. Without this the
    // address can differ per boot, so a host that bonded with us sees a
    // stranger. The top two bits mark it a *static* random address.
    let mac = interface_mac_address(InterfaceMacAddress::Bluetooth);
    let mut identity = [0u8; 6];
    for (slot, byte) in identity.iter_mut().zip(mac.as_bytes()) {
        *slot = *byte;
    }
    identity[5] |= 0xC0;
    log::info!("ble: identity {identity:02x?}");

    let mut resources: HostResources<DefaultPacketPool, CONNS, CHANNELS> = HostResources::new();
    let stack = trouble_host::new(controller, &mut resources)
        .set_random_address(Address::random(identity))
        .set_random_generator_seed(&mut trng);
    let host = stack.build();

    let mut report_store = [0u8; 8];
    let mut boot_store = [0u8; 8];
    let mut protocol_mode = [1u8];
    let mut control_point = [0u8];
    let mut leds = [0u8];
    let (server, input_report, boot_report) = build_server(
        &mut report_store,
        &mut boot_store,
        &mut protocol_mode,
        &mut control_point,
        &mut leds,
    );

    let mut peripheral = host.peripheral;
    let mut runner = host.runner;

    let hci = async {
        loop {
            if let Err(e) = runner.run().await {
                log::error!("ble: host runner stopped: {e:?}");
                Timer::after(ERROR_BACKOFF).await;
            }
        }
    };

    let sessions = async {
        loop {
            if let Err(e) = session(
                &mut peripheral,
                &server,
                &input_report,
                &boot_report,
                &stack,
                state,
            )
            .await
            {
                log::error!("ble: session failed: {e:?}");
                Timer::after(ERROR_BACKOFF).await;
            }
        }
    };

    embassy_futures::join::join(hci, sessions).await;
}

/// Advertises until a host connects, then pumps [`KEYS`] until it goes away.
async fn session<C: Controller>(
    peripheral: &mut Peripheral<'_, C, DefaultPacketPool>,
    server: &HidServer<'_>,
    input_report: &Characteristic<[u8; 8]>,
    boot_report: &Characteristic<[u8; 8]>,
    stack: &Stack<'_, C, DefaultPacketPool>,
    state: &'static AppState,
) -> Result<(), BleHostError<C::Error>> {
    let mut adv_data = [0u8; 31];
    let adv_len = AdStructure::encode_slice(
        &[
            AdStructure::Flags(LE_GENERAL_DISCOVERABLE | BR_EDR_NOT_SUPPORTED),
            AdStructure::Unknown {
                ty: 0x19,
                data: &APPEARANCE_KEYBOARD,
            },
            AdStructure::ServiceUuids16(&[HID_SERVICE_UUID]),
            AdStructure::CompleteLocalName(DEVICE_NAME.as_bytes()),
        ],
        &mut adv_data[..],
    )?;

    // A real scan response rather than an empty one: a host that actively
    // scans asks for this, and answering with nothing is not the same as
    // answering.
    let mut scan_data = [0u8; 31];
    let scan_len = AdStructure::encode_slice(
        &[
            AdStructure::ServiceUuids16(&[HID_SERVICE_UUID]),
            AdStructure::ShortenedLocalName(b"Macropad"),
        ],
        &mut scan_data[..],
    )?;

    let params = AdvertisementParameters {
        interval_min: ADV_INTERVAL_MIN,
        interval_max: ADV_INTERVAL_MAX,
        ..AdvertisementParameters::default()
    };

    log::info!("ble: advertising {:02x?}", &adv_data[..adv_len]);
    let advertiser = peripheral
        .advertise(
            &params,
            Advertisement::ConnectableScannableUndirected {
                adv_data: &adv_data[..adv_len],
                scan_data: &scan_data[..scan_len],
            },
        )
        .await?;
    let conn = advertiser.accept().await?;
    let gatt = conn.with_attribute_server(server)?;
    log::info!("ble: host connected");

    // Connections are **not** bondable by default. A host pairing a keyboard
    // wants a bond, and without this the pairing never completes — no error on
    // either side, just a spinner that never stops.
    if let Err(e) = gatt.raw().set_bondable(true) {
        log::warn!("ble: set_bondable failed: {e:?}");
    }

    // Ask the central to pair. A HOGP keyboard is expected to send an SMP
    // Security Request on connect; without it macOS simply connects and waits
    // to be told what we need, while we wait for it to start pairing. Neither
    // side times out and nothing is logged — it just hangs.
    match gatt.raw().request_security() {
        Ok(()) => log::info!("ble: security requested"),
        Err(e) => log::warn!("ble: security request failed: {e:?}"),
    }
    state.set_ble_linked(true);

    loop {
        match embassy_futures::select::select(gatt.next(), KEYS.receive()).await {
            embassy_futures::select::Either::First(event) => match event {
                GattConnectionEvent::Disconnected { reason } => {
                    log::info!("ble: disconnected ({reason:?})");
                    state.set_ble_linked(false);
                    return Ok(());
                }
                GattConnectionEvent::Gatt { event } => match event.accept() {
                    Ok(reply) => reply.send().await,
                    Err(e) => log::warn!("ble: gatt reply failed: {e:?}"),
                },
                // Must be answered — the peer waits for it. macOS sends one
                // immediately after connecting.
                GattConnectionEvent::RequestConnectionParams(request) => {
                    if let Err(e) = request.accept(None, stack).await {
                        log::warn!("ble: connection params rejected: {e:?}");
                    }
                }
                // Numeric comparison. With no display of our own there is
                // nothing for the user to compare, so agree — and say so, since
                // silence here is a pairing that hangs.
                GattConnectionEvent::PassKeyConfirm(key) => {
                    log::info!("ble: confirming passkey {key:?}");
                    if let Err(e) = gatt.pass_key_confirm() {
                        log::warn!("ble: passkey confirm failed: {e:?}");
                    }
                }
                GattConnectionEvent::PassKeyDisplay(key) => log::info!("ble: passkey {key:?}"),
                GattConnectionEvent::PassKeyInput => {
                    log::warn!("ble: host wants a passkey typed in — unsupported");
                }
                GattConnectionEvent::PairingComplete {
                    security_level,
                    bond,
                } => {
                    log::info!("ble: paired ({security_level:?}, bond={})", bond.is_some());
                }
                GattConnectionEvent::PairingFailed(e) => {
                    log::warn!("ble: pairing failed: {e:?}");
                }
                _ => {}
            },
            embassy_futures::select::Either::Second(report) => {
                // Notify both: which one the host subscribed to depends on
                // whether it chose report or boot protocol mode, and notifying
                // an unsubscribed characteristic is a no-op rather than an
                // error — so there is nothing to lose by sending both.
                let report_result = input_report.notify(&gatt, &report).await;
                let boot_result = boot_report.notify(&gatt, &report).await;
                log::info!("ble: sent {report:02x?} report={report_result:?} boot={boot_result:?}");
            }
        }
    }
}

/// Queues one chord as a press followed by a release, so the host sees a
/// complete keystroke. Drops silently if the queue is full: a macropad with no
/// host paired must not stall the UI loop.
pub fn send_chord(modifiers: u8, usage: u8) {
    let press: Report = [modifiers, 0, usage, 0, 0, 0, 0, 0];
    if KEYS.try_send(press).is_err() || KEYS.try_send([0; 8]).is_err() {
        log::warn!("ble: key queue full, chord dropped");
    }
}
