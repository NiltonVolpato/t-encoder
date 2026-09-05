//! Pure app-launcher core.
//!
//! Hardware-independent so it unit-tests on the host: the `App` trait, the
//! router that owns which app is active, input/dirty types, and layout maths.
//! The device binary (`firmware`) supplies the framebuffer and the event source.
//!
//! Populated in P1 — this is currently the crate skeleton, present so the
//! host-test and lint pipeline is wired end to end.

#![no_std]

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

#[cfg(test)]
mod tests {
    use super::geometry;

    #[test]
    fn centre_is_panel_midpoint() {
        assert_eq!(geometry::centre(), (195, 195));
    }
}
