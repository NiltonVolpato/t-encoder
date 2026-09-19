// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

//! Display and screen power management service.

use core::time::Duration;

pub const DEFAULT_DIM_TIMEOUT: Duration = Duration::from_secs(30);
pub const DEFAULT_SLEEP_TIMEOUT: Duration = Duration::from_secs(60);
pub const DEFAULT_BRIGHTNESS: u8 = 0xFF;
pub const DEFAULT_DIM_BRIGHTNESS: u8 = 0x80; // Half brightness

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ScreenPowerState {
    Active,
    Dimmed,
    Sleeping,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum WakeAction {
    DispatchEvent,
    SwallowEvent,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PowerTransition {
    None,
    DimTo(u8),
    Sleep,
    WakeFromDim(u8),
    WakeFromSleep(u8),
}

#[derive(Clone, Debug)]
pub struct ScreenPowerManager {
    state: ScreenPowerState,
    dim_timeout: Duration,
    sleep_timeout: Duration,
    default_brightness: u8,
    dim_brightness: u8,
}

impl Default for ScreenPowerManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ScreenPowerManager {
    pub fn new() -> Self {
        Self {
            state: ScreenPowerState::Active,
            dim_timeout: DEFAULT_DIM_TIMEOUT,
            sleep_timeout: DEFAULT_SLEEP_TIMEOUT,
            default_brightness: DEFAULT_BRIGHTNESS,
            dim_brightness: DEFAULT_DIM_BRIGHTNESS,
        }
    }

    pub fn with_timeouts(mut self, dim_timeout: Duration, sleep_timeout: Duration) -> Self {
        self.dim_timeout = dim_timeout;
        self.sleep_timeout = sleep_timeout;
        self
    }

    pub fn state(&self) -> ScreenPowerState {
        self.state
    }

    pub fn is_sleeping(&self) -> bool {
        self.state == ScreenPowerState::Sleeping
    }

    pub fn is_dimmed(&self) -> bool {
        self.state == ScreenPowerState::Dimmed
    }

    pub fn is_active(&self) -> bool {
        self.state == ScreenPowerState::Active
    }

    pub fn default_brightness(&self) -> u8 {
        self.default_brightness
    }

    pub fn dim_brightness(&self) -> u8 {
        self.dim_brightness
    }

    pub fn set_default_brightness(&mut self, brightness: u8) {
        self.default_brightness = brightness;
    }

    pub fn set_dim_brightness(&mut self, brightness: u8) {
        self.dim_brightness = brightness;
    }

    /// Evaluates inactivity time and advances the power state if needed.
    /// Returns the required hardware transition, if any.
    pub fn update(&mut self, idle_time: Duration) -> PowerTransition {
        match self.state {
            ScreenPowerState::Active => {
                if idle_time >= self.sleep_timeout {
                    self.state = ScreenPowerState::Sleeping;
                    PowerTransition::Sleep
                } else if idle_time >= self.dim_timeout {
                    self.state = ScreenPowerState::Dimmed;
                    PowerTransition::DimTo(self.dim_brightness)
                } else {
                    PowerTransition::None
                }
            }
            ScreenPowerState::Dimmed => {
                if idle_time >= self.sleep_timeout {
                    self.state = ScreenPowerState::Sleeping;
                    PowerTransition::Sleep
                } else {
                    PowerTransition::None
                }
            }
            ScreenPowerState::Sleeping => PowerTransition::None,
        }
    }

    /// Handles user input activity, returning whether the event should be dispatched or swallowed.
    /// Also returns the required hardware transition to restore the display.
    pub fn handle_input(&mut self) -> (WakeAction, PowerTransition) {
        match self.state {
            ScreenPowerState::Sleeping => {
                self.state = ScreenPowerState::Active;
                (
                    WakeAction::SwallowEvent,
                    PowerTransition::WakeFromSleep(self.default_brightness),
                )
            }
            ScreenPowerState::Dimmed => {
                self.state = ScreenPowerState::Active;
                (
                    WakeAction::DispatchEvent,
                    PowerTransition::WakeFromDim(self.default_brightness),
                )
            }
            ScreenPowerState::Active => (WakeAction::DispatchEvent, PowerTransition::None),
        }
    }

    /// Returns the maximum duration until the next power state transition,
    /// or `None` if the screen is already sleeping.
    pub fn time_until_next_transition(&self, idle_time: Duration) -> Option<Duration> {
        match self.state {
            ScreenPowerState::Active => {
                if idle_time < self.dim_timeout {
                    Some(self.dim_timeout - idle_time)
                } else if idle_time < self.sleep_timeout {
                    Some(self.sleep_timeout - idle_time)
                } else {
                    Some(Duration::ZERO)
                }
            }
            ScreenPowerState::Dimmed => {
                if idle_time < self.sleep_timeout {
                    Some(self.sleep_timeout - idle_time)
                } else {
                    Some(Duration::ZERO)
                }
            }
            ScreenPowerState::Sleeping => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initial_state() {
        let manager = ScreenPowerManager::new();
        assert_eq!(manager.state(), ScreenPowerState::Active);
        assert!(manager.is_active());
        assert!(!manager.is_dimmed());
        assert!(!manager.is_sleeping());
        assert_eq!(manager.default_brightness(), 0xFF);
        assert_eq!(manager.dim_brightness(), 0x80);
    }

    #[test]
    fn test_active_input_does_not_swallow() {
        let mut manager = ScreenPowerManager::new();
        let (action, transition) = manager.handle_input();
        assert_eq!(action, WakeAction::DispatchEvent);
        assert_eq!(transition, PowerTransition::None);
        assert!(manager.is_active());
    }

    #[test]
    fn test_dim_transition() {
        let mut manager = ScreenPowerManager::new();

        // Under 30s: still active
        let t1 = manager.update(Duration::from_secs(29));
        assert_eq!(t1, PowerTransition::None);
        assert!(manager.is_active());

        // At 30s: transitions to Dimmed
        let t2 = manager.update(Duration::from_secs(30));
        assert_eq!(t2, PowerTransition::DimTo(0x80));
        assert!(manager.is_dimmed());

        // Subsequent updates while dimmed under 60s
        let t3 = manager.update(Duration::from_secs(45));
        assert_eq!(t3, PowerTransition::None);
        assert!(manager.is_dimmed());
    }

    #[test]
    fn test_sleep_transition_from_dimmed() {
        let mut manager = ScreenPowerManager::new();
        manager.update(Duration::from_secs(30));
        assert!(manager.is_dimmed());

        // At 60s: transitions to Sleeping
        let t = manager.update(Duration::from_secs(60));
        assert_eq!(t, PowerTransition::Sleep);
        assert!(manager.is_sleeping());

        // Subsequent updates while sleeping
        let t2 = manager.update(Duration::from_secs(100));
        assert_eq!(t2, PowerTransition::None);
        assert!(manager.is_sleeping());
    }

    #[test]
    fn test_direct_sleep_transition_from_active() {
        let mut manager = ScreenPowerManager::new();
        // Jump directly to 65s
        let t = manager.update(Duration::from_secs(65));
        assert_eq!(t, PowerTransition::Sleep);
        assert!(manager.is_sleeping());
    }

    #[test]
    fn test_wake_from_dimmed_dispatches_event() {
        let mut manager = ScreenPowerManager::new();
        manager.update(Duration::from_secs(35));
        assert!(manager.is_dimmed());

        let (action, transition) = manager.handle_input();
        assert_eq!(action, WakeAction::DispatchEvent);
        assert_eq!(transition, PowerTransition::WakeFromDim(0xFF));
        assert!(manager.is_active());
    }

    #[test]
    fn test_wake_from_sleeping_swallows_event() {
        let mut manager = ScreenPowerManager::new();
        manager.update(Duration::from_secs(65));
        assert!(manager.is_sleeping());

        let (action, transition) = manager.handle_input();
        assert_eq!(action, WakeAction::SwallowEvent);
        assert_eq!(transition, PowerTransition::WakeFromSleep(0xFF));
        assert!(manager.is_active());
    }

    #[test]
    fn test_time_until_next_transition() {
        let mut manager = ScreenPowerManager::new();

        // In Active state:
        assert_eq!(
            manager.time_until_next_transition(Duration::from_secs(10)),
            Some(Duration::from_secs(20))
        );
        assert_eq!(
            manager.time_until_next_transition(Duration::from_secs(30)),
            Some(Duration::from_secs(30))
        );

        // In Dimmed state:
        manager.update(Duration::from_secs(30));
        assert_eq!(
            manager.time_until_next_transition(Duration::from_secs(45)),
            Some(Duration::from_secs(15))
        );
        assert_eq!(
            manager.time_until_next_transition(Duration::from_secs(60)),
            Some(Duration::ZERO)
        );

        // In Sleeping state:
        manager.update(Duration::from_secs(60));
        assert_eq!(manager.time_until_next_transition(Duration::from_secs(65)), None);
    }
}
