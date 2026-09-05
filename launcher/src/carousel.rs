//! Horizontal card carousel: layout and hit-testing.
//!
//! Cards sit on one horizontal line through the centre of the round panel. The
//! focal card is centred; neighbours peek in at the rim. Slint owns the motion
//! — the strip's `animate x` does the sliding — so what survives here is the
//! geometry a *tap* has to be resolved against.

/// Geometry of the card strip.
#[derive(Clone, Copy, Debug)]
pub struct Carousel {
    /// Distance between adjacent card centres.
    pub pitch: i32,
    /// Card width.
    pub card_w: i32,
    /// Card height.
    pub card_h: i32,
    /// X of the focal position (panel centre).
    pub centre_x: i32,
    /// Y of the card centres.
    pub centre_y: i32,
    /// Number of cards.
    pub count: usize,
}

impl Carousel {
    /// The scroll offset at which `index` is centred.
    #[must_use]
    pub fn scroll_for(&self, index: usize) -> i32 {
        let index = i32::try_from(index).unwrap_or(0);
        index.saturating_mul(self.pitch)
    }

    /// X of card `index`'s centre at scroll offset `scroll`.
    #[must_use]
    pub fn card_centre_x(&self, index: usize, scroll: i32) -> i32 {
        self.centre_x
            .saturating_add(self.scroll_for(index))
            .saturating_sub(scroll)
    }

    /// Whether card `index` is at least partly on screen at `scroll`.
    ///
    /// Used to skip drawing the cards the panel cannot show, which is most of
    /// them once a registry grows.
    #[must_use]
    pub fn is_visible(&self, index: usize, scroll: i32, panel_w: i32) -> bool {
        let half = self.card_w.saturating_div(2);
        let cx = self.card_centre_x(index, scroll);
        let left = cx.saturating_sub(half);
        let right = cx.saturating_add(half);
        right >= 0 && left < panel_w
    }

    /// The card containing panel point `(x, y)` at `scroll`, if any.
    #[must_use]
    pub fn hit_test(&self, x: i32, y: i32, scroll: i32) -> Option<usize> {
        let half_h = self.card_h.saturating_div(2);
        if y < self.centre_y.saturating_sub(half_h) || y >= self.centre_y.saturating_add(half_h) {
            return None;
        }
        let half_w = self.card_w.saturating_div(2);
        (0..self.count).find(|&index| {
            let cx = self.card_centre_x(index, scroll);
            x >= cx.saturating_sub(half_w) && x < cx.saturating_add(half_w)
        })
    }

    /// Moves `selected` by `delta` cards, clamped to the registry.
    ///
    /// Clamped rather than wrapping: with cards visibly sliding, wrapping from
    /// the last to the first would animate the whole strip backwards, which
    /// reads as a glitch.
    #[must_use]
    pub fn step(&self, selected: usize, delta: i32) -> usize {
        if self.count == 0 {
            return 0;
        }
        let last = self.count.saturating_sub(1);
        let current = i64::try_from(selected).unwrap_or(0);
        let next = current.saturating_add(i64::from(delta));
        let last_i64 = i64::try_from(last).unwrap_or(0);
        usize::try_from(next.clamp(0, last_i64)).unwrap_or(0)
    }
}
