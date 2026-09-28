#![no_std]

extern crate alloc;

use alloc::collections::VecDeque;
use core::cell::RefCell;

use critical_section::Mutex;
use slint::ComponentHandle;

slint::include_modules!();

/// System-level device menu snapshot (battery, Wi-Fi, brightness).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemMenuState {
    pub battery_percent: i32,
    pub wifi_connected: bool,
    pub wifi_ssid: heapless::String<32>,
    pub is_provisioning: bool,
    pub brightness_percent: i32,
}

impl Default for SystemMenuState {
    fn default() -> Self {
        Self {
            battery_percent: 100,
            wifi_connected: false,
            wifi_ssid: heapless::String::new(),
            is_provisioning: false,
            brightness_percent: 80,
        }
    }
}

static SYSTEM_MENU_STATE: Mutex<RefCell<SystemMenuState>> =
    Mutex::new(RefCell::new(SystemMenuState {
        battery_percent: 100,
        wifi_connected: false,
        wifi_ssid: heapless::String::new(),
        is_provisioning: false,
        brightness_percent: 80,
    }));

/// Actions dispatched from the Slint Control Center UI to the platform layer.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SystemMenuAction {
    RequestProvisioning,
    StopProvisioning,
    BrightnessChanged(i32),
    DrawerClosed,
}

static ACTION_QUEUE: Mutex<RefCell<VecDeque<SystemMenuAction>>> =
    Mutex::new(RefCell::new(VecDeque::new()));

/// Queues an action from the UI to be handled by the platform layer.
pub fn send_system_menu_action(action: SystemMenuAction) -> bool {
    critical_section::with(|cs| {
        let mut q = ACTION_QUEUE.borrow(cs).borrow_mut();
        if q.len() < 16 {
            q.push_back(action);
            true
        } else {
            false
        }
    })
}

/// Receives the next pending action requested by the UI.
pub fn try_receive_system_menu_action() -> Option<SystemMenuAction> {
    critical_section::with(|cs| ACTION_QUEUE.borrow(cs).borrow_mut().pop_front())
}

/// Updates the shared system menu state.
pub fn update_system_menu_state(f: impl FnOnce(&mut SystemMenuState)) {
    critical_section::with(|cs| {
        f(&mut SYSTEM_MENU_STATE.borrow(cs).borrow_mut());
    });
}

/// Returns a clone of the current system menu state.
pub fn get_system_menu_state() -> SystemMenuState {
    critical_section::with(|cs| SYSTEM_MENU_STATE.borrow(cs).borrow().clone())
}

#[allow(dead_code)]
struct SendTimer(slint::Timer);
unsafe impl Send for SendTimer {}

static SYNC_TIMER: Mutex<RefCell<Option<SendTimer>>> = Mutex::new(RefCell::new(None));

/// Configures default exit navigation, threshold haptics, and system menu sync for an application.
pub fn setup_navigation<T: ComponentHandle + 'static>(
    app: &T,
    on_exit: impl Fn() + 'static,
    on_threshold: impl Fn() + 'static,
) where
    for<'a> Navigation<'a>: slint::Global<'a, T>,
    for<'a> SystemMenu<'a>: slint::Global<'a, T>,
{
    let nav = Navigation::get(app);
    nav.on_exit(on_exit);
    nav.on_threshold_reached(on_threshold);

    let menu = SystemMenu::get(app);
    let state = get_system_menu_state();
    menu.set_battery_percent(state.battery_percent);
    menu.set_wifi_connected(state.wifi_connected);
    menu.set_wifi_ssid(state.wifi_ssid.as_str().into());
    menu.set_is_provisioning(state.is_provisioning);
    menu.set_brightness_percent(state.brightness_percent);

    menu.on_request_provisioning(|| {
        send_system_menu_action(SystemMenuAction::RequestProvisioning);
    });
    menu.on_stop_provisioning(|| {
        send_system_menu_action(SystemMenuAction::StopProvisioning);
    });
    menu.on_brightness_changed(|val| {
        let clamped = clamp_brightness(val);
        update_system_menu_state(|s| s.brightness_percent = clamped);
        send_system_menu_action(SystemMenuAction::BrightnessChanged(clamped));
    });
    menu.on_drawer_closed(|| {
        send_system_menu_action(SystemMenuAction::DrawerClosed);
    });

    let app_weak = app.as_weak();
    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, core::time::Duration::from_millis(500), move || {
        if let Some(app) = app_weak.upgrade() {
            let menu = SystemMenu::get(&app);
            let state = get_system_menu_state();
            if menu.get_battery_percent() != state.battery_percent {
                menu.set_battery_percent(state.battery_percent);
            }
            if menu.get_wifi_connected() != state.wifi_connected {
                menu.set_wifi_connected(state.wifi_connected);
            }
            if menu.get_wifi_ssid().as_str() != state.wifi_ssid.as_str() {
                menu.set_wifi_ssid(state.wifi_ssid.as_str().into());
            }
            if menu.get_is_provisioning() != state.is_provisioning {
                menu.set_is_provisioning(state.is_provisioning);
            }
            if menu.get_brightness_percent() != state.brightness_percent {
                menu.set_brightness_percent(state.brightness_percent);
            }
        }
    });

    critical_section::with(|cs| {
        *SYNC_TIMER.borrow(cs).borrow_mut() = Some(SendTimer(timer));
    });
}

/// Minimum allowable display brightness percentage (prevents complete blackouts).
pub const MIN_BRIGHTNESS_PERCENT: i32 = 15;

/// Maximum allowable display brightness percentage.
pub const MAX_BRIGHTNESS_PERCENT: i32 = 100;

/// Clamps brightness percentage to safe bounds [15, 100].
#[inline]
pub const fn clamp_brightness(percent: i32) -> i32 {
    if percent < MIN_BRIGHTNESS_PERCENT {
        MIN_BRIGHTNESS_PERCENT
    } else if percent > MAX_BRIGHTNESS_PERCENT {
        MAX_BRIGHTNESS_PERCENT
    } else {
        percent
    }
}

/// Converts brightness percent (15..=100) to 8-bit PWM duty cycle (0..=255).
#[inline]
pub const fn brightness_percent_to_duty(percent: i32) -> u8 {
    let clamped = clamp_brightness(percent);
    ((clamped as u32 * 255) / 100) as u8
}

/// Converts 8-bit PWM duty cycle (0..=255) to brightness percent (15..=100).
#[inline]
pub const fn duty_to_brightness_percent(duty: u8) -> i32 {
    let pct = ((duty as u32 * 100 + 127) / 255) as i32;
    clamp_brightness(pct)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clamp_brightness() {
        assert_eq!(clamp_brightness(0), 15);
        assert_eq!(clamp_brightness(10), 15);
        assert_eq!(clamp_brightness(15), 15);
        assert_eq!(clamp_brightness(50), 50);
        assert_eq!(clamp_brightness(100), 100);
        assert_eq!(clamp_brightness(120), 100);
    }

    #[test]
    fn test_brightness_duty_conversion() {
        // Minimum brightness 15% should be > 0 duty (never dark)
        let min_duty = brightness_percent_to_duty(15);
        assert_eq!(min_duty, 38);
        assert!(min_duty > 0);

        // Maximum brightness 100% should be full 255 duty
        let max_duty = brightness_percent_to_duty(100);
        assert_eq!(max_duty, 255);

        // Clamping ensures out-of-range values produce safe duty
        assert_eq!(brightness_percent_to_duty(0), 38);
        assert_eq!(brightness_percent_to_duty(200), 255);

        // Duty to percent converts correctly
        assert_eq!(duty_to_brightness_percent(0), 15);
        assert_eq!(duty_to_brightness_percent(38), 15);
        assert_eq!(duty_to_brightness_percent(255), 100);
    }

    #[test]
    fn test_drawer_position_math() {
        let drawer_height: f32 = 315.0;
        let drawer_open_y: f32 = 0.0;
        let threshold: f32 = 180.0;

        let calc_y = |drag_dist: f32| -> f32 {
            let p = (drag_dist / threshold).min(1.0);
            -drawer_height + p * (drawer_height + drawer_open_y)
        };

        // When drag starts, drawer is hidden above screen
        assert_eq!(calc_y(0.0), -315.0);
        // Halfway through drag
        assert_eq!(calc_y(90.0), -157.5);
        // At threshold, drawer is at full open position (0.0)
        assert_eq!(calc_y(180.0), 0.0);
        // Past threshold, drawer is clamped to open position
        assert_eq!(calc_y(220.0), 0.0);
    }
}
