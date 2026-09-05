//! Pomodoro timer.
//!
//! Rotate to set the time, press to start, press to pause, rotate to adjust or
//! cancel. Needs no network: it exercises every input the board has and works
//! on a bare device.
//!
//! There is one source of truth — `remaining` seconds — for both the digits and
//! the ring, so they cannot disagree. Rotation always adjusts `remaining`,
//! whatever the phase, and winding it down to zero cancels. While running, the
//! value is derived from an absolute start instant rather than accumulated per
//! tick, so a slow frame cannot make it drift.

use launcher::{
    App, AppFactory, Ctx, Feedback, IconId, InputEvent, Manifest, Outcome, TouchAccess, ViewId,
};
use ui::{PomodoroState, Shell};

/// Seconds per minute; also the rotation step.
const SECS_PER_MIN: u32 = 60;
/// A full ring is one hour, so one of the 60 dots is one minute.
const RING_SECS: u32 = 60 * SECS_PER_MIN;

/// Where the timer is in its lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// Stopped. `remaining` is the duration being dialled in.
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

/// Builds [`Pomodoro`] instances. Held in the launcher registry; the app itself
/// exists only while it is on screen.
pub struct PomodoroFactory {
    manifest: Manifest,
    shell: slint::Weak<Shell>,
}

impl PomodoroFactory {
    /// Creates the factory against the shared Slint tree.
    #[must_use]
    pub fn new(shell: slint::Weak<Shell>) -> PomodoroFactory {
        PomodoroFactory {
            manifest: Manifest {
                name: "Pomodoro",
                icon: IconId(0),
                view: ViewId(1),
                // Warm red — this is the "do not disturb" app.
                accent: embedded_graphics::pixelcolor::Rgb565::new(28, 18, 8),
                touch: TouchAccess::Gestures,
            },
            shell,
        }
    }
}

impl AppFactory for PomodoroFactory {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn create(&self) -> alloc::boxed::Box<dyn App + '_> {
        alloc::boxed::Box::new(Pomodoro::new(self.shell.clone()))
    }
}

/// A running pomodoro timer.
pub struct Pomodoro {
    /// Weak handle to the shared Slint tree — strong would be a reference
    /// cycle, and Slint component handles are not `Clone`.
    shell: slint::Weak<Shell>,
    phase: Phase,
    /// Seconds left. The single source of truth for the digits and the ring.
    remaining: u32,
    /// `remaining` when the current run started.
    remaining_at_start: u32,
    /// `now_ms` when the current run started; `None` unless running.
    started_ms: Option<u64>,
}

impl Pomodoro {
    /// A fresh timer, unset. Starting at zero matches the physical model: you
    /// dial the time in rather than start from a default someone chose.
    fn new(shell: slint::Weak<Shell>) -> Pomodoro {
        Pomodoro {
            shell,
            phase: Phase::Idle,
            remaining: 0,
            remaining_at_start: 0,
            started_ms: None,
        }
    }

    /// Recomputes `remaining` from the absolute start time. Returns whether the
    /// displayed second changed, so a running timer repaints once a second
    /// rather than on every 5 ms tick.
    fn recompute(&mut self, now_ms: u64) -> bool {
        let Some(started) = self.started_ms else {
            return false;
        };
        let elapsed = u32::try_from(now_ms.saturating_sub(started) / 1_000).unwrap_or(u32::MAX);
        let next = self.remaining_at_start.saturating_sub(elapsed);
        let changed = next != self.remaining;
        self.remaining = next;
        changed
    }

    /// Starts or resumes counting down from `remaining`.
    fn start(&mut self, now_ms: u64) {
        self.remaining_at_start = self.remaining;
        self.started_ms = Some(now_ms);
        self.phase = Phase::Running;
    }

    /// Stops counting, keeping the time on the clock.
    fn pause(&mut self, now_ms: u64) {
        self.recompute(now_ms);
        self.started_ms = None;
        self.phase = Phase::Paused;
    }

    /// Winds back to an unset timer.
    fn cancel(&mut self) {
        self.phase = Phase::Idle;
        self.remaining = 0;
        self.remaining_at_start = 0;
        self.started_ms = None;
    }
}

impl App for Pomodoro {
    fn handle(&mut self, event: InputEvent, ctx: &Ctx<'_>) -> Outcome {
        match event {
            InputEvent::Rotate(delta) => {
                // Rotation works in every phase, including while running: the
                // timer is a dial, and a dial you cannot turn mid-use is
                // surprising. Winding down to zero cancels, which is the only
                // way out of a running or paused timer besides finishing it.
                if self.phase == Phase::Running {
                    self.recompute(ctx.now_ms);
                }
                let step = i64::from(delta).saturating_mul(i64::from(SECS_PER_MIN));
                let next = i64::from(self.remaining)
                    .saturating_add(step)
                    .clamp(0, i64::from(RING_SECS));
                let remaining = u32::try_from(next).unwrap_or(0);
                if remaining == self.remaining && self.phase != Phase::Done {
                    return Outcome::NONE;
                }
                if remaining == 0 {
                    self.cancel();
                    return Outcome::CHANGED;
                }
                self.remaining = remaining;
                match self.phase {
                    // Rebase so the countdown continues from the new value.
                    Phase::Running => self.start(ctx.now_ms),
                    // Adjusting a finished timer starts setting a new one.
                    Phase::Done => self.phase = Phase::Idle,
                    Phase::Idle | Phase::Paused => {}
                }
                Outcome::CHANGED
            }
            InputEvent::Select => {
                match self.phase {
                    // Nothing dialled in yet, so a press would start a
                    // zero-length timer. Do nothing instead.
                    Phase::Idle if self.remaining == 0 => return Outcome::NONE,
                    Phase::Idle | Phase::Paused => self.start(ctx.now_ms),
                    Phase::Running => self.pause(ctx.now_ms),
                    Phase::Done => self.cancel(),
                }
                Outcome::buzz(Feedback::Beep)
            }
        }
    }

    fn tick(&mut self, ctx: &Ctx<'_>) -> Outcome {
        if self.phase != Phase::Running || !self.recompute(ctx.now_ms) {
            return Outcome::NONE;
        }
        if self.remaining == 0 {
            self.phase = Phase::Done;
            self.started_ms = None;
            // Haptic rather than a beep: a finished pomodoro should be felt
            // even if the device is face-down or the room is noisy.
            return Outcome::buzz(Feedback::Haptic);
        }
        Outcome::CHANGED
    }

    fn sync(&self) {
        // The ring shows remaining time against a fixed one-hour scale, so a
        // dot is always one minute. Dialling in 30 minutes lights half the ring
        // and counting down unlights it at the same rate.
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
