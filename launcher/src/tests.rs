//! Host tests for the pure launcher core: navigation, the app lifecycle and
//! carousel geometry. Touch has its own file — see [`gesture`].

mod gesture;

use embedded_graphics::pixelcolor::Rgb565;
use enc_state::AppState;

use alloc::boxed::Box;
use core::cell::Cell;

use crate::{
    Action, App, AppFactory, Ctx, Feedback, IconId, Input, InputEvent, KeyChord, Manifest, Outcome,
    Router, TouchAccess, TouchPhase, TouchSample, View, ViewId, default_carousel, geometry,
};

/// A sample at `(x, y)`, `at_ms` after boot.
fn sample(phase: TouchPhase, x: i32, y: i32, at_ms: u64) -> TouchSample {
    TouchSample { phase, x, y, at_ms }
}

/// A tap landing at `at_ms`: down and up in the same place.
fn tap_at(x: i32, y: i32, at_ms: u64) -> [TouchSample; 2] {
    [
        sample(TouchPhase::Down, x, y, at_ms),
        sample(TouchPhase::Up, x, y, at_ms.saturating_add(40)),
    ]
}

/// A tap at boot, for the tests that do not care when it happened.
fn tap(x: i32, y: i32) -> [TouchSample; 2] {
    tap_at(x, y, 0)
}

/// A stroke from `(x0, y0)` to `(x1, y1)`, with one midpoint move.
fn swipe(x0: i32, y0: i32, x1: i32, y1: i32) -> [TouchSample; 3] {
    [
        sample(TouchPhase::Down, x0, y0, 0),
        sample(
            TouchPhase::Move,
            i32::midpoint(x0, x1),
            i32::midpoint(y0, y1),
            40,
        ),
        sample(TouchPhase::Up, x1, y1, 80),
    ]
}

/// Feeds a whole stroke to the router, reporting whether any of it changed
/// anything.
fn feed(router: &mut Router<'_>, ctx: &Ctx<'_>, stroke: &[TouchSample]) -> bool {
    stroke.iter().fold(false, |changed, &s| {
        changed | router.handle(Input::Touch(s), ctx)
    })
}

/// Shared counters, so a test can observe an app that the router created and
/// dropped without ever holding a reference to it.
#[derive(Default)]
struct Log {
    created: Cell<u32>,
    exited: Cell<u32>,
    dropped: Cell<u32>,
    events: Cell<u32>,
    touches: Cell<u32>,
}

/// A stub app whose whole life is recorded in a shared [`Log`].
struct StubApp<'a> {
    log: &'a Log,
    action: Action,
    feedback: Option<Feedback>,
    keys: Option<KeyChord>,
}

impl App for StubApp<'_> {
    fn on_exit(&mut self) {
        self.log.exited.set(self.log.exited.get().saturating_add(1));
    }

    fn handle(&mut self, _event: InputEvent, _ctx: &Ctx<'_>) -> Outcome {
        self.log.events.set(self.log.events.get().saturating_add(1));
        match self.action {
            Action::None => Outcome {
                changed: true,
                action: Action::None,
                feedback: self.feedback,
                keys: self.keys,
            },
            Action::Exit => Outcome::exit(),
        }
    }

    fn touch(&mut self, _sample: TouchSample, _ctx: &Ctx<'_>) -> Outcome {
        self.log
            .touches
            .set(self.log.touches.get().saturating_add(1));
        Outcome::CHANGED
    }

    fn sync(&self) {}
}

impl Drop for StubApp<'_> {
    fn drop(&mut self) {
        self.log
            .dropped
            .set(self.log.dropped.get().saturating_add(1));
    }
}

/// Builds [`StubApp`]s and counts how many it has made.
struct StubFactory<'a> {
    manifest: Manifest,
    log: &'a Log,
    action: Action,
    feedback: Option<Feedback>,
    keys: Option<KeyChord>,
}

impl<'a> StubFactory<'a> {
    fn new(name: &'static str, log: &'a Log) -> StubFactory<'a> {
        StubFactory {
            manifest: Manifest {
                name,
                icon: IconId(0),
                view: ViewId(1),
                accent: Rgb565::new(31, 0, 0),
                touch: TouchAccess::Gestures,
            },
            log,
            action: Action::None,
            feedback: None,
            keys: None,
        }
    }

    /// Gives this stub its own shell view, so a test can tell which app the
    /// router is asking the host to draw.
    fn with_view(mut self, view: ViewId) -> StubFactory<'a> {
        self.manifest.view = view;
        self
    }

    /// Makes this stub one of the apps that owns the panel outright.
    fn with_raw_touch(mut self) -> StubFactory<'a> {
        self.manifest.touch = TouchAccess::Raw;
        self
    }
}

impl AppFactory for StubFactory<'_> {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn create(&self) -> Box<dyn App + '_> {
        self.log
            .created
            .set(self.log.created.get().saturating_add(1));
        Box::new(StubApp {
            log: self.log,
            action: self.action,
            feedback: self.feedback,
            keys: self.keys,
        })
    }
}

/// Runs `body` with a two-app router.
fn with_router(body: impl FnOnce(&mut Router<'_>, &Ctx<'_>)) {
    let state = AppState::new(4);
    let ctx = Ctx {
        now_ms: 0,
        state: &state,
    };
    let log = Log::default();
    let first = StubFactory::new("First", &log).with_view(ViewId(1));
    let second = StubFactory::new("Second", &log).with_view(ViewId(2));
    let registry: [&dyn AppFactory; 2] = [&first, &second];
    let mut router = Router::new(&registry, default_carousel(0));
    body(&mut router, &ctx);
}

#[test]
fn centre_is_panel_midpoint() {
    assert_eq!(geometry::centre(), (195, 195));
}

#[test]
fn router_starts_on_the_launcher() {
    with_router(|router, _ctx| {
        assert_eq!(router.view(), View::Launcher);
        assert_eq!(router.selected(), 0);
    });
}

#[test]
fn router_takes_card_count_from_the_registry() {
    // default_carousel(0) is deliberately wrong; the router must correct it.
    with_router(|router, _ctx| {
        assert_eq!(router.carousel().count, 2);
    });
}

#[test]
fn selection_clamps_at_both_ends() {
    with_router(|router, ctx| {
        router.handle(Input::Rotate(-5), ctx);
        assert_eq!(router.selected(), 0, "cannot scroll before the first card");
        router.handle(Input::Rotate(99), ctx);
        assert_eq!(router.selected(), 1, "cannot scroll past the last card");
    });
}

#[test]
fn rotate_onto_the_same_card_changes_nothing() {
    with_router(|router, ctx| {
        assert!(!(router.handle(Input::Rotate(-1), ctx)));
    });
}

#[test]
fn short_press_launches_and_long_press_returns_home() {
    with_router(|router, ctx| {
        assert!(router.handle(Input::ShortPress, ctx));
        assert_eq!(router.view(), View::App(0));

        assert!(router.handle(Input::LongPress, ctx));
        assert_eq!(router.view(), View::Launcher);
    });
}

#[test]
fn the_view_id_follows_the_active_app() {
    with_router(|router, ctx| {
        assert_eq!(router.view_id(), ViewId::LAUNCHER);
        router.handle(Input::Rotate(1), ctx);
        router.handle(Input::ShortPress, ctx);
        // The second app's own id, not its registry index — the host publishes
        // this blind, so the two must not be conflated.
        assert_eq!(router.view_id(), ViewId(2));
        router.handle(Input::LongPress, ctx);
        assert_eq!(router.view_id(), ViewId::LAUNCHER);
    });
}

#[test]
fn long_press_on_the_launcher_does_nothing() {
    with_router(|router, ctx| {
        assert!(!(router.handle(Input::LongPress, ctx)));
        assert_eq!(router.view(), View::Launcher);
    });
}

#[test]
fn carousel_hit_test_is_the_inverse_of_layout() {
    let carousel = default_carousel(4);
    for index in 0..4 {
        let scroll = carousel.scroll_for(index);
        let cx = carousel.card_centre_x(index, scroll);
        assert_eq!(cx, carousel.centre_x, "the selected card must be centred");
        assert_eq!(
            carousel.hit_test(cx, carousel.centre_y, scroll),
            Some(index)
        );
    }
}

#[test]
fn only_nearby_cards_are_visible() {
    let carousel = default_carousel(20);
    let scroll = carousel.scroll_for(10);
    assert!(carousel.is_visible(10, scroll, 390));
    assert!(carousel.is_visible(9, scroll, 390));
    assert!(carousel.is_visible(11, scroll, 390));
    assert!(!carousel.is_visible(0, scroll, 390));
    assert!(!carousel.is_visible(19, scroll, 390));
}

#[test]
fn carousel_handles_an_empty_registry() {
    let carousel = default_carousel(0);
    assert_eq!(carousel.step(0, 3), 0);
    assert_eq!(carousel.hit_test(195, 195, 0), None);
}

/// Slint owns the slide, and its own dirty tracking decides what gets flushed;
/// all the router says is that something moved.
#[test]
fn rotate_moves_selection_and_delegates_painting_to_slint() {
    with_router(|router, ctx| {
        let dirty = router.handle(Input::Rotate(1), ctx);
        assert_eq!(router.selected(), 1);
        assert!(dirty);
    });
}

/// The launcher has no tick-driven animation of its own any more — Slint runs
/// its animation clock independently.
#[test]
fn launcher_tick_is_never_dirty() {
    with_router(|router, ctx| {
        router.handle(Input::Rotate(1), ctx);
        assert!(!router.tick(ctx));
    });
}

/// Apps cannot touch the buzzer directly, so the router collects their
/// requests for the firmware to act on.

#[test]
fn long_press_never_reaches_the_app() {
    let state = AppState::new(1);
    let ctx = Ctx {
        now_ms: 0,
        state: &state,
    };
    let log = Log::default();
    let factory = StubFactory::new("First", &log);
    let registry: [&dyn AppFactory; 1] = [&factory];
    let mut router = Router::new(&registry, default_carousel(0));

    router.handle(Input::ShortPress, &ctx); // launch
    router.handle(Input::LongPress, &ctx); // back home

    assert_eq!(log.events.get(), 0, "navigation is consumed by the router");
    assert_eq!(log.created.get(), 1);
    assert_eq!(log.exited.get(), 1);
}

#[test]
fn an_app_can_exit_itself() {
    let state = AppState::new(1);
    let ctx = Ctx {
        now_ms: 0,
        state: &state,
    };
    let log = Log::default();
    let mut factory = StubFactory::new("First", &log);
    factory.action = Action::Exit;
    let registry: [&dyn AppFactory; 1] = [&factory];
    let mut router = Router::new(&registry, default_carousel(0));

    router.handle(Input::ShortPress, &ctx);
    assert_eq!(router.view(), View::App(0));
    router.handle(Input::ShortPress, &ctx);
    assert_eq!(router.view(), View::Launcher);
    assert_eq!(log.exited.get(), 1, "self-exit still runs on_exit");
}

#[test]
fn feedback_requests_reach_the_router() {
    let state = AppState::new(1);
    let ctx = Ctx {
        now_ms: 0,
        state: &state,
    };
    let log = Log::default();
    let mut factory = StubFactory::new("First", &log);
    factory.feedback = Some(Feedback::Haptic);
    let registry: [&dyn AppFactory; 1] = [&factory];
    let mut router = Router::new(&registry, default_carousel(0));

    router.handle(Input::ShortPress, &ctx); // launch; the app sees no event
    assert_eq!(router.take_feedback(), None);

    router.handle(Input::ShortPress, &ctx); // now a Select reaches it
    assert_eq!(router.take_feedback(), Some(Feedback::Haptic));
    assert_eq!(router.take_feedback(), None, "taking clears it");
}

/// The point of the factory model: leaving an app destroys it, so reopening is
/// a genuine reset rather than resuming whatever it was doing.
#[test]
fn leaving_an_app_drops_it_and_reopening_builds_a_fresh_one() {
    let state = AppState::new(1);
    let ctx = Ctx {
        now_ms: 0,
        state: &state,
    };
    let log = Log::default();
    let factory = StubFactory::new("First", &log);
    let registry: [&dyn AppFactory; 1] = [&factory];
    let mut router = Router::new(&registry, default_carousel(0));

    router.handle(Input::ShortPress, &ctx);
    assert_eq!(log.created.get(), 1);
    assert_eq!(log.dropped.get(), 0, "still running");

    router.handle(Input::LongPress, &ctx);
    assert_eq!(log.dropped.get(), 1, "leaving must drop the instance");

    router.handle(Input::ShortPress, &ctx);
    assert_eq!(log.created.get(), 2, "reopening builds a new instance");
}

/// Nothing runs off screen: an app that is not on screen does not exist.
#[test]
fn nothing_ticks_while_on_the_launcher() {
    with_router(|router, ctx| {
        assert_eq!(router.view(), View::Launcher);
        assert!(!router.tick(ctx));
    });
}
