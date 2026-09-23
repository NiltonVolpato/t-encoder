// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

//! PCNT quadrature encoder and button driver for LilyGO T-Encoder Pro.

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
use esp_hal::peripherals::{GPIO1, GPIO2, IO_MUX, PCNT};

use super::input::{InputEvent, send_input_event};
use super::touch;

/// Glitch filter threshold in APB clock cycles.
const FILTER_THRESHOLD: u16 = 1000;

/// Debounce settle time after a button edge.
const DEBOUNCE_TIME: Duration = Duration::from_millis(25);

/// Hold duration threshold before a button press counts as a long-press.
const LONG_PRESS: Duration = Duration::from_millis(500);

/// PCNT-backed quadrature encoder hardware holding pins and counter unit.
pub struct EncoderHw {
    unit: Unit<'static, 0>,
    pin_a: Input<'static>,
    pin_b: Input<'static>,
    /// Pin levels at boot, and as last seen by the `rotary-debug` interrupt
    /// capture (for edge-polarity classification).
    level_a: bool,
    level_b: bool,
}

impl EncoderHw {
    /// Configures PCNT unit 0 for 4x quadrature decode of `pin_a`/`pin_b`, and
    /// arms both pins to interrupt on any edge.
    pub fn new(pcnt: PCNT<'static>, pin_a: GPIO1<'static>, pin_b: GPIO2<'static>) -> Self {
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

    /// Reads current hardware accumulator counter value.
    #[must_use]
    fn raw(&self) -> i16 {
        self.unit.value()
    }

    /// Clears whichever pin(s) raised the interrupt that woke [`rotary_isr`].
    fn clear_pending(&mut self) {
        if self.pin_a.is_interrupt_set() {
            self.pin_a.clear_interrupt();
        }
        if self.pin_b.is_interrupt_set() {
            self.pin_b.clear_interrupt();
        }
    }
}

/// Adapts settled (counter, pin-level) samples into detent reports via
/// [`detent_decoder::Decoder`].
///
/// PCNT is the accumulator: it holds a persistent, hardware-filtered count
/// that never loses an edge, so fast bursts arrive as multi-step deltas. The
/// rest-position predicate comes from the pin levels, sampled after the
/// glitch-filter delay has passed so bounce has already settled.
pub struct Encoder {
    last_raw: i16,
    last_a: bool,
    last_b: bool,
    decoder: detent_decoder::Decoder,
}

impl Encoder {
    /// Creates the decoder anchored at the given initial pin levels (the
    /// encoder's rest state at boot).
    #[must_use]
    pub fn new(a: bool, b: bool) -> Self {
        Self { last_raw: 0, last_a: a, last_b: b, decoder: detent_decoder::Decoder::new(2) }
    }

    /// Feeds one settled sample; returns the signed detents to report
    /// (positive is clockwise) and the pin-transition step from the
    /// quadrature table as a cross-check: a counter delta whose table step is
    /// `0` means the intermediate state went unobserved (a burst faster than
    /// the settle window).
    pub fn update(&mut self, raw: i16, a: bool, b: bool) -> (i32, i8) {
        let steps = i32::from(raw) - i32::from(self.last_raw);
        let table = detent_decoder::quad_step(self.last_a, self.last_b, a, b);
        let at_rest = a == b;
        self.last_raw = raw;
        self.last_a = a;
        self.last_b = b;
        let detents = self.decoder.update(steps, at_rest);

        #[cfg(feature = "rotary-debug")]
        defmt::info!(
            "rotary update: raw={} steps={} rest={} table={} net={} detents={}",
            raw,
            steps,
            at_rest,
            table,
            self.decoder.pending(),
            detents
        );
        (detents, table)
    }
}

/// Hardware and accumulator state shared with [`rotary_isr`].
struct RotaryState {
    hw: EncoderHw,
    encoder: Encoder,
}

static ROTARY: Mutex<RefCell<Option<RotaryState>>> = Mutex::new(RefCell::new(None));

/// Registers the GPIO interrupt handler and arms the rotary encoder.
///
/// [`rotary_isr`] only captures and logs the pin state at handler entry and
/// wakes [`rotary_decode_once`]; decoding happens there, after the pin has
/// been quiet past the glitch-filter delay, so the counter and the pin levels
/// are sampled in agreement. In production [`rotary_task`] drives that loop;
/// the manual log test drives single rounds inline.
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

/// Interrupt-entry pin diagnostics, compiled only with the `rotary-debug`
/// cargo feature. Everything the handler knows at wake time — which pin(s)
/// fired, their levels right now, the counter — captured before the pending
/// flags are cleared, exactly as the hardware shows it mid-bounce.
#[cfg(feature = "rotary-debug")]
mod debug {
    use super::EncoderHw;

    /// Snapshot of one interrupt wake, logged for diagnostics.
    pub(super) struct IsrSample {
        pub(super) raw: i16,
        pub(super) level_a: bool,
        pub(super) level_b: bool,
        pub(super) pending_a: bool,
        pub(super) pending_b: bool,
        pub(super) event_a: &'static str,
        pub(super) event_b: &'static str,
    }

    impl IsrSample {
        pub(super) fn empty() -> Self {
            Self {
                raw: 0,
                level_a: false,
                level_b: false,
                pending_a: false,
                pending_b: false,
                event_a: "-",
                event_b: "-",
            }
        }
    }

    /// Classifies one pin's interrupt into a printable edge event, updating
    /// the remembered level: `"up"`/`"down"` for a net single edge, `"~"`
    /// when the pin fired but its level is back to what it was (an even
    /// number of edges, e.g. bounce inside one interrupt latency), `"-"`
    /// when it didn't fire.
    fn classify_edge(pending: bool, level: bool, last: &mut bool) -> &'static str {
        if !pending {
            return "-";
        }
        if level == *last {
            return "~";
        }
        *last = level;
        if level { "up" } else { "down" }
    }

    impl EncoderHw {
        /// Captures the wake sample at handler entry, then clears the pending
        /// flags so the interrupt can fire again.
        pub(super) fn capture(&mut self) -> IsrSample {
            let pending_a = self.pin_a.is_interrupt_set();
            let pending_b = self.pin_b.is_interrupt_set();
            let level_a = self.pin_a.is_high();
            let level_b = self.pin_b.is_high();
            let raw = self.raw();
            self.clear_pending();
            let event_a = classify_edge(pending_a, level_a, &mut self.level_a);
            let event_b = classify_edge(pending_b, level_b, &mut self.level_b);
            IsrSample { raw, level_a, level_b, pending_a, pending_b, event_a, event_b }
        }
    }
}

#[handler]
fn rotary_isr() {
    // Decoding does NOT happen here: the glitch filter lags the pin by
    // ~12.5us, so the counter may not have absorbed this very edge yet. The
    // wake is captured for diagnostics (under `rotary-debug`), the pending
    // flags are cleared, and the settled sample is decoded later by
    // `rotary_decode_once`.
    #[cfg(feature = "rotary-debug")]
    {
        let sample = critical_section::with(|cs| {
            let mut state = ROTARY.borrow_ref_mut(cs);
            match state.as_mut() {
                Some(state) => state.hw.capture(),
                None => debug::IsrSample::empty(),
            }
        });
        defmt::info!(
            "rotary isr: raw={} a={} b={} pend_a={} pend_b={} a:{} b:{}",
            sample.raw,
            sample.level_a,
            sample.level_b,
            sample.pending_a,
            sample.pending_b,
            sample.event_a,
            sample.event_b
        );
    }
    #[cfg(not(feature = "rotary-debug"))]
    critical_section::with(|cs| {
        if let Some(state) = ROTARY.borrow_ref_mut(cs).as_mut() {
            state.hw.clear_pending();
        }
    });
    ROTARY_SIGNAL.signal(());
}

/// Wakes [`rotary_decode_once`] after every pin edge. Only the latest wake is
/// kept — decoding re-reads the accumulated counter, so coalesced edges lose
/// nothing.
static ROTARY_SIGNAL: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// Quiet time after the last pin edge before sampling.
///
/// The glitch filter delays counting by ~12.5us (1000 APB cycles at 80MHz),
/// so a sample taken while the pin is still moving can disagree with the
/// counter. Waiting past the filter delay guarantees both reflect the same
/// settled state.
const SETTLE_TIME: Duration = Duration::from_micros(40);

/// Waits for a pin edge, then for the pin to stay quiet past [`SETTLE_TIME`],
/// then samples the settled counter and pin levels and reports decoded
/// detents. Cancellation-safe: an unconsumed wake stays latched in
/// [`ROTARY_SIGNAL`] and is picked up by the next call, and the counter
/// accumulates every edge in the meantime.
pub async fn rotary_decode_once() {
    ROTARY_SIGNAL.wait().await;
    loop {
        match select(ROTARY_SIGNAL.wait(), Timer::after(SETTLE_TIME)).await {
            Either::First(()) => {}
            Either::Second(()) => break,
        }
    }
    let (_raw, detents) = critical_section::with(|cs| {
        let mut state = ROTARY.borrow_ref_mut(cs);
        let Some(state) = state.as_mut() else {
            return (0, 0);
        };
        let a = state.hw.pin_a.is_high();
        let b = state.hw.pin_b.is_high();
        let raw = state.hw.raw();
        (raw, state.encoder.update(raw, a, b).0)
    });
    if detents != 0 {
        #[cfg(feature = "rotary-debug")]
        defmt::info!("rotary detents: raw={} delta={}", _raw, detents);
        send_input_event(InputEvent::Rotate(detents));
    }
}

/// Drives [`rotary_decode_once`] forever. Spawned in production; the manual
/// log test drives single rounds inline instead.
#[embassy_executor::task]
pub async fn rotary_task() {
    loop {
        rotary_decode_once().await;
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
