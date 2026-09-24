// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

//! PCNT quadrature encoder driver for Waveshare ESP32-S3-Knob-Touch-LCD-1.8.

use core::cell::RefCell;

use critical_section::Mutex;
use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Timer};
use esp_hal::gpio::{Event, Input, InputConfig, Io, Pull};
use esp_hal::handler;
use esp_hal::pcnt::Pcnt;
use esp_hal::pcnt::channel::{CtrlMode, EdgeMode};
use esp_hal::pcnt::unit::Unit;
use esp_hal::peripherals::{GPIO7, GPIO8, IO_MUX, PCNT};

use common::channels::send_input_event;
use common::event::InputEvent;

/// Glitch filter threshold in APB clock cycles.
const FILTER_THRESHOLD: u16 = 1000;

/// PCNT-backed quadrature encoder hardware holding pins and counter unit.
pub struct EncoderHw {
    unit: Unit<'static, 0>,
    pin_a: Input<'static>,
    pin_b: Input<'static>,
    level_a: bool,
    level_b: bool,
}

impl EncoderHw {
    /// Configures PCNT unit 0 for 4x quadrature decode of `pin_a`/`pin_b` (GPIO 8 and 7).
    pub fn new(pcnt: PCNT<'static>, pin_a: GPIO8<'static>, pin_b: GPIO7<'static>) -> Self {
        let pcnt = Pcnt::new(pcnt);
        let unit = pcnt.unit0;
        let _ = unit.set_filter(Some(FILTER_THRESHOLD));
        unit.clear();

        let cfg = InputConfig::default().with_pull(Pull::Up);
        let mut a = Input::new(pin_a, cfg);
        let mut b = Input::new(pin_b, cfg);
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

        let level_a = a.is_high();
        let level_b = b.is_high();

        a.listen(Event::AnyEdge);
        b.listen(Event::AnyEdge);

        Self { unit, pin_a: a, pin_b: b, level_a, level_b }
    }

    #[must_use]
    fn raw(&self) -> i16 {
        self.unit.value()
    }

    fn clear_pending(&mut self) {
        if self.pin_a.is_interrupt_set() {
            self.pin_a.clear_interrupt();
        }
        if self.pin_b.is_interrupt_set() {
            self.pin_b.clear_interrupt();
        }
    }
}

pub struct Encoder {
    last_raw: i16,
    last_a: bool,
    last_b: bool,
    decoder: detent_decoder::Decoder,
}

impl Encoder {
    #[must_use]
    pub fn new(a: bool, b: bool) -> Self {
        // Optical / ball bearing encoder: 2 or 4 state steps per virtual detent
        Self { last_raw: 0, last_a: a, last_b: b, decoder: detent_decoder::Decoder::new(2) }
    }

    pub fn update(&mut self, raw: i16, a: bool, b: bool) -> (i32, i8) {
        let steps = i32::from(raw) - i32::from(self.last_raw);
        let table = detent_decoder::quad_step(self.last_a, self.last_b, a, b);
        let at_rest = a == b;
        self.last_raw = raw;
        self.last_a = a;
        self.last_b = b;
        let detents = self.decoder.update(steps, at_rest);
        (detents, table)
    }
}

struct RotaryState {
    hw: EncoderHw,
    encoder: Encoder,
}

static ROTARY: Mutex<RefCell<Option<RotaryState>>> = Mutex::new(RefCell::new(None));

pub fn init_rotary(io_mux: IO_MUX<'static>, hw: EncoderHw) {
    let mut io = Io::new(io_mux);
    io.set_interrupt_handler(rotary_isr);

    critical_section::with(|cs| {
        let (a, b) = (hw.level_a, hw.level_b);
        ROTARY
            .borrow_ref_mut(cs)
            .replace(RotaryState { hw, encoder: Encoder::new(a, b) });
    });
}

#[handler]
fn rotary_isr() {
    critical_section::with(|cs| {
        if let Some(state) = ROTARY.borrow_ref_mut(cs).as_mut() {
            state.hw.clear_pending();
        }
    });
    ROTARY_SIGNAL.signal(());
}

static ROTARY_SIGNAL: Signal<CriticalSectionRawMutex, ()> = Signal::new();

const SETTLE_TIME: Duration = Duration::from_micros(40);

pub async fn rotary_decode_once() {
    ROTARY_SIGNAL.wait().await;
    loop {
        match select(ROTARY_SIGNAL.wait(), Timer::after(SETTLE_TIME)).await {
            Either::First(()) => {}
            Either::Second(()) => break,
        }
    }
    let detents = critical_section::with(|cs| {
        let mut state = ROTARY.borrow_ref_mut(cs);
        let Some(state) = state.as_mut() else {
            return 0;
        };
        let a = state.hw.pin_a.is_high();
        let b = state.hw.pin_b.is_high();
        let raw = state.hw.raw();
        state.encoder.update(raw, a, b).0
    });
    if detents != 0 {
        send_input_event(InputEvent::Rotate(detents));
    }
}

/// Drives [`rotary_decode_once`] forever.
#[embassy_executor::task]
pub async fn rotary_task() {
    loop {
        rotary_decode_once().await;
    }
}
