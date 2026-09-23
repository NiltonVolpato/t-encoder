// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

//! Unified system event model and central event queue for LilyGO T-Encoder Pro.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;

use super::touch::TouchPoint;

/// Central event queue capacity.
const QUEUE_CAPACITY: usize = 32;

/// User input events from physical controls.
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

/// Screen display power and brightness events.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ScreenEvent {
    /// Dim display relative to user's configured base brightness (e.g. 0.50 for 50%, 0.25 for 25%).
    DimRelative(f32),
    /// Set absolute brightness level (0..255).
    DimAbsolute(u8),
    /// Turn off display panel (sleep mode).
    TurnOff,
    /// Turn on display panel (wake mode).
    TurnOn,
}

/// Unified system events dispatched through the central main loop.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Event {
    /// User input interaction.
    Input(InputEvent),
    /// Screen power/brightness change request.
    Screen(ScreenEvent),
}

/// Global event channel feeding the main event loop.
pub static EVENTS: Channel<CriticalSectionRawMutex, Event, QUEUE_CAPACITY> = Channel::new();

/// Dispatches a system event to the central channel.
pub fn send_event(event: Event) {
    if EVENTS.try_send(event).is_err() {
        defmt::error!("EVENTS channel full, dropped event");
    }
}

/// Helper to dispatch user input events directly.
pub fn send_input_event(input: InputEvent) {
    send_event(Event::Input(input));
}
