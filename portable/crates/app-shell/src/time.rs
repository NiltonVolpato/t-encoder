// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

//! System wall-clock and local time provider interface.

use core::cell::RefCell;

use critical_section::Mutex;

/// Broken-down local wall-clock time representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WallTime {
    pub hours: u8,
    pub minutes: u8,
    pub seconds: u8,
    pub year: i16,
    pub month: u8,
    pub day: u8,
    /// 1 = Monday, ..., 7 = Sunday (ISO 8601)
    pub weekday: u8,
}

impl WallTime {
    /// 3-letter English abbreviation for the weekday (e.g. "MON").
    pub const fn weekday_str(&self) -> &'static str {
        match self.weekday {
            1 => "MON",
            2 => "TUE",
            3 => "WED",
            4 => "THU",
            5 => "FRI",
            6 => "SAT",
            _ => "SUN",
        }
    }

    /// 3-letter English abbreviation for the month (e.g. "SEP").
    pub const fn month_str(&self) -> &'static str {
        match self.month {
            1 => "JAN",
            2 => "FEB",
            3 => "MAR",
            4 => "APR",
            5 => "MAY",
            6 => "JUN",
            7 => "JUL",
            8 => "AUG",
            9 => "SEP",
            10 => "OCT",
            11 => "NOV",
            _ => "DEC",
        }
    }
}

pub type TimeProvider = fn() -> Option<WallTime>;

static TIME_PROVIDER: Mutex<RefCell<Option<TimeProvider>>> = Mutex::new(RefCell::new(None));

/// Registers a system-wide local time provider function.
pub fn register_time_provider(provider: TimeProvider) {
    critical_section::with(|cs| {
        *TIME_PROVIDER.borrow(cs).borrow_mut() = Some(provider);
    });
}

/// Retrieves the current system local time if a time provider is registered.
pub fn now() -> Option<WallTime> {
    critical_section::with(|cs| TIME_PROVIDER.borrow(cs).borrow().and_then(|p| p()))
}
