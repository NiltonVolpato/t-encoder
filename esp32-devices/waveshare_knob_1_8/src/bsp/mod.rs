// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

//! Board Support Package (BSP) for Waveshare ESP32-S3-Knob-Touch-LCD-1.8.

pub mod board;
pub mod display;
pub mod haptics;
pub mod platform;
pub mod rotary;
pub mod touch;

pub use board::{
    Board, Core0Peripherals, Core1Peripherals, DisplayPeripherals, EncoderPeripherals,
    I2c0Peripherals, SystemPeripherals,
};
pub use common::channels::{
    EVENTS, receive_event, report_user_activity, send_event, send_input_event, send_screen_event,
    subscribe_user_activity, try_receive_event,
};
pub use common::event::{Event, InputEvent, ScreenEvent, TouchEvent, TouchPoint};
pub use display::{
    DISPLAY_HEIGHT, DISPLAY_WIDTH, DirtyRect, DisplayCommand, FlushJob, Framebuffer, NativeRgb565,
    RENDER_HEIGHT, RENDER_STRIDE, RENDER_WIDTH, Sh8601, TX_BUF_BYTES, display_task,
};
use esp_hal::i2c::master::{BusTimeout, Config as I2cConfig, I2c};
use esp_hal::time::Rate;
pub use haptics::{Feedback, haptic_task, signal_feedback};
pub use platform::{EspPlatform, WaveshareFeedback, WindowHolder, run_event_loop};
pub use rotary::{EncoderHw, init_rotary, rotary_decode_once, rotary_task};
pub use touch::{TouchHw, touch_task};

pub struct Bsp {
    pub window: WindowHolder,
    pub touch: Option<TouchHw>,
    pub haptic_i2c: Option<I2c<'static, esp_hal::Async>>,
    pub profiler_timer: esp_hal::peripherals::TIMG1<'static>,
}

impl Bsp {
    /// Initializes touch, encoder, and registers the Slint platform on Core 0.
    pub fn init(core0: Core0Peripherals) -> Self {
        // 1. Initialize async I2C0 for CST816D touch controller and DRV2605 haptics
        let (touch, haptic_i2c) = match I2c::new(
            core0.i2c0.i2c,
            I2cConfig::default()
                .with_frequency(Rate::from_khz(400))
                .with_timeout(BusTimeout::Maximum),
        ) {
            Ok(i2c) => {
                let i2c = i2c.with_sda(core0.i2c0.sda).with_scl(core0.i2c0.scl).into_async();
                let touch_dev = TouchHw::new(i2c, core0.i2c0.touch_int, core0.i2c0.touch_rst);
                (Some(touch_dev), None)
            }
            Err(e) => {
                defmt::error!("I2C0 initialization failed: {:?}", defmt::Debug2Format(&e));
                (None, None)
            }
        };

        // 2. Initialize PCNT quadrature rotary encoder
        let encoder_hw =
            EncoderHw::new(core0.encoder.pcnt, core0.encoder.pin_a, core0.encoder.pin_b);
        init_rotary(core0.encoder.io_mux, encoder_hw);

        // 3. Set Slint platform
        let (platform, window) = platform::create_platform();
        slint::platform::set_platform(alloc::boxed::Box::new(platform))
            .expect("Slint platform already set");

        Self {
            window,
            touch,
            haptic_i2c,
            profiler_timer: core0.profiler_timer,
        }
    }
}
