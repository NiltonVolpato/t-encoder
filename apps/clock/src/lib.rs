// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use core::any::Any;

slint::include_modules!();

#[derive(Clone)]
pub struct ClockAppFactory {
    info: app_shell::AppInfo,
}

impl ClockAppFactory {
    pub fn new() -> Self {
        let app = ClockApp::new().expect("Failed to create ClockApp");
        let info = app.global::<ClockInfo>().get_info();
        Self { info }
    }
}

impl Default for ClockAppFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl app_shell::AppFactory for ClockAppFactory {
    fn info(&self) -> app_shell::AppInfo {
        self.info.clone()
    }

    fn launch(&self, context: app_shell::ShellContext) -> Box<dyn Any> {
        let app = ClockApp::new().expect("Failed to create ClockApp");
        let initial_time = Time::new(10, 42, 35);
        setup_clock(&app, &initial_time);

        let app_weak = app.as_weak();
        let timer = slint::Timer::default();
        let time = alloc::rc::Rc::new(core::cell::RefCell::new(initial_time));
        let time_clone = time.clone();
        timer.start(
            slint::TimerMode::Repeated,
            core::time::Duration::from_secs(1),
            move || {
                if let Some(app) = app_weak.upgrade() {
                    let mut time = time_clone.borrow_mut();
                    time.tick();
                    app.set_hours(time.hours as i32);
                    app.set_minutes(time.minutes as i32);
                    app.set_seconds(time.seconds as i32);
                }
            },
        );

        let ctx = context.clone();
        theme::setup_navigation(&app, move || ctx.exit());
        app.on_exit(move || context.exit());
        let _ = app.show();

        Box::new((app, timer))
    }

    fn clone_box(&self) -> Box<dyn app_shell::AppFactory> {
        Box::new(self.clone())
    }
}

/// Representation of time for the clock app.
#[cfg_attr(test, derive(Debug, Clone, Copy, PartialEq, Eq))]
pub struct Time {
    pub hours: u8,
    pub minutes: u8,
    pub seconds: u8,
}

impl Time {
    pub const fn new(hours: u8, minutes: u8, seconds: u8) -> Self {
        Self {
            hours,
            minutes,
            seconds,
        }
    }

    /// Advances time by one second, handling minute and hour rollover.
    pub fn tick(&mut self) {
        self.seconds += 1;
        if self.seconds >= 60 {
            self.seconds = 0;
            self.minutes += 1;
            if self.minutes >= 60 {
                self.minutes = 0;
                self.hours = (self.hours + 1) % 24;
            }
        }
    }

    /// Adjusts minutes by `delta` (+1 or -1 from rotary dial), handling rollover.
    pub fn adjust_minutes(&mut self, delta: i32) {
        let total = self.minutes as i32 + delta;
        if total < 0 {
            self.minutes = (total + 60) as u8 % 60;
            self.hours = (self.hours + 23) % 24;
        } else if total >= 60 {
            self.minutes = (total % 60) as u8;
            self.hours = (self.hours + 1) % 24;
        } else {
            self.minutes = total as u8;
        }
    }
}

/// Binds default reactive controller logic to a `ClockApp` instance.
pub fn setup_clock(app: &ClockApp, initial_time: &Time) {
    app.set_hours(initial_time.hours as i32);
    app.set_minutes(initial_time.minutes as i32);
    app.set_seconds(initial_time.seconds as i32);

    let weak_app = app.as_weak();
    app.on_toggle_seconds(move || {
        if let Some(app) = weak_app.upgrade() {
            let current = app.get_show_seconds();
            app.set_show_seconds(!current);
        }
    });

    let weak_app = app.as_weak();
    app.on_adjust_minutes(move |delta| {
        if let Some(app) = weak_app.upgrade() {
            let mut time = Time::new(
                app.get_hours() as u8,
                app.get_minutes() as u8,
                app.get_seconds() as u8,
            );
            time.adjust_minutes(delta);
            app.set_hours(time.hours as i32);
            app.set_minutes(time.minutes as i32);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tick_second_rollover() {
        let mut t = Time::new(12, 30, 59);
        t.tick();
        assert_eq!(t, Time::new(12, 31, 0));
    }

    #[test]
    fn test_tick_hour_rollover() {
        let mut t = Time::new(12, 59, 59);
        t.tick();
        assert_eq!(t, Time::new(13, 0, 0));
    }

    #[test]
    fn test_tick_midnight_rollover() {
        let mut t = Time::new(23, 59, 59);
        t.tick();
        assert_eq!(t, Time::new(0, 0, 0));
    }

    #[test]
    fn test_adjust_minutes_increment() {
        let mut t = Time::new(10, 59, 0);
        t.adjust_minutes(1);
        assert_eq!(t, Time::new(11, 0, 0));
    }

    #[test]
    fn test_adjust_minutes_decrement() {
        let mut t = Time::new(0, 0, 0);
        t.adjust_minutes(-1);
        assert_eq!(t, Time::new(23, 59, 0));
    }
}
