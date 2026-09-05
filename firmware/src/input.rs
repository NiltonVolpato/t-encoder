// Adapted from rust-enc (https://github.com/rayslava/rust-enc), MIT licensed.
// Copyright (c) rayslava. Copied from enc-app, which is a binary crate and so
// cannot be reached by path dependency; diverges from upstream from here on.

//! Rotary-encoder hardware: PCNT quadrature decode on the encoder's A/B lines.
//!
//! A free-running 16-bit counter is read by [`EncoderHw::raw`]; turning the
//! raw value into detents is done by [`enc_input::Encoder`]. Polling with
//! wrapping deltas means no interrupt or limit bookkeeping is needed for a
//! human-speed knob.

use esp_hal::gpio::{Input, InputConfig, Pull};
use esp_hal::pcnt::Pcnt;
use esp_hal::pcnt::channel::{CtrlMode, EdgeMode};
use esp_hal::pcnt::unit::Unit;
use esp_hal::peripherals::{GPIO1, GPIO2, PCNT};

/// Glitch filter threshold in APB clock cycles (max 1023).
const FILTER_THRESHOLD: u16 = 1000;

/// PCNT-backed quadrature encoder on the A/B pins.
pub struct EncoderHw {
    unit: Unit<'static, 0>,
    _pin_a: Input<'static>,
    _pin_b: Input<'static>,
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

        // Edge modes inverted vs the esp-hal example so clockwise increments
        // on this board's A/B wiring.
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
            _pin_a: a,
            _pin_b: b,
        }
    }

    /// The current raw quadrature counter value.
    #[must_use]
    pub fn raw(&self) -> i16 {
        self.unit.counter.get()
    }
}
