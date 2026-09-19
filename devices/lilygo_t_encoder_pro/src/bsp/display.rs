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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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

    fn from_rgb(red: u8, green: u8, blue: u8) -> BigEndianRgb565 {
        BigEndianRgb565::from_native(Rgb565Pixel::from_rgb(red, green, blue))
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

    /// Send one rectangle of the frame buffer to the panel.
    pub fn write_region(
        &mut self,
        frame_buffer: &[BigEndianRgb565],
        stride: usize,
        x: u16,
        y: u16,
        width: u16,
        height: u16,
    ) -> Result<(), esp_hal::spi::Error> {
        if width == 0 || height == 0 || x >= DISPLAY_WIDTH || y >= DISPLAY_HEIGHT {
            return Ok(());
        }

        // CO5300 needs even column and row boundaries.
        let x_aligned = x & !1;
        let y_aligned = y & !1;
        if x_aligned >= DISPLAY_WIDTH || y_aligned >= DISPLAY_HEIGHT {
            return Ok(());
        }

        let width = ((width + (x - x_aligned) + 1) & !1).min(DISPLAY_WIDTH - x_aligned);
        let height = ((height + (y - y_aligned) + 1) & !1).min(DISPLAY_HEIGHT - y_aligned);
        if width == 0 || height == 0 {
            return Ok(());
        }

        let (x, y) = (x_aligned, y_aligned);

        self.set_window(x, y, width, height)?;

        let row_bytes = width as usize * 2;
        let rows_per_chunk = (TX_BUF_BYTES / row_bytes).max(1);
        let mut first = true;

        let is_full_width = x == 0 && width == DISPLAY_WIDTH;

        let mut row = y;
        while row < y + height {
            let rows = rows_per_chunk.min((y + height - row) as usize);
            let Port { spi, mut tx } = self.port.take().unwrap();
            let tx_slice = tx.as_mut_slice();

            let used = if is_full_width {
                let total = rows * row_bytes;
                let start = row as usize * stride;
                let pixels = &frame_buffer[start..start + rows * stride];
                let src_bytes = unsafe {
                    core::slice::from_raw_parts(pixels.as_ptr() as *const u8, total)
                };
                tx_slice[..total].copy_from_slice(src_bytes);
                total
            } else {
                let mut offset = 0;
                for line in 0..rows {
                    let start = (row as usize + line) * stride + x as usize;
                    let pixels = &frame_buffer[start..start + width as usize];
                    let src_bytes = unsafe {
                        core::slice::from_raw_parts(pixels.as_ptr() as *const u8, row_bytes)
                    };
                    tx_slice[offset..offset + row_bytes].copy_from_slice(src_bytes);
                    offset += row_bytes;
                }
                offset
            };

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
            row += rows as u16;
        }

        Ok(())
    }
}
