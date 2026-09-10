//! Touch-gesture recognition: a stroke of samples in, one [`Gesture`] out.
//!
//! Touch belongs to the system, not to apps. This is the router's own
//! vocabulary — an app sees a gesture's *effect* (navigation), never the
//! samples, unless its manifest opts into the raw panel.
//!
//! Pure and host-tested: the recogniser measures pixels and milliseconds and
//! knows nothing about I2C, so the whole gesture contract is exercised on the
//! host and only the sampling loop has to be trusted on device.

use crate::geometry;
use enc_touch::{TouchEvent, TouchPoint};

/// One reading from the touch panel, in panel coordinates.
///
/// `y` grows downward, matching the framebuffer — so a swipe *up* the screen
/// has a negative `dy`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TouchSample {
    /// Position and event type reported by the touch controller.
    pub point: TouchPoint,
    /// Monotonic milliseconds, from the same clock as [`crate::Ctx::now_ms`].
    pub at_ms: u64,
}

/// A completed touch gesture. Emitted on the stroke's `Up`, never before: a
/// swipe is only a swipe once you know where the finger stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gesture {
    /// The finger landed and left without travelling.
    Tap {
        /// X of the touch-down point.
        x: i32,
        /// Y of the touch-down point.
        y: i32,
    },
    /// Dragged right-to-left across the panel — "back".
    SwipeLeft,
    /// Dragged left-to-right across the panel.
    SwipeRight,
    /// Dragged bottom-to-top across the panel — "exit".
    SwipeUp,
    /// Dragged top-to-bottom across the panel.
    SwipeDown,
    /// Hold progress reported while finger is held stationary (0..100%).
    HoldProgress {
        /// Progress percentage from 300ms (0%) to 600ms (100%).
        progress_pct: u8,
    },
    /// Stationary touch hold completed (600ms).
    Hold,
}

/// Travel along the dominant axis before a drag counts as a swipe: 220 px of
/// the panel's 390. A navigation gesture has to cross most of the screen, so
/// an in-app drag can never be mistaken for one — the plan's "all the way
/// across", with enough slack that a finger need not reach the round rim.
const SWIPE_MIN_TRAVEL: i32 = 220;

/// How far a swipe must beat its off-axis travel. At 2 the gesture has to be
/// recognisably horizontal or vertical, so a diagonal drag resolves to nothing
/// rather than to whichever axis happened to win by a pixel.
const AXIS_RATIO: i32 = 2;

/// How much longer than its straight-line travel a swipe's path may be, in
/// percent. A straight drag measures ~100 and a gentle arc ~103, while a V
/// measures ~135 and a spiral far worse. Endpoints alone are not enough:
/// circling the middle of the screen and finishing at the left rim is not a
/// swipe left, however much it looks like one to a subtraction.
const SWIPE_MAX_PATH_PCT: i32 = 120;

/// Minimum movement before a segment counts toward the path length.
///
/// Without it the path measurement depends on the sample rate: a finger held
/// steady still reports a pixel or two of jitter every 10 ms, and summing those
/// makes a slow straight drag score worse than a fast crooked one. Below this,
/// a sample moves nothing.
const PATH_STEP: i32 = 20;

/// How long a swipe may take. A navigation gesture is a flick, not a journey —
/// without this, a minute of wandering that happens to end at the far edge
/// still quit the app.
const SWIPE_MAX_MS: u64 = 800;

/// How far a stroke may wander and still be a tap. Fingers roll a little on a
/// panel this small; anything past this was a drag, and a drag that is not a
/// swipe means nothing.
const TAP_MAX_WANDER: i32 = 16;

/// Pressing the encoder also registers a touch on this panel. A stroke that
/// begins within this long after the button is released is that phantom
/// settling, not a new gesture.
const PHANTOM_GRACE_MS: u64 = 250;

/// Time before a stationary touch begins reporting hold progress.
pub const HOLD_START_MS: u64 = 300;

/// Time at which a stationary touch completes as a Hold gesture.
pub const HOLD_COMPLETE_MS: u64 = 600;

/// Whether a reported point is actually on the panel.
///
/// The controller's coordinate registers are 12-bit and can read back garbage —
/// the first poll after reset returns a stale point, `501,3784` observed on
/// this unit. A garbage endpoint is indistinguishable from an enormous swipe
/// (that one measures as 3591 px upward), and panel bounds are the one thing
/// we can check it against.
fn on_panel(x: u16, y: u16) -> bool {
    x < geometry::WIDTH && y < geometry::HEIGHT
}

/// A stroke in progress.
#[derive(Clone, Copy, Debug)]
struct Stroke {
    start_x: i32,
    start_y: i32,
    /// When the finger landed, for the swipe's time limit.
    start_ms: u64,
    /// Last point that advanced the path, so jitter accumulates nothing.
    anchor_x: i32,
    anchor_y: i32,
    /// Distance actually travelled, in `PATH_STEP`-sized segments.
    path: i32,
    /// Farthest the finger has been from the start, so a stroke that wandered
    /// out and came back is not reported as a tap.
    wander: i32,
    /// Set when the stroke overlaps an encoder press. Pressing the encoder
    /// registers a phantom touch, and a phantom must never navigate.
    suppressed: bool,
    /// Whether the Hold gesture has already been emitted for this stroke.
    hold_emitted: bool,
    /// Last progress percentage reported for hold (0..100).
    last_progress_pct: Option<u8>,
}

impl Stroke {
    /// Starts a stroke at `sample`.
    fn landed(sample: TouchSample, suppressed: bool) -> Stroke {
        let x = i32::from(sample.point.x);
        let y = i32::from(sample.point.y);
        Stroke {
            start_x: x,
            start_y: y,
            start_ms: sample.at_ms,
            anchor_x: x,
            anchor_y: y,
            path: 0,
            wander: 0,
            suppressed,
            hold_emitted: false,
            last_progress_pct: None,
        }
    }

    /// Records that the finger reached `(x, y)`.
    fn reached(&mut self, x: i32, y: i32) {
        let dx = x.saturating_sub(self.anchor_x);
        let dy = y.saturating_sub(self.anchor_y);
        if dx.saturating_abs().max(dy.saturating_abs()) >= PATH_STEP {
            self.path = self.path.saturating_add(distance(dx, dy));
            self.anchor_x = x;
            self.anchor_y = y;
        }

        let from_start_x = x.saturating_sub(self.start_x).saturating_abs();
        let from_start_y = y.saturating_sub(self.start_y).saturating_abs();
        self.wander = self.wander.max(from_start_x.max(from_start_y));
    }

    /// Adds the last part-segment, so the path is not short by up to one step.
    fn closed(&mut self, x: i32, y: i32) {
        let dx = x.saturating_sub(self.anchor_x);
        let dy = y.saturating_sub(self.anchor_y);
        self.path = self.path.saturating_add(distance(dx, dy));
    }
}

/// Length of the vector `(dx, dy)`, rounded down.
///
/// Euclidean rather than Manhattan: on a near-straight drag Manhattan charges
/// the full cross-axis jitter, which is what made the measurement depend on how
/// crooked the *controller* was rather than how crooked the finger was.
fn distance(dx: i32, dy: i32) -> i32 {
    dx.saturating_mul(dx)
        .saturating_add(dy.saturating_mul(dy))
        .isqrt()
}

/// Turns a stream of [`TouchSample`]s into [`Gesture`]s.
///
/// It also needs to know when the encoder button is down — see
/// [`Recognizer::set_button`] — because on this board a press is felt by the
/// touch panel too.
#[derive(Clone, Copy, Debug, Default)]
pub struct Recognizer {
    stroke: Option<Stroke>,
    button_down: bool,
    /// When the button was last released, for the phantom-touch window.
    button_up_ms: Option<u64>,
}

impl Recognizer {
    /// A recogniser with no stroke in flight.
    #[must_use]
    pub const fn new() -> Recognizer {
        Recognizer {
            stroke: None,
            button_down: false,
            button_up_ms: None,
        }
    }

    /// Feeds one sample, returning the gesture it completed.
    pub fn push(&mut self, sample: TouchSample) -> Option<Gesture> {
        // An off-panel reading is the controller talking nonsense, not a
        // finger. Drop it whole rather than let it start, extend or end a
        // stroke: a stroke built on a garbage endpoint measures as a swipe.
        if !on_panel(sample.point.x, sample.point.y) {
            return None;
        }
        let x = i32::from(sample.point.x);
        let y = i32::from(sample.point.y);
        match sample.point.event {
            TouchEvent::Down => {
                self.stroke = Some(Stroke::landed(sample, self.is_phantom(sample.at_ms)));
                None
            }
            TouchEvent::Move => {
                let stroke = self.stroke.as_mut()?;
                stroke.reached(x, y);
                if stroke.suppressed || stroke.hold_emitted {
                    return None;
                }
                if stroke.wander <= TAP_MAX_WANDER {
                    let elapsed = sample.at_ms.saturating_sub(stroke.start_ms);
                    if elapsed >= HOLD_COMPLETE_MS {
                        stroke.hold_emitted = true;
                        stroke.last_progress_pct = Some(100);
                        return Some(Gesture::Hold);
                    }
                    if elapsed >= HOLD_START_MS {
                        let span = HOLD_COMPLETE_MS.saturating_sub(HOLD_START_MS);
                        let progress = if span > 0 {
                            elapsed
                                .saturating_sub(HOLD_START_MS)
                                .saturating_mul(100)
                                .checked_div(span)
                                .unwrap_or(0)
                        } else {
                            100
                        };
                        let progress_pct = u8::try_from(progress.min(100)).unwrap_or(100);
                        if stroke.last_progress_pct != Some(progress_pct) {
                            stroke.last_progress_pct = Some(progress_pct);
                            return Some(Gesture::HoldProgress { progress_pct });
                        }
                    }
                } else if stroke.last_progress_pct.is_some_and(|p| p > 0) {
                    stroke.last_progress_pct = Some(0);
                    return Some(Gesture::HoldProgress { progress_pct: 0 });
                }
                None
            }
            TouchEvent::Up => {
                // Take it either way: an `Up` always ends the stroke, even a
                // suppressed one, so the next press starts clean.
                let mut stroke = self.stroke.take()?;
                stroke.reached(x, y);
                stroke.closed(x, y);
                if stroke.suppressed || stroke.hold_emitted {
                    return None;
                }
                if stroke.last_progress_pct.is_some_and(|p| p > 0) {
                    return Some(Gesture::HoldProgress { progress_pct: 0 });
                }
                let gesture = classify(&stroke, x, y, sample.at_ms);
                if let Some(ref g) = gesture {
                    log::debug!("gesture: recognized {g:?}");
                }
                gesture
            }
            TouchEvent::Unknown(_) => None,
        }
    }

    /// Tells the recogniser whether the encoder button is physically down.
    ///
    /// This is deliberately *not* one of the router's press events: those are
    /// semantic (a short press fires on release, up to the long-press
    /// threshold later), while the phantom touch tracks the physical contact.
    /// Repeated calls with an unchanged state are free.
    pub fn set_button(&mut self, down: bool, at_ms: u64) {
        if down == self.button_down {
            return;
        }
        self.button_down = down;
        if down {
            // A press part-way through a stroke condemns it: whatever the
            // panel is reporting, the user's intent was the button.
            if let Some(stroke) = self.stroke.as_mut() {
                stroke.suppressed = true;
            }
        } else {
            self.button_up_ms = Some(at_ms);
        }
    }

    /// Whether a stroke starting at `at_ms` is the encoder press's phantom.
    fn is_phantom(&self, at_ms: u64) -> bool {
        self.button_down
            || self
                .button_up_ms
                .is_some_and(|up| at_ms.saturating_sub(up) < PHANTOM_GRACE_MS)
    }
}

/// Classifies a finished stroke that ended at `(end_x, end_y)` at `end_ms`.
fn classify(stroke: &Stroke, end_x: i32, end_y: i32, end_ms: u64) -> Option<Gesture> {
    let dx = end_x.saturating_sub(stroke.start_x);
    let dy = end_y.saturating_sub(stroke.start_y);
    let across = dx.saturating_abs();
    let down = dy.saturating_abs();

    // `y` grows downward, so a negative `dy` is a swipe up the screen.
    let (travel, direction) = if across >= down {
        (
            across,
            if dx < 0 {
                Gesture::SwipeLeft
            } else {
                Gesture::SwipeRight
            },
        )
    } else {
        (
            down,
            if dy < 0 {
                Gesture::SwipeUp
            } else {
                Gesture::SwipeDown
            },
        )
    };

    // Far enough, straight enough, one-directional enough, and quick enough.
    // Any one of these alone is trivially fooled — the last two were both found
    // on hardware, by a spiral and by a leisurely wander respectively.
    let off_axis = across.min(down);
    if travel >= SWIPE_MIN_TRAVEL
        && travel >= off_axis.saturating_mul(AXIS_RATIO)
        && stroke.path.saturating_mul(100) <= travel.saturating_mul(SWIPE_MAX_PATH_PCT)
        && end_ms.saturating_sub(stroke.start_ms) <= SWIPE_MAX_MS
    {
        return Some(direction);
    }
    if stroke.wander <= TAP_MAX_WANDER {
        return Some(Gesture::Tap {
            x: stroke.start_x,
            y: stroke.start_y,
        });
    }
    // A drag that went somewhere but not far enough, in no clear direction.
    None
}
