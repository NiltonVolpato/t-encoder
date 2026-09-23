# detent-decoder

Detent-event decoding for incremental quadrature rotary encoders. `no_std`,
zero dependencies, host-testable.

```rust
use detent_decoder::PinDecoder;

let mut encoder = PinDecoder::new(2); // 2 pulses per detent

// Call on every pin change (edge interrupt) or once the pins have settled:
let detents = encoder.update(pin_a.is_high(), pin_b.is_high());
if detents != 0 {
    position += detents; // +1 per clockwise detent, -1 per counter-clockwise
}
```

## The problem this solves

Detented encoders are the most common rotary input, and the most
surprisingly hard to decode reliably. A hand-turned detent does not produce a
clean two-step quadrature advance. It produces, variously:

- **Spring-backs (twitches)** — the mechanism steps out one count and returns
  to rest. From any counter or threshold decoder this is indistinguishable
  from a real reversal, and it *ticks*.
- **Bounce** — contact chatter around transitions, in quantities your glitch
  filter does not fully absorb.
- **Hesitation** — the dial crosses one count, returns, then completes the
  detent. Two detents' worth of edges, one detent of intent.
- **Bursts** — fast flicks deliver several edges between your samples.
- **Stale reads** — hardware glitch filters delay counting by several
  microseconds after the pin edge, so a counter sampled at interrupt time
  can be one edge behind the pins.

Decoder after decoder (remainder accumulators, direction-tracking rewrites,
threshold crossings) fixes some of these and trips over the rest, usually
reporting a phantom step or swallowing a real one depending on invisible
phase alignment.

## The model

A detented encoder's **mechanical rest positions are the only unambiguous
landmarks** in the quadrature stream. On the common detented alignment both
pins are equal at rest (`A == B`), and exactly `steps_per_detent` (typically
2) filtered steps separate one rest from the next.

This crate decodes by anchoring to rests: steps accumulate **algebraically**
(out-and-back motion cancels itself), and detents are reported when the net
count reaches `steps_per_detent` exactly at a rest position:

| Input | Threshold decoders | This crate |
|---|---|---|
| Clean detent | 1 tick | 1 tick |
| Spring-back (out 1, back 1) | **1 phantom tick** | 0 |
| Hesitation then commit | 0–2 ticks (phase-dependent) | 1 tick, on arrival |
| Fast flick, 3 detents in one sample | lost or mis-counted | 3 ticks |
| Reversal detent | often special-cased | uniform: 2 steps, 1 tick |

Because only the *net* count at rests matters, the order of intermediate
steps is irrelevant and no direction-change special cases exist.

## Two ways to feed it

### From a hardware pulse counter (ESP32 PCNT, RP2040 PIO, timer QEI…)

The counter — behind its hardware glitch filter — is the step source. Feed
deltas, and derive the rest predicate from the pin levels (or, if your wiring
is fixed, from counter parity):

```rust
use detent_decoder::Decoder;

let mut decoder = Decoder::new(2);
// Whenever you sample settled state:
let steps = counter.value().wrapping_sub(last_raw);
let detents = decoder.update(steps as i32, pin_a.is_high() == pin_b.is_high());
```

This mode is burst-safe (a 6-count delta across three detents reports 3) and
never loses an edge. `quad_step()` is provided to cross-check the counter
against observed pin levels.

### From raw pin levels (`PinDecoder`)

For platforms with just two GPIOs and no counter. Levels are diffed through
the quadrature table; invalid jumps (both pins changed between samples) are
ignored and the rest anchor recovers on the next valid steps.

Limitation: states that occurred entirely between two samples are
unobservable. Call it on **every pin edge**, or after a **quiet window**
short enough that a flick can't complete a detent inside it.

## When to sample

This crate does not decide *when* you look at the pins, and getting that
wrong breaks any decoder:

- **Edge interrupt**: fine, but remember a hardware glitch filter delays the
  counter after the edge — sample or decode after the filter delay
  (~12.5 µs at 1000 APB cycles on ESP32), not at handler entry.
- **Polling**: ~1 kHz works for hand-turned dials; sample only settled levels.
- If your counter has no glitch filter, add a short **quiet-window**
  requirement (e.g. re-read after 5 ms of no edges) before trusting a rest.

## Why not the existing crates?

- [`sb-rotary-encoder-rs`](https://crates.io/crates/sb-rotary-encoder) fires
  whenever the accumulator crosses a multiple of the pulse divider — a
  spring-back (`+1`, `−1`) returns through zero and **ticks on the way back**
  (verified against real device logs). Its resync also assumes a single fixed
  rest state, while many detented encoders alternate between two.
- [`rotary-encoder-embedded`](https://crates.io/crates/rotary-encoder-embedded)
  `QuadratureTableMode` (threshold accumulator) is the closest match and does
  cancel spring-backs, but keeps a residue instead of re-anchoring at rests,
  so a missed edge can leave it half a detent off until enough clean motion
  accumulates.
- [`rotary-encoder-hal`](https://crates.io/crates/rotary-encoder-hal) decodes
  direction per sample but has no detent/rest concept; you supply the
  threshold policy yourself.

All three are fine libraries; none anchor to rest positions.

## Tests

The test suite replays sequences captured from a real (bouncy, twitchy,
hand-turned) encoder on an ESP32, including a spring-back excerpt that makes
threshold decoders emit five phantom ticks (this crate emits zero).

```sh
cargo test
```

## License

MIT
