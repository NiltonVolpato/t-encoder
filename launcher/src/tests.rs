//! Host tests for the pure launcher core.

use embedded_graphics::pixelcolor::Rgb565;
use enc_state::AppState;

use crate::{
    Action, App, Canvas, Ctx, Dirty, IconId, Input, InputEvent, Manifest, Outcome, Router, Tween,
    View, default_carousel, geometry,
};

/// Records what the router did to it, so lifecycle can be asserted.
struct StubApp {
    manifest: Manifest,
    entered: u32,
    exited: u32,
    events: u32,
    /// Returned from `handle`, letting a test drive self-exit.
    action: Action,
}

impl StubApp {
    fn new(name: &'static str) -> StubApp {
        StubApp {
            manifest: Manifest {
                name,
                icon: IconId(0),
                accent: Rgb565::new(31, 0, 0),
            },
            entered: 0,
            exited: 0,
            events: 0,
            action: Action::None,
        }
    }
}

impl App for StubApp {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn on_enter(&mut self, _ctx: &Ctx<'_>) {
        self.entered = self.entered.saturating_add(1);
    }

    fn on_exit(&mut self) {
        self.exited = self.exited.saturating_add(1);
    }

    fn handle(&mut self, _event: InputEvent, _ctx: &Ctx<'_>) -> Outcome {
        self.events = self.events.saturating_add(1);
        match self.action {
            Action::None => Outcome::dirty(Dirty::Full),
            Action::Exit => Outcome::exit(),
        }
    }

    fn render(&self, _ctx: &Ctx<'_>, _fb: &mut Canvas<'_>) {}
}

/// Runs `body` with a two-app router. The registry borrow is fiddly enough
/// that building it once here keeps the tests readable.
fn with_router(body: impl FnOnce(&mut Router<'_>, &Ctx<'_>)) {
    let state = AppState::new(4);
    let ctx = Ctx {
        now_ms: 0,
        state: &state,
    };
    let mut first = StubApp::new("First");
    let mut second = StubApp::new("Second");
    let mut registry: [&mut dyn App; 2] = [&mut first, &mut second];
    let mut router = Router::new(&mut registry, default_carousel(0));
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
fn rotate_moves_selection_and_dirties_only_the_card_band() {
    with_router(|router, ctx| {
        let dirty = router.handle(Input::Rotate(1), ctx);
        assert_eq!(router.selected(), 1);
        assert_eq!(dirty, router.carousel().band());
        assert_ne!(
            dirty,
            Dirty::Full,
            "a slide must not repaint the whole panel"
        );
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
fn long_press_never_reaches_the_app() {
    let state = AppState::new(4);
    let ctx = Ctx {
        now_ms: 0,
        state: &state,
    };
    let mut first = StubApp::new("First");
    {
        let mut registry: [&mut dyn App; 1] = [&mut first];
        let mut router = Router::new(&mut registry, default_carousel(0));
        router.handle(Input::ShortPress, &ctx);
        router.handle(Input::LongPress, &ctx);
    }
    assert_eq!(first.events, 0, "navigation must be consumed by the router");
    assert_eq!(first.entered, 1);
    assert_eq!(first.exited, 1);
}

#[test]
fn an_app_can_exit_itself() {
    let state = AppState::new(4);
    let ctx = Ctx {
        now_ms: 0,
        state: &state,
    };
    let mut first = StubApp::new("First");
    first.action = Action::Exit;
    {
        let mut registry: [&mut dyn App; 1] = [&mut first];
        let mut router = Router::new(&mut registry, default_carousel(0));
        router.handle(Input::ShortPress, &ctx);
        assert_eq!(router.view(), View::App(0));
        router.handle(Input::ShortPress, &ctx);
        assert_eq!(router.view(), View::Launcher);
    }
    assert_eq!(first.exited, 1, "self-exit must still run on_exit");
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
fn tween_runs_from_start_to_target() {
    let tween = Tween::settled(0).retarget(100, 1_000, 250);
    assert_eq!(tween.value_at(1_000), 0, "starts at the origin");
    assert_eq!(tween.value_at(1_250), 100, "lands exactly on target");
    assert_eq!(tween.value_at(9_999), 100, "stays there afterwards");
    assert_eq!(tween.target(), 100);
}

#[test]
fn tween_eases_out() {
    let tween = Tween::settled(0).retarget(1_000, 0, 1_000);
    let mid = tween.value_at(500);
    assert!(
        mid > 500,
        "ease-out is past halfway at the midpoint, got {mid}"
    );
    assert!(mid < 1_000);
}

#[test]
fn tween_is_monotonic() {
    let tween = Tween::settled(0).retarget(500, 0, 300);
    let mut previous = i32::MIN;
    for now in 0..400 {
        let value = tween.value_at(now);
        assert!(value >= previous, "went backwards at {now}ms");
        previous = value;
    }
}

#[test]
fn settled_tween_is_immediately_done() {
    let tween = Tween::settled(42);
    assert!(tween.is_settled(0));
    assert_eq!(tween.value_at(0), 42);
}

#[test]
fn retargeting_mid_flight_starts_from_the_current_value() {
    let first = Tween::settled(0).retarget(1_000, 0, 1_000);
    let at_mid = first.value_at(500);
    let second = first.retarget(0, 500, 1_000);
    assert_eq!(
        second.value_at(500),
        at_mid,
        "a redirected slide must not jump"
    );
}

#[test]
fn zero_duration_tween_does_not_divide_by_zero() {
    let tween = Tween::settled(0).retarget(100, 0, 0);
    assert_eq!(tween.value_at(0), 100);
    assert!(tween.is_settled(0));
}

#[test]
fn launcher_is_dirty_only_while_sliding() {
    let state = AppState::new(4);
    let mut first = StubApp::new("First");
    let mut second = StubApp::new("Second");
    let mut registry: [&mut dyn App; 2] = [&mut first, &mut second];
    let mut router = Router::new(&mut registry, default_carousel(0));

    let start = Ctx {
        now_ms: 0,
        state: &state,
    };
    router.handle(Input::Rotate(1), &start);
    assert_ne!(router.tick(&start), Dirty::None, "slide in progress");

    let settled = Ctx {
        now_ms: 10_000,
        state: &state,
    };
    assert_eq!(router.tick(&settled), Dirty::None, "slide finished");
}

#[test]
fn scroll_settles_on_the_selected_card() {
    with_router(|router, ctx| {
        router.handle(Input::Rotate(1), ctx);
        let carousel = *router.carousel();
        assert_eq!(router.scroll_at(10_000), carousel.scroll_for(1));
    });
}

#[test]
fn dirty_bands_merge_into_a_covering_band() {
    let a = Dirty::Band { y: 10, h: 20 };
    let b = Dirty::Band { y: 40, h: 10 };
    assert_eq!(a.merge(b), Dirty::Band { y: 10, h: 40 });
    assert_eq!(a.merge(Dirty::Full), Dirty::Full);
    assert_eq!(a.merge(Dirty::None), a);
}
