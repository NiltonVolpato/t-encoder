//! Host tests for the pure launcher core.

use embedded_graphics::pixelcolor::Rgb565;
use enc_state::AppState;

use alloc::boxed::Box;
use core::cell::Cell;

use crate::{
    Action, App, AppFactory, Ctx, Dirty, Feedback, IconId, Input, InputEvent, Manifest, Outcome,
    Router, View, default_carousel, geometry,
};

/// Shared counters, so a test can observe an app that the router created and
/// dropped without ever holding a reference to it.
#[derive(Default)]
struct Log {
    created: Cell<u32>,
    exited: Cell<u32>,
    dropped: Cell<u32>,
    events: Cell<u32>,
}

/// A stub app whose whole life is recorded in a shared [`Log`].
struct StubApp<'a> {
    log: &'a Log,
    action: Action,
    feedback: Option<Feedback>,
}

impl App for StubApp<'_> {
    fn on_exit(&mut self) {
        self.log.exited.set(self.log.exited.get().saturating_add(1));
    }

    fn handle(&mut self, _event: InputEvent, _ctx: &Ctx<'_>) -> Outcome {
        self.log.events.set(self.log.events.get().saturating_add(1));
        match self.action {
            Action::None => Outcome {
                dirty: Dirty::Full,
                action: Action::None,
                feedback: self.feedback,
            },
            Action::Exit => Outcome::exit(),
        }
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
}

impl<'a> StubFactory<'a> {
    fn new(name: &'static str, log: &'a Log) -> StubFactory<'a> {
        StubFactory {
            manifest: Manifest {
                name,
                icon: IconId(0),
                accent: Rgb565::new(31, 0, 0),
            },
            log,
            action: Action::None,
            feedback: None,
        }
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
    let first = StubFactory::new("First", &log);
    let second = StubFactory::new("Second", &log);
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
fn rotate_onto_the_same_card_is_not_dirty() {
    with_router(|router, ctx| {
        assert_eq!(router.handle(Input::Rotate(-1), ctx), Dirty::None);
    });
}

#[test]
fn short_press_launches_and_long_press_returns_home() {
    with_router(|router, ctx| {
        assert_eq!(router.handle(Input::ShortPress, ctx), Dirty::Full);
        assert_eq!(router.view(), View::App(0));

        assert_eq!(router.handle(Input::LongPress, ctx), Dirty::Full);
        assert_eq!(router.view(), View::Launcher);
    });
}

#[test]
fn long_press_on_the_launcher_does_nothing() {
    with_router(|router, ctx| {
        assert_eq!(router.handle(Input::LongPress, ctx), Dirty::None);
        assert_eq!(router.view(), View::Launcher);
    });
}

#[test]
fn tapping_a_card_launches_that_card() {
    with_router(|router, ctx| {
        let carousel = *router.carousel();
        // Card 1 sits one pitch right of centre while card 0 is focal.
        let x = carousel.card_centre_x(1, 0);
        let dirty = router.handle(Input::Touch { x, y: 195 }, ctx);
        assert_eq!(dirty, Dirty::Full);
        assert_eq!(router.view(), View::App(1));
        assert_eq!(router.selected(), 1);
    });
}

#[test]
fn tapping_outside_the_cards_does_nothing() {
    with_router(|router, ctx| {
        // Well above the card band.
        assert_eq!(
            router.handle(Input::Touch { x: 195, y: 10 }, ctx),
            Dirty::None
        );
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

#[test]
fn dirty_bands_merge_into_a_covering_band() {
    let a = Dirty::Band { y: 10, h: 20 };
    let b = Dirty::Band { y: 40, h: 10 };
    assert_eq!(a.merge(b), Dirty::Band { y: 10, h: 40 });
    assert_eq!(a.merge(Dirty::Full), Dirty::Full);
    assert_eq!(a.merge(Dirty::None), a);
}

/// Slint owns the slide now, so a selection change reports `Full` and lets
/// Slint's own dirty tracking decide what actually gets flushed.
#[test]
fn rotate_moves_selection_and_delegates_painting_to_slint() {
    with_router(|router, ctx| {
        let dirty = router.handle(Input::Rotate(1), ctx);
        assert_eq!(router.selected(), 1);
        assert_eq!(dirty, Dirty::Full);
    });
}

/// The launcher has no tick-driven animation of its own any more — Slint runs
/// its animation clock independently.
#[test]
fn launcher_tick_is_never_dirty() {
    with_router(|router, ctx| {
        router.handle(Input::Rotate(1), ctx);
        assert_eq!(router.tick(ctx), Dirty::None);
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
        assert_eq!(router.tick(ctx), Dirty::None);
    });
}
