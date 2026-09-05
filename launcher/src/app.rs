//! The `App` trait and its surrounding types.
//!
//! An app is a self-contained screen: it consumes normalized [`InputEvent`]s,
//! reports a [`Dirty`] region so the host flushes minimally, and paints itself
//! onto the shared framebuffer. Apps are held as trait objects so that adding
//! one is a struct plus a registry entry — never an edit to an enum and every
//! match arm over it.

use embedded_graphics::pixelcolor::Rgb565;
use enc_state::AppState;
use enc_ui::Dirty;

/// Identifies an app's icon. Bound to a real sprite by the asset pipeline in
/// P2; until then it is carried through the launcher untouched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IconId(pub u16);

/// Static description of an app, used to draw its launcher card.
#[derive(Clone, Copy, Debug)]
pub struct Manifest {
    /// Display name, shown under the icon.
    pub name: &'static str,
    /// Icon to draw on the card.
    pub icon: IconId,
    /// Accent colour for the card and any in-app highlights.
    pub accent: Rgb565,
}

/// Per-frame context handed to every app.
pub struct Ctx<'a> {
    /// Monotonic milliseconds since boot.
    ///
    /// Supplied by the host rather than read from `embassy_time` so this crate
    /// stays hardware-free and testable with a plain counter.
    pub now_ms: u64,
    /// Lock-free shared state bridging apps, UI and networking.
    pub state: &'a AppState,
}

/// What an app wants the router to do after handling an event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Action {
    /// Stay in the app.
    #[default]
    None,
    /// Return to the launcher (equivalent to the user long-pressing).
    Exit,
}

/// Physical feedback an app can ask the firmware to produce. Apps cannot reach
/// the buzzer directly — it is a hardware resource the firmware owns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Feedback {
    /// Short audible beep.
    Beep,
    /// Low-frequency buzz, felt rather than heard.
    Haptic,
}

/// An app's response to an event: what changed, where to go next, and whether
/// to buzz.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Outcome {
    /// Region that needs repainting.
    pub dirty: Dirty,
    /// Requested navigation.
    pub action: Action,
    /// Optional buzzer/haptic request.
    pub feedback: Option<Feedback>,
}

impl Outcome {
    /// Nothing changed, stay put.
    pub const NONE: Outcome = Outcome {
        dirty: Dirty::None,
        action: Action::None,
        feedback: None,
    };

    /// Repaint `dirty`, stay in the app.
    #[must_use]
    pub const fn dirty(dirty: Dirty) -> Outcome {
        Outcome {
            dirty,
            action: Action::None,
            feedback: None,
        }
    }

    /// Repaint `dirty` and buzz.
    #[must_use]
    pub const fn buzz(dirty: Dirty, feedback: Feedback) -> Outcome {
        Outcome {
            dirty,
            action: Action::None,
            feedback: Some(feedback),
        }
    }

    /// Return to the launcher.
    #[must_use]
    pub const fn exit() -> Outcome {
        Outcome {
            dirty: Dirty::Full,
            action: Action::Exit,
            feedback: None,
        }
    }
}

/// A launchable app.
///
/// Object-safe on purpose: the registry holds `&mut dyn App`, so a new app
/// costs one struct and one registry line.
pub trait App {
    /// Static description used to draw this app's launcher card.
    fn manifest(&self) -> &Manifest;

    /// Called when the app becomes active, before its first render.
    fn on_enter(&mut self, ctx: &Ctx<'_>) {
        let _ = ctx;
    }

    /// Called when the app is left. Should drop transient state; anything that
    /// must survive belongs in shared state or flash.
    fn on_exit(&mut self) {}

    /// Handles one input event.
    fn handle(&mut self, event: enc_ui::InputEvent, ctx: &Ctx<'_>) -> Outcome;

    /// Periodic update — countdowns, adopting external state changes.
    fn tick(&mut self, ctx: &Ctx<'_>) -> Outcome {
        let _ = ctx;
        Outcome::NONE
    }

    /// Pushes current state into the shared Slint tree.
    ///
    /// Takes `&self`, never `&mut self`: publishing state must not mutate it.
    /// All changes belong in [`App::handle`] or [`App::tick`]. That keeps a
    /// later update/render split across CPU cores a refactor rather than a
    /// rewrite — see the concurrency section of the architecture plan.
    ///
    /// Deliberately argument-free: the app holds its own handle to its Slint
    /// component, so this trait — and therefore `launcher` — stays free of any
    /// dependency on the UI toolkit.
    fn sync(&self) {}
}
