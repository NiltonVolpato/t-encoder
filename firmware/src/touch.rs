// Adapted from rust-enc (https://github.com/rayslava/rust-enc), MIT licensed.
// Copyright (c) rayslava. Copied from enc-app, which is a binary crate and so
// cannot be reached by path dependency; diverges from upstream from here on.

//! CHSC5816 touch bring-up over the shared I2C bus (async).

use embassy_time::Delay;
use enc_touch::Chsc5816;
use esp_hal::Async;
use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull};
use esp_hal::i2c::master::{Config, I2c};
use esp_hal::peripherals::{GPIO5, GPIO6, GPIO8, GPIO9, I2C0};

/// Concrete CHSC5816 type on this board (async I2C + INT/RST GPIO).
pub type Touch = Chsc5816<I2c<'static, Async>, Input<'static>, Output<'static>>;

/// Pins/peripherals the touch controller needs.
pub struct TouchPins {
    pub i2c: I2C0<'static>,
    pub sda: GPIO5<'static>,
    pub scl: GPIO6<'static>,
    pub int: GPIO9<'static>,
    pub rst: GPIO8<'static>,
}

/// Sets up the async I2C bus and the CHSC5816, pulsing its reset line.
///
/// Returns `None` if the I2C peripheral cannot be configured.
pub async fn init(pins: TouchPins, address: u8) -> Option<Touch> {
    let i2c = match I2c::new(pins.i2c, Config::default()) {
        Ok(i2c) => i2c.with_sda(pins.sda).with_scl(pins.scl).into_async(),
        Err(e) => {
            log::error!("touch: i2c init failed: {e:?}");
            return None;
        }
    };
    let interrupt = Input::new(pins.int, InputConfig::default().with_pull(Pull::Up));
    let reset = Output::new(pins.rst, Level::High, OutputConfig::default());
    let mut touch = Chsc5816::new(i2c, interrupt, reset, address);
    if let Err(e) = touch.init(&mut Delay).await {
        log::error!("touch: init failed: {e:?}");
        return None;
    }
    Some(touch)
}
