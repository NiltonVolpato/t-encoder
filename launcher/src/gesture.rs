//! Touch-gesture recognition: a stroke of samples in, one [`Gesture`] out.
//!
//! Touch belongs to the system, not to apps. This is the router's own
//! vocabulary — an app sees a gesture's *effect* (navigation), never the
//! samples, unless its manifest opts into the raw panel.
//!
//! Pure and host-tested: the recogniser measures pixels and milliseconds and
//! knows nothing about I2C, so the whole gesture contract is exercised on the
//! host and only the sampling loop has to be trusted on device.

/// Where a sample sits in a stroke.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TouchPhase {
    /// A finger arrived.
    Down,
    /// A finger moved while still on the panel.
    Move,
    /// A finger left the panel; the sample carries its last known position.
    Up,
}

/// One reading from the touch panel, in panel coordinates.
///
/// `y` grows downward, matching the framebuffer — so a swipe *up* the screen
/// has a negative `dy`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TouchSample {
    /// Where this sample sits in the stroke.
    pub phase: TouchPhase,
    /// X position in panel pixels.
    pub x: i32,
    /// Y position in panel pixels.
    pub y: i32,
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

/// How far a stroke may wander and still be a tap. Fingers roll a little on a
/// panel this small; anything past this was a drag, and a drag that is not a
/// swipe means nothing.
const TAP_MAX_WANDER: i32 = 16;

/// Pressing the encoder also registers a touch on this panel. A stroke that
/// begins within this long after the button is released is that phantom
/// settling, not a new gesture.
const PHANTOM_GRACE_MS: u64 = 250;

/// A stroke in progress: only its ends and its worst excursion matter.
#[derive(Clone, Copy, Debug)]
struct Stroke {
    start_x: i32,
    start_y: i32,
    /// Farthest the finger has been from the start, so a stroke that wandered
    /// out and came back is not reported as a tap.
    wander: i32,
    /// Set when the stroke overlaps an encoder press. Pressing the encoder
    /// registers a phantom touch, and a phantom must never navigate.
    suppressed: bool,
}

impl Stroke {
    /// Records that the finger reached `(x, y)`.
    fn reached(&mut self, x: i32, y: i32) {
        let dx = x.saturating_sub(self.start_x).saturating_abs();
        let dy = y.saturating_sub(self.start_y).saturating_abs();
        self.wander = self.wander.max(dx.max(dy));
    }
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
        match sample.phase {
            TouchPhase::Down => {
                self.stroke = Some(Stroke {
                    start_x: sample.x,
                    start_y: sample.y,
                    wander: 0,
                    suppressed: self.is_phantom(sample.at_ms),
                });
                None
            }
            TouchPhase::Move => {
                if let Some(stroke) = self.stroke.as_mut() {
                    stroke.reached(sample.x, sample.y);
                }
                None
            }
            TouchPhase::Up => {
                // Take it either way: an `Up` always ends the stroke, even a
                // suppressed one, so the next press starts clean.
                let mut stroke = self.stroke.take()?;
                stroke.reached(sample.x, sample.y);
                if stroke.suppressed {
                    return None;
                }
                classify(&stroke, sample.x, sample.y)
            }
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

/// Classifies a finished stroke that ended at `(end_x, end_y)`.
fn classify(stroke: &Stroke, end_x: i32, end_y: i32) -> Option<Gesture> {
    let dx = end_x.saturating_sub(stroke.start_x);
    let dy = end_y.saturating_sub(stroke.start_y);
    let across = dx.saturating_abs();
    let down = dy.saturating_abs();

    if across >= SWIPE_MIN_TRAVEL && across >= down.saturating_mul(AXIS_RATIO) {
        return Some(if dx < 0 {
            Gesture::SwipeLeft
        } else {
            Gesture::SwipeRight
        });
    }
    if down >= SWIPE_MIN_TRAVEL && down >= across.saturating_mul(AXIS_RATIO) {
        // `y` grows downward, so a negative `dy` is a swipe up the screen.
        return Some(if dy < 0 {
            Gesture::SwipeUp
        } else {
            Gesture::SwipeDown
        });
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
