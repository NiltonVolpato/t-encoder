// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

//! System feedback service (buzzer, haptics, and musical tones).

use alloc::collections::VecDeque;
use core::cell::RefCell;
use critical_section::Mutex;

const MAX_QUEUE_CAPACITY: usize = 16;

/// Audible and haptic feedback requests.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Feedback {
    /// Two-tone audible click/chirp (e.g., dial rotation detent, short press).
    Beep,
    /// Low-frequency vibration buzz (e.g., long press, alarm).
    Haptic,
    /// Arbitrary frequency tone for musical or game feedback.
    Tone { hz: u32, ms: u32 },
}

static QUEUE: Mutex<RefCell<VecDeque<Feedback>>> = Mutex::new(RefCell::new(VecDeque::new()));

/// Signals a feedback event to the system.
///
/// If the internal queue is full, the event is dropped to avoid unbounded memory growth.
pub fn signal(feedback: Feedback) {
    critical_section::with(|cs| {
        let mut queue = QUEUE.borrow(cs).borrow_mut();
        if queue.len() < MAX_QUEUE_CAPACITY {
            queue.push_back(feedback);
        }
    });
}

/// Attempts to receive the next queued feedback event.
///
/// Returns `None` if no feedback events are currently pending.
pub fn try_receive() -> Option<Feedback> {
    critical_section::with(|cs| QUEUE.borrow(cs).borrow_mut().pop_front())
}

/// Clears all pending feedback events from the queue.
pub fn clear() {
    critical_section::with(|cs| QUEUE.borrow(cs).borrow_mut().clear());
}
