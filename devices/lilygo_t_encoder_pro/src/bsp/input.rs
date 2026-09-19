// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

//! Input events and central event queue for LilyGO T-Encoder Pro.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;

use super::touch::TouchPoint;

/// Central input event queue capacity.
const QUEUE_CAPACITY: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputEvent {
    /// Rotary dial rotated by delta detents (+1 clockwise, -1 counter-clockwise).
    Rotate(i32),
    /// Dial button short click (pressed and released within threshold).
    Click,
    /// Dial button long press (held past threshold).
    LongPress,
    /// Touch event on capacitive panel.
    Touch(TouchPoint),
}

/// Global event channel feeding the Slint event loop.
pub static INPUT_EVENTS: Channel<CriticalSectionRawMutex, InputEvent, QUEUE_CAPACITY> =
    Channel::new();

/// Dispatches an input event to the channel, logging a warning if the queue is full.
pub fn send_input_event(event: InputEvent) {
    if INPUT_EVENTS.try_send(event).is_err() {
        defmt::warn!("INPUT_EVENTS channel full, dropped event");
    }
}
