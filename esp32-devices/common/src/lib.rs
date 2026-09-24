// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

//! Shared platform layer and tasks for ESP32 devices running Slint.

#![no_std]
#![feature(asm_experimental_arch)]

extern crate alloc;

pub mod channels;
pub mod event;
pub mod platform;
pub mod tasks;

pub use channels::*;
pub use event::*;
pub use platform::*;
