// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

//! CO5300 AMOLED controller on QSPI for LilyGO T-Encoder Pro (390x390).
//! Adapted from Slint's m5stack_stopwatch board support.

use esp_hal::Blocking;
use esp_hal::delay::Delay;
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_hal::peripherals::{GPIO3, GPIO4};
use esp_hal::spi::master::{Address, Command, DataMode, SpiDmaBus};
use slint::platform::software_renderer::Rgb565Pixel;

pub const DISPLAY_WIDTH: u16 = 390;
pub const DISPLAY_HEIGHT: u16 = 390;

const X_OFFSET: u16 = 0;
const Y_OFFSET: u16 = 0;

/// One DMA transfer's worth of pixel data.
pub const DMA_CHUNK_SIZE: usize = 16380;

const QSPI_CONTROL_OPCODE: u16 = 0x02;
const QSPI_PIXEL_OPCODE: u16 = 0x32;
const CMD_RAMWR: u32 = 0x2c;
const CMD_RAMWRC: u32 = 0x3c;

pub struct Co5300 {
    pub spi: SpiDmaBus<'static, Blocking>,
    pub power_en: Output<'static>,
    pub reset_pin: Output<'static>,
}

impl Co5300 {
    pub fn new(
        spi: SpiDmaBus<'static, Blocking>,
        en: GPIO3<'static>,
        rst: GPIO4<'static>,
    ) -> Self {
        let power_en = Output::new(en, Level::Low, OutputConfig::default());
        let reset_pin = Output::new(rst, Level::High, OutputConfig::default());
        Self {
            spi,
            power_en,
            reset_pin,
        }
    }

    pub fn command(&mut self, command: u8, parameters: &[u8]) -> Result<(), esp_hal::spi::Error> {
        self.spi.half_duplex_write(
            DataMode::Single,
            Command::_8Bit(QSPI_CONTROL_OPCODE, DataMode::Single),
            Address::_24Bit((command as u32) << 8, DataMode::Single),
            0,
            parameters,
        )
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
    #[allow(clippy::too_many_arguments)]
    pub fn write_region(
        &mut self,
        frame_buffer: &[Rgb565Pixel],
        stride: usize,
        x: u16,
        y: u16,
        width: u16,
        height: u16,
        scratch: &mut [u8],
    ) -> Result<(), esp_hal::spi::Error> {
        if width == 0 || height == 0 {
            return Ok(());
        }

        // CO5300 needs even column and row boundaries.
        let x_aligned = x & !1;
        let y_aligned = y & !1;
        let width = ((width + (x - x_aligned) + 1) & !1).min(DISPLAY_WIDTH - x_aligned);
        let height = ((height + (y - y_aligned) + 1) & !1).min(DISPLAY_HEIGHT - y_aligned);
        let (x, y) = (x_aligned, y_aligned);

        self.set_window(x, y, width, height)?;

        let rows_per_chunk = (scratch.len() / (width as usize * 2)).max(1);
        let mut first = true;

        let mut row = y;
        while row < y + height {
            let rows = rows_per_chunk.min((y + height - row) as usize);

            let mut used = 0;
            for line in 0..rows {
                let start = (row as usize + line) * stride + x as usize;
                let pixels = &frame_buffer[start..start + width as usize];
                for pixel in pixels {
                    scratch[used..used + 2].copy_from_slice(&pixel.0.to_be_bytes());
                    used += 2;
                }
            }

            let address = if first { CMD_RAMWR } else { CMD_RAMWRC } << 8;
            self.spi.half_duplex_write(
                DataMode::Quad,
                Command::_8Bit(QSPI_PIXEL_OPCODE, DataMode::Single),
                Address::_24Bit(address, DataMode::Single),
                0,
                &scratch[..used],
            )?;

            first = false;
            row += rows as u16;
        }

        Ok(())
    }
}
