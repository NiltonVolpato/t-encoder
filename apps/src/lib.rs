//! The launcher's apps.
//!
//! Pure and host-testable: an app owns its state and pushes it into the shared
//! Slint tree. It knows nothing about SPI, PSRAM or Embassy, and it does not
//! paint — Slint renders from the properties the app sets.

#![no_std]

extern crate alloc;

mod macropad;
mod magic8;
mod pomodoro;
mod simon;

pub use macropad::{Macropad, MacropadFactory};
pub use magic8::{Magic8, Magic8Factory};
pub use pomodoro::{Pomodoro, PomodoroFactory};
pub use simon::{Simon, SimonFactory};
