//! The launcher's apps.
//!
//! Pure and host-testable: an app owns its state and pushes it into the shared
//! Slint tree. It knows nothing about SPI, PSRAM or Embassy, and it does not
//! paint — Slint renders from the properties the app sets.

#![no_std]

mod pomodoro;

pub use pomodoro::Pomodoro;
