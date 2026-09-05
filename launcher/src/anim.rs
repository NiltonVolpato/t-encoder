//! Time-driven tweening for launcher transitions.
//!
//! Provisional: if the P2 Slint spike succeeds, Slint supplies transitions
//! declaratively and this module goes away. It is kept small for that reason,
//! and because the embedded-graphics fallback path needs it either way.
//!
//! Values are fixed-point hundredths rather than `f32`, so the maths is exact,
//! panic-free, and identical on host and device.

/// Fixed-point scale: progress runs `0..=SCALE`.
const SCALE: i64 = 10_000;

/// Cubic ease-out applied to normalized progress `t` in `0..=SCALE`.
///
/// `1 - (1 - t)^3` — fast at first, settling gently. Chosen because a carousel
/// that decelerates into place reads as physical; linear motion reads as cheap.
#[must_use]
fn ease_out_cubic(t: i64) -> i64 {
    let t = t.clamp(0, SCALE);
    let inv = SCALE.saturating_sub(t);
    // inv^3 / SCALE^2, staying inside i64 by dividing as we go.
    let inv2 = inv.saturating_mul(inv).saturating_div(SCALE);
    let inv3 = inv2.saturating_mul(inv).saturating_div(SCALE);
    SCALE.saturating_sub(inv3)
}

/// A scalar animated from one value to another over a fixed duration.
///
/// Sampled by absolute time, so a dropped or late frame lands at the right
/// place rather than accumulating drift.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tween {
    from: i32,
    to: i32,
    start_ms: u64,
    duration_ms: u32,
}

impl Tween {
    /// A tween that is already finished at `to` — the resting state.
    #[must_use]
    pub const fn settled(to: i32) -> Tween {
        Tween {
            from: to,
            to,
            start_ms: 0,
            duration_ms: 0,
        }
    }

    /// Starts a tween from the current value toward `to`.
    ///
    /// Retargeting mid-flight starts from wherever the value is *now*, so
    /// rapid encoder turns chain smoothly instead of snapping back.
    #[must_use]
    pub fn retarget(self, to: i32, now_ms: u64, duration_ms: u32) -> Tween {
        Tween {
            from: self.value_at(now_ms),
            to,
            start_ms: now_ms,
            duration_ms,
        }
    }

    /// The target value.
    #[must_use]
    pub const fn target(self) -> i32 {
        self.to
    }

    /// Whether the tween has reached its target.
    #[must_use]
    pub fn is_settled(self, now_ms: u64) -> bool {
        self.progress(now_ms) >= SCALE
    }

    /// Normalized progress in `0..=SCALE`.
    fn progress(self, now_ms: u64) -> i64 {
        if self.duration_ms == 0 {
            return SCALE;
        }
        let elapsed = now_ms.saturating_sub(self.start_ms);
        let elapsed = i64::try_from(elapsed).unwrap_or(i64::MAX);
        let duration = i64::from(self.duration_ms);
        elapsed
            .saturating_mul(SCALE)
            .checked_div(duration)
            .unwrap_or(SCALE)
            .clamp(0, SCALE)
    }

    /// The eased value at `now_ms`.
    #[must_use]
    pub fn value_at(self, now_ms: u64) -> i32 {
        let eased = ease_out_cubic(self.progress(now_ms));
        let span = i64::from(self.to).saturating_sub(i64::from(self.from));
        let offset = span
            .saturating_mul(eased)
            .checked_div(SCALE)
            .unwrap_or(span);
        let value = i64::from(self.from).saturating_add(offset);
        i32::try_from(value).unwrap_or(self.to)
    }
}
