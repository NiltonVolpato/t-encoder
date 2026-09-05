//! The launcher's apps.
//!
//! Pure and host-testable: an app owns its state and paints a framebuffer, and
//! knows nothing about SPI, PSRAM or Embassy.

#![no_std]

mod screen_app;

pub use screen_app::ScreenApp;

use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::Point;
use enc_ui::{ClockScreen, Layout, Menu, MenuScreen};
use launcher::{IconId, Manifest};

/// Demo menu item labels, carried over from upstream's demo.
pub const MENU_ITEMS: [&str; 4] = ["Beep", "Invert", "Option C", "Option D"];

/// Builds the toggle-menu app (upstream's `MenuScreen`).
#[must_use]
pub fn menu_app() -> ScreenApp<MenuScreen> {
    let layout = Layout {
        top: 95,
        row_height: 56,
        left: 70,
        width: 250,
        count: MENU_ITEMS.len(),
    };
    ScreenApp::new(
        Manifest {
            name: "Toggles",
            icon: IconId(1),
            accent: Rgb565::new(6, 40, 28),
        },
        MenuScreen::new(Menu::new(MENU_ITEMS.len()), layout, &MENU_ITEMS),
    )
}

/// Builds the clock app (upstream's `ClockScreen`).
#[must_use]
pub fn clock_app() -> ScreenApp<ClockScreen> {
    ScreenApp::new(
        Manifest {
            name: "Clock",
            icon: IconId(2),
            accent: Rgb565::new(10, 24, 31),
        },
        ClockScreen::new(Point::new(195, 195), 180),
    )
}
