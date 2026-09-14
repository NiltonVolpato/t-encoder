// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

//! PCNT quadrature encoder and button driver for LilyGO T-Encoder Pro.

use esp_hal::gpio::{Input, InputConfig, Pull};
use esp_hal::pcnt::Pcnt;
use esp_hal::pcnt::channel::{CtrlMode, EdgeMode};
use esp_hal::pcnt::unit::Unit;
use esp_hal::peripherals::{GPIO0, GPIO1, GPIO2, PCNT};
use esp_hal::time::Instant;

const FILTER_THRESHOLD: u16 = 1000;
const COUNTS_PER_DETENT: i16 = 2;
const DEBOUNCE_MS: u64 = 25;
const LONG_PRESS_MS: u64 = 600;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonEvent {
    Click,
    LongPress,
}

pub struct Rotary {
    unit: Unit<'static, 0>,
    _pin_a: Input<'static>,
    _pin_b: Input<'static>,
    btn: Input<'static>,
    last_count: i16,
    sub_count: i16,
    btn_pressed: bool,
    btn_press_time: Option<Instant>,
    long_press_emitted: bool,
}

impl Rotary {
    pub fn new(
        pcnt: PCNT<'static>,
        pin_a: GPIO1<'static>,
        pin_b: GPIO2<'static>,
        btn_pin: GPIO0<'static>,
    ) -> Self {
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
        ch0.set_ctrl_signal(sig_a);
        ch0.set_edge_signal(sig_b);
        ch0.set_ctrl_mode(CtrlMode::Reverse, CtrlMode::Keep);
        ch0.set_input_mode(EdgeMode::Decrement, EdgeMode::Increment);

        let btn = Input::new(btn_pin, InputConfig::default().with_pull(Pull::Up));

        Self {
            unit,
            _pin_a: a,
            _pin_b: b,
            btn,
            last_count: 0,
            sub_count: 0,
            btn_pressed: false,
            btn_press_time: None,
            long_press_emitted: false,
        }
    }

    /// Polls rotation delta in mechanical detents.
    /// Positive = Clockwise, Negative = Counter-Clockwise.
    pub fn poll_rotation(&mut self) -> i32 {
        let raw = self.unit.value();
        let delta = raw.wrapping_sub(self.last_count);
        self.last_count = raw;

        self.sub_count += delta;
        let detents = self.sub_count / COUNTS_PER_DETENT;
        self.sub_count %= COUNTS_PER_DETENT;

        detents as i32
    }

    /// Polls button state for debounced click and long-press events.
    pub fn poll_button(&mut self) -> Option<ButtonEvent> {
        let is_down = self.btn.is_low(); // Active low
        let now = Instant::now();

        if is_down && !self.btn_pressed {
            self.btn_pressed = true;
            self.btn_press_time = Some(now);
            self.long_press_emitted = false;
            None
        } else if is_down && self.btn_pressed {
            if let Some(press_time) = self.btn_press_time {
                let duration_ms = (now - press_time).as_millis();
                if duration_ms >= LONG_PRESS_MS && !self.long_press_emitted {
                    self.long_press_emitted = true;
                    return Some(ButtonEvent::LongPress);
                }
            }
            None
        } else if !is_down && self.btn_pressed {
            self.btn_pressed = false;
            let press_time = self.btn_press_time.take();
            if let Some(t) = press_time {
                let duration_ms = (now - t).as_millis();
                if duration_ms >= DEBOUNCE_MS && !self.long_press_emitted {
                    return Some(ButtonEvent::Click);
                }
            }
            None
        } else {
            None
        }
    }
}
