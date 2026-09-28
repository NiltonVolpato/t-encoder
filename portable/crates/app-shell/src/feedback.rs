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
    /// Forward / clockwise dial rotation detent (523 Hz -> 659 Hz).
    DialStepForward,
    /// Backward / counter-clockwise dial rotation detent (659 Hz -> 523 Hz).
    DialStepBackward,
    /// Button click / press (crisp blip: 880 Hz for 15ms).
    Click,
    /// Low-frequency vibration buzz (e.g., long press, alarm).
    Haptic,
    /// Arbitrary frequency tone for musical or game feedback.
    Tone { hz: u32, ms: u32 },
}

static QUEUE: Mutex<RefCell<VecDeque<Feedback>>> = Mutex::new(RefCell::new(VecDeque::new()));
type WakerSlot = Mutex<RefCell<Option<fn()>>>;
static WAKER: WakerSlot = Mutex::new(RefCell::new(None));

/// Registers a callback to be invoked whenever a feedback event is signaled.
///
/// Embedded drivers (such as DRV2605 haptics or piezo buzzer) can register
/// a function here that wakes their parked async Embassy task.
pub fn register_waker(waker: fn()) {
    critical_section::with(|cs| {
        *WAKER.borrow(cs).borrow_mut() = Some(waker);
    });
}

/// Signals a feedback event to the system.
///
/// If the internal queue is full, the event is dropped to avoid unbounded memory growth,
/// and `false` is returned so the caller can report it (this crate is platform-agnostic
/// and has no logging facility of its own).
pub fn signal(feedback: Feedback) -> bool {
    let (queued, waker) = critical_section::with(|cs| {
        let mut queue = QUEUE.borrow(cs).borrow_mut();
        if queue.len() < MAX_QUEUE_CAPACITY {
            queue.push_back(feedback);
            (true, *WAKER.borrow(cs).borrow())
        } else {
            (false, None)
        }
    });
    if let Some(w) = waker {
        w();
    }
    queued
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
