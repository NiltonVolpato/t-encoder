//! The router: owns which app is active and how the user moves between them.
//!
//! Raw hardware input arrives as [`Input`]. The router consumes navigation
//! itself (long-press to go home, short-press to launch) and forwards the rest
//! to the active app as an [`InputEvent`], so an app never has to know about
//! press durations or the launcher.

use enc_ui::{Dirty, InputEvent};

use crate::app::{Action, App, Ctx, Feedback};
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
    apps: &'a mut [&'a mut dyn App],
    carousel: Carousel,
    view: View,
    selected: usize,
    feedback: Option<Feedback>,
}

impl<'a> Router<'a> {
    /// Builds a router over `apps`, laid out by `carousel`.
    ///
    /// `carousel.count` is taken from the registry so the two cannot disagree.
    #[must_use]
    pub fn new(apps: &'a mut [&'a mut dyn App], mut carousel: Carousel) -> Router<'a> {
        carousel.count = apps.len();
        Router {
            apps,
            carousel,
            view: View::Launcher,
            selected: 0,
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

    /// The app registry, for the renderer to read manifests from.
    #[must_use]
    pub fn apps(&self) -> &[&'a mut dyn App] {
        self.apps
    }

    /// Handles one raw input, returning the region to repaint.
    pub fn handle(&mut self, input: Input, ctx: &Ctx<'_>) -> Dirty {
        match self.view {
            View::Launcher => self.handle_launcher(input, ctx),
            View::App(index) => self.handle_app(index, input, ctx),
        }
    }

    /// Advances animations and the active app's own clock.
    pub fn tick(&mut self, ctx: &Ctx<'_>) -> Dirty {
        match self.view {
            // Slint drives its own animation clock and reports what changed,
            // so the launcher has no tick-driven dirty region of its own.
            View::Launcher => Dirty::None,
            View::App(index) => {
                let Some(app) = self.apps.get_mut(index) else {
                    return Dirty::None;
                };
                let outcome = app.tick(ctx);
                self.feedback = self.feedback.or(outcome.feedback);
                match outcome.action {
                    Action::None => outcome.dirty,
                    Action::Exit => self.go_home(),
                }
            }
        }
    }

    /// Pushes the active app's state into the shared Slint tree.
    pub fn sync_app(&self) {
        if let View::App(index) = self.view
            && let Some(app) = self.apps.get(index)
        {
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

    fn handle_app(&mut self, index: usize, input: Input, ctx: &Ctx<'_>) -> Dirty {
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
        let Some(app) = self.apps.get_mut(index) else {
            return self.go_home();
        };
        let outcome = app.handle(event, ctx);
        self.feedback = self.feedback.or(outcome.feedback);
        match outcome.action {
            Action::None => outcome.dirty,
            Action::Exit => self.go_home(),
        }
    }

    fn launch(&mut self, index: usize, ctx: &Ctx<'_>) -> Dirty {
        let Some(app) = self.apps.get_mut(index) else {
            return Dirty::None;
        };
        app.on_enter(ctx);
        self.view = View::App(index);
        Dirty::Full
    }

    fn go_home(&mut self) -> Dirty {
        if let View::App(index) = self.view
            && let Some(app) = self.apps.get_mut(index)
        {
            app.on_exit();
        }
        self.view = View::Launcher;
        Dirty::Full
    }
}
