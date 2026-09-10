//! Wi-Fi and BLE bring-up, behind the `radio` feature (on by default).
//!
//! The feature exists for `just qemu`. QEMU emulates neither radio, so nothing
//! here could work there anyway — but the reason it has to be a *cargo feature*
//! rather than an `if` in `main` is sharper than that: merely depending on
//! `esp-radio` force-enables `xtensa-lx-rt/float-save-restore`, and the FPU
//! context save that pulls in (`rur.fcr`) segfaults qemu-system-xtensa itself
//! on the first exception. The dependency has to leave the graph, not just the
//! call path.
//!
//! Both builds hand `main` the same shape, so the UI loop needs no `cfg`.

use embassy_executor::Spawner;
use embassy_net::Stack;
#[cfg(feature = "radio")]
use embassy_time::{Duration, Timer};
#[cfg(feature = "radio")]
use enc_state::{AppState, ConnState};
use esp_hal::peripherals::{ADC1, BT, RNG, WIFI};

#[cfg(feature = "radio")]
static APP_STATE: AppState = AppState::new(1);

/// Everything the radios own, moved across in one go so `main` gives them up
/// exactly once whether or not the feature is on.
pub struct Parts {
    pub wifi: WIFI<'static>,
    pub bt: BT<'static>,
    pub rng: RNG<'static>,
    pub adc1: ADC1<'static>,
}

/// Keeps the TRNG that backs the BLE security manager alive: the generator only
/// exists while this guard does. `main` never returns, so binding it there
/// keeps entropy available for the life of the program.
pub struct Guard {
    #[cfg(feature = "radio")]
    _trng: esp_hal::rng::TrngSource<'static>,
}

/// Periodically observes the DHCP lease and mirrors the IP into `AppState`.
#[cfg(feature = "radio")]
#[embassy_executor::task]
pub async fn net_monitor_task(stack: Stack<'static>) {
    let mut had_ip = false;
    loop {
        if let Some(cfg) = stack.config_v4() {
            let octets = cfg.address.address().octets();
            APP_STATE.set_ip(octets);
            APP_STATE.set_conn(ConnState::Connected);
            if !had_ip {
                had_ip = true;
                let [a, b, c, d] = octets;
                log::info!("net: ip={a}.{b}.{c}.{d}");
            }
        } else {
            APP_STATE.clear_ip();
            had_ip = false;
        }
        Timer::after(Duration::from_secs(1)).await;
    }
}

/// Starts Wi-Fi STA (with embassy-net) and the BLE HID keyboard task.
///
/// The returned stack is `None` when Wi-Fi could not start — including when the
/// SSID was never set at build time, which is not an error.
#[cfg(feature = "radio")]
pub fn start(spawner: Spawner, parts: Parts) -> (Option<Stack<'static>>, Guard) {
    // A random seed salts DHCP transaction IDs and ephemeral ports; the tasks
    // own association, and the UI loop polls the lease for the IP.
    let rng = esp_hal::rng::Rng::new();
    let [a0, a1, a2, a3] = rng.random().to_be_bytes();
    let [b0, b1, b2, b3] = rng.random().to_be_bytes();
    let seed = u64::from_be_bytes([a0, a1, a2, a3, b0, b1, b2, b3]);
    let stack = crate::net::start(&spawner, parts.wifi, seed, &APP_STATE);
    if let Some(stack) = stack {
        match net_monitor_task(stack) {
            Ok(token) => spawner.spawn(token),
            Err(_) => log::error!("boot: failed to spawn net monitor task"),
        }
    } else {
        log::error!("net: Wi-Fi stack unavailable");
    }

    let guard = Guard {
        _trng: esp_hal::rng::TrngSource::new(parts.rng, parts.adc1),
    };
    match crate::ble::task(parts.bt, &APP_STATE) {
        Ok(token) => spawner.spawn(token),
        Err(_) => log::error!("boot: failed to spawn ble task"),
    }

    (stack, guard)
}

/// No-radio build: consumes the peripherals and reports the absence once, so a
/// QEMU log makes clear the silence is deliberate rather than a failure.
#[cfg(not(feature = "radio"))]
pub fn start(_spawner: Spawner, parts: Parts) -> (Option<Stack<'static>>, Guard) {
    // Destructured rather than dropped whole: it releases WIFI/BT/RNG/ADC1 back
    // just the same, and it keeps the fields "read" so the no-radio build stays
    // warning-clean without an `allow`.
    let Parts {
        wifi,
        bt,
        rng,
        adc1,
    } = parts;
    let _ = (wifi, bt, rng, adc1);
    log::info!("radio: built without the `radio` feature — no Wi-Fi, no BLE");
    (None, Guard {})
}

/// Returns whether a BLE host is currently connected and paired.
pub fn ble_linked() -> bool {
    #[cfg(feature = "radio")]
    {
        APP_STATE.ble_linked()
    }
    #[cfg(not(feature = "radio"))]
    {
        false
    }
}

/// Returns Wi-Fi IP and connected status, if available.
pub fn wifi_info() -> Option<([u8; 4], bool)> {
    #[cfg(feature = "radio")]
    {
        let connected = APP_STATE.conn() == ConnState::Connected;
        APP_STATE.ip().map(|ip| (ip, connected))
    }
    #[cfg(not(feature = "radio"))]
    {
        None
    }
}

/// Queues one HID chord for the BLE task. A no-op without the feature; the
/// caller still drains the router either way, so nothing accumulates.
#[cfg(feature = "radio")]
pub fn send_chord(modifiers: u8, usage: u8) {
    crate::ble::send_chord(modifiers, usage);
}

#[cfg(not(feature = "radio"))]
pub fn send_chord(_modifiers: u8, _usage: u8) {}

/// Signals the BLE subsystem to enable advertising and accept connections.
pub fn enable_ble() {
    #[cfg(feature = "radio")]
    crate::ble::enable_ble();
}

/// Signals the BLE subsystem to stop advertising and disconnect any active link.
pub fn disable_ble() {
    #[cfg(feature = "radio")]
    crate::ble::disable_ble();
}
