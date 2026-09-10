//! The `App` trait and its surrounding types.
//!
//! An app is a self-contained screen: it consumes normalized input, owns its
//! state, and publishes that state into the shared Slint tree. It never paints
//! — Slint renders from the properties [`App::sync`] sets, and computes its own
//! repaint region. Apps are held as trait objects so that adding one is a
//! struct plus a registry entry — never an edit to an enum and every match arm
//! over it.

use crate::gesture::TouchSample;
use embedded_graphics::pixelcolor::Rgb565;

/// Normalized input, after the router has taken navigation out of the stream.
///
/// Owned here rather than reused from `enc_ui`, whose version also carries a
/// `Touch` variant that cannot occur in this design: the router turns touch
/// into gestures, and an app that wants the panel gets samples through
/// [`App::touch`] instead. Keeping the impossible variant meant every app
/// writing a match arm for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputEvent {
    /// Encoder detents, signed; positive is clockwise.
    Rotate(i32),
    /// The encoder button was pressed and released.
    Select,
    /// A tap was completed at the given panel coordinates.
    Tap {
        /// X coordinate on the panel.
        x: i32,
        /// Y coordinate on the panel.
        y: i32,
    },
}

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

/// Who owns the touch panel while an app is on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TouchAccess {
    /// The router owns it: touch becomes navigation and the app never sees a
    /// sample. What every encoder-driven app wants, and the default.
    #[default]
    Gestures,
    /// The router handles navigation swipes (`SwipeUp` / `SwipeLeft` exit),
    /// but forwards completed [`crate::gesture::Gesture::Tap`] events to the app
    /// as [`InputEvent::Tap`].
    Taps,
    /// The app owns it: every sample goes to [`App::touch`] and no gesture is
    /// recognised, so a canvas can draw a stroke right across the screen
    /// without quitting itself. The encoder long-press is then the only way
    /// out — which is exactly why it is kept.
    Raw,
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
    /// Whether this app wants the raw panel instead of the router's gestures.
    pub touch: TouchAccess,
    /// Whether this app requires Bluetooth (e.g. Macropad HID keyboard).
    pub requires_ble: bool,
}

/// Per-frame context handed to every app.
#[derive(Clone, Copy, Debug)]
pub struct Ctx {
    /// Monotonic milliseconds since boot.
    ///
    /// Supplied by the host rather than read from `embassy_time` so this crate
    /// stays hardware-free and testable with a plain counter.
    pub now_ms: u64,
    /// Whether BLE keyboard link is active.
    pub ble_linked: bool,
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
    /// Arbitrary tone at specified frequency in Hertz for `ms` milliseconds.
    Tone {
        /// Tone frequency in Hertz.
        hz: u32,
        /// Duration of the tone in milliseconds.
        ms: u32,
    },
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

/// An app's response to an event: whether anything changed, where to go next,
/// and whether to buzz or send a keystroke.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Outcome {
    /// Whether any state changed, so the host republishes to Slint.
    ///
    /// Deliberately a flag and not a region: Slint tracks its own dirty
    /// rectangle and hands one back from `render`, so an app describing *where*
    /// it changed would be describing something nobody reads. All this decides
    /// is whether [`App::sync`] is worth calling — republishing unconditionally
    /// would dirty Slint every tick and repaint at full loop speed.
    pub changed: bool,
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
        changed: false,
        action: Action::None,
        feedback: None,
        keys: None,
    };

    /// State changed, stay in the app.
    pub const CHANGED: Outcome = Outcome {
        changed: true,
        action: Action::None,
        feedback: None,
        keys: None,
    };

    /// State changed; buzz as well.
    #[must_use]
    pub const fn buzz(feedback: Feedback) -> Outcome {
        Outcome {
            changed: true,
            action: Action::None,
            feedback: Some(feedback),
            keys: None,
        }
    }

    /// State changed; play a tone at `hz` for `ms` milliseconds.
    #[must_use]
    pub const fn tone(hz: u32, ms: u32) -> Outcome {
        Outcome {
            changed: true,
            action: Action::None,
            feedback: Some(Feedback::Tone { hz, ms }),
            keys: None,
        }
    }

    /// Send `keys` to the paired host and buzz to confirm.
    #[must_use]
    pub const fn send_keys(keys: KeyChord, feedback: Feedback) -> Outcome {
        Outcome {
            changed: true,
            action: Action::None,
            feedback: Some(feedback),
            keys: Some(keys),
        }
    }

    /// Return to the launcher.
    #[must_use]
    pub const fn exit() -> Outcome {
        Outcome {
            changed: true,
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
    fn handle(&mut self, event: InputEvent, ctx: &Ctx) -> Outcome;

    /// Handles one raw touch sample.
    ///
    /// Only ever called for an app whose [`Manifest::touch`] is
    /// [`TouchAccess::Raw`]. By default the panel belongs to the router, which
    /// turns strokes into navigation, so this stays a no-op.
    fn touch(&mut self, sample: TouchSample, ctx: &Ctx) -> Outcome {
        let _ = (sample, ctx);
        Outcome::NONE
    }

    /// Periodic update — countdowns, adopting external state changes.
    fn tick(&mut self, ctx: &Ctx) -> Outcome {
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
