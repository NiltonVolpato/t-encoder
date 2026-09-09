//! The device's single Slint UI tree, plus the `no_std` platform glue.
//!
//! Slint is depended on **only here**. Apps drive properties on the generated
//! components; nothing else in the workspace links a renderer.
//!
//! Rendering uses `RepaintBufferType::ReusedBuffer`, so Slint reports what
//! actually changed and we flush only that, which matters because PSRAM
//! bandwidth (~14-15 MB/s measured) is the frame-rate ceiling on this board —
//! touching fewer bytes beats drawing them faster.
//!
//! The framebuffer lives here and goes nowhere: [`Ui::render`] draws into it
//! and hands the changed region straight to a [`Panel`].
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

/// Publishes the current time to Slint and advances animation clocks.
/// Call once per frame, before updating properties or rendering.
pub fn set_now_ms(now_ms: u64) {
    let wrapped = u32::try_from(now_ms % u64::from(u32::MAX)).unwrap_or(0);
    NOW_MS.store(wrapped, Ordering::Relaxed);
    slint::platform::update_timers_and_animations();
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

/// Where a rendered frame goes — the panel, as much of it as rendering needs.
///
/// A trait because `ui` cannot name the firmware's display type and should not
/// want to. It is also the whole reason the framebuffer can stay private: with
/// somewhere to send the pixels, [`Ui`] has no need to hand them out.
pub trait Panel {
    /// What can go wrong writing to the panel.
    type Error: core::fmt::Debug;

    /// Writes the pixels of `rect` out to the display.
    ///
    /// `framebuffer` is the whole frame in panel byte order, at full-frame
    /// stride; `rect` says which part of it changed.
    ///
    /// # Errors
    /// Returns the panel's own error if the transfer fails.
    fn flush(&mut self, rect: DirtyRect, framebuffer: &[u8]) -> Result<(), Self::Error>;
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

/// The live UI: the Slint window, the root component, and the framebuffer they
/// render into.
///
/// The framebuffer is **private on purpose**. Nothing outside this crate writes
/// pixels — apps publish properties and Slint draws — so handing the buffer out
/// only created a seam where the caller had to remember to render and flush the
/// same bytes in the right order.
pub struct Ui {
    window: Rc<MinimalSoftwareWindow>,
    shell: Shell,
    framebuffer: &'static mut [u8],
}

impl Ui {
    /// Installs the platform, builds the shell, and takes the framebuffer.
    ///
    /// `framebuffer` must be `WIDTH * HEIGHT * 2` bytes; the caller owns the
    /// memory it comes from (PSRAM, here) and this crate owns its contents from
    /// now on.
    ///
    /// # Errors
    /// Returns an error if a platform was already installed or the component
    /// could not be created.
    pub fn new(framebuffer: &'static mut [u8]) -> Result<Ui, PlatformError> {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
        window.set_size(slint::PhysicalSize::new(WIDTH, HEIGHT));
        slint::platform::set_platform(Box::new(DevicePlatform {
            window: window.clone(),
        }))
        .map_err(|_| PlatformError::from("a Slint platform was already installed"))?;
        let shell = Shell::new()?;
        Ok(Ui {
            window,
            shell,
            framebuffer,
        })
    }

    /// The root component, for setting properties.
    #[must_use]
    pub fn shell(&self) -> &Shell {
        &self.shell
    }

    /// Renders one frame and sends what changed to `panel`.
    ///
    /// Returns the rectangle that went out, or `None` for a frame in which
    /// nothing changed — Slint decides whether there was anything to draw. That
    /// is reporting, not a decision: the caller has nothing to do with it but
    /// count and log, and is free to ignore it entirely.
    ///
    /// # Errors
    /// Returns the panel's own error if the transfer fails. The frame is still
    /// rendered; only its delivery failed.
    pub fn render<P: Panel>(&mut self, panel: &mut P) -> Result<Option<DirtyRect>, P::Error> {
        slint::platform::update_timers_and_animations();

        let stride = usize::try_from(WIDTH).unwrap_or(0);
        // Split the borrow: `draw_if_needed` takes the window by shared
        // reference while the closure needs the framebuffer mutably.
        let Ui {
            window,
            framebuffer,
            ..
        } = self;

        let mut dirty = None;
        let drawn = window.draw_if_needed(|renderer| {
            let pixels = as_pixels(framebuffer);
            let region = renderer.render(pixels, stride);
            // Only the bounding box: `PhysicalRegion` can hold up to three
            // disjoint rectangles, and unioning them over-sends whenever the
            // changes are scattered. Flushing them separately is a real
            // improvement and a deliberate separate change — it alters the
            // burst pattern on the wire, which is the variable A2 is measuring.
            let (origin, size) = (region.bounding_box_origin(), region.bounding_box_size());
            dirty = Some(DirtyRect {
                x: u16::try_from(origin.x).unwrap_or(0),
                y: u16::try_from(origin.y).unwrap_or(0),
                w: u16::try_from(size.width).unwrap_or(0),
                h: u16::try_from(size.height).unwrap_or(0),
            });
        });

        match dirty.filter(|_| drawn) {
            Some(rect) => panel.flush(rect, framebuffer).map(|()| Some(rect)),
            None => Ok(None),
        }
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
