// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

//! PCNT quadrature encoder and button driver for LilyGO T-Encoder Pro.

use embassy_futures::select::{Either, select};
use embassy_time::{Duration, Timer};
use esp_hal::gpio::{Input, InputConfig, Pull};
use esp_hal::pcnt::Pcnt;
use esp_hal::pcnt::channel::{CtrlMode, EdgeMode};
use esp_hal::pcnt::unit::Unit;
use esp_hal::peripherals::{GPIO1, GPIO2, PCNT};

use super::input::{InputEvent, send_input_event};
use super::touch;

/// Glitch filter threshold in APB clock cycles.
const FILTER_THRESHOLD: u16 = 1000;

/// Number of quadrature counter edges per mechanical detent (this encoder emits 2 per click).
const COUNTS_PER_DETENT: u8 = 2;

/// Debounce settle time after a button edge.
const DEBOUNCE_TIME: Duration = Duration::from_millis(25);

/// Hold duration threshold before a button press counts as a long-press.
const LONG_PRESS: Duration = Duration::from_millis(500);

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

/// Accumulates raw quadrature counter readings into whole-detent deltas.
///
/// The raw counter is a free-running 16-bit value; deltas are computed with
/// wrapping subtraction, so any movement smaller than half the counter range
/// between calls is tracked exactly.
pub struct Encoder {
    last_raw: i16,
    accum: i32,
    counts_per_detent: i32,
}

impl Encoder {
    /// Creates an accumulator for an encoder producing `counts_per_detent`
    /// quadrature counts per mechanical detent (typically 4). Values below 1
    /// are clamped to 1.
    #[must_use]
    pub fn new(counts_per_detent: u8) -> Self {
        Self {
            last_raw: 0,
            accum: 0,
            counts_per_detent: i32::from(counts_per_detent.max(1)),
        }
    }

    /// Feeds a raw counter reading and returns the number of whole detents
    /// moved since the previous call (signed; positive is clockwise).
    ///
    /// The leftover sub-detent remainder is retained for the next call, so no
    /// movement is lost to rounding.
    pub fn update(&mut self, raw: i16) -> i32 {
        let delta = i32::from(raw.wrapping_sub(self.last_raw));
        self.last_raw = raw;
        self.accum = self.accum.saturating_add(delta);
        let detents = self.accum.checked_div(self.counts_per_detent).unwrap_or(0);
        self.accum = self
            .accum
            .saturating_sub(detents.saturating_mul(self.counts_per_detent));
        detents
    }
}

/// Asynchronous Embassy task listening for rotary encoder edge interrupts.
#[embassy_executor::task]
pub async fn encoder_task(mut encoder_hw: EncoderHw) {
    let mut encoder = Encoder::new(COUNTS_PER_DETENT);
    loop {
        encoder_hw.wait_for_rotation().await;
        let detents = encoder.update(encoder_hw.raw());
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
        touch::set_button(true);

        match select(button.wait_for_rising_edge(), Timer::after(LONG_PRESS)).await {
            Either::First(()) => {
                touch::set_button(false);
                send_input_event(InputEvent::Click);
            }
            Either::Second(()) => {
                send_input_event(InputEvent::LongPress);
                button.wait_for_rising_edge().await;
                touch::set_button(false);
            }
        }

        Timer::after(DEBOUNCE_TIME).await;
    }
}
