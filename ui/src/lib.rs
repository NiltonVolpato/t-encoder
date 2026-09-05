//! The device's single Slint UI tree, plus the `no_std` platform glue.
//!
//! Slint is depended on **only here**. Apps drive properties on the generated
//! components; nothing else in the workspace links a renderer.
//!
//! Rendering uses `RepaintBufferType::ReusedBuffer`, so `render` returns a
//! `PhysicalRegion` describing what actually changed. We flush only that
//! region, which matters because PSRAM bandwidth (~14-15 MB/s measured) is the
//! frame-rate ceiling on this board — touching fewer bytes beats drawing them
//! faster.

#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use alloc::rc::Rc;
use core::sync::atomic::{AtomicU32, Ordering};
use core::time::Duration;

use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType, Rgb565Pixel};
use slint::platform::{Platform, PlatformError, WindowAdapter};

slint::include_modules!();

/// Panel width in pixels.
pub const WIDTH: u32 = 390;
/// Panel height in pixels.
pub const HEIGHT: u32 = 390;

/// Milliseconds since boot, published by the firmware so Slint's animation
/// clock is the same one the rest of the system uses.
///
/// `u32` because xtensa has no 64-bit atomics. Wraps after ~49 days; an
/// animation straddling the wrap would glitch once, which is acceptable.
static NOW_MS: AtomicU32 = AtomicU32::new(0);

/// Publishes the current time to Slint. Call once per frame, before rendering.
pub fn set_now_ms(now_ms: u64) {
    let wrapped = u32::try_from(now_ms % u64::from(u32::MAX)).unwrap_or(0);
    NOW_MS.store(wrapped, Ordering::Relaxed);
}

/// A rectangular region of the panel that needs flushing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirtyRect {
    /// Left edge.
    pub x: u16,
    /// Top edge.
    pub y: u16,
    /// Width in pixels.
    pub w: u16,
    /// Height in pixels.
    pub h: u16,
}

/// Minimal `no_std` platform: a window and a clock, nothing more.
///
/// Slint does **not** take the event loop — `MinimalSoftwareWindow` is driven
/// frame by frame from the firmware's existing loop.
struct DevicePlatform {
    window: Rc<MinimalSoftwareWindow>,
}

impl Platform for DevicePlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.window.clone())
    }

    fn duration_since_start(&self) -> Duration {
        Duration::from_millis(u64::from(NOW_MS.load(Ordering::Relaxed)))
    }
}

/// The live UI: the Slint window plus the root component.
pub struct Ui {
    window: Rc<MinimalSoftwareWindow>,
    shell: Shell,
}

impl Ui {
    /// Installs the platform and builds the shell.
    ///
    /// # Errors
    /// Returns an error if a platform was already installed or the component
    /// could not be created.
    pub fn new() -> Result<Ui, PlatformError> {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
        window.set_size(slint::PhysicalSize::new(WIDTH, HEIGHT));
        slint::platform::set_platform(Box::new(DevicePlatform {
            window: window.clone(),
        }))
        .map_err(|_| PlatformError::from("a Slint platform was already installed"))?;
        let shell = Shell::new()?;
        Ok(Ui { window, shell })
    }

    /// The root component, for setting properties.
    #[must_use]
    pub fn shell(&self) -> &Shell {
        &self.shell
    }

    /// Renders one frame and returns the region that changed, or `None` if
    /// nothing needed redrawing.
    ///
    /// **Two buffers, and they must stay separate.** `render_buf` is Slint's:
    /// with [`RepaintBufferType::ReusedBuffer`] Slint assumes it still holds
    /// *its own* previous frame, in its own native-endian format, and reads it
    /// back when blending partially-redrawn areas. The CO5300 wants big-endian,
    /// so the changed rows are swap-*copied* into `flush_buf` for the DMA
    /// rather than swapped in place.
    ///
    /// Swapping `render_buf` in place instead corrupts exactly what Slint reads
    /// back, which shows up as wrong colours on partially-redrawn text.
    pub fn render(&self, render_buf: &mut [u8], flush_buf: &mut [u8]) -> Option<DirtyRect> {
        slint::platform::update_timers_and_animations();

        let stride = usize::try_from(WIDTH).unwrap_or(0);
        let mut dirty = None;
        let drawn = self.window.draw_if_needed(|renderer| {
            let pixels = as_pixels(render_buf);
            let region = renderer.render(pixels, stride);
            let (origin, size) = (region.bounding_box_origin(), region.bounding_box_size());
            dirty = Some(DirtyRect {
                x: u16::try_from(origin.x).unwrap_or(0),
                y: u16::try_from(origin.y).unwrap_or(0),
                w: u16::try_from(size.width).unwrap_or(0),
                h: u16::try_from(size.height).unwrap_or(0),
            });
        });

        if !drawn {
            return None;
        }
        let rect = dirty?;
        swap_rows_into(render_buf, flush_buf, rect.y, rect.h);
        Some(rect)
    }

    /// Whether an animation is still running, so the caller keeps ticking.
    #[must_use]
    pub fn has_active_animations(&self) -> bool {
        self.window.has_active_animations()
    }
}

/// Reinterprets framebuffer bytes as 16-bit pixels.
fn as_pixels(bytes: &mut [u8]) -> &mut [Rgb565Pixel] {
    let len = bytes.len() / 2;
    // SAFETY: `Rgb565Pixel` is `#[repr(transparent)]` over `u16`. The PSRAM
    // framebuffer base is page-aligned, so the `u16` alignment requirement
    // holds, and `len` is floored so the slice cannot run past the end.
    unsafe { core::slice::from_raw_parts_mut(bytes.as_mut_ptr().cast::<Rgb565Pixel>(), len) }
}

/// Copies rows `[y, y + h)` from `src` to `dst`, swapping each pixel to the
/// panel's big-endian order on the way.
///
/// Full-width rows only: a full-width band is contiguous in both buffers, so
/// this is one pass with no stride arithmetic, and the same band streams to the
/// panel with no gather.
fn swap_rows_into(src: &[u8], dst: &mut [u8], y: u16, h: u16) {
    let row_bytes = usize::try_from(WIDTH).unwrap_or(0).saturating_mul(2);
    let start = usize::from(y).saturating_mul(row_bytes);
    let end = start.saturating_add(usize::from(h).saturating_mul(row_bytes));
    let (Some(from), Some(to)) = (src.get(start..end), dst.get_mut(start..end)) else {
        return;
    };
    for (source, target) in from.chunks_exact(2).zip(to.chunks_exact_mut(2)) {
        if let (Some(&low), Some(&high)) = (source.first(), source.get(1)) {
            target.copy_from_slice(&[high, low]);
        }
    }
}
