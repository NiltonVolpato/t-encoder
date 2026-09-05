//! Pomodoro timer.
//!
//! Encoder sets the duration, press starts/pauses, the ring counts down, and
//! the buzzer fires at zero. Deliberately needs no network: it exercises every
//! input the board has and works on a bare device.
//!
//! Timing is derived from an absolute start instant rather than accumulated per
//! tick, so a slow frame or a missed tick cannot make the timer drift.

use launcher::{Action, App, Ctx, Dirty, Feedback, IconId, InputEvent, Manifest, Outcome};
use ui::{PomodoroState, Shell};

/// Shortest settable duration.
const MIN_MINUTES: u32 = 1;
/// Longest settable duration. Capped at 60 so the ring maps one dot per
/// minute — a full circle is one hour, like a clock face.
const MAX_MINUTES: u32 = 60;
/// Default work interval.
const DEFAULT_MINUTES: u32 = 25;
/// Seconds per minute.
const SECS_PER_MIN: u32 = 60;
/// Seconds in the full ring (60 dots x 1 minute).
const RING_SECS: u32 = MAX_MINUTES * SECS_PER_MIN;

/// Where the timer is in its lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// Stopped; the encoder sets the duration.
    Idle,
    /// Counting down.
    Running,
    /// Held, keeping its remaining time.
    Paused,
    /// Reached zero and buzzed.
    Done,
}

impl Phase {
    /// Label shown under the clock.
    const fn label(self) -> &'static str {
        match self {
            Phase::Idle => "SET",
            Phase::Running => "FOCUS",
            Phase::Paused => "PAUSED",
            Phase::Done => "DONE",
        }
    }
}

/// The pomodoro app.
pub struct Pomodoro {
    manifest: Manifest,
    /// Weak handle to the shared Slint tree. Weak rather than strong so the
    /// app never keeps the shell alive — Slint component handles are not
    /// `Clone`, and a strong handle here would be a reference cycle.
    shell: slint::Weak<Shell>,
    phase: Phase,
    /// Configured duration in whole minutes.
    minutes: u32,
    /// Seconds left when the current run started.
    remaining_at_start: u32,
    /// `now_ms` when the current run started; `None` unless running.
    started_ms: Option<u64>,
    /// Seconds left, recomputed each tick.
    remaining: u32,
}

impl Pomodoro {
    /// Builds the app against the shared shell.
    #[must_use]
    pub fn new(shell: slint::Weak<Shell>) -> Pomodoro {
        let minutes = DEFAULT_MINUTES;
        Pomodoro {
            manifest: Manifest {
                name: "Pomodoro",
                icon: IconId(3),
                // Warm red — this is the "do not disturb" app.
                accent: embedded_graphics::pixelcolor::Rgb565::new(28, 18, 8),
            },
            shell,
            phase: Phase::Idle,
            minutes,
            remaining_at_start: minutes.saturating_mul(SECS_PER_MIN),
            started_ms: None,
            remaining: minutes.saturating_mul(SECS_PER_MIN),
        }
    }

    /// Total configured seconds.
    fn total_secs(&self) -> u32 {
        self.minutes.saturating_mul(SECS_PER_MIN).max(1)
    }

    /// Recomputes `remaining` from the absolute start time.
    ///
    /// Returns whether the displayed second changed, so a running timer only
    /// repaints once a second instead of every 5 ms tick.
    fn recompute(&mut self, now_ms: u64) -> bool {
        let Some(started) = self.started_ms else {
            return false;
        };
        let elapsed_secs =
            u32::try_from(now_ms.saturating_sub(started) / 1_000).unwrap_or(u32::MAX);
        let next = self.remaining_at_start.saturating_sub(elapsed_secs);
        let changed = next != self.remaining;
        self.remaining = next;
        changed
    }

    /// Starts or resumes the countdown.
    fn start(&mut self, now_ms: u64) {
        self.remaining_at_start = if self.phase == Phase::Paused {
            self.remaining
        } else {
            self.total_secs()
        };
        self.remaining = self.remaining_at_start;
        self.started_ms = Some(now_ms);
        self.phase = Phase::Running;
    }

    /// Stops the countdown, keeping the remaining time.
    fn pause(&mut self) {
        self.started_ms = None;
        self.phase = Phase::Paused;
    }

    /// Returns to the idle state at the configured duration.
    fn reset(&mut self) {
        self.started_ms = None;
        self.phase = Phase::Idle;
        self.remaining = self.total_secs();
    }
}

impl App for Pomodoro {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn on_enter(&mut self, _ctx: &Ctx<'_>) {
        // Entering never disturbs a run in progress — you can check on a timer
        // and leave without resetting it.
        if self.phase == Phase::Idle {
            self.remaining = self.total_secs();
        }
    }

    fn handle(&mut self, event: InputEvent, ctx: &Ctx<'_>) -> Outcome {
        match event {
            // The encoder only means something while stopped; adjusting the
            // duration mid-run would be ambiguous about what it applies to.
            InputEvent::Rotate(delta) => {
                // Rotating a paused timer abandons it and goes back to setting
                // a duration — otherwise a paused timer has no way out except
                // finishing it, which is the gap that made "how do I reset?"
                // unanswerable.
                if self.phase == Phase::Paused || self.phase == Phase::Done {
                    self.reset();
                }
                if self.phase != Phase::Idle {
                    return Outcome::NONE;
                }
                let next = i64::from(self.minutes).saturating_add(i64::from(delta));
                let clamped = next.clamp(i64::from(MIN_MINUTES), i64::from(MAX_MINUTES));
                let minutes = u32::try_from(clamped).unwrap_or(DEFAULT_MINUTES);
                if minutes == self.minutes {
                    return Outcome::NONE;
                }
                self.minutes = minutes;
                self.remaining = self.total_secs();
                Outcome::dirty(Dirty::Full)
            }
            InputEvent::Select => {
                match self.phase {
                    Phase::Idle | Phase::Paused => self.start(ctx.now_ms),
                    Phase::Running => self.pause(),
                    Phase::Done => self.reset(),
                }
                Outcome::buzz(Dirty::Full, Feedback::Beep)
            }
            // Touch is reserved for global gestures; see the plan. Apps do not
            // receive it, so this arm exists only for completeness.
            InputEvent::Touch { .. } => Outcome::NONE,
        }
    }

    fn tick(&mut self, ctx: &Ctx<'_>) -> Outcome {
        if self.phase != Phase::Running {
            return Outcome::NONE;
        }
        if !self.recompute(ctx.now_ms) {
            return Outcome::NONE;
        }
        if self.remaining == 0 {
            self.phase = Phase::Done;
            self.started_ms = None;
            // Haptic rather than a beep: a finished pomodoro should be felt
            // even if the device is face-down or muted by ambient noise.
            return Outcome {
                dirty: Dirty::Full,
                action: Action::None,
                feedback: Some(Feedback::Haptic),
            };
        }
        Outcome::dirty(Dirty::Full)
    }

    fn sync(&self) {
        // The ring shows *remaining* time against a fixed one-hour scale, so a
        // dot is always one minute. Setting 30 minutes lights half the ring and
        // counting down unlights it at the same rate — the dial and the digits
        // never disagree, and the ring depletes rather than fills.
        let progress = (f32::from(u16::try_from(self.remaining).unwrap_or(u16::MAX))
            / f32::from(u16::try_from(RING_SECS).unwrap_or(1).max(1)))
        .clamp(0.0, 1.0);

        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        shell.set_pomodoro(PomodoroState {
            time_text: format_mmss(self.remaining).as_str().into(),
            progress,
            label: self.phase.label().into(),
            accent: phase_accent(self.phase),
        });
    }
}

/// Formats seconds as `MM:SS`.
fn format_mmss(secs: u32) -> heapless::String<8> {
    use core::fmt::Write;
    let mut text = heapless::String::new();
    let minutes = secs / 60;
    let seconds = secs % 60;
    let _ = write!(text, "{minutes:02}:{seconds:02}");
    text
}

/// Ring colour per phase — the state should be readable across a room without
/// reading the label.
fn phase_accent(phase: Phase) -> slint::Color {
    match phase {
        Phase::Idle => slint::Color::from_rgb_u8(120, 120, 130),
        Phase::Running => slint::Color::from_rgb_u8(235, 110, 60),
        Phase::Paused => slint::Color::from_rgb_u8(220, 180, 60),
        Phase::Done => slint::Color::from_rgb_u8(90, 220, 130),
    }
}
