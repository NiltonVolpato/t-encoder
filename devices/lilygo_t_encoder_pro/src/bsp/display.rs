// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

//! CO5300 AMOLED controller on QSPI for LilyGO T-Encoder Pro (390x390).
//! Adapted from Slint's m5stack_stopwatch board support and t-encoder review.

use esp_hal::Blocking;
use esp_hal::delay::Delay;
use esp_hal::dma::DmaTxBuf;
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_hal::peripherals::{GPIO3, GPIO4};
use esp_hal::spi::master::{Address, Command, DataMode, SpiDma};
use slint::platform::software_renderer::{PremultipliedRgbaColor, Rgb565Pixel, TargetPixel};

pub const DISPLAY_WIDTH: u16 = 390;
pub const DISPLAY_HEIGHT: u16 = 390;
pub const RENDER_WIDTH: u16 = 195;
pub const RENDER_HEIGHT: u16 = 195;
pub const RENDER_STRIDE: usize = 200;
pub const BUFFER_HEIGHT: usize = 196;

const X_OFFSET: u16 = 0;
const Y_OFFSET: u16 = 0;

pub const TX_BUF_BYTES: usize = 16 * 1024;

const QSPI_CONTROL_OPCODE: u16 = 0x02;
const QSPI_PIXEL_OPCODE: u16 = 0x32;
const CMD_RAMWR: u32 = 0x2c;
const CMD_RAMWRC: u32 = 0x3c;

/// RGB565 stored in the CO5300's byte order (big-endian).
///
/// Slint's `software_renderer` is generic over [`TargetPixel`], so Slint can
/// render directly into the panel's pixel format in PSRAM without requiring a
/// separate conversion pass or intermediate scratch buffer.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, defmt::Format)]
pub struct BigEndianRgb565(pub u16);

impl BigEndianRgb565 {
    /// Reads the pixel back as a native-endian [`Rgb565Pixel`].
    pub fn to_native(self) -> Rgb565Pixel {
        Rgb565Pixel(u16::from_be(self.0))
    }

    /// Stores a native-endian [`Rgb565Pixel`] in panel order.
    pub fn from_native(pixel: Rgb565Pixel) -> BigEndianRgb565 {
        BigEndianRgb565(pixel.0.to_be())
    }
}

impl TargetPixel for BigEndianRgb565 {
    fn blend(&mut self, color: PremultipliedRgbaColor) {
        let mut native = self.to_native();
        native.blend(color);
        *self = BigEndianRgb565::from_native(native);
    }

    fn fill_slice(slice: &mut [Self], pixel: Self) {
        super::simd::fill_slice(slice, pixel);
    }

    fn from_rgb(red: u8, green: u8, blue: u8) -> BigEndianRgb565 {
        BigEndianRgb565::from_native(Rgb565Pixel::from_rgb(red, green, blue))
    }
}

/// Native-endian RGB565 pixel for internal SRAM rendering with hardware SIMD fill acceleration.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NativeRgb565(pub Rgb565Pixel);

impl defmt::Format for NativeRgb565 {
    fn format(&self, fmt: defmt::Formatter) {
        defmt::write!(fmt, "NativeRgb565({=u16:#04x})", self.0.0);
    }
}

impl NativeRgb565 {
    pub const fn new(raw: u16) -> Self {
        Self(Rgb565Pixel(raw))
    }

    pub fn raw(self) -> u16 {
        self.0.0
    }
}

impl TargetPixel for NativeRgb565 {
    #[inline(always)]
    fn blend(&mut self, color: PremultipliedRgbaColor) {
        self.0.blend(color);
    }

    #[inline(always)]
    fn fill_slice(slice: &mut [Self], pixel: Self) {
        super::simd::fill_slice(slice, pixel);
    }

    #[inline(always)]
    fn from_rgb(red: u8, green: u8, blue: u8) -> Self {
        Self(Rgb565Pixel::from_rgb(red, green, blue))
    }
}

/// The SPI peripheral and the TX DMA buffer it writes from.
struct Port {
    spi: SpiDma<'static, Blocking>,
    tx: DmaTxBuf,
}

pub struct Co5300 {
    port: Option<Port>,
    pub power_en: Output<'static>,
    pub reset_pin: Output<'static>,
}

impl Co5300 {
    pub fn new(
        spi: SpiDma<'static, Blocking>,
        tx: DmaTxBuf,
        en: GPIO3<'static>,
        rst: GPIO4<'static>,
    ) -> Self {
        let power_en = Output::new(en, Level::Low, OutputConfig::default());
        let reset_pin = Output::new(rst, Level::High, OutputConfig::default());
        Self {
            port: Some(Port { spi, tx }),
            power_en,
            reset_pin,
        }
    }

    pub fn command(&mut self, command: u8, parameters: &[u8]) -> Result<(), esp_hal::spi::Error> {
        let Port { spi, mut tx } = self.port.take().unwrap();
        let len = parameters.len();
        if len > 0 {
            tx.as_mut_slice()[..len].copy_from_slice(parameters);
        }
        let transfer = spi.half_duplex_write(
            DataMode::Single,
            Command::_8Bit(QSPI_CONTROL_OPCODE, DataMode::Single),
            Address::_24Bit((command as u32) << 8, DataMode::Single),
            0,
            len,
            tx,
        );
        match transfer {
            Ok(t) => {
                let (spi, tx) = t.wait();
                self.port = Some(Port { spi, tx });
                Ok(())
            }
            Err((e, spi, tx)) => {
                self.port = Some(Port { spi, tx });
                Err(e)
            }
        }
    }

    pub fn power_on_and_reset(&mut self, delay: &mut Delay) {
        // Enable panel power rail
        self.power_en.set_high();
        delay.delay_millis(50);

        // Hardware reset sequence
        self.reset_pin.set_low();
        delay.delay_millis(20);
        self.reset_pin.set_high();
        delay.delay_millis(150);
    }

    pub fn init(&mut self, delay: &mut Delay) -> Result<(), esp_hal::spi::Error> {
        self.power_on_and_reset(delay);

        self.command(0x11, &[])?; // sleep out
        delay.delay_millis(120);
        self.command(0x34, &[0x00])?; // tearing effect off
        self.command(0xfe, &[0x00])?; // switch to the user command page
        self.command(0xc4, &[0x80])?; // QSPI mode
        self.command(0x3a, &[0x55])?; // 16 bits per pixel (RGB565)
        self.command(0x36, &[0x00])?; // memory access control
        self.command(0x53, &[0x20])?; // brightness control on
        self.command(0x63, &[0xff])?; // brightness in high brightness mode
        self.command(0x29, &[])?; // display on
        self.command(0x51, &[0xff])?; // brightness in normal mode
        self.command(0x58, &[0x00])?; // high contrast mode off
        Ok(())
    }

    /// Sets display brightness level (0..255).
    pub fn set_brightness(&mut self, level: u8) -> Result<(), esp_hal::spi::Error> {
        self.command(0x51, &[level])
    }

    /// Turns the display on.
    pub fn display_on(&mut self) -> Result<(), esp_hal::spi::Error> {
        self.command(0x29, &[])
    }

    /// Turns the display off.
    pub fn display_off(&mut self) -> Result<(), esp_hal::spi::Error> {
        self.command(0x28, &[])
    }

    pub fn set_window(
        &mut self,
        x: u16,
        y: u16,
        width: u16,
        height: u16,
    ) -> Result<(), esp_hal::spi::Error> {
        let x_start = x + X_OFFSET;
        let x_end = x_start + width - 1;
        self.command(
            0x2a,
            &[
                (x_start >> 8) as u8,
                x_start as u8,
                (x_end >> 8) as u8,
                x_end as u8,
            ],
        )?;

        let y_start = y + Y_OFFSET;
        let y_end = y_start + height - 1;
        self.command(
            0x2b,
            &[
                (y_start >> 8) as u8,
                y_start as u8,
                (y_end >> 8) as u8,
                y_end as u8,
            ],
        )
    }

    /// Send one rectangle of the frame buffer to the panel with 2x2 pixel doubling.
    ///
    /// Coordinates and sizes are in logical rendered space (0..RENDER_WIDTH, 0..RENDER_HEIGHT).
    /// The physical display window is automatically set to (2*x, 2*y, 2*width, 2*height).
    pub fn write_region(
        &mut self,
        frame_buffer: &[NativeRgb565],
        stride: usize,
        x: u16,
        y: u16,
        width: u16,
        height: u16,
    ) -> Result<(), esp_hal::spi::Error> {
        if width == 0 || height == 0 || x >= RENDER_WIDTH || y >= RENDER_HEIGHT {
            return Ok(());
        }

        let width = width.min(RENDER_WIDTH - x);
        let height = height.min(RENDER_HEIGHT - y);

        let phys_x = x * 2;
        let phys_y = y * 2;
        let phys_width = width * 2;
        let phys_height = height * 2;

        self.set_window(phys_x, phys_y, phys_width, phys_height)?;

        // Each logical row produces 2 physical rows of phys_width pixels (width * 8 bytes).
        let logical_row_bytes = width as usize * 8;
        let logical_rows_per_chunk = (TX_BUF_BYTES / logical_row_bytes).max(1);

        let mut first = true;
        let mut row = y;
        while row < y + height {
            let logical_rows = logical_rows_per_chunk.min((y + height - row) as usize);
            let Port { spi, mut tx } = self.port.take().unwrap();
            let tx_slice = tx.as_mut_slice();

            let used = expand_2x2_chunk(
                frame_buffer,
                stride,
                x as usize,
                row as usize,
                width as usize,
                logical_rows,
                tx_slice,
            );

            let address = if first { CMD_RAMWR } else { CMD_RAMWRC } << 8;
            let transfer = spi.half_duplex_write(
                DataMode::Quad,
                Command::_8Bit(QSPI_PIXEL_OPCODE, DataMode::Single),
                Address::_24Bit(address, DataMode::Single),
                0,
                used,
                tx,
            );

            match transfer {
                Ok(t) => {
                    let (s, tx_back) = t.wait();
                    self.port = Some(Port { spi: s, tx: tx_back });
                }
                Err((e, s, tx_back)) => {
                    self.port = Some(Port { spi: s, tx: tx_back });
                    return Err(e);
                }
            }

            first = false;
            row += logical_rows as u16;
        }

        Ok(())
    }
}

/// Expands `logical_rows` from `frame_buffer` into `tx_slice` with Scale2x / EPX upscaling.
///
/// Scale2x / EPX inspects the 4 cardinal neighbors (U, D, L, R) of each pixel P:
/// - If U != D and L != R:
///   - Top-left:     E0 = (L == U) ? U : P
///   - Top-right:    E1 = (R == U) ? U : P
///   - Bottom-left:  E2 = (L == D) ? D : P
///   - Bottom-right: E3 = (R == D) ? D : P
/// - Else: all 4 subpixels remain P.
///
/// Because it avoids color averaging, straight horizontal/vertical font strokes remain
/// 100% sharp and crisp (no ink-blot smearing), while circular curves and diagonals
/// are smoothed cleanly.
///
/// Returns the total number of bytes written to `tx_slice`.
pub fn expand_2x2_chunk(
    frame_buffer: &[NativeRgb565],
    stride: usize,
    x: usize,
    y: usize,
    width: usize,
    logical_rows: usize,
    tx_slice: &mut [u8],
) -> usize {
    let phys_row_words = width; // 1 word = 2 pixels = 4 bytes
    let two_rows_bytes = phys_row_words * 8; // 2 physical rows: width * 4 * 2 bytes

    assert!(tx_slice.len() >= logical_rows * two_rows_bytes);
    assert!((tx_slice.as_ptr() as usize & 0x3) == 0);

    let tx_words: &mut [u32] = unsafe {
        core::slice::from_raw_parts_mut(
            tx_slice.as_mut_ptr() as *mut u32,
            logical_rows * phys_row_words * 2,
        )
    };

    let max_col = (stride - 1).min(RENDER_WIDTH as usize - 1);
    let total_rows = frame_buffer.len() / stride;
    let max_row = (total_rows - 1).min(RENDER_HEIGHT as usize - 1);

    for l in 0..logical_rows {
        let curr_y = y + l;
        let prev_y = curr_y.saturating_sub(1);
        let next_y = (curr_y + 1).min(max_row);

        let prev_row = &frame_buffer[prev_y * stride..];
        let curr_row = &frame_buffer[curr_y * stride..];
        let next_row = &frame_buffer[next_y * stride..];

        let row0_start = l * phys_row_words * 2;
        let row1_start = row0_start + phys_row_words;

        let mut c = 0;
        let safe_unroll_limit = if width >= 4 && (x + c) > 0 && x + 4 <= max_col {
            (max_col - 4 - x).min(width - 4)
        } else {
            usize::MAX
        };

        if safe_unroll_limit != usize::MAX {
            while c <= safe_unroll_limit {
                let col = x + c;

                let p0 = curr_row[col].raw();
                let p1 = curr_row[col + 1].raw();
                let p2 = curr_row[col + 2].raw();
                let p3 = curr_row[col + 3].raw();

                let u0 = prev_row[col].raw();
                let u1 = prev_row[col + 1].raw();
                let u2 = prev_row[col + 2].raw();
                let u3 = prev_row[col + 3].raw();

                let d0 = next_row[col].raw();
                let d1 = next_row[col + 1].raw();
                let d2 = next_row[col + 2].raw();
                let d3 = next_row[col + 3].raw();

                let l0 = curr_row[col - 1].raw();
                let r3 = curr_row[col + 4].raw();

                // Col 0
                let (e0_0, e1_0, e2_0, e3_0) = if u0 != d0 && l0 != p1 {
                    (
                        if l0 == u0 { u0 } else { p0 },
                        if p1 == u0 { u0 } else { p0 },
                        if l0 == d0 { d0 } else { p0 },
                        if p1 == d0 { d0 } else { p0 },
                    )
                } else {
                    (p0, p0, p0, p0)
                };

                // Col 1
                let (e0_1, e1_1, e2_1, e3_1) = if u1 != d1 && p0 != p2 {
                    (
                        if p0 == u1 { u1 } else { p1 },
                        if p2 == u1 { u1 } else { p1 },
                        if p0 == d1 { d1 } else { p1 },
                        if p2 == d1 { d1 } else { p1 },
                    )
                } else {
                    (p1, p1, p1, p1)
                };

                // Col 2
                let (e0_2, e1_2, e2_2, e3_2) = if u2 != d2 && p1 != p3 {
                    (
                        if p1 == u2 { u2 } else { p2 },
                        if p3 == u2 { u2 } else { p2 },
                        if p1 == d2 { d2 } else { p2 },
                        if p3 == d2 { d2 } else { p2 },
                    )
                } else {
                    (p2, p2, p2, p2)
                };

                // Col 3
                let (e0_3, e1_3, e2_3, e3_3) = if u3 != d3 && p2 != r3 {
                    (
                        if p2 == u3 { u3 } else { p3 },
                        if r3 == u3 { u3 } else { p3 },
                        if p2 == d3 { d3 } else { p3 },
                        if r3 == d3 { d3 } else { p3 },
                    )
                } else {
                    (p3, p3, p3, p3)
                };

                tx_words[row0_start + c] = (e0_0.to_be() as u32) | ((e1_0.to_be() as u32) << 16);
                tx_words[row0_start + c + 1] = (e0_1.to_be() as u32) | ((e1_1.to_be() as u32) << 16);
                tx_words[row0_start + c + 2] = (e0_2.to_be() as u32) | ((e1_2.to_be() as u32) << 16);
                tx_words[row0_start + c + 3] = (e0_3.to_be() as u32) | ((e1_3.to_be() as u32) << 16);

                tx_words[row1_start + c] = (e2_0.to_be() as u32) | ((e3_0.to_be() as u32) << 16);
                tx_words[row1_start + c + 1] = (e2_1.to_be() as u32) | ((e3_1.to_be() as u32) << 16);
                tx_words[row1_start + c + 2] = (e2_2.to_be() as u32) | ((e3_2.to_be() as u32) << 16);
                tx_words[row1_start + c + 3] = (e2_3.to_be() as u32) | ((e3_3.to_be() as u32) << 16);

                c += 4;
            }
        }

        // Tail loop with edge clamping
        while c < width {
            let col = x + c;
            let left_col = col.saturating_sub(1);
            let right_col = (col + 1).min(max_col);

            let p = curr_row[col].raw();
            let u = prev_row[col].raw();
            let d = next_row[col].raw();
            let l = curr_row[left_col].raw();
            let r = curr_row[right_col].raw();

            let (e0, e1, e2, e3) = if u != d && l != r {
                (
                    if l == u { u } else { p },
                    if r == u { u } else { p },
                    if l == d { d } else { p },
                    if r == d { d } else { p },
                )
            } else {
                (p, p, p, p)
            };

            tx_words[row0_start + c] = (e0.to_be() as u32) | ((e1.to_be() as u32) << 16);
            tx_words[row1_start + c] = (e2.to_be() as u32) | ((e3.to_be() as u32) << 16);

            c += 1;
        }
    }

    logical_rows * two_rows_bytes
}
