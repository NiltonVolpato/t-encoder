//! BLE HID keyboard — the transport behind the macropad app.
//!
//! esp-radio's [`BleConnector`] is an HCI transport (`bt_hci::transport`), so
//! the host side is `trouble-host` over `ExternalController`. This module owns
//! the whole radio side: the GATT server (HID over GATT, plus the battery and
//! device-information services hosts expect), advertising, and the connection
//! loop that turns queued reports into notifications.
//!
//! Apps never see any of this. They push a [`Report`] onto [`KEYS`] and the
//! task here delivers it — the same split as the buzzer, where the app asks and
//! the firmware owns the hardware.

use bt_hci::controller::ExternalController;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use esp_hal::peripherals::BT;
use esp_hal::rng::Trng;
use esp_radio::ble::controller::BleConnector;
use trouble_host::prelude::*;

/// One HID keyboard report: a modifier bitmap, a reserved byte, then up to six
/// simultaneous keycodes. The boot-protocol layout, which every host accepts.
pub type Report = [u8; 8];

/// Depth of the queue from the UI task to the radio. Four is two full
/// keypresses (press then release) with slack; a macropad cannot outrun it by
/// hand, and dropping beats blocking the UI loop.
const KEY_QUEUE: usize = 4;

/// Reports waiting to go out. The app pushes, [`run`] delivers.
pub static KEYS: Channel<CriticalSectionRawMutex, Report, KEY_QUEUE> = Channel::new();

/// How the host should describe us. 0x03C1 is "Keyboard" under the HID
/// category, which is what makes the picker show a keyboard icon.
const APPEARANCE_KEYBOARD: [u8; 2] = [0xC1, 0x03];

/// Advertised name; also the GAP device name.
const DEVICE_NAME: &str = "T-Encoder Macropad";

/// Standard boot-keyboard report descriptor: 8 modifier bits, one reserved
/// byte, then six key slots.
const REPORT_MAP: [u8; 43] = [
    0x05, 0x01, // Usage Page (Generic Desktop)
    0x09, 0x06, // Usage (Keyboard)
    0xA1, 0x01, // Collection (Application)
    0x05, 0x07, //   Usage Page (Keyboard/Keypad)
    0x19, 0xE0, //   Usage Minimum (Left Control)
    0x29, 0xE7, //   Usage Maximum (Right GUI)
    0x15, 0x00, //   Logical Minimum (0)
    0x25, 0x01, //   Logical Maximum (1)
    0x75, 0x01, //   Report Size (1)
    0x95, 0x08, //   Report Count (8)
    0x81, 0x02, //   Input (Data, Variable, Absolute) — modifiers
    0x95, 0x01, //   Report Count (1)
    0x75, 0x08, //   Report Size (8)
    0x81, 0x01, //   Input (Constant) — reserved byte
    0x95, 0x06, //   Report Count (6)
    0x75, 0x08, //   Report Size (8)
    0x15, 0x00, //   Logical Minimum (0)
    0x25, 0x65, //   Logical Maximum (101)
    0x19, 0x00, //   Usage Minimum (0)
    0x29, 0x65, //   Usage Maximum (101)
    0x81, 0x00, //   Input (Data, Array) — the six key slots
    0xC0, // End Collection
];

/// HID Information: HID 1.11, no localization, remote-wake capable.
const HID_INFORMATION: [u8; 4] = [0x11, 0x01, 0x00, 0x02];

/// Report Reference for the input report: report id 0, type 1 (input).
const INPUT_REPORT_REFERENCE: [u8; 2] = [0x00, 0x01];

/// Vendor id source 2 (USB), a placeholder VID/PID, product version 1. Hosts
/// use this to pick a driver quirk table; without it some refuse to bond.
const PNP_ID: [u8; 7] = [0x02, 0xE5, 0x02, 0xA0, 0x00, 0x01, 0x00];

/// Reported battery level. The board has no fuel gauge; hosts show something
/// sane rather than nothing.
const BATTERY_LEVEL: u8 = 100;

/// Concurrent connections. A keyboard talks to one host at a time.
const CONNECTIONS_MAX: usize = 1;
/// L2CAP channels: ATT, plus the security manager's fixed channel.
const L2CAP_CHANNELS_MAX: usize = 3;

/// HID over GATT. `report` is the characteristic the host subscribes to.
#[gatt_service(uuid = service::HUMAN_INTERFACE_DEVICE)]
struct HidService {
    #[characteristic(uuid = characteristic::HID_INFORMATION, read, value = HID_INFORMATION)]
    info: [u8; 4],
    #[characteristic(uuid = characteristic::REPORT_MAP, read, value = REPORT_MAP)]
    report_map: [u8; 43],
    #[characteristic(uuid = characteristic::HID_CONTROL_POINT, write_without_response, value = 0)]
    control_point: u8,
    #[characteristic(uuid = characteristic::PROTOCOL_MODE, read, write_without_response, value = 1)]
    protocol_mode: u8,
    #[descriptor(uuid = descriptors::REPORT_REFERENCE, read, value = INPUT_REPORT_REFERENCE)]
    #[characteristic(uuid = characteristic::REPORT, read, notify, value = [0u8; 8])]
    report: [u8; 8],
}

/// Battery level, in percent. Hosts show this next to the device name.
#[gatt_service(uuid = service::BATTERY)]
struct BatteryService {
    #[characteristic(uuid = characteristic::BATTERY_LEVEL, read, notify, value = 100)]
    level: u8,
}

/// Who we claim to be.
#[gatt_service(uuid = service::DEVICE_INFORMATION)]
struct DeviceInformationService {
    #[characteristic(uuid = characteristic::PNP_ID, read, value = PNP_ID)]
    pnp_id: [u8; 7],
}

/// The full attribute table.
#[gatt_server]
struct Server {
    hid: HidService,
    battery: BatteryService,
    device_information: DeviceInformationService,
}

/// Owns the radio for the life of the program: advertises, accepts one host at
/// a time, and forwards queued reports as notifications.
///
/// On disconnect it advertises again, and on a stack error it rebuilds from
/// advertising rather than giving up — the only alternative on a keyboard is to
/// stop being a keyboard. It returns only if the radio never came up at all.
#[embassy_executor::task]
pub async fn task(bt: BT<'static>) {
    let connector = BleConnector::new(bt, esp_radio::ble::Config::default());
    let connector = match connector {
        Ok(connector) => connector,
        Err(e) => {
            log::error!("ble: connector init failed: {e:?}");
            return;
        }
    };
    let controller: ExternalController<_, 20> = ExternalController::new(connector);

    let mut resources: HostResources<DefaultPacketPool, CONNECTIONS_MAX, L2CAP_CHANNELS_MAX, 1> =
        HostResources::new();
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
    let stack = trouble_host::new(controller, &mut resources).set_random_generator_seed(&mut trng);
    // Just Works pairing: the board has no keypad to type a passkey into, and
    // HID hosts require an encrypted link either way.
    stack.set_io_capabilities(IoCapabilities::NoInputNoOutput);

    let server = match Server::new_with_config(GapConfig::Peripheral(PeripheralConfig {
        name: DEVICE_NAME,
        appearance: &appearance::human_interface_device::KEYBOARD,
    })) {
        Ok(server) => server,
        Err(e) => {
            log::error!("ble: gatt table too small: {e}");
            return;
        }
    };

    // No fuel gauge on this board, so the level is a constant. Reading the
    // PnP id back is how the device-information service earns its place: hosts
    // fetch it during pairing, and a mismatch here is worth seeing in the log.
    if let Err(e) = server.set(&server.battery.level, &BATTERY_LEVEL) {
        log::warn!("ble: battery level rejected: {e:?}");
    }
    match server.get(&server.device_information.pnp_id) {
        Ok(pnp) => log::info!("ble: pnp id {pnp:02x?}"),
        Err(e) => log::warn!("ble: pnp id unreadable: {e:?}"),
    }

    let Host {
        mut peripheral,
        mut runner,
        ..
    } = stack.build();

    let host = async {
        loop {
            if let Err(e) = runner.run().await {
                log::error!("ble: host runner stopped: {e:?}");
            }
        }
    };

    let sessions = async {
        loop {
            match advertise_and_serve(&mut peripheral, &server).await {
                Ok(()) => log::info!("ble: host disconnected"),
                Err(e) => log::error!("ble: session failed: {e:?}"),
            }
        }
    };

    embassy_futures::join::join(host, sessions).await;
}

/// Advertises until a host connects, then pumps [`KEYS`] until it goes away.
async fn advertise_and_serve<C: Controller>(
    peripheral: &mut Peripheral<'_, C, DefaultPacketPool>,
    server: &Server<'_>,
) -> Result<(), BleHostError<C::Error>> {
    let mut adv_data = [0u8; 31];
    let len = AdStructure::encode_slice(
        &[
            AdStructure::Flags(LE_GENERAL_DISCOVERABLE | BR_EDR_NOT_SUPPORTED),
            AdStructure::ServiceUuids16(&[service::HUMAN_INTERFACE_DEVICE.into()]),
            AdStructure::Unknown {
                ty: 0x19,
                data: &APPEARANCE_KEYBOARD,
            },
            AdStructure::CompleteLocalName(DEVICE_NAME.as_bytes()),
        ],
        &mut adv_data[..],
    )?;

    log::info!("ble: advertising as {DEVICE_NAME}");
    let advertiser = peripheral
        .advertise(
            &AdvertisementParameters::default(),
            Advertisement::ConnectableScannableUndirected {
                adv_data: &adv_data[..len],
                scan_data: &[],
            },
        )
        .await?;
    let conn = advertiser.accept().await?.with_attribute_server(server)?;
    log::info!("ble: host connected");

    loop {
        match embassy_futures::select::select(conn.next(), KEYS.receive()).await {
            embassy_futures::select::Either::First(event) => match event {
                GattConnectionEvent::Disconnected { reason } => {
                    log::info!("ble: disconnected ({reason:?})");
                    return Ok(());
                }
                GattConnectionEvent::Gatt { event } => {
                    if let Err(e) = event.accept() {
                        log::warn!("ble: gatt reply failed: {e:?}");
                    }
                }
                _ => {}
            },
            embassy_futures::select::Either::Second(report) => {
                if let Err(e) = server.hid.report.notify(&conn, &report).await {
                    log::warn!("ble: notify failed: {e:?}");
                }
            }
        }
    }
}
