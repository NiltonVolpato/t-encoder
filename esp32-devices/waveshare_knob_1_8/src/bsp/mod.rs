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
    DISPLAY_HEIGHT, DISPLAY_WIDTH, DirtyRect, DisplayCommand, FlushJob, Framebuffer, BigEndianRgb565,
    RENDER_HEIGHT, RENDER_STRIDE, RENDER_WIDTH, Sh8601, TX_BUF_BYTES, display_task,
};
use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use esp_hal::gpio::{Input, InputConfig, Pull};
use esp_hal::i2c::master::{BusTimeout, Config as I2cConfig, I2c};
use esp_hal::time::Rate;

pub type SharedI2cBus = Mutex<CriticalSectionRawMutex, I2c<'static, esp_hal::Async>>;
pub type SharedI2c = I2cDevice<'static, CriticalSectionRawMutex, I2c<'static, esp_hal::Async>>;

static I2C_BUS: static_cell::StaticCell<SharedI2cBus> = static_cell::StaticCell::new();

pub use haptics::{Feedback, haptic_task, signal_feedback};
pub use platform::{EspPlatform, WaveshareFeedback, WindowHolder, run_event_loop};
pub use rotary::rotary_task;
pub use touch::{TouchHw, touch_task};

pub struct Bsp {
    pub window: WindowHolder,
    pub touch: Option<TouchHw>,
    pub haptic_i2c: Option<SharedI2c>,
    pub rotary_a: Input<'static>,
    pub rotary_b: Input<'static>,
    pub profiler_timer: esp_hal::peripherals::TIMG1<'static>,
}

impl Bsp {
    /// Initializes touch, encoder, and registers the Slint platform on Core 0.
    pub fn init(core0: Core0Peripherals) -> Self {
        // 1. Initialize async I2C0 shared bus for CST816D touch controller and DRV2605 haptics
        let (touch, haptic_i2c) = match I2c::new(
            core0.i2c0.i2c,
            I2cConfig::default()
                .with_frequency(Rate::from_khz(400))
                .with_timeout(BusTimeout::Maximum),
        ) {
            Ok(i2c) => {
                let i2c = i2c.with_sda(core0.i2c0.sda).with_scl(core0.i2c0.scl).into_async();
                let bus = I2C_BUS.init(Mutex::new(i2c));
                let touch_dev = TouchHw::new(I2cDevice::new(bus), core0.i2c0.touch_int, core0.i2c0.touch_rst);
                let haptic_dev = I2cDevice::new(bus);
                (Some(touch_dev), Some(haptic_dev))
            }
            Err(e) => {
                defmt::error!("I2C0 initialization failed: {:?}", defmt::Debug2Format(&e));
                (None, None)
            }
        };

        // 2. Initialize pull-up inputs for bidirectional pulsed rotary knob
        let cfg = InputConfig::default().with_pull(Pull::Up);
        let rotary_a = Input::new(core0.encoder.pin_a, cfg);
        let rotary_b = Input::new(core0.encoder.pin_b, cfg);

        // 3. Set Slint platform
        let (platform, window) = platform::create_platform();
        slint::platform::set_platform(alloc::boxed::Box::new(platform))
            .expect("Slint platform already set");

        Self {
            window,
            touch,
            haptic_i2c,
            rotary_a,
            rotary_b,
            profiler_timer: core0.profiler_timer,
        }
    }
}
