//! The router: owns which app is active and how the user moves between them.
//!
//! Raw hardware input arrives as [`Input`]. The router consumes navigation
//! itself (long-press to go home, short-press to launch) and forwards the rest
//! to the active app as an [`InputEvent`], so an app never has to know about
//! press durations or the launcher.

use enc_ui::{Dirty, InputEvent};

use alloc::boxed::Box;

use crate::app::{Action, App, AppFactory, Ctx, Feedback, ViewId};
use crate::carousel::Carousel;

/// Raw, denormalized input from the hardware loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    /// Encoder detents, signed.
    Rotate(i32),
    /// Button released before the long-press threshold.
    ShortPress,
    /// Button held past the long-press threshold.
    LongPress,
    /// Touch down at panel coordinates.
    Touch { x: i32, y: i32 },
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

    /// Handles one raw input, returning the region to repaint.
    pub fn handle(&mut self, input: Input, ctx: &Ctx<'_>) -> Dirty {
        match self.view {
            View::Launcher => self.handle_launcher(input, ctx),
            View::App(_) => self.handle_app(input, ctx),
        }
    }

    /// Ticks the running app, if any.
    ///
    /// Only the running app ticks. There is no background execution: leaving an
    /// app drops it, so a timer left behind is gone. Background work needs its
    /// own design rather than keeping every app alive forever.
    pub fn tick(&mut self, ctx: &Ctx<'_>) -> Dirty {
        // Slint drives its own animation clock and reports what changed, so the
        // launcher has no tick-driven dirty region of its own.
        let Some(app) = self.active.as_mut() else {
            return Dirty::None;
        };
        let outcome = app.tick(ctx);
        self.feedback = self.feedback.or(outcome.feedback);
        match outcome.action {
            Action::None => outcome.dirty,
            Action::Exit => self.go_home(),
        }
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

    fn handle_launcher(&mut self, input: Input, ctx: &Ctx<'_>) -> Dirty {
        match input {
            Input::Rotate(delta) => {
                let next = self.carousel.step(self.selected, delta);
                if next == self.selected {
                    return Dirty::None;
                }
                self.selected = next;
                // Slint owns the slide: setting `selected` on the shell drives
                // an `animate x`, so there is no scroll state to keep here.
                Dirty::Full
            }
            Input::ShortPress => self.launch(self.selected, ctx),
            // Already home; nothing to go back to.
            Input::LongPress => Dirty::None,
            Input::Touch { x, y } => {
                // Hit-test at the settled position: Slint owns the in-flight
                // offset. Superseded once touch becomes global gestures.
                let scroll = self.carousel.scroll_for(self.selected);
                match self.carousel.hit_test(x, y, scroll) {
                    Some(index) => {
                        self.selected = index;
                        self.launch(index, ctx)
                    }
                    None => Dirty::None,
                }
            }
        }
    }

    fn handle_app(&mut self, input: Input, ctx: &Ctx<'_>) -> Dirty {
        // Long-press is navigation and never reaches the app.
        if input == Input::LongPress {
            return self.go_home();
        }
        let event = match input {
            Input::Rotate(delta) => InputEvent::Rotate(delta),
            Input::ShortPress => InputEvent::Select,
            Input::Touch { x, y } => InputEvent::Touch { x, y },
            Input::LongPress => return self.go_home(),
        };
        let Some(app) = self.active.as_mut() else {
            return self.go_home();
        };
        let outcome = app.handle(event, ctx);
        self.feedback = self.feedback.or(outcome.feedback);
        match outcome.action {
            Action::None => outcome.dirty,
            Action::Exit => self.go_home(),
        }
    }

    /// Constructs a fresh instance of app `index` and shows it.
    fn launch(&mut self, index: usize, ctx: &Ctx<'_>) -> Dirty {
        let _ = ctx;
        let Some(factory) = self.factories.get(index) else {
            return Dirty::None;
        };
        self.active = Some(factory.create());
        self.view = View::App(index);
        Dirty::Full
    }

    /// Drops the running app and returns to the launcher.
    fn go_home(&mut self) -> Dirty {
        if let Some(mut app) = self.active.take() {
            app.on_exit();
            // `app` is dropped here: all of its state goes with it, which is
            // what makes reopening a real reset.
        }
        self.view = View::Launcher;
        Dirty::Full
    }
}
