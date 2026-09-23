// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

//! Detent-event decoding for incremental quadrature rotary encoders.
//!
//! A detented encoder's mechanical rest positions are the only unambiguous
//! landmarks in the quadrature stream: between two rests, exactly
//! `steps_per_detent` filtered steps happened, in some order, possibly with
//! twitches that went out and sprang back. This crate decodes by anchoring to
//! those rests: steps accumulate algebraically (out-and-back motion cancels
//! itself), and detents are reported when the accumulated count reaches
//! `steps_per_detent` exactly at a rest position. Because only the *net* count
//! at rests matters, the order of intermediate steps is irrelevant — bounce,
//! hesitation, and spring-backs never produce phantom events.
//!
//! The input is hardware-filtered steps (e.g. the delta of a pulse counter
//! with a glitch filter) plus a rest-position predicate, via [`Decoder`]; or
//! raw pin levels via [`PinDecoder`], for platforms without a hardware
//! counter. For the common detent alignment, both pins are equal at rest
//! (`a == b`), but any predicate the caller can evaluate works.

#![cfg_attr(not(test), no_std)]

/// Converts filtered quadrature steps into detent events, anchored at rest
/// positions.
///
/// See the [crate-level documentation](self) for the decoding model.
#[derive(Debug, Clone)]
pub struct Decoder {
    steps_per_detent: i32,
    /// Net steps accumulated since the last rest evaluation.
    net: i32,
}

impl Decoder {
    /// Creates a decoder that reports one detent per `steps_per_detent` net
    /// steps arriving at a rest position. Use `2` for the common detented
    /// encoders whose detents sit every half quadrature cycle.
    #[must_use]
    pub fn new(steps_per_detent: u32) -> Self {
        assert!(steps_per_detent > 0, "steps_per_detent must be positive");
        Self {
            steps_per_detent: i32::try_from(steps_per_detent).unwrap(),
            net: 0,
        }
    }

    /// Feeds one observation and returns the signed detents to report
    /// (positive clockwise, negative counter-clockwise; usually -1, 0, or 1,
    /// but a batched burst across several detents can return more).
    ///
    /// `steps` is the net filtered step count since the previous call, in
    /// either direction. `at_rest` must be true exactly when the encoder sits
    /// at a mechanical rest position at the time of this observation.
    ///
    /// Detents are evaluated only at rest: intermediate steps are carried, so
    /// a twitch that leaves a rest and returns to it nets to zero and reports
    /// nothing. At a rest, any full `steps_per_detent` groups are reported and
    /// the remainder (a glitch, in practice) is carried over.
    pub fn update(&mut self, steps: i32, at_rest: bool) -> i32 {
        self.net += steps;
        if !at_rest {
            return 0;
        }
        let detents = self.net / self.steps_per_detent;
        self.net -= detents * self.steps_per_detent;
        detents
    }

    /// Net steps currently carried since the last rest evaluation. Diagnostic
    /// aid; `0` whenever the encoder has been at rest since the last movement.
    #[must_use]
    pub fn pending(&self) -> i32 {
        self.net
    }
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new(2)
    }
}

/// Quadrature transition table: signed step for a pin-level transition.
///
/// Positive is clockwise, following the cycle `11→01→00→10→11`; negative is
/// counter-clockwise. Direct jumps that change both pins (a missed
/// intermediate state) and non-transitions are `0`.
#[rustfmt::skip]
const QUAD_TABLE: [[i8; 4]; 4] = [
    [ 0, -1,  1,  0], // from 00
    [ 1,  0,  0, -1], // from 01
    [-1,  0,  0,  1], // from 10
    [ 0,  1, -1,  0], // from 11
];

/// Maps pin levels to a quadrature state index in `0..4`.
#[must_use]
pub const fn state(a: bool, b: bool) -> u8 {
    ((a as u8) << 1) | (b as u8)
}

/// Signed step for a pin-level transition: `+1` clockwise, `-1`
/// counter-clockwise, `0` for no change or an invalid both-pins jump.
///
/// Useful both as a building block for level-driven decoding and as a
/// cross-check against a hardware pulse counter: a nonzero counter delta with
/// a `0` here means the intermediate state was never observed.
#[must_use]
pub fn quad_step(from_a: bool, from_b: bool, to_a: bool, to_b: bool) -> i8 {
    QUAD_TABLE[state(from_a, from_b) as usize][state(to_a, to_b) as usize]
}

/// Pin-level front end for platforms without a hardware step counter.
///
/// Feed the pin levels on every observed change — from a pin-edge interrupt,
/// or from a quiet-window sample once the contacts have settled. Each call
/// diffs the levels through the quadrature table and forwards the step to the
/// anchored [`Decoder`], using the common detent alignment's rest predicate
/// (both pins equal).
///
/// The first call only establishes the baseline and reports nothing.
///
/// # Limitation
///
/// If several edges occur between calls, the intermediate states are
/// unobservable: a level jump that skipped a state yields `0` steps for that
/// gap. Sampling on every edge (or after a quiet window short enough that a
/// human turn can't complete a detent inside it) keeps the loss negligible.
/// A hardware counter driving [`Decoder`] directly has no such gap.
#[derive(Debug, Clone)]
pub struct PinDecoder {
    last: Option<(bool, bool)>,
    decoder: Decoder,
}

impl PinDecoder {
    /// Creates a pin-level decoder reporting one detent per
    /// `steps_per_detent` net steps at a rest position.
    #[must_use]
    pub fn new(steps_per_detent: u32) -> Self {
        Self {
            last: None,
            decoder: Decoder::new(steps_per_detent),
        }
    }

    /// Feeds one pin-level observation; returns the signed detents to report
    /// (positive is clockwise). Calling it again with unchanged levels is
    /// harmless and reports nothing.
    pub fn update(&mut self, a: bool, b: bool) -> i32 {
        let steps = match self.last {
            Some((last_a, last_b)) => i32::from(quad_step(last_a, last_b, a, b)),
            None => 0,
        };
        self.last = Some((a, b));
        self.decoder.update(steps, a == b)
    }
}

impl Default for PinDecoder {
    fn default() -> Self {
        Self::new(2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feeds a replayed raw-counter stream (one entry per observed position,
    /// as captured in device logs), deriving steps from deltas and the rest
    /// predicate from even positions. Returns the nonzero detent reports.
    fn replay(raws: &[i16]) -> Vec<i32> {
        let mut dec = Decoder::new(2);
        let mut last = raws[0];
        let mut reports = Vec::new();
        for &raw in &raws[1..] {
            let steps = i32::from(raw) - i32::from(last);
            last = raw;
            let detents = dec.update(steps, raw % 2 == 0);
            if detents != 0 {
                reports.push(detents);
            }
        }
        reports
    }

    /// Clean run replayed from a device log: 3 detents clockwise, 5 counter,
    /// then five single alternating reversal detents. Every detent ticks
    /// exactly once, on arrival at the rest position.
    #[test]
    fn clean_run_replayed_from_device_log() {
        let raws = [
            0, 1, 2, 3, 4, 5, 6, 5, 4, 3, 2, 1, 0, -1, -2, -3, -4, //
            -3, -2, -3, -4, -3, -2, -3, -4, -3, -2,
        ];
        assert_eq!(
            replay(&raws),
            [1, 1, 1, -1, -1, -1, -1, -1, 1, -1, 1, -1, 1]
        );
    }

    /// Spring-back twitches replayed from a device log: the dial repeatedly
    /// stepped out one count and returned to rest (`11↔10`, `11↔01`) plus
    /// hardware-filtered bounce. The old count-dividing decoder reported five
    /// phantom ticks here; the anchored decoder reports none.
    #[test]
    fn spring_back_twitch_cancels() {
        let raws = [6, 5, 6, 6, 7, 6, 5, 6, 5, 6, 5];
        assert!(replay(&raws).is_empty());
    }

    /// A twitch that hesitates out and back but then commits to the detent
    /// reports exactly one tick, on arrival.
    #[test]
    fn hesitation_then_commit_reports_once() {
        let raws = [6, 5, 6, 5, 4];
        assert_eq!(replay(&raws), [-1]);
    }

    /// A reversal detent is decoded identically to a continuing one: two net
    /// steps, one tick at arrival. (Device logs show the "1-count reversal" is
    /// a decoder-phase artifact, not hardware behavior.)
    #[test]
    fn reversal_detent_is_uniform() {
        assert_eq!(replay(&[4, 5, 6]), [1]);
        assert_eq!(replay(&[6, 5, 4]), [-1]);
    }

    /// Batched bursts (fast flicks, one observation spanning several detents)
    /// report every detent crossed.
    #[test]
    fn batched_burst_reports_every_detent() {
        let mut dec = Decoder::new(2);
        assert_eq!(dec.update(4, false), 0);
        assert_eq!(dec.update(0, true), 2);
        assert_eq!(dec.update(-4, true), -2);
        // Partial burst: remainder carries until the rest position completes it.
        assert_eq!(dec.update(3, false), 0);
        assert_eq!(dec.update(1, true), 2);
    }

    /// Repeated observations at rest with no movement are silent.
    #[test]
    fn rest_repeats_are_silent() {
        let mut dec = Decoder::new(2);
        for _ in 0..5 {
            assert_eq!(dec.update(0, true), 0);
        }
        assert_eq!(dec.pending(), 0);
    }

    /// An odd step count at rest (a glitch; anchors normally enforce evenness)
    /// reports the truncated detents and carries the remainder.
    #[test]
    fn odd_net_at_rest_carries_remainder() {
        let mut dec = Decoder::new(2);
        assert_eq!(dec.update(3, true), 1);
        assert_eq!(dec.pending(), 1);
        assert_eq!(dec.update(-1, true), 0);
        assert_eq!(dec.pending(), 0);
        assert_eq!(dec.update(-3, true), -1);
        assert_eq!(dec.pending(), -1);
    }

    /// The full clockwise cycle steps +1 per transition, the reverse -1, and
    /// both-pins jumps are invalid.
    #[test]
    fn quad_step_directions_and_invalid_jumps() {
        let cw = [
            (true, true),
            (false, true),
            (false, false),
            (true, false),
            (true, true),
        ];
        for pair in cw.windows(2) {
            let ((fa, fb), (ta, tb)) = (pair[0], pair[1]);
            assert_eq!(quad_step(fa, fb, ta, tb), 1);
            assert_eq!(quad_step(ta, tb, fa, fb), -1);
        }
        assert_eq!(quad_step(false, false, true, true), 0);
        assert_eq!(quad_step(true, true, false, false), 0);
        assert_eq!(quad_step(true, false, true, false), 0);
        assert_eq!(state(true, false), 2);
    }

    /// Drives a `PinDecoder` with a level sequence, returning nonzero reports.
    fn drive(levels: &[(bool, bool)]) -> Vec<i32> {
        let mut dec = PinDecoder::new(2);
        levels
            .iter()
            .map(|&(a, b)| dec.update(a, b))
            .filter(|&d| d != 0)
            .collect()
    }

    /// One clockwise detent through the pin-level front end: two steps,
    /// arriving at the resting position.
    #[test]
    fn pin_decoder_clean_detent() {
        // One detent: 11 -> 01 -> 00. The full cycle back to 11 is a second.
        let one = [(true, true), (false, true), (false, false)];
        assert_eq!(drive(&one), [1]);
        // Full clockwise cycle: 11 -> 01 -> 00 -> 10 -> 11 = two detents.
        let full = [
            (true, true),
            (false, true),
            (false, false),
            (true, false),
            (true, true),
        ];
        assert_eq!(drive(&full), [1, 1]);
    }

    /// A spring-back twitch through the pin-level front end reports nothing,
    /// and the first call only establishes the baseline.
    #[test]
    fn pin_decoder_spring_back_cancels() {
        let levels = [(true, true), (false, true), (true, true)];
        assert!(drive(&levels).is_empty());
        // Repeating the resting levels is silent.
        assert!(drive(&[(true, true), (true, true), (true, true)]).is_empty());
    }

    /// A level sample that skipped an intermediate state (both pins changed
    /// between observations) recovers on the following valid steps: the rest
    /// anchor comes from the levels, not from counted steps.
    #[test]
    fn pin_decoder_recovers_from_skipped_state() {
        // 00 ->(skipped) 11 -> 01 -> 00: the invalid jump contributes no step,
        // the next two valid steps still net 2 at the rest position.
        let levels = [(false, false), (true, true), (false, true), (false, false)];
        assert_eq!(drive(&levels), [1]);
    }
}
