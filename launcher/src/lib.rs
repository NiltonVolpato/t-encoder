//! Pure app-launcher core.
//!
//! Hardware-independent so it unit-tests on the host: the [`App`] trait, the
//! [`Router`] that owns which app is active, and carousel geometry for
//! hit-testing. **Rendering and animation belong to Slint** (the `ui` crate) —
//! this crate holds navigation state, not pixels.
//!
//! Input and dirty-region types are reused from `enc_ui` rather than
//! redefined — the launcher sits alongside upstream's UI code, not on top of a
//! replacement for it.

#![no_std]

extern crate alloc;

mod app;
mod carousel;
mod router;

pub use app::{
    Action, App, AppFactory, Ctx, Feedback, IconId, KeyChord, Manifest, Outcome, ViewId,
};
pub use carousel::Carousel;
pub use router::{Input, Router, View};

// Re-exported so apps need only depend on `launcher`.
pub use enc_ui::{Dirty, InputEvent};

/// Panel geometry the launcher lays out against. Mirrors `enc_config::display`,
/// duplicated here so this crate stays free of device dependencies.
pub mod geometry {
    /// Panel width in pixels.
    pub const WIDTH: u16 = 390;
    /// Panel height in pixels.
    pub const HEIGHT: u16 = 390;

    /// Centre point of the round panel.
    #[must_use]
    pub const fn centre() -> (u16, u16) {
        (WIDTH / 2, HEIGHT / 2)
    }
}

/// A carousel sized for the 390x390 round panel: one focal card with its
/// neighbours peeking in at the rim.
#[must_use]
pub fn default_carousel(count: usize) -> Carousel {
    Carousel {
        pitch: 210,
        card_w: 180,
        card_h: 200,
        centre_x: 195,
        centre_y: 195,
        count,
    }
}

#[cfg(test)]
mod tests;
