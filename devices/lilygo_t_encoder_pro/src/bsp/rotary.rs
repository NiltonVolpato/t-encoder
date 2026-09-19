// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

//! PCNT quadrature encoder and button driver for LilyGO T-Encoder Pro.

use embassy_futures::select::{Either, select};
use embassy_time::{Duration, Instant, Timer};
use esp_hal::gpio::{Input, InputConfig, Pull};
use esp_hal::pcnt::Pcnt;
use esp_hal::pcnt::channel::{CtrlMode, EdgeMode};
use esp_hal::pcnt::unit::Unit;
use esp_hal::peripherals::{GPIO1, GPIO2, PCNT};

use super::input::{InputEvent, send_input_event};
use super::touch::set_button;

/// Glitch filter threshold in APB clock cycles.
const FILTER_THRESHOLD: u16 = 1000;

/// Number of quadrature counter edges per mechanical detent (this encoder emits 2 per click).
const COUNTS_PER_DETENT: i16 = 2;

/// Debounce settle time after a button edge.
const DEBOUNCE_MS: u64 = 25;

/// Hold duration threshold before a button press counts as a long-press.
const LONG_PRESS_MS: u64 = 600;

/// PCNT-backed quadrature encoder hardware holding pins and counter unit.
pub struct EncoderHw {
    unit: Unit<'static, 0>,
    pin_a: Input<'static>,
    pin_b: Input<'static>,
}

impl EncoderHw {
    /// Configures PCNT unit 0 for 4x quadrature decode of `pin_a`/`pin_b`.
    pub fn new(pcnt: PCNT<'static>, pin_a: GPIO1<'static>, pin_b: GPIO2<'static>) -> Self {
        let pcnt = Pcnt::new(pcnt);
        let unit = pcnt.unit0;
        let _ = unit.set_filter(Some(FILTER_THRESHOLD));
        unit.clear();

        let cfg = InputConfig::default().with_pull(Pull::Up);
        let a = Input::new(pin_a, cfg);
        let b = Input::new(pin_b, cfg);
        let sig_a = a.peripheral_input();
        let sig_b = b.peripheral_input();

        let ch0 = &unit.channel0;
        ch0.set_ctrl_signal(sig_a.clone());
        ch0.set_edge_signal(sig_b.clone());
        ch0.set_ctrl_mode(CtrlMode::Reverse, CtrlMode::Keep);
        ch0.set_input_mode(EdgeMode::Decrement, EdgeMode::Increment);

        let ch1 = &unit.channel1;
        ch1.set_ctrl_signal(sig_b);
        ch1.set_edge_signal(sig_a);
        ch1.set_ctrl_mode(CtrlMode::Reverse, CtrlMode::Keep);
        ch1.set_input_mode(EdgeMode::Increment, EdgeMode::Decrement);

        unit.resume();

        Self {
            unit,
            pin_a: a,
            pin_b: b,
        }
    }

    /// Awaits any edge transition on either Phase A or Phase B.
    pub async fn wait_for_rotation(&mut self) {
        select(
            self.pin_a.wait_for_any_edge(),
            self.pin_b.wait_for_any_edge(),
        )
        .await;
    }

    /// Reads current hardware accumulator counter value.
    #[must_use]
    pub fn raw(&self) -> i16 {
        self.unit.value()
    }
}

/// Asynchronous Embassy task listening for rotary encoder edge interrupts.
#[embassy_executor::task]
pub async fn encoder_task(mut hw: EncoderHw) {
    let mut last_count = hw.raw();
    let mut sub_count = 0i16;

    loop {
        hw.wait_for_rotation().await;
        let raw = hw.raw();
        let delta = raw.wrapping_sub(last_count);
        last_count = raw;

        sub_count += delta;
        let detents = sub_count / COUNTS_PER_DETENT;
        sub_count %= COUNTS_PER_DETENT;

        if detents != 0 {
            send_input_event(InputEvent::Rotate(detents as i32));
        }
    }
}

/// Asynchronous Embassy task listening for dial button edge interrupts.
#[embassy_executor::task]
pub async fn button_task(mut button: Input<'static>) {
    loop {
        // Sleep on falling edge (button pressed, active-low pull-up)
        button.wait_for_falling_edge().await;
        set_button(true);
        let press_start = Instant::now();

        match select(
            button.wait_for_rising_edge(),
            Timer::after(Duration::from_millis(LONG_PRESS_MS)),
        )
        .await
        {
            Either::First(()) => {
                set_button(false);
                let duration_ms = (Instant::now() - press_start).as_millis();
                if duration_ms >= DEBOUNCE_MS {
                    send_input_event(InputEvent::Click);
                }
            }
            Either::Second(()) => {
                send_input_event(InputEvent::LongPress);
                button.wait_for_rising_edge().await;
                set_button(false);
            }
        }

        Timer::after(Duration::from_millis(DEBOUNCE_MS)).await;
    }
}
