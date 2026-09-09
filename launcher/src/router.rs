//! The router: owns which app is active and how the user moves between them.
//!
//! Raw hardware input arrives as [`Input`]. The router consumes navigation
//! itself (long-press to go home, short-press to launch, swipes to leave) and
//! forwards the rest to the active app as an [`InputEvent`], so an app never
//! has to know about press durations or the launcher.
//!
//! Touch is the router's throughout: samples feed a [`Recognizer`] and only
//! the resulting [`Gesture`] means anything. An app receives raw samples only
//! if its manifest asks for them.

use alloc::boxed::Box;

use crate::app::{
    Action, App, AppFactory, Ctx, Feedback, InputEvent, KeyChord, Outcome, TouchAccess, ViewId,
};
use crate::carousel::Carousel;
use crate::gesture::{Gesture, Recognizer, TouchSample};

/// Raw, denormalized input from the hardware loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    /// Encoder detents, signed.
    Rotate(i32),
    /// Button released before the long-press threshold.
    ShortPress,
    /// Button held past the long-press threshold.
    LongPress,
    /// One reading from the touch panel.
    Touch(TouchSample),
}

/// What the user is currently looking at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    /// The app carousel.
    Launcher,
    /// A running app, by registry index.
    App(usize),
}

/// Owns the app registry and the current view.
pub struct Router<'a> {
    factories: &'a [&'a dyn AppFactory],
    carousel: Carousel,
    view: View,
    selected: usize,
    /// The running app, or `None` on the launcher. Dropped on exit, so state
    /// never survives leaving.
    active: Option<Box<dyn App + 'a>>,
    feedback: Option<Feedback>,
    keys: Option<KeyChord>,
    gestures: Recognizer,
}

impl<'a> Router<'a> {
    /// Builds a router over `factories`, laid out by `carousel`.
    ///
    /// `carousel.count` is taken from the registry so the two cannot disagree.
    #[must_use]
    pub fn new(factories: &'a [&'a dyn AppFactory], mut carousel: Carousel) -> Router<'a> {
        carousel.count = factories.len();
        Router {
            factories,
            carousel,
            view: View::Launcher,
            selected: 0,
            active: None,
            feedback: None,
            keys: None,
            gestures: Recognizer::new(),
        }
    }

    /// The current view.
    #[must_use]
    pub fn view(&self) -> View {
        self.view
    }

    /// The highlighted launcher card.
    #[must_use]
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Carousel geometry, for the renderer.
    #[must_use]
    pub fn carousel(&self) -> &Carousel {
        &self.carousel
    }

    /// The app registry, for the launcher to read manifests from.
    #[must_use]
    pub fn factories(&self) -> &[&'a dyn AppFactory] {
        self.factories
    }

    /// Which shell component should be on screen right now.
    ///
    /// This is what keeps the host free of per-app knowledge: it publishes the
    /// id and never learns which app it belongs to.
    #[must_use]
    pub fn view_id(&self) -> ViewId {
        match self.view {
            View::Launcher => ViewId::LAUNCHER,
            View::App(index) => self
                .factories
                .get(index)
                .map_or(ViewId::LAUNCHER, |factory| factory.manifest().view),
        }
    }

    /// Handles one raw input, returning whether anything changed.
    pub fn handle(&mut self, input: Input, ctx: &Ctx) -> bool {
        match input {
            // Touch is the system's: it becomes navigation, unless the app on
            // screen asked for the raw panel.
            Input::Touch(sample) => self.handle_touch(sample, ctx),
            // Long press is navigation everywhere, and never reaches an app.
            // From the launcher there is nowhere to go back to.
            Input::LongPress => match self.view {
                View::Launcher => false,
                View::App(_) => self.go_home(),
            },
            Input::Rotate(delta) => match self.view {
                View::Launcher => self.move_selection(delta),
                View::App(_) => self.deliver(InputEvent::Rotate(delta), ctx),
            },
            Input::ShortPress => match self.view {
                View::Launcher => self.launch(self.selected, ctx),
                View::App(_) => self.deliver(InputEvent::Select, ctx),
            },
        }
    }

    /// Tells the router whether the encoder button is physically down.
    ///
    /// Not an [`Input`] on purpose: [`Input::ShortPress`] fires on *release*,
    /// up to the long-press threshold after the contact closed, whereas the
    /// phantom touch a press generates has to be matched against the contact
    /// itself. The firmware calls this every tick; unchanged states are free.
    pub fn set_button(&mut self, down: bool, now_ms: u64) {
        self.gestures.set_button(down, now_ms);
    }

    /// Ticks the running app, if any.
    ///
    /// Only the running app ticks. There is no background execution: leaving an
    /// app drops it, so a timer left behind is gone. Background work needs its
    /// own design rather than keeping every app alive forever.
    pub fn tick(&mut self, ctx: &Ctx) -> bool {
        // Slint drives its own animation clock, so the launcher has nothing of
        // its own to advance on a tick.
        let Some(app) = self.active.as_mut() else {
            return false;
        };
        let outcome = app.tick(ctx);
        self.apply(outcome)
    }

    /// Pushes the running app's state into the shared Slint tree.
    pub fn sync_app(&self) {
        if let Some(app) = self.active.as_ref() {
            app.sync();
        }
    }

    /// Takes any pending buzzer/haptic request. Apps cannot reach the buzzer
    /// themselves; the firmware polls this after driving the router.
    pub fn take_feedback(&mut self) -> Option<Feedback> {
        self.feedback.take()
    }

    /// Takes any pending keystroke. Apps cannot reach the radio themselves;
    /// the firmware polls this after driving the router, exactly as it does
    /// for [`Self::take_feedback`].
    pub fn take_keys(&mut self) -> Option<KeyChord> {
        self.keys.take()
    }

    /// Moves the launcher's highlight by `delta` cards.
    fn move_selection(&mut self, delta: i32) -> bool {
        let next = self.carousel.step(self.selected, delta);
        if next == self.selected {
            return false;
        }
        self.selected = next;
        // Slint owns the slide: setting `selected` on the shell drives an
        // `animate x`, so there is no scroll state to keep here.
        true
    }

    /// Forwards a normalized event to the running app.
    fn deliver(&mut self, event: InputEvent, ctx: &Ctx) -> bool {
        let Some(app) = self.active.as_mut() else {
            return self.go_home();
        };
        let outcome = app.handle(event, ctx);
        self.apply(outcome)
    }

    /// Routes one touch sample: to the app if it owns the panel, otherwise
    /// through the recogniser and on to navigation.
    fn handle_touch(&mut self, sample: TouchSample, ctx: &Ctx) -> bool {
        if self.app_owns_touch() {
            let Some(app) = self.active.as_mut() else {
                return self.go_home();
            };
            let outcome = app.touch(sample, ctx);
            return self.apply(outcome);
        }
        let Some(gesture) = self.gestures.push(sample) else {
            return false;
        };
        self.handle_gesture(gesture, ctx)
    }

    /// Routes a completed gesture to navigation.
    pub fn handle_gesture(&mut self, gesture: Gesture, ctx: &Ctx) -> bool {
        match self.view {
            View::Launcher => self.handle_launcher_gesture(gesture, ctx),
            // Swipe up is the primary way out of an app; swipe left is "back",
            // which with one level of navigation is the same place. The rest is
            // swallowed — apps do not see touch.
            View::App(_) => match gesture {
                Gesture::SwipeUp | Gesture::SwipeLeft => self.go_home(),
                Gesture::Tap { x, y } if self.app_wants_taps() => {
                    self.deliver(InputEvent::Tap { x, y }, ctx)
                }
                Gesture::SwipeDown | Gesture::SwipeRight | Gesture::Tap { .. } => false,
            },
        }
    }

    /// The launcher's own view of touch: a tap picks a card.
    fn handle_launcher_gesture(&mut self, gesture: Gesture, ctx: &Ctx) -> bool {
        // Already home, so a swipe has nowhere to go.
        let Gesture::Tap { x, y } = gesture else {
            return false;
        };
        // Hit-test at the settled position: Slint owns the in-flight offset.
        let scroll = self.carousel.scroll_for(self.selected);
        match self.carousel.hit_test(x, y, scroll) {
            // Tapping the focal card opens it; tapping a neighbour peeking in
            // at the rim brings it to the centre instead, which is what a
            // carousel normally does. Opening a half-visible card straight from
            // the rim was the odd part: you could launch a card you could not
            // scroll to by dragging.
            Some(index) if index == self.selected => self.launch(index, ctx),
            Some(index) => {
                self.selected = index;
                true
            }
            None => false,
        }
    }

    /// Whether the app on screen asked for tap gestures.
    fn app_wants_taps(&self) -> bool {
        let View::App(index) = self.view else {
            return false;
        };
        self.factories
            .get(index)
            .is_some_and(|factory| factory.manifest().touch == TouchAccess::Taps)
    }

    /// Whether the app on screen asked for the raw panel.
    fn app_owns_touch(&self) -> bool {
        let View::App(index) = self.view else {
            return false;
        };
        self.factories
            .get(index)
            .is_some_and(|factory| factory.manifest().touch == TouchAccess::Raw)
    }

    /// Banks an app's feedback and keystroke requests and resolves its action.
    fn apply(&mut self, outcome: Outcome) -> bool {
        self.feedback = self.feedback.or(outcome.feedback);
        self.keys = self.keys.or(outcome.keys);
        match outcome.action {
            Action::None => outcome.changed,
            Action::Exit => self.go_home(),
        }
    }

    /// Constructs a fresh instance of app `index` and shows it.
    fn launch(&mut self, index: usize, ctx: &Ctx) -> bool {
        let _ = ctx;
        let Some(factory) = self.factories.get(index) else {
            return false;
        };
        self.active = Some(factory.create());
        self.view = View::App(index);
        true
    }

    /// Drops the running app and returns to the launcher.
    fn go_home(&mut self) -> bool {
        if let Some(mut app) = self.active.take() {
            app.on_exit();
            // `app` is dropped here: all of its state goes with it, which is
            // what makes reopening a real reset.
        }
        self.view = View::Launcher;
        true
    }
}
