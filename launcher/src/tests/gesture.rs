//! Host tests for touch: what the recogniser makes of a stroke, and what the
//! router then does with it.

use enc_state::AppState;

use crate::{
    AppFactory, Ctx, Dirty, Gesture, Input, Recognizer, Router, TouchPhase, TouchSample, View,
    default_carousel,
};

use super::{Log, StubFactory, feed, sample, swipe, tap, tap_at, with_router};

// --- Recognition ---------------------------------------------------------

/// Runs a stroke through a fresh recogniser and returns what it recognised.
fn recognise(stroke: &[TouchSample]) -> Option<Gesture> {
    let mut recognizer = Recognizer::new();
    stroke.iter().filter_map(|&s| recognizer.push(s)).last()
}

#[test]
fn a_stroke_that_does_not_move_is_a_tap() {
    assert_eq!(
        recognise(&tap(120, 240)),
        Some(Gesture::Tap { x: 120, y: 240 })
    );
}

/// The tap keeps the *landing* point: a finger rolls a little on a panel this
/// small, and where it first touched is what the user aimed at.
#[test]
fn a_tap_reports_where_the_finger_landed() {
    let stroke = [
        sample(TouchPhase::Down, 100, 100, 0),
        sample(TouchPhase::Up, 106, 104, 60),
    ];
    assert_eq!(recognise(&stroke), Some(Gesture::Tap { x: 100, y: 100 }));
}

#[test]
fn a_full_width_stroke_is_a_swipe_in_each_direction() {
    assert_eq!(
        recognise(&swipe(340, 195, 40, 195)),
        Some(Gesture::SwipeLeft)
    );
    assert_eq!(
        recognise(&swipe(40, 195, 340, 195)),
        Some(Gesture::SwipeRight)
    );
    assert_eq!(recognise(&swipe(195, 340, 195, 40)), Some(Gesture::SwipeUp));
    assert_eq!(
        recognise(&swipe(195, 40, 195, 340)),
        Some(Gesture::SwipeDown)
    );
}

/// "All the way across" is the whole point: a short drag is a mis-swipe, and
/// quitting the app on one would be worse than doing nothing.
#[test]
fn a_short_drag_is_neither_a_swipe_nor_a_tap() {
    assert_eq!(recognise(&swipe(300, 195, 180, 195)), None);
}

/// A diagonal drag has no dominant axis, so it resolves to nothing rather than
/// to whichever direction won by a pixel.
#[test]
fn a_diagonal_drag_is_not_a_swipe() {
    assert_eq!(recognise(&swipe(340, 340, 40, 40)), None);
}

/// A stroke that wandered out and came back is a drag the user abandoned, not
/// a tap — even though it starts and ends in the same place.
#[test]
fn a_stroke_that_wanders_and_returns_is_not_a_tap() {
    let stroke = [
        sample(TouchPhase::Down, 195, 195, 0),
        sample(TouchPhase::Move, 195, 100, 40),
        sample(TouchPhase::Up, 195, 195, 80),
    ];
    assert_eq!(recognise(&stroke), None);
}

/// A lift with no landing is a stroke that started before the recogniser was
/// looking; there is nothing to measure it against.
#[test]
fn a_lift_without_a_landing_recognises_nothing() {
    assert_eq!(recognise(&[sample(TouchPhase::Up, 195, 195, 0)]), None);
}

/// Observed on device: the first poll after reset returned `501,3784`, which
/// as a stroke start measures 3591 px upward — a swipe up big enough to quit
/// whatever was on screen.
#[test]
fn an_off_panel_reading_cannot_start_a_swipe() {
    let stroke = [
        sample(TouchPhase::Down, 501, 3784, 0),
        sample(TouchPhase::Up, 204, 193, 60),
    ];
    assert_eq!(recognise(&stroke), None);
}

/// A glitched sample mid-stroke is dropped, not treated as a lift: one bad
/// reading must not cut a real swipe in half.
#[test]
fn an_off_panel_reading_mid_stroke_is_ignored() {
    let stroke = [
        sample(TouchPhase::Down, 340, 195, 0),
        sample(TouchPhase::Move, 4000, 4000, 20),
        sample(TouchPhase::Move, 190, 195, 40),
        sample(TouchPhase::Up, 40, 195, 80),
    ];
    assert_eq!(recognise(&stroke), Some(Gesture::SwipeLeft));
}

/// Observed on device: circling the middle of the screen and finishing at the
/// left rim quit the app, because only the endpoints were being measured.
#[test]
fn a_spiral_that_ends_at_the_far_edge_is_not_a_swipe() {
    let mut stroke = alloc::vec::Vec::new();
    stroke.push(sample(TouchPhase::Down, 340, 195, 0));
    // Four laps around the middle, then out to the left rim.
    for lap in 0..4 {
        let at = 20 + lap * 80;
        stroke.push(sample(TouchPhase::Move, 260, 120, at));
        stroke.push(sample(TouchPhase::Move, 190, 195, at + 20));
        stroke.push(sample(TouchPhase::Move, 260, 270, at + 40));
        stroke.push(sample(TouchPhase::Move, 330, 195, at + 60));
    }
    stroke.push(sample(TouchPhase::Up, 40, 195, 400));
    assert_eq!(recognise(&stroke), None);
}

/// Observed on device: a V counted as a swipe toward whichever arm it ended on.
#[test]
fn a_v_is_not_a_swipe() {
    let stroke = [
        sample(TouchPhase::Down, 380, 190, 0),
        sample(TouchPhase::Move, 200, 350, 60),
        sample(TouchPhase::Up, 30, 190, 120),
    ];
    assert_eq!(recognise(&stroke), None);
}

/// A swipe is a flick, not a journey. Nilton's report: "if I take 1 minute
/// swiping it works".
#[test]
fn a_slow_drag_across_the_panel_is_not_a_swipe() {
    let stroke = [
        sample(TouchPhase::Down, 340, 195, 0),
        sample(TouchPhase::Move, 190, 195, 30_000),
        sample(TouchPhase::Up, 40, 195, 60_000),
    ];
    assert_eq!(recognise(&stroke), None);
}

/// The straightness test must survive a real finger: 100 Hz sampling with a
/// few pixels of cross-axis jitter on every single sample. Summing raw segment
/// lengths would charge all of that to the path and reject a clean swipe, which
/// is why the path only advances in `PATH_STEP` chunks.
#[test]
fn a_dense_jittery_but_straight_swipe_still_counts() {
    let mut stroke = alloc::vec::Vec::new();
    stroke.push(sample(TouchPhase::Down, 340, 195, 0));
    for step in 1..60 {
        let x = 340 - step * 5;
        let y = 195 + if step % 2 == 0 { 4 } else { -4 };
        stroke.push(sample(
            TouchPhase::Move,
            x,
            y,
            u64::try_from(step).unwrap_or(0) * 10,
        ));
    }
    stroke.push(sample(TouchPhase::Up, 40, 195, 600));
    assert_eq!(recognise(&stroke), Some(Gesture::SwipeLeft));
}

/// The same swipe sampled coarsely must be classified the same way: the
/// measurement is about the finger's path, not the controller's poll rate.
#[test]
fn the_sample_rate_does_not_change_the_verdict() {
    let mut sparse = alloc::vec::Vec::new();
    sparse.push(sample(TouchPhase::Down, 340, 195, 0));
    for step in 1..6 {
        let x = 340 - step * 50;
        sparse.push(sample(
            TouchPhase::Move,
            x,
            195,
            u64::try_from(step).unwrap_or(0) * 100,
        ));
    }
    sparse.push(sample(TouchPhase::Up, 40, 195, 600));
    assert_eq!(recognise(&sparse), Some(Gesture::SwipeLeft));
}

/// A suppressed stroke still has to *end*, or the next real gesture would
/// inherit the phantom's start point.
#[test]
fn a_suppressed_stroke_does_not_poison_the_next_one() {
    let mut recognizer = Recognizer::new();
    recognizer.set_button(true, 0);
    for s in tap_at(300, 195, 10) {
        assert_eq!(recognizer.push(s), None);
    }
    recognizer.set_button(false, 100);

    let mut recognised = None;
    for s in tap_at(120, 240, 1_000) {
        recognised = recognizer.push(s).or(recognised);
    }
    assert_eq!(recognised, Some(Gesture::Tap { x: 120, y: 240 }));
}

// --- Routing -------------------------------------------------------------

#[test]
fn tapping_the_focal_card_launches_it() {
    with_router(|router, ctx| {
        let x = router.carousel().card_centre_x(0, 0);
        assert_eq!(feed(router, ctx, &tap(x, 195)), Dirty::Full);
        assert_eq!(router.view(), View::App(0));
    });
}

/// A neighbour peeking in at the rim scrolls to the centre rather than opening
/// from where it stands — the card you can see is the card you can open.
#[test]
fn tapping_a_neighbour_brings_it_to_the_centre() {
    with_router(|router, ctx| {
        let carousel = *router.carousel();
        // Card 1's centre sits at 405 — off the panel entirely. Only its left
        // sliver is reachable, which is exactly the card this is about.
        let x = carousel
            .card_centre_x(1, 0)
            .saturating_sub(carousel.card_w / 2)
            .saturating_add(20);
        assert_eq!(feed(router, ctx, &tap(x, 195)), Dirty::Full);
        assert_eq!(router.selected(), 1);
        assert_eq!(router.view(), View::Launcher, "the first tap only scrolls");

        // Now it is focal, so the same tap opens it.
        let x = carousel.card_centre_x(1, carousel.scroll_for(1));
        feed(router, ctx, &tap(x, 195));
        assert_eq!(router.view(), View::App(1));
    });
}

#[test]
fn tapping_outside_the_cards_does_nothing() {
    with_router(|router, ctx| {
        // Well above the card band.
        assert_eq!(feed(router, ctx, &tap(195, 10)), Dirty::None);
        assert_eq!(router.view(), View::Launcher);
    });
}

/// Nothing happens until the finger lifts: a swipe is only a swipe once you
/// know where it stopped, so the touch-down alone must not launch anything.
#[test]
fn a_card_launches_on_the_lift_not_the_landing() {
    with_router(|router, ctx| {
        let x = router.carousel().card_centre_x(0, 0);
        assert_eq!(
            router.handle(Input::Touch(sample(TouchPhase::Down, x, 195, 0)), ctx),
            Dirty::None
        );
        assert_eq!(router.view(), View::Launcher);
        router.handle(Input::Touch(sample(TouchPhase::Up, x, 195, 0)), ctx);
        assert_eq!(router.view(), View::App(0));
    });
}

/// A swipe across a card is a navigation gesture, not a pick.
#[test]
fn swiping_across_the_launcher_does_not_launch() {
    with_router(|router, ctx| {
        assert_eq!(feed(router, ctx, &swipe(340, 195, 40, 195)), Dirty::None);
        assert_eq!(router.view(), View::Launcher);
    });
}

#[test]
fn swiping_up_leaves_the_app() {
    with_router(|router, ctx| {
        router.handle(Input::ShortPress, ctx);
        assert_eq!(router.view(), View::App(0));
        assert_eq!(feed(router, ctx, &swipe(195, 340, 195, 40)), Dirty::Full);
        assert_eq!(router.view(), View::Launcher);
    });
}

#[test]
fn swiping_left_goes_back() {
    with_router(|router, ctx| {
        router.handle(Input::ShortPress, ctx);
        assert_eq!(feed(router, ctx, &swipe(340, 195, 40, 195)), Dirty::Full);
        assert_eq!(router.view(), View::Launcher);
    });
}

/// Swipes are one-way navigation: the reverse directions are swallowed rather
/// than doing something surprising.
#[test]
fn swiping_down_or_right_stays_in_the_app() {
    with_router(|router, ctx| {
        router.handle(Input::ShortPress, ctx);
        assert_eq!(feed(router, ctx, &swipe(195, 40, 195, 340)), Dirty::None);
        assert_eq!(feed(router, ctx, &swipe(40, 195, 340, 195)), Dirty::None);
        assert_eq!(router.view(), View::App(0));
    });
}

/// Touch belongs to the system: an app that did not ask for the raw panel sees
/// neither the taps nor the swipes that pass over it.
#[test]
fn an_app_never_sees_touch_by_default() {
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
    feed(&mut router, &ctx, &tap(195, 195));
    feed(&mut router, &ctx, &swipe(100, 195, 260, 195));

    assert_eq!(log.touches.get(), 0, "the router owns the panel");
    assert_eq!(log.events.get(), 0, "and touch is not a normalized event");
    assert_eq!(router.view(), View::App(0));
}

/// An app that opts in owns the panel outright — including the strokes that
/// would otherwise navigate, so a canvas can draw right across the screen.
#[test]
fn a_raw_touch_app_receives_every_sample_and_keeps_the_swipes() {
    let state = AppState::new(1);
    let ctx = Ctx {
        now_ms: 0,
        state: &state,
    };
    let log = Log::default();
    let factory = StubFactory::new("Canvas", &log).with_raw_touch();
    let registry: [&dyn AppFactory; 1] = [&factory];
    let mut router = Router::new(&registry, default_carousel(0));

    router.handle(Input::ShortPress, &ctx);
    let dirty = feed(&mut router, &ctx, &swipe(340, 195, 40, 195));

    assert_eq!(dirty, Dirty::Full);
    assert_eq!(log.touches.get(), 3, "down, move and up all reach the app");
    assert_eq!(
        router.view(),
        View::App(0),
        "a swipe right across a canvas must not quit it"
    );
}

// --- The phantom touch an encoder press generates -------------------------

/// The bug that had touch disabled: pressing the encoder also registers a
/// touch, so every press used to launch an app twice over.
#[test]
fn an_encoder_press_does_not_also_tap() {
    with_router(|router, ctx| {
        let x = router.carousel().card_centre_x(0, 0);
        // The contact closes, the panel feels it, then the press is released.
        router.set_button(true, 100);
        let dirty = feed(router, ctx, &tap(x, 195));
        assert_eq!(dirty, Dirty::None, "the phantom must not launch anything");
        assert_eq!(router.view(), View::Launcher);
    });
}

/// The phantom can outlast the contact, so the grace window covers a stroke
/// that only starts after the button comes back up.
#[test]
fn a_touch_just_after_a_press_is_still_the_phantom() {
    with_router(|router, ctx| {
        let x = router.carousel().card_centre_x(0, 0);
        router.set_button(true, 100);
        router.set_button(false, 200);
        feed(router, ctx, &tap_at(x, 195, 300));
        assert_eq!(router.view(), View::Launcher);
    });
}

/// …but the suppression is a window, not a latch: touch has to come back once
/// the phantom has settled, or a single press would disable the panel for good.
#[test]
fn touch_works_again_once_the_phantom_window_passes() {
    with_router(|router, ctx| {
        let x = router.carousel().card_centre_x(0, 0);
        router.set_button(true, 100);
        router.set_button(false, 200);
        feed(router, ctx, &tap_at(x, 195, 2_000));
        assert_eq!(router.view(), View::App(0));
    });
}

/// A stroke already in flight when the button closes is the phantom too — the
/// panel can feel the press coming before the contact does.
#[test]
fn a_press_part_way_through_a_stroke_condemns_it() {
    with_router(|router, ctx| {
        let x = router.carousel().card_centre_x(0, 0);
        router.handle(Input::Touch(sample(TouchPhase::Down, x, 195, 1_000)), ctx);
        router.set_button(true, 1_005);
        router.handle(Input::Touch(sample(TouchPhase::Up, x, 195, 1_040)), ctx);
        assert_eq!(router.view(), View::Launcher);
    });
}
