// Adapted from rust-enc (https://github.com/rayslava/rust-enc), MIT licensed.
// Copyright (c) rayslava. Copied from enc-app, which is a binary crate and so
// cannot be reached by path dependency; diverges from upstream from here on.

//! CHSC5816 touch bring-up, and the stroke stream it feeds the router.
//!
//! [`task`] listens for falling edges on the INT line (GPIO9) and reads
//! touch reports. It forwards them to the launcher gesture recogniser.
//!
//! When idle, the task sleeps on `wait_for_interrupt()` with zero I2C bus traffic.
//! During an active stroke, it waits for subsequent falling edges with a 35 ms
//! watchdog timeout to ensure lifts are never stranded if a release edge is missed.
//!
//! Touch is the board's only I2C device — I2C1 is entirely free.

use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Delay, Duration, Instant, Timer};
use enc_touch::{Chsc5816, TouchEvent, TouchPoint};
use esp_hal::Async;
use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull};
use esp_hal::i2c::master::{Config, I2c};
use esp_hal::peripherals::{GPIO5, GPIO6, GPIO8, GPIO9, I2C0};
use launcher::{Recognizer, TouchSample};

/// I2C address of the touch controller on this board. Upstream's docs claim a
/// CST816 at 0x15; this unit is a CHSC5816 at 0x2E, confirmed by observation.
const ADDRESS: u8 = 0x2E;

/// Maximum duration to wait between touch reports during an active stroke
/// before assuming the finger has lifted. The controller reports at ~80–100 Hz
/// (~10–12 ms) while a finger is down.
const STROKE_TIMEOUT: Duration = Duration::from_millis(35);

/// Concrete CHSC5816 type on this board (async I2C + INT/RST GPIO).
pub type Touch = Chsc5816<I2c<'static, Async>, Input<'static>, Output<'static>>;

/// Button events queued for phantom touch suppression.
static BUTTON_EVENTS: Channel<CriticalSectionRawMutex, (bool, u64), 8> = Channel::new();

/// Notifies the touch driver of button state changes to suppress phantom touches.
pub fn set_button(down: bool) {
    let at_ms = Instant::now().as_millis();
    let _ = BUTTON_EVENTS.try_send((down, at_ms));
}

/// Drains queued button events into the recognizer.
fn sync_button(recognizer: &mut Recognizer) {
    while let Ok((down, at_ms)) = BUTTON_EVENTS.try_receive() {
        recognizer.set_button(down, at_ms);
    }
}

/// Pins/peripherals the touch controller needs.
pub struct TouchPins {
    /// I2C bus the controller sits on.
    pub i2c: I2C0<'static>,
    /// I2C data line.
    pub sda: GPIO5<'static>,
    /// I2C clock line.
    pub scl: GPIO6<'static>,
    /// Falling-edge interrupt line.
    pub int: GPIO9<'static>,
    /// Active-low reset.
    pub rst: GPIO8<'static>,
}

/// Sets up the async I2C bus and the CHSC5816, pulsing its reset line.
///
/// Returns `None` if the I2C peripheral cannot be configured or the controller
/// does not come up; touch is optional, and the encoder still drives everything.
pub async fn init(pins: TouchPins) -> Option<Touch> {
    let i2c = match I2c::new(pins.i2c, Config::default()) {
        Ok(i2c) => i2c.with_sda(pins.sda).with_scl(pins.scl).into_async(),
        Err(e) => {
            log::error!("touch: i2c init failed: {e:?}");
            return None;
        }
    };
    let interrupt = Input::new(pins.int, InputConfig::default().with_pull(Pull::Up));
    let reset = Output::new(pins.rst, Level::High, OutputConfig::default());
    let mut touch = Chsc5816::new(i2c, interrupt, reset, ADDRESS);
    if let Err(e) = touch.init(&mut Delay).await {
        log::error!("touch: init failed: {e:?}");
        return None;
    }
    Some(touch)
}

/// Emits a touch lift ([`TouchEvent::Up`]) sample and logs stroke travel.
fn emit_up(
    recognizer: &mut Recognizer,
    contact: &mut Option<(u16, u16)>,
    landed: &mut Option<(u16, u16, u64)>,
    points: u32,
) {
    if let Some((x, y)) = contact.take() {
        let at_ms = Instant::now().as_millis();
        log::trace!("touch: emit Up {x},{y}");
        if let Some((x0, y0, t0)) = landed.take() {
            let ms = at_ms.saturating_sub(t0);
            log::info!("touch: {x0},{y0} -> {x},{y} {ms}ms {points}p");
        }
        if let Some(gesture) = recognizer.push(TouchSample {
            point: TouchPoint {
                x,
                y,
                event: TouchEvent::Up,
            },
            at_ms,
        }) {
            crate::event::send(crate::event::Event::Gesture(gesture));
        }
    }
}

/// Waits for hardware touch interrupts and recognizes gestures.
#[embassy_executor::task]
pub async fn task(mut touch: Touch) {
    let mut recognizer = Recognizer::new();
    // Where the finger was last seen, and therefore whether one is down.
    let mut contact: Option<(u16, u16)> = None;
    // Where and when the current stroke started, and how many samples it has
    // taken — all three only for the travel log.
    let mut landed: Option<(u16, u16, u64)> = None;
    let mut points: u32 = 0;

    loop {
        let is_touching = contact.is_some();
        if is_touching {
            // Active stroke: wait for next report edge with watchdog timeout.
            match select(touch.wait_for_interrupt(), Timer::after(STROKE_TIMEOUT)).await {
                Either::First(Ok(())) => {}
                Either::First(Err(e)) => {
                    log::error!("touch: interrupt wait error: {e:?}");
                }
                Either::Second(()) => {
                    // Watchdog fired: no edge for STROKE_TIMEOUT -> finger has lifted.
                    log::trace!("touch: watchdog timeout (Up)");
                    sync_button(&mut recognizer);
                    emit_up(&mut recognizer, &mut contact, &mut landed, points);
                    continue;
                }
            }
            sync_button(&mut recognizer);
        } else {
            // Idle: sleep indefinitely on hardware falling edge with zero I2C traffic.
            if let Err(e) = touch.wait_for_interrupt().await {
                log::error!("touch: interrupt wait error: {e:?}");
                Timer::after(Duration::from_millis(50)).await;
                continue;
            }
            sync_button(&mut recognizer);
        }

        match touch.read_point().await {
            Ok(Some(mut point)) => {
                let at_ms = Instant::now().as_millis();
                let x = point.x;
                let y = point.y;

                match point.event {
                    TouchEvent::Up => {
                        emit_up(&mut recognizer, &mut contact, &mut landed, points);
                    }
                    TouchEvent::Down => {
                        if contact.is_none() {
                            log::trace!("touch: Down {x},{y}");
                            landed = Some((x, y, at_ms));
                            points = 1;
                            contact = Some((x, y));
                            if let Some(gesture) = recognizer.push(TouchSample { point, at_ms }) {
                                crate::event::send(crate::event::Event::Gesture(gesture));
                            }
                        } else {
                            points = points.saturating_add(1);
                            log::trace!("touch: Move {x},{y}");
                            contact = Some((x, y));
                            point.event = TouchEvent::Move;
                            if let Some(gesture) = recognizer.push(TouchSample { point, at_ms }) {
                                crate::event::send(crate::event::Event::Gesture(gesture));
                            }
                        }
                    }
                    TouchEvent::Move | TouchEvent::Unknown(_) => {
                        if contact.is_some() {
                            points = points.saturating_add(1);
                            log::trace!("touch: Move {x},{y}");
                            point.event = TouchEvent::Move;
                        } else {
                            log::trace!("touch: Down {x},{y}");
                            landed = Some((x, y, at_ms));
                            points = 1;
                            point.event = TouchEvent::Down;
                        }
                        contact = Some((x, y));
                        if let Some(gesture) = recognizer.push(TouchSample { point, at_ms }) {
                            crate::event::send(crate::event::Event::Gesture(gesture));
                        }
                    }
                }
            }
            Ok(None) => {
                if contact.is_some() {
                    emit_up(&mut recognizer, &mut contact, &mut landed, points);
                }
            }
            Err(_) => {
                log::error!("touch: read failed");
                if contact.is_some() {
                    emit_up(&mut recognizer, &mut contact, &mut landed, points);
                }
            }
        }
    }
}
