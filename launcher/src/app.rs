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

/// Selects which component of the shell's view tree an app is drawn by.
///
/// The host publishes this verbatim when the app is on screen, so it never has
/// to know one app from another. `0` is the launcher itself; each app claims
/// its own id, matching an arm in the shell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewId(pub u16);

impl ViewId {
    /// The launcher's own view — the one id no app may claim.
    pub const LAUNCHER: ViewId = ViewId(0);
}

/// Static description of an app, used to draw its launcher card.
#[derive(Clone, Copy, Debug)]
pub struct Manifest {
    /// Display name, shown under the icon.
    pub name: &'static str,
    /// Icon to draw on the card.
    pub icon: IconId,
    /// Which shell component draws this app.
    pub view: ViewId,
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

/// A keystroke an app wants sent to whatever host is listening.
///
/// Modifiers and usage are USB HID values, which is what both BLE and USB
/// keyboards speak — but the app has no idea which transport carries it, or
/// whether one is even connected. The firmware owns that, exactly as it owns
/// the buzzer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyChord {
    /// HID modifier bitmap: ctrl 0x01, shift 0x02, alt 0x04, gui 0x08.
    pub modifiers: u8,
    /// HID keyboard usage id.
    pub usage: u8,
}

/// An app's response to an event: what changed, where to go next, and whether
/// to buzz or send a keystroke.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Outcome {
    /// Region that needs repainting.
    pub dirty: Dirty,
    /// Requested navigation.
    pub action: Action,
    /// Optional buzzer/haptic request.
    pub feedback: Option<Feedback>,
    /// Optional keystroke to send to a paired host.
    pub keys: Option<KeyChord>,
}

impl Outcome {
    /// Nothing changed, stay put.
    pub const NONE: Outcome = Outcome {
        dirty: Dirty::None,
        action: Action::None,
        feedback: None,
        keys: None,
    };

    /// Repaint `dirty`, stay in the app.
    #[must_use]
    pub const fn dirty(dirty: Dirty) -> Outcome {
        Outcome {
            dirty,
            action: Action::None,
            feedback: None,
            keys: None,
        }
    }

    /// Repaint `dirty` and buzz.
    #[must_use]
    pub const fn buzz(dirty: Dirty, feedback: Feedback) -> Outcome {
        Outcome {
            dirty,
            action: Action::None,
            feedback: Some(feedback),
            keys: None,
        }
    }

    /// Send `keys` to the paired host, buzz to confirm, and repaint `dirty`.
    #[must_use]
    pub const fn send_keys(dirty: Dirty, keys: KeyChord, feedback: Feedback) -> Outcome {
        Outcome {
            dirty,
            action: Action::None,
            feedback: Some(feedback),
            keys: Some(keys),
        }
    }

    /// Return to the launcher.
    #[must_use]
    pub const fn exit() -> Outcome {
        Outcome {
            dirty: Dirty::Full,
            action: Action::Exit,
            feedback: None,
            keys: None,
        }
    }
}

/// Creates app instances. The registry holds factories, not apps.
///
/// An app exists only while it is on screen: launching constructs it, leaving
/// drops it. That is what makes quitting and reopening a genuine reset, and it
/// means an unopened app costs nothing but its manifest. The cost is that an
/// app cannot run in the background — a timer left behind is gone. Background
/// work will need its own shape (a timer service, or an app-supplied runner),
/// not simply keeping every app alive forever.
pub trait AppFactory {
    /// Static description used to draw this app's launcher card. Lives on the
    /// factory because the launcher must describe apps that are not running.
    fn manifest(&self) -> &Manifest;

    /// Builds a fresh instance, with no state carried over from last time.
    ///
    /// The instance borrows from the factory (`+ '_`), so a factory can lend
    /// its app a handle — a Slint component, a shared bus — without that data
    /// having to be `'static`. An app never outlives its factory, and factories
    /// live in the registry for the life of the program.
    fn create(&self) -> alloc::boxed::Box<dyn App + '_>;
}

/// A running app.
///
/// Object-safe on purpose: the router owns a `Box<dyn App>`, so adding an app
/// costs one struct, one factory and one registry line.
pub trait App {
    /// Called before the instance is dropped. Anything that must survive
    /// belongs in shared state or flash — the instance itself does not.
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
