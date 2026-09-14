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
/// Full framebuffer size in bytes (WIDTH * HEIGHT * 2).
pub const FRAMEBUFFER_BYTES: usize = (WIDTH as usize) * (HEIGHT as usize) * 2;

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

impl DirtyRect {
    /// Smallest bounding box enclosing both rectangles.
    #[must_use]
    pub fn bounding_box(self, other: Self) -> Self {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let right = (self.x.saturating_add(self.w)).max(other.x.saturating_add(other.w));
        let bottom = (self.y.saturating_add(self.h)).max(other.y.saturating_add(other.h));
        Self {
            x,
            y,
            w: right.saturating_sub(x),
            h: bottom.saturating_sub(y),
        }
    }

    /// Rectangle area in pixels.
    #[must_use]
    pub fn area(self) -> usize {
        usize::from(self.w).saturating_mul(usize::from(self.h))
    }
}

/// Maximum extra pixels we are willing to transmit in order to merge two dirty
/// rectangles and avoid a separate SPI transaction.
const COALESCE_THRESHOLD_PIXELS: usize = 200;

/// Merges spatially close dirty rectangles to minimize SPI transaction overhead.
///
/// Rectangles are sorted by `(y, x)` and evaluated against the cost difference:
/// `area(bbox) - (area(last) + area(next)) <= threshold`.
///
/// # Note on potential overlap
/// Because this is a single-pass greedy merge on `(y, x)` sorted rectangles, if an
/// earlier merge expands a bounding box downwards or rightwards past a subsequent
/// rectangle that does not merge with it, the resulting rectangles in `coalesced` can
/// partially overlap. Because the framebuffer contains identical pixels across any
/// overlap, this is visually harmless and only results in redundant SPI transfers for
/// the overlapping region.
fn coalesce_dirty_rects(mut dirty_rects: alloc::vec::Vec<DirtyRect>) -> alloc::vec::Vec<DirtyRect> {
    if dirty_rects.len() <= 1 {
        return dirty_rects;
    }

    dirty_rects.sort_by_key(|r| (r.y, r.x));

    let mut coalesced = alloc::vec::Vec::with_capacity(dirty_rects.len());
    coalesced.push(dirty_rects[0]);

    for next in dirty_rects.into_iter().skip(1) {
        let last = coalesced.last_mut().expect("coalesced is not empty");
        let merged = last.bounding_box(next);
        let extra_pixels = merged
            .area()
            .saturating_sub(last.area().saturating_add(next.area()));

        if extra_pixels <= COALESCE_THRESHOLD_PIXELS {
            *last = merged;
        } else {
            coalesced.push(next);
        }
    }

    coalesced
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

const CYCLES_PER_MICROSECOND: u32 = 240;

#[inline]
fn cycle_count() -> u32 {
    #[cfg(target_arch = "xtensa")]
    {
        xtensa_lx::timer::get_cycle_count()
    }
    #[cfg(not(target_arch = "xtensa"))]
    {
        0
    }
}

#[inline]
fn cycles_to_micros(cycles: u32) -> u32 {
    cycles / CYCLES_PER_MICROSECOND
}

struct RenderStatsBuilder {
    stats: RenderStats,
    draw_start_cycles: u32,
    rect_start_cycles: u32,
}

impl RenderStatsBuilder {
    fn new() -> Self {
        Self {
            stats: RenderStats::default(),
            draw_start_cycles: 0,
            rect_start_cycles: 0,
        }
    }

    fn record_draw(&mut self, elapsed_cycles: u32) {
        self.stats.draw_duration_us = cycles_to_micros(elapsed_cycles);
    }

    fn draw_start(&mut self) {
        self.draw_start_cycles = cycle_count();
    }

    fn draw_end(&mut self) {
        let elapsed = cycle_count().wrapping_sub(self.draw_start_cycles);
        self.record_draw(elapsed);
    }

    fn rectangle_start(&mut self, rect: &DirtyRect) {
        self.stats.rectangles = self.stats.rectangles.saturating_add(1);
        self.stats.total_pixels = self
            .stats
            .total_pixels
            .saturating_add(u32::try_from(rect.area()).unwrap_or(u32::MAX));
        self.rect_start_cycles = cycle_count();
    }

    fn record_rect_flush(&mut self, elapsed_cycles: u32) {
        let us = cycles_to_micros(elapsed_cycles);
        self.stats.total_flush_duration_us = self.stats.total_flush_duration_us.saturating_add(us);
        if self.stats.rectangles == 1 {
            self.stats.min_flush_duration_us = us;
            self.stats.max_flush_duration_us = us;
        } else {
            self.stats.min_flush_duration_us = self.stats.min_flush_duration_us.min(us);
            self.stats.max_flush_duration_us = self.stats.max_flush_duration_us.max(us);
        }
    }

    fn rectangle_end(&mut self) {
        let elapsed = cycle_count().wrapping_sub(self.rect_start_cycles);
        self.record_rect_flush(elapsed);
    }

    fn build(self) -> RenderStats {
        self.stats
    }
}

/// Performance and geometry metrics collected during a single [`Ui::render`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RenderStats {
    /// Number of dirty rectangles flushed to the display panel.
    pub rectangles: u8,
    /// Total pixel count across all flushed rectangles.
    pub total_pixels: u32,
    /// Duration of the Slint draw phase in microseconds.
    pub draw_duration_us: u32,
    /// Total duration spent in panel flush operations in microseconds.
    pub total_flush_duration_us: u32,
    /// Minimum duration for a single rectangle flush in microseconds (0 if no rectangles).
    pub min_flush_duration_us: u32,
    /// Maximum duration for a single rectangle flush in microseconds (0 if no rectangles).
    pub max_flush_duration_us: u32,
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
    framebuffer: Option<&'static mut [u8]>,
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
            framebuffer: Some(framebuffer),
        })
    }

    /// The root component, for setting properties.
    #[must_use]
    pub fn shell(&self) -> &Shell {
        &self.shell
    }

    /// Renders one frame and sends what changed to `panel`.
    ///
    /// Returns [`RenderStats`] describing what was drawn and flushed, or default
    /// (zeroed) stats for a frame in which nothing changed.
    ///
    /// # Errors
    /// Returns the panel's own error if the transfer fails. The frame is still
    /// rendered; only its delivery failed.
    pub fn render<P: Panel>(&mut self, panel: &mut P) -> Result<RenderStats, P::Error> {
        slint::platform::update_timers_and_animations();

        let Some(framebuffer) = self.framebuffer.as_mut() else {
            return Ok(RenderStats::default());
        };

        let stride = usize::try_from(WIDTH).unwrap_or(0);
        let window = &self.window;

        let mut dirty_rects = alloc::vec::Vec::new();
        let mut stats = RenderStatsBuilder::new();
        stats.draw_start();
        let drawn = window.draw_if_needed(|renderer| {
            let pixels = as_pixels(framebuffer);
            let region = renderer.render(pixels, stride);
            for (pos, sz) in region.iter() {
                if let (Ok(x), Ok(y), Ok(w), Ok(h)) = (
                    u16::try_from(pos.x),
                    u16::try_from(pos.y),
                    u16::try_from(sz.width),
                    u16::try_from(sz.height),
                ) {
                    dirty_rects.push(DirtyRect { x, y, w, h });
                }
            }
        });
        stats.draw_end();

        if drawn {
            let dirty_rects = coalesce_dirty_rects(dirty_rects);
            if dirty_rects.is_empty() {
                log::error!("no dirty rects but drawn = true");
            }
            for rect in dirty_rects {
                stats.rectangle_start(&rect);
                panel.flush(rect, framebuffer)?;
                stats.rectangle_end();
            }
        }
        Ok(stats.build())
    }

    /// Whether an animation is still running, so the caller keeps ticking.
    #[must_use]
    pub fn has_active_animations(&self) -> bool {
        self.window.has_active_animations()
    }

    /// Temporarily lends the framebuffer to another task (e.g. for screenshot streaming).
    ///
    /// While absent, [`Ui::render`] skips drawing and returns `Ok(None)` without losing Slint's dirty state.
    pub fn take_framebuffer(&mut self) -> Option<&'static mut [u8]> {
        self.framebuffer.take()
    }

    /// Restores the loaned framebuffer back to the UI.
    pub fn return_framebuffer(&mut self, framebuffer: &'static mut [u8]) {
        self.framebuffer = Some(framebuffer);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_coalesce_empty_and_single() {
        assert_eq!(coalesce_dirty_rects(alloc::vec![]), alloc::vec![]);

        let single = alloc::vec![DirtyRect {
            x: 10,
            y: 20,
            w: 30,
            h: 40
        }];
        assert_eq!(coalesce_dirty_rects(single.clone()), single);
    }

    #[test]
    fn test_coalesce_adjacent_horizontal_bars() {
        // 4 equalizer bars at y=200, height 10, separated by 4px gaps
        let bars = alloc::vec![
            DirtyRect {
                x: 170,
                y: 200,
                w: 9,
                h: 10
            },
            DirtyRect {
                x: 183,
                y: 200,
                w: 9,
                h: 10
            },
            DirtyRect {
                x: 196,
                y: 200,
                w: 9,
                h: 10
            },
            DirtyRect {
                x: 209,
                y: 200,
                w: 9,
                h: 10
            },
        ];
        let coalesced = coalesce_dirty_rects(bars);
        assert_eq!(coalesced.len(), 1);
        assert_eq!(
            coalesced[0],
            DirtyRect {
                x: 170,
                y: 200,
                w: 48,
                h: 10
            }
        );
    }

    #[test]
    fn test_coalesce_vertical_separation_not_merged() {
        // Marquee near top, equalizer in middle
        let rects = alloc::vec![
            DirtyRect {
                x: 105,
                y: 95,
                w: 180,
                h: 20
            },
            DirtyRect {
                x: 170,
                y: 200,
                w: 50,
                h: 20
            },
        ];
        let coalesced = coalesce_dirty_rects(rects);
        assert_eq!(coalesced.len(), 2);
    }

    #[test]
    fn test_coalesce_mixed_clusters() {
        // Marquee + 4 equalizer bars (scrambled order) + progress ring
        let rects = alloc::vec![
            DirtyRect {
                x: 105,
                y: 95,
                w: 180,
                h: 20
            },
            DirtyRect {
                x: 183,
                y: 200,
                w: 9,
                h: 10
            },
            DirtyRect {
                x: 170,
                y: 200,
                w: 9,
                h: 10
            },
            DirtyRect {
                x: 209,
                y: 200,
                w: 9,
                h: 10
            },
            DirtyRect {
                x: 196,
                y: 200,
                w: 9,
                h: 10
            },
            DirtyRect {
                x: 155,
                y: 344,
                w: 80,
                h: 8
            },
        ];
        let coalesced = coalesce_dirty_rects(rects);
        assert_eq!(coalesced.len(), 3);
        assert_eq!(
            coalesced[0],
            DirtyRect {
                x: 105,
                y: 95,
                w: 180,
                h: 20
            }
        );
        assert_eq!(
            coalesced[1],
            DirtyRect {
                x: 170,
                y: 200,
                w: 48,
                h: 10
            }
        );
        assert_eq!(
            coalesced[2],
            DirtyRect {
                x: 155,
                y: 344,
                w: 80,
                h: 8
            }
        );
    }

    #[test]
    fn test_render_stats_default() {
        let stats = RenderStats::default();
        assert_eq!(stats.rectangles, 0);
        assert_eq!(stats.total_pixels, 0);
        assert_eq!(stats.draw_duration_us, 0);
        assert_eq!(stats.total_flush_duration_us, 0);
        assert_eq!(stats.min_flush_duration_us, 0);
        assert_eq!(stats.max_flush_duration_us, 0);
    }

    #[test]
    fn test_cycles_to_micros_and_wrap() {
        assert_eq!(cycles_to_micros(0), 0);
        assert_eq!(cycles_to_micros(240), 1);
        assert_eq!(cycles_to_micros(2400), 10);
        assert_eq!(cycles_to_micros(240_000), 1000);

        // Test wrapping subtraction across u32::MAX
        let start = u32::MAX - 239;
        let end: u32 = 240; // wrapped past u32::MAX (total 480 cycles)
        let elapsed = end.wrapping_sub(start);
        assert_eq!(elapsed, 480);
        assert_eq!(cycles_to_micros(elapsed), 2);
    }

    #[test]
    fn test_render_stats_builder_single_rect() {
        let mut builder = RenderStatsBuilder::new();
        builder.record_draw(240 * 150); // 150 us

        let rect = DirtyRect {
            x: 10,
            y: 20,
            w: 30,
            h: 40,
        }; // area = 1200
        builder.rectangle_start(&rect);
        builder.record_rect_flush(240 * 80); // 80 us

        let stats = builder.build();
        assert_eq!(stats.rectangles, 1);
        assert_eq!(stats.total_pixels, 1200);
        assert_eq!(stats.draw_duration_us, 150);
        assert_eq!(stats.total_flush_duration_us, 80);
        assert_eq!(stats.min_flush_duration_us, 80);
        assert_eq!(stats.max_flush_duration_us, 80);
    }

    #[test]
    fn test_render_stats_builder_multiple_rects() {
        let mut builder = RenderStatsBuilder::new();
        builder.record_draw(240 * 300); // 300 us

        let r1 = DirtyRect {
            x: 0,
            y: 0,
            w: 10,
            h: 10,
        }; // area = 100
        builder.rectangle_start(&r1);
        builder.record_rect_flush(240 * 50); // 50 us

        let r2 = DirtyRect {
            x: 50,
            y: 50,
            w: 20,
            h: 20,
        }; // area = 400
        builder.rectangle_start(&r2);
        builder.record_rect_flush(240 * 200); // 200 us

        let r3 = DirtyRect {
            x: 100,
            y: 100,
            w: 10,
            h: 30,
        }; // area = 300
        builder.rectangle_start(&r3);
        builder.record_rect_flush(240 * 30); // 30 us

        let stats = builder.build();
        assert_eq!(stats.rectangles, 3);
        assert_eq!(stats.total_pixels, 800);
        assert_eq!(stats.draw_duration_us, 300);
        assert_eq!(stats.total_flush_duration_us, 280);
        assert_eq!(stats.min_flush_duration_us, 30);
        assert_eq!(stats.max_flush_duration_us, 200);
    }
}
