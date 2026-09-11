//! System event definitions and event channel.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use launcher::Gesture;

/// Central event queue capacity.
const QUEUE_CAPACITY: usize = 32;

/// Wrapper around the active framebuffer slice for channel transit.
pub struct Framebuffer(pub &'static mut [u8]);

impl core::fmt::Debug for Framebuffer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "Framebuffer({:p}, len={})",
            self.0.as_ptr(),
            self.0.len()
        )
    }
}

impl PartialEq for Framebuffer {
    fn eq(&self, other: &Self) -> bool {
        core::ptr::eq(self.0, other.0)
    }
}

impl Eq for Framebuffer {}

/// Events delivered to the main Device event loop.
#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    /// Rotary dial rotated by `delta` detents (+1 clockwise, -1 counter-clockwise).
    Rotate(i32),
    /// Dial button short press (press and release within threshold).
    ShortPress,
    /// Dial button long press (held past threshold).
    LongPress,
    /// Touch gesture completed on the panel.
    Gesture(Gesture),
    /// Framebuffer screenshot capture request.
    Screenshot,
    /// Return of the loaned framebuffer from Core 0 after streaming.
    FramebufferReturn(Framebuffer),
}

/// Global event channel feeding the Device main loop.
pub static EVENTS: Channel<CriticalSectionRawMutex, Event, QUEUE_CAPACITY> = Channel::new();

/// Sends an event to the main Device loop, logging if the queue is full.
pub fn send(event: Event) {
    if EVENTS.try_send(event).is_err() {
        log::warn!("event: queue full, dropped an event");
    }
}
