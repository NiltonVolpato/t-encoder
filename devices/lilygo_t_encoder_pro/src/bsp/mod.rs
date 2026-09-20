// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

//! Board Support Package (BSP) for LilyGO T-Encoder Pro (ESP32-S3).

pub mod board;
pub mod buzzer;
pub mod display;
pub mod input;
pub mod platform;
pub mod profiler;
pub mod rotary;
pub mod simd;
pub mod touch;

pub use board::{
    Board, BuzzerPeripherals, Core0Peripherals, Core1Peripherals, DisplayPeripherals,
    EncoderPeripherals, SystemPeripherals, TouchPeripherals,
};
pub use buzzer::signal_feedback;
pub use display::{
    BigEndianRgb565, Co5300, DISPLAY_HEIGHT, DISPLAY_WIDTH, RENDER_HEIGHT, RENDER_WIDTH,
    TX_BUF_BYTES, display_task,
};
pub use input::{INPUT_EVENTS, InputEvent, send_input_event};
pub use platform::{EspPlatform, WindowHolder, run_event_loop};
pub use rotary::{EncoderHw, button_task, encoder_task};
pub use touch::{Chsc5816, touch_task};

use esp_hal::gpio::{Input, InputConfig, Pull};
use esp_hal::i2c::master::{BusTimeout, Config as I2cConfig, I2c};
use esp_hal::time::Rate;

pub struct Bsp {
    pub window: WindowHolder,
    pub display: Co5300,
    pub touch: Option<Chsc5816>,
    pub encoder_hw: EncoderHw,
    pub button: Input<'static>,
    pub buzzer: BuzzerPeripherals,
}

impl Bsp {
    /// Initializes display, touch, rotary, profiler, and registers the Slint platform.
    pub fn init(core0: Core0Peripherals, display_peripherals: DisplayPeripherals) -> Self {
        // 1. Initialize display
        let display = Co5300::new(display_peripherals);

        // 2. Initialize async I2C0 for CHSC5816 touch controller
        let touch = match I2c::new(
            core0.touch.i2c,
            I2cConfig::default()
                .with_frequency(Rate::from_khz(400))
                .with_timeout(BusTimeout::Maximum),
        ) {
            Ok(i2c) => {
                let i2c = i2c
                    .with_sda(core0.touch.sda)
                    .with_scl(core0.touch.scl)
                    .into_async();
                let touch_dev = Chsc5816::new(i2c, core0.touch.int, core0.touch.rst);
                Some(touch_dev)
            }
            Err(e) => {
                defmt::error!("I2C0 initialization failed: {:?}", defmt::Debug2Format(&e));
                None
            }
        };

        // 3. Initialize PCNT quadrature rotary encoder and button
        let encoder_hw =
            EncoderHw::new(core0.encoder.pcnt, core0.encoder.pin_a, core0.encoder.pin_b);
        let button = Input::new(
            core0.encoder.button,
            InputConfig::default().with_pull(Pull::Up),
        );

        // 4. Set Slint platform
        let (platform, window) = EspPlatform::new();
        slint::platform::set_platform(alloc::boxed::Box::new(platform))
            .expect("Slint platform already set");

        // 5. Initialize statistical sampling profiler
        profiler::init(core0.profiler_timer);

        // 6. Enable PIE SIMD coprocessor
        simd::enable_pie();

        Self {
            window,
            display,
            touch,
            encoder_hw,
            button,
            buzzer: core0.buzzer,
        }
    }
}
