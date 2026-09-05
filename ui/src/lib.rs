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
//!
//! Slint renders **straight into the panel's byte order** via a custom
//! [`TargetPixel`], so there is one framebuffer and no conversion pass.

#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use alloc::rc::Rc;
use core::sync::atomic::{AtomicU32, Ordering};
use core::time::Duration;

use slint::platform::software_renderer::{
    MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType, Rgb565Pixel, TargetPixel,
};
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

/// RGB565 stored in the CO5300's byte order (big-endian).
///
/// `SoftwareRenderer::render` is generic over [`TargetPixel`], so Slint can be
/// told the panel's pixel format directly instead of being made to render
/// native-endian and then converted. That is the difference between one
/// framebuffer and two: a conversion pass would have to write somewhere, and
/// writing back over Slint's own buffer corrupts what `ReusedBuffer` reads
/// between frames.
///
/// The blend maths is delegated to Slint's [`Rgb565Pixel`] so the packing and
/// rounding stay identical to upstream; only the storage order differs. Opaque
/// fills go through the default `blend_slice`, which converts once and then
/// `fill`s, so the swap costs nothing on the common path.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BigEndianRgb565(u16);

impl BigEndianRgb565 {
    /// Reads the pixel back as a native-endian [`Rgb565Pixel`].
    fn to_native(self) -> Rgb565Pixel {
        Rgb565Pixel(u16::from_be(self.0))
    }

    /// Stores a native-endian [`Rgb565Pixel`] in panel order.
    fn from_native(pixel: Rgb565Pixel) -> BigEndianRgb565 {
        BigEndianRgb565(pixel.0.to_be())
    }
}

impl TargetPixel for BigEndianRgb565 {
    fn blend(&mut self, color: PremultipliedRgbaColor) {
        let mut native = self.to_native();
        native.blend(color);
        *self = BigEndianRgb565::from_native(native);
    }

    fn from_rgb(red: u8, green: u8, blue: u8) -> BigEndianRgb565 {
        BigEndianRgb565::from_native(Rgb565Pixel::from_rgb(red, green, blue))
    }
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

    /// Renders one frame into `fb_bytes` and returns the region that changed,
    /// or `None` if nothing needed redrawing.
    ///
    /// The bytes are already in the panel's order, so the caller streams the
    /// dirty rows straight to the display with no conversion.
    pub fn render(&self, fb_bytes: &mut [u8]) -> Option<DirtyRect> {
        slint::platform::update_timers_and_animations();

        let stride = usize::try_from(WIDTH).unwrap_or(0);
        let mut dirty = None;
        let drawn = self.window.draw_if_needed(|renderer| {
            let pixels = as_pixels(fb_bytes);
            let region = renderer.render(pixels, stride);
            let (origin, size) = (region.bounding_box_origin(), region.bounding_box_size());
            dirty = Some(DirtyRect {
                x: u16::try_from(origin.x).unwrap_or(0),
                y: u16::try_from(origin.y).unwrap_or(0),
                w: u16::try_from(size.width).unwrap_or(0),
                h: u16::try_from(size.height).unwrap_or(0),
            });
        });

        if drawn { dirty } else { None }
    }

    /// Whether an animation is still running, so the caller keeps ticking.
    #[must_use]
    pub fn has_active_animations(&self) -> bool {
        self.window.has_active_animations()
    }
}

/// Reinterprets framebuffer bytes as panel-order 16-bit pixels.
fn as_pixels(bytes: &mut [u8]) -> &mut [BigEndianRgb565] {
    let len = bytes.len() / 2;
    // SAFETY: `BigEndianRgb565` is `#[repr(transparent)]` over `u16`. The PSRAM
    // framebuffer base is page-aligned, so the `u16` alignment requirement
    // holds, and `len` is floored so the slice cannot run past the end.
    unsafe { core::slice::from_raw_parts_mut(bytes.as_mut_ptr().cast::<BigEndianRgb565>(), len) }
}
