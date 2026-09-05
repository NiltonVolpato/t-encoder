//! embedded-graphics renderer for the launcher carousel.
//!
//! Baseline implementation: primitives and a built-in mono font, no sprites.
//! It exists partly to be *measured* — the P2 Slint spike is judged on flash,
//! RAM and frame-time deltas against this, and a delta needs a baseline. The
//! asset pipeline (anti-aliased sprites, `u8g2-fonts`) replaces the placeholder
//! icon and text here if the spike does not displace the whole approach.

use embedded_graphics::mono_font::{MonoTextStyle, ascii::FONT_10X20};
use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{
    Circle, PrimitiveStyle, PrimitiveStyleBuilder, Rectangle, RoundedRectangle,
};
use embedded_graphics::text::{Alignment, Baseline, Text, TextStyleBuilder};

use crate::app::{Canvas, Ctx};
use crate::carousel::Carousel;
use crate::router::Router;
use enc_ui::Dirty;

/// Panel background. True black costs no power on AMOLED and makes the accent
/// colours pop, so the launcher is deliberately black rather than dark grey.
const BACKGROUND: Rgb565 = Rgb565::new(0, 0, 0);
/// Corner radius of a card.
const CARD_RADIUS: u32 = 24;
/// Icon placeholder diameter.
const ICON_DIAMETER: u32 = 88;
/// Y of the page-dot strip.
const DOTS_Y: i32 = 344;
/// Spacing between page dots.
const DOT_PITCH: i32 = 18;
/// Page dot radius.
const DOT_DIAMETER: u32 = 8;

/// Dirty region covering everything the launcher repaints when the selection
/// changes.
///
/// This is the card strip **union the page dots**, not just the cards: moving
/// the selection also moves the filled dot, and the dots sit well below
/// [`Carousel::band`]. Reporting only the card band leaves a stale dot on the
/// panel until something else forces a full flush.
#[must_use]
pub fn launcher_band(carousel: &Carousel) -> Dirty {
    carousel.band().merge(dots_band())
}

/// Dirty band covering the page-dot strip, matching [`draw_dots`]'s geometry:
/// each dot's top edge is `DOTS_Y - radius` and it is `DOT_DIAMETER` tall.
fn dots_band() -> Dirty {
    let radius = i32::try_from(DOT_DIAMETER.saturating_div(2)).unwrap_or(0);
    let top = DOTS_Y.saturating_sub(radius).max(0);
    Dirty::Band {
        y: u16::try_from(top).unwrap_or(0),
        h: u16::try_from(DOT_DIAMETER).unwrap_or(0),
    }
}

/// Scales a colour toward black by `numerator/16`, for unfocused cards.
///
/// Works on the raw 5/6/5 channels — no float, no gamma. Good enough to read
/// as "further away", which is all the carousel needs.
fn dim(color: Rgb565, numerator: u8) -> Rgb565 {
    let scale = |channel: u8| -> u8 {
        let scaled = u16::from(channel).saturating_mul(u16::from(numerator)) / 16;
        u8::try_from(scaled).unwrap_or(0)
    };
    Rgb565::new(scale(color.r()), scale(color.g()), scale(color.b()))
}

/// Paints the launcher: cards, labels, and the page-dot strip.
pub fn render_launcher(router: &Router<'_>, ctx: &Ctx<'_>, fb: &mut Canvas<'_>) {
    fb.clear_color(BACKGROUND);

    let carousel = router.carousel();
    let scroll = router.scroll_at(ctx.now_ms);
    let panel_w = i32::from(crate::geometry::WIDTH);

    for (index, app) in router.apps().iter().enumerate() {
        if !carousel.is_visible(index, scroll, panel_w) {
            continue;
        }
        let focused = index == router.selected();
        draw_card(
            fb,
            app.manifest(),
            carousel.card_centre_x(index, scroll),
            carousel.centre_y,
            carousel.card_w,
            carousel.card_h,
            focused,
        );
    }

    draw_dots(fb, router.apps().len(), router.selected());
}

/// Draws one app card centred on `(cx, cy)`.
fn draw_card(
    fb: &mut Canvas<'_>,
    manifest: &crate::app::Manifest,
    cx: i32,
    cy: i32,
    card_w: i32,
    card_h: i32,
    focused: bool,
) {
    // Unfocused cards recede rather than disappear, so the strip reads as a
    // row of things rather than one card floating alone.
    let accent = if focused {
        manifest.accent
    } else {
        dim(manifest.accent, 6)
    };
    let label_color = if focused {
        Rgb565::WHITE
    } else {
        dim(Rgb565::WHITE, 8)
    };

    let half_w = card_w.saturating_div(2);
    let half_h = card_h.saturating_div(2);
    let top_left = Point::new(cx.saturating_sub(half_w), cy.saturating_sub(half_h));
    let size = Size::new(
        u32::try_from(card_w).unwrap_or(0),
        u32::try_from(card_h).unwrap_or(0),
    );

    let card_style = PrimitiveStyleBuilder::new()
        .fill_color(dim(accent, 3))
        .stroke_color(accent)
        .stroke_width(if focused { 3 } else { 1 })
        .build();
    let _ = RoundedRectangle::with_equal_corners(
        Rectangle::new(top_left, size),
        Size::new(CARD_RADIUS, CARD_RADIUS),
    )
    .into_styled(card_style)
    .draw(fb);

    // Icon placeholder: a filled disc in the accent colour. Replaced by an
    // anti-aliased sprite once the asset pipeline lands.
    let icon_radius = i32::try_from(ICON_DIAMETER.saturating_div(2)).unwrap_or(0);
    let icon_top_left = Point::new(
        cx.saturating_sub(icon_radius),
        cy.saturating_sub(half_h).saturating_add(28),
    );
    let _ = Circle::new(icon_top_left, ICON_DIAMETER)
        .into_styled(PrimitiveStyle::with_fill(accent))
        .draw(fb);

    let text_style = TextStyleBuilder::new()
        .alignment(Alignment::Center)
        .baseline(Baseline::Middle)
        .build();
    let _ = Text::with_text_style(
        manifest.name,
        Point::new(cx, cy.saturating_add(half_h).saturating_sub(36)),
        MonoTextStyle::new(&FONT_10X20, label_color),
        text_style,
    )
    .draw(fb);
}

/// Draws the page-dot strip: one dot per app, the current one filled.
fn draw_dots(fb: &mut Canvas<'_>, count: usize, selected: usize) {
    if count <= 1 {
        return;
    }
    let count_i32 = i32::try_from(count).unwrap_or(0);
    let total_w = count_i32.saturating_sub(1).saturating_mul(DOT_PITCH);
    let start_x = i32::from(crate::geometry::WIDTH)
        .saturating_div(2)
        .saturating_sub(total_w.saturating_div(2));
    let radius = i32::try_from(DOT_DIAMETER.saturating_div(2)).unwrap_or(0);

    for index in 0..count {
        let offset = i32::try_from(index).unwrap_or(0).saturating_mul(DOT_PITCH);
        let color = if index == selected {
            Rgb565::WHITE
        } else {
            Rgb565::new(8, 16, 8)
        };
        let top_left = Point::new(
            start_x.saturating_add(offset).saturating_sub(radius),
            DOTS_Y.saturating_sub(radius),
        );
        let _ = Circle::new(top_left, DOT_DIAMETER)
            .into_styled(PrimitiveStyle::with_fill(color))
            .draw(fb);
    }
}
