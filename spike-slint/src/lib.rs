//! P2 spike: Slint's `no_std` software renderer on this board. **Disposable.**
//!
//! Answers the four go/no-go criteria in the architecture plan:
//! flash delta, RAM delta, carousel frame time, and whether it composes with
//! our `App` trait instead of demanding ownership of the event loop.
//!
//! Two integration frictions found while writing this, both worth recording
//! even if Slint wins:
//!
//! 1. **Endianness.** Slint's software renderer writes `Rgb565Pixel`, a native
//!    (little-endian) `u16`. The CO5300 wants big-endian bytes, which is why
//!    `enc_co5300::FrameBuffer` stores them that way and can stream straight to
//!    the panel. So a Slint frame needs a byte swap somewhere. Done here
//!    in-place so the cost is honestly included in the measurement; in a real
//!    integration it would fold into the flush's existing PSRAM→DRAM chunk copy
//!    for close to nothing.
//! 2. **Platform ownership.** Slint wants a `Platform` with its own clock, set
//!    once globally. It does *not* insist on running the event loop — a
//!    `MinimalSoftwareWindow` can be driven frame-by-frame from our loop, which
//!    is what makes coexistence with the `App` trait possible at all.

#![no_std]

extern crate alloc;

use alloc::rc::Rc;
use core::sync::atomic::{AtomicU32, Ordering};
use core::time::Duration;

use slint::platform::software_renderer::{MinimalSoftwareWindow, Rgb565Pixel, RepaintBufferType};
use slint::platform::{Platform, PlatformError, WindowAdapter};

slint::include_modules!();

/// Panel geometry.
const WIDTH: u32 = 390;
const HEIGHT: u32 = 390;

/// Milliseconds since boot, published by the firmware so Slint's animation
/// clock matches the one the launcher already uses.
///
/// `u32` because xtensa has no 64-bit atomics; it wraps after ~49 days, which
/// a spike does not care about but a real integration would.
static NOW_MS: AtomicU32 = AtomicU32::new(0);

/// Publishes the current time for Slint's clock. Call once per frame.
pub fn set_now_ms(now_ms: u64) {
    NOW_MS.store(u32::try_from(now_ms % u64::from(u32::MAX)).unwrap_or(0), Ordering::Relaxed);
}

/// Minimal `no_std` platform: a window plus a clock, nothing else.
struct SpikePlatform {
    window: Rc<MinimalSoftwareWindow>,
}

impl Platform for SpikePlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.window.clone())
    }

    fn duration_since_start(&self) -> Duration {
        Duration::from_millis(u64::from(NOW_MS.load(Ordering::Relaxed)))
    }
}

/// Installs the platform and returns the window plus the compiled UI.
///
/// # Errors
/// Returns the platform error if a platform was already installed.
pub fn init() -> Result<(Rc<MinimalSoftwareWindow>, LauncherUI), PlatformError> {
    // `ReusedBuffer` makes Slint track dirty regions and repaint only what
    // changed — the same idea as our `Dirty::Band`, and what would let a Slint
    // frame use `flush_window` rather than a full 15ms flush.
    let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    window.set_size(slint::PhysicalSize::new(WIDTH, HEIGHT));
    slint::platform::set_platform(alloc::boxed::Box::new(SpikePlatform {
        window: window.clone(),
    }))
    .map_err(|_| PlatformError::from("a platform was already installed"))?;
    let ui = LauncherUI::new()?;
    Ok((window, ui))
}

/// Renders one frame into `fb_bytes` (the PSRAM framebuffer), returning whether
/// anything was drawn.
///
/// `fb_bytes` is reinterpreted as `Rgb565Pixel` for Slint, then byte-swapped
/// back into the panel's big-endian order — see the endianness note above.
pub fn render_frame(window: &MinimalSoftwareWindow, fb_bytes: &mut [u8]) -> bool {
    slint::platform::update_timers_and_animations();

    let pixels = bytemuck_cast(fb_bytes);
    let drawn = window.draw_if_needed(|renderer| {
        renderer.render(pixels, usize::try_from(WIDTH).unwrap_or(0));
    });

    if drawn {
        swap_bytes(fb_bytes);
    }
    drawn
}

/// Reinterprets the framebuffer bytes as 16-bit pixels.
///
/// Safe in practice: the PSRAM framebuffer base is page-aligned and therefore
/// `u16`-aligned, and the length is an exact multiple of two.
fn bytemuck_cast(bytes: &mut [u8]) -> &mut [Rgb565Pixel] {
    let len = bytes.len() / 2;
    // SAFETY: `Rgb565Pixel` is a `#[repr(transparent)]` newtype over `u16`.
    // The PSRAM base is page-aligned so the `u16` alignment requirement holds,
    // and `len` is floored so the slice never runs past the end.
    unsafe { core::slice::from_raw_parts_mut(bytes.as_mut_ptr().cast::<Rgb565Pixel>(), len) }
}

/// Swaps every 16-bit pixel to big-endian, in place.
fn swap_bytes(bytes: &mut [u8]) {
    for pixel in bytes.chunks_exact_mut(2) {
        pixel.swap(0, 1);
    }
}
