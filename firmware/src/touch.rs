// Adapted from rust-enc (https://github.com/rayslava/rust-enc), MIT licensed.
// Copyright (c) rayslava. Copied from enc-app, which is a binary crate and so
// cannot be reached by path dependency; diverges from upstream from here on.

//! CHSC5816 touch bring-up, and the stroke stream it feeds the router.
//!
//! The controller's INT line is unreliable on this unit, so [`task`] polls the
//! point register and derives the stroke edges itself: the first report of a
//! contact is a [`TouchPhase::Down`], later ones are `Move`s, and the report
//! going empty is an `Up` at the last known position. The recogniser in
//! `launcher` wants edges, not levels, and it is the only thing that reads
//! them — this file recognises nothing.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Delay, Duration, Instant, Timer};
use enc_touch::Chsc5816;
use esp_hal::Async;
use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull};
use esp_hal::i2c::master::{Config, I2c};
use esp_hal::peripherals::{GPIO5, GPIO6, GPIO8, GPIO9, I2C0};
use launcher::{TouchPhase, TouchSample};

/// I2C address of the touch controller on this board. Upstream's docs claim a
/// CST816 at 0x15; this unit is a CHSC5816 at 0x2E, confirmed by observation.
const ADDRESS: u8 = 0x2E;

/// Poll interval. ~100 Hz is far finer than a swipe needs, and leaves the I2C
/// bus idle between the UI loop's 5 ms ticks.
const POLL: Duration = Duration::from_millis(10);

/// Queue depth. The UI loop drains it every tick; the only thing that stalls it
/// is a full-frame flush (~29 ms), which is three samples' worth.
const QUEUE: usize = 16;

/// Concrete CHSC5816 type on this board (async I2C + INT/RST GPIO).
pub type Touch = Chsc5816<I2c<'static, Async>, Input<'static>, Output<'static>>;

/// Touch samples awaiting the UI loop.
pub static SAMPLES: Channel<CriticalSectionRawMutex, TouchSample, QUEUE> = Channel::new();

/// Pins/peripherals the touch controller needs.
pub struct TouchPins {
    /// I2C bus the controller sits on.
    pub i2c: I2C0<'static>,
    /// I2C data line.
    pub sda: GPIO5<'static>,
    /// I2C clock line.
    pub scl: GPIO6<'static>,
    /// Interrupt line (unused: polled instead).
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

/// Polls the controller and publishes one [`TouchSample`] per state change.
///
/// Each completed stroke is logged with its travel. The recogniser's swipe
/// threshold cannot be tuned from the host — it is a question about fingers on
/// a 35 mm round panel — and this one line is what makes it answerable.
#[embassy_executor::task]
pub async fn task(mut touch: Touch) {
    // Where the finger was last seen, and therefore whether one is down.
    let mut contact: Option<(i32, i32)> = None;
    // Where the current stroke started, for the travel log.
    let mut landed: Option<(i32, i32)> = None;
    loop {
        Timer::after(POLL).await;
        let at_ms = Instant::now().as_millis();
        match touch.read_point().await {
            Ok(Some(point)) => {
                let x = i32::from(point.x);
                let y = i32::from(point.y);
                let phase = if contact.is_some() {
                    TouchPhase::Move
                } else {
                    landed = Some((x, y));
                    TouchPhase::Down
                };
                contact = Some((x, y));
                send(TouchSample { phase, x, y, at_ms });
            }
            Ok(None) => {
                // The lift carries the last position: the controller reports
                // nothing at all once the finger is gone, and a swipe is
                // measured between where it landed and where it left.
                if let Some((x, y)) = contact.take() {
                    if let Some((x0, y0)) = landed.take() {
                        // Short on purpose: a line past the 64-byte
                        // USB-Serial/JTAG FIFO blocks until the host drains it.
                        log::info!("touch: {x0},{y0} -> {x},{y}");
                    }
                    send(TouchSample {
                        phase: TouchPhase::Up,
                        x,
                        y,
                        at_ms,
                    });
                }
            }
            Err(_) => log::error!("touch: read failed"),
        }
    }
}

/// Queues a sample, dropping it if the UI loop has fallen far behind. A lost
/// `Move` costs nothing — only the ends of a stroke are measured — and a lost
/// edge costs one gesture, which is better than stalling the touch task.
fn send(sample: TouchSample) {
    let _ = SAMPLES.try_send(sample);
}
