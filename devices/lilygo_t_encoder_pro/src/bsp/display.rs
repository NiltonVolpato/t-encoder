// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

//! CO5300 AMOLED controller on QSPI for LilyGO T-Encoder Pro (390x390).
//! Adapted from Slint's m5stack_stopwatch board support and t-encoder review.

use core::cell::RefCell;
use critical_section::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;
use esp_hal::Blocking;
use esp_hal::delay::Delay;
use esp_hal::dma::DmaTxBuf;
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_hal::interrupt::{InterruptHandler, Priority};
use esp_hal::peripherals::{GPIO3, GPIO4};
use esp_hal::spi::master::{Address, Command, DataMode, SpiDma, SpiDmaTransfer, SpiInterrupt};
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

/// A borrowed framebuffer allocation from internal SRAM for baton passing.
pub struct Framebuffer(pub &'static mut [NativeRgb565; RENDER_STRIDE * BUFFER_HEIGHT]);

impl core::fmt::Debug for Framebuffer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Framebuffer({:p})", self.0.as_ptr())
    }
}

/// A dirty rectangle in rendered half-resolution coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, defmt::Format)]
pub struct DirtyRect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

/// A display flush request containing the loaned framebuffer and dirty regions.
pub struct FlushJob {
    pub fb: Framebuffer,
    pub rects: heapless::Vec<DirtyRect, 3>,
    pub render_cycles: u32,
    pub total_pixels: u32,
    pub rect_count: u16,
}

/// Commands sent to the dedicated display worker.
pub enum DisplayCommand {
    Flush(FlushJob),
    SetBrightness(u8),
    DisplayOff,
    DisplayOn,
}

/// A completed frame buffer returned from the display worker with timing metrics.
pub struct ReturnedBuffer {
    pub fb: Framebuffer,
    pub render_cycles: u32,
    pub transfer_cycles: u32,
    pub dirty_pixels: u32,
    pub rect_count: u16,
}

pub static DISPLAY_COMMAND_CHANNEL: Channel<CriticalSectionRawMutex, DisplayCommand, 4> =
    Channel::new();
pub static FLUSH_RETURN_CHANNEL: Channel<CriticalSectionRawMutex, ReturnedBuffer, 2> =
    Channel::new();

#[inline(always)]
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

/// The SPI peripheral and the TX DMA buffer it writes from.
pub struct Port {
    pub spi: SpiDma<'static, Blocking>,
    pub tx: DmaTxBuf,
}

struct ActiveTransfer {
    job: FlushJob,
    rect_idx: usize,
    current_row: u16,
    first_chunk_of_rect: bool,
    t_start: u32,
    transfer: SpiDmaTransfer<'static, Blocking, DmaTxBuf>,
}

enum TransferState {
    Empty,
    Idle(Port),
    Active(ActiveTransfer),
}

static TRANSFER_STATE: Mutex<RefCell<TransferState>> =
    Mutex::new(RefCell::new(TransferState::Empty));

static TRANSFER_DONE_SIGNAL: Signal<CriticalSectionRawMutex, ()> = Signal::new();

#[esp_hal::ram]
fn raw_command(
    spi: SpiDma<'static, Blocking>,
    mut tx: DmaTxBuf,
    command: u8,
    parameters: &[u8],
) -> Result<(SpiDma<'static, Blocking>, DmaTxBuf), (esp_hal::spi::Error, SpiDma<'static, Blocking>, DmaTxBuf)> {
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
            let (s, tx_back) = t.wait();
            Ok((s, tx_back))
        }
        Err((e, s, tx_back)) => Err((e, s, tx_back)),
    }
}

#[esp_hal::ram]
fn raw_set_window(
    spi: SpiDma<'static, Blocking>,
    tx: DmaTxBuf,
    x: u16,
    y: u16,
    width: u16,
    height: u16,
) -> Result<(SpiDma<'static, Blocking>, DmaTxBuf), (esp_hal::spi::Error, SpiDma<'static, Blocking>, DmaTxBuf)> {
    let x_start = x + X_OFFSET;
    let x_end = x_start + width - 1;
    let (spi, tx) = raw_command(
        spi,
        tx,
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
    raw_command(
        spi,
        tx,
        0x2b,
        &[
            (y_start >> 8) as u8,
            y_start as u8,
            (y_end >> 8) as u8,
            y_end as u8,
        ],
    )
}

#[esp_hal::ram]
extern "C" fn spi_dma_isr() {
    critical_section::with(|cs| {
        let mut state_ref = TRANSFER_STATE.borrow(cs).borrow_mut();
        let state = core::mem::replace(&mut *state_ref, TransferState::Empty);

        match state {
            TransferState::Active(mut active) => {
                let (mut spi, mut tx) = active.transfer.wait();
                spi.clear_interrupts(SpiInterrupt::TransferDone);

                loop {
                    let rect = &active.job.rects[active.rect_idx];
                    let x = rect.x;
                    let y = rect.y;
                    let width = rect.width.min(RENDER_WIDTH.saturating_sub(x));
                    let height = rect.height.min(RENDER_HEIGHT.saturating_sub(y));

                    if active.current_row < y + height {
                        let logical_row_bytes = width as usize * 8;
                        let logical_rows_per_chunk = (TX_BUF_BYTES / logical_row_bytes).max(1);
                        let remaining_rows = (y + height - active.current_row) as usize;
                        let logical_rows = logical_rows_per_chunk.min(remaining_rows);

                        let used = expand_2x2_chunk(
                            &active.job.fb.0[..],
                            RENDER_STRIDE,
                            x as usize,
                            active.current_row as usize,
                            width as usize,
                            logical_rows,
                            tx.as_mut_slice(),
                        );

                        let address = if active.first_chunk_of_rect {
                            CMD_RAMWR
                        } else {
                            CMD_RAMWRC
                        } << 8;
                        active.first_chunk_of_rect = false;
                        active.current_row += logical_rows as u16;

                        let transfer = match spi.half_duplex_write(
                            DataMode::Quad,
                            Command::_8Bit(QSPI_PIXEL_OPCODE, DataMode::Single),
                            Address::_24Bit(address, DataMode::Single),
                            0,
                            used,
                            tx,
                        ) {
                            Ok(t) => t,
                            Err((_e, mut s, tx_back)) => {
                                s.unlisten(SpiInterrupt::TransferDone);
                                *state_ref = TransferState::Idle(Port { spi: s, tx: tx_back });
                                TRANSFER_DONE_SIGNAL.signal(());
                                return;
                            }
                        };

                        active.transfer = transfer;
                        *state_ref = TransferState::Active(active);
                        return;
                    }

                    // Move to next rect
                    active.rect_idx += 1;
                    if active.rect_idx < active.job.rects.len() {
                        let next_rect = &active.job.rects[active.rect_idx];
                        let nx = next_rect.x;
                        let ny = next_rect.y;
                        let nw = next_rect.width.min(RENDER_WIDTH.saturating_sub(nx));
                        let nh = next_rect.height.min(RENDER_HEIGHT.saturating_sub(ny));

                        if nw == 0 || nh == 0 {
                            continue;
                        }

                        // Delay needed to avoid CO5300 AMOLED write pointer glitching between rects
                        Delay::new().delay_micros(10);

                        spi.unlisten(SpiInterrupt::TransferDone);
                        let (s, t) = match raw_set_window(spi, tx, nx * 2, ny * 2, nw * 2, nh * 2) {
                            Ok(res) => res,
                            Err((_e, s, t)) => {
                                *state_ref = TransferState::Idle(Port { spi: s, tx: t });
                                TRANSFER_DONE_SIGNAL.signal(());
                                return;
                            }
                        };
                        spi = s;
                        tx = t;
                        spi.listen(SpiInterrupt::TransferDone);

                        active.current_row = ny;
                        active.first_chunk_of_rect = true;
                        continue;
                    }

                    // All rects completed!
                    spi.unlisten(SpiInterrupt::TransferDone);
                    let transfer_cycles = cycle_count().wrapping_sub(active.t_start);

                    let _ = FLUSH_RETURN_CHANNEL.try_send(ReturnedBuffer {
                        fb: active.job.fb,
                        render_cycles: active.job.render_cycles,
                        transfer_cycles,
                        dirty_pixels: active.job.total_pixels,
                        rect_count: active.job.rect_count,
                    });

                    *state_ref = TransferState::Idle(Port { spi, tx });
                    TRANSFER_DONE_SIGNAL.signal(());
                    return;
                }
            }
            other => {
                *state_ref = other;
            }
        }
    });
}

async fn start_flush(job: FlushJob) {
    let t_start = cycle_count();
    TRANSFER_DONE_SIGNAL.reset();

    // Check if there is at least one non-empty rect
    let mut has_valid_rect = false;
    for rect in &job.rects {
        let x = rect.x;
        let y = rect.y;
        if x < RENDER_WIDTH && y < RENDER_HEIGHT && rect.width > 0 && rect.height > 0 {
            has_valid_rect = true;
            break;
        }
    }

    if !has_valid_rect {
        let _ = FLUSH_RETURN_CHANNEL
            .send(ReturnedBuffer {
                fb: job.fb,
                render_cycles: job.render_cycles,
                transfer_cycles: 0,
                dirty_pixels: job.total_pixels,
                rect_count: job.rect_count,
            })
            .await;
        return;
    }

    let mut pending_job = Some(job);
    let mut started = false;
    critical_section::with(|cs| {
        let mut state = TRANSFER_STATE.borrow(cs).borrow_mut();
        if let TransferState::Idle(mut port) =
            core::mem::replace(&mut *state, TransferState::Empty)
        {
            let job_ref = pending_job.as_ref().unwrap();
            for rect_idx in 0..job_ref.rects.len() {
                let rect = &job_ref.rects[rect_idx];
                let x = rect.x;
                let y = rect.y;
                if x >= RENDER_WIDTH || y >= RENDER_HEIGHT || rect.width == 0 || rect.height == 0 {
                    continue;
                }
                let width = rect.width.min(RENDER_WIDTH - x);
                let height = rect.height.min(RENDER_HEIGHT - y);

                let phys_x = x * 2;
                let phys_y = y * 2;
                let phys_width = width * 2;
                let phys_height = height * 2;

                port.spi.unlisten(SpiInterrupt::TransferDone);
                let (mut spi, mut tx) = match raw_set_window(
                    port.spi,
                    port.tx,
                    phys_x,
                    phys_y,
                    phys_width,
                    phys_height,
                ) {
                    Ok(res) => res,
                    Err((_e, s, t)) => {
                        *state = TransferState::Idle(Port { spi: s, tx: t });
                        return;
                    }
                };

                let logical_row_bytes = width as usize * 8;
                let logical_rows_per_chunk = (TX_BUF_BYTES / logical_row_bytes).max(1);
                let logical_rows = logical_rows_per_chunk.min(height as usize);

                let used = expand_2x2_chunk(
                    &job_ref.fb.0[..],
                    RENDER_STRIDE,
                    x as usize,
                    y as usize,
                    width as usize,
                    logical_rows,
                    tx.as_mut_slice(),
                );

                spi.listen(SpiInterrupt::TransferDone);
                let transfer = match spi.half_duplex_write(
                    DataMode::Quad,
                    Command::_8Bit(QSPI_PIXEL_OPCODE, DataMode::Single),
                    Address::_24Bit(CMD_RAMWR << 8, DataMode::Single),
                    0,
                    used,
                    tx,
                ) {
                    Ok(t) => t,
                    Err((_e, mut s, tx_back)) => {
                        s.unlisten(SpiInterrupt::TransferDone);
                        *state = TransferState::Idle(Port { spi: s, tx: tx_back });
                        return;
                    }
                };

                let active_job = pending_job.take().unwrap();
                *state = TransferState::Active(ActiveTransfer {
                    job: active_job,
                    rect_idx,
                    current_row: y + logical_rows as u16,
                    first_chunk_of_rect: false,
                    t_start,
                    transfer,
                });
                started = true;
                break;
            }
        }
    });

    if started {
        TRANSFER_DONE_SIGNAL.wait().await;
    } else if let Some(job) = pending_job {
        let _ = FLUSH_RETURN_CHANNEL
            .send(ReturnedBuffer {
                fb: job.fb,
                render_cycles: job.render_cycles,
                transfer_cycles: 0,
                dirty_pixels: job.total_pixels,
                rect_count: job.rect_count,
            })
            .await;
    }
}

/// Background worker task that exclusively owns the CO5300 display and processes flush jobs.
#[embassy_executor::task]
pub async fn display_task(mut display: Co5300) {
    critical_section::with(|cs| {
        if let Some(port) = display.port.take() {
            *TRANSFER_STATE.borrow(cs).borrow_mut() = TransferState::Idle(port);
        }
    });

    loop {
        match DISPLAY_COMMAND_CHANNEL.receive().await {
            DisplayCommand::Flush(job) => {
                start_flush(job).await;
            }
            DisplayCommand::SetBrightness(level) => {
                critical_section::with(|cs| {
                    let mut state = TRANSFER_STATE.borrow(cs).borrow_mut();
                    if let TransferState::Idle(port) =
                        core::mem::replace(&mut *state, TransferState::Empty)
                    {
                        match raw_command(port.spi, port.tx, 0x51, &[level]) {
                            Ok((spi, tx)) => *state = TransferState::Idle(Port { spi, tx }),
                            Err((_e, spi, tx)) => *state = TransferState::Idle(Port { spi, tx }),
                        }
                    }
                });
            }
            DisplayCommand::DisplayOff => {
                critical_section::with(|cs| {
                    let mut state = TRANSFER_STATE.borrow(cs).borrow_mut();
                    if let TransferState::Idle(port) =
                        core::mem::replace(&mut *state, TransferState::Empty)
                    {
                        match raw_command(port.spi, port.tx, 0x28, &[]) {
                            Ok((spi, tx)) => *state = TransferState::Idle(Port { spi, tx }),
                            Err((_e, spi, tx)) => *state = TransferState::Idle(Port { spi, tx }),
                        }
                    }
                });
            }
            DisplayCommand::DisplayOn => {
                critical_section::with(|cs| {
                    let mut state = TRANSFER_STATE.borrow(cs).borrow_mut();
                    if let TransferState::Idle(port) =
                        core::mem::replace(&mut *state, TransferState::Empty)
                    {
                        match raw_command(port.spi, port.tx, 0x29, &[]) {
                            Ok((spi, tx)) => *state = TransferState::Idle(Port { spi, tx }),
                            Err((_e, spi, tx)) => *state = TransferState::Idle(Port { spi, tx }),
                        }
                    }
                });
            }
        }
    }
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
        let Port { spi, tx } = self.port.take().unwrap();
        match raw_command(spi, tx, command, parameters) {
            Ok((spi, tx)) => {
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

        self.port
            .as_mut()
            .unwrap()
            .spi
            .set_interrupt_handler(InterruptHandler::new(spi_dma_isr, Priority::Priority1));

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
        let Port { spi, tx } = self.port.take().unwrap();
        match raw_set_window(spi, tx, x, y, width, height) {
            Ok((spi, tx)) => {
                self.port = Some(Port { spi, tx });
                Ok(())
            }
            Err((e, spi, tx)) => {
                self.port = Some(Port { spi, tx });
                Err(e)
            }
        }
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
#[esp_hal::ram]
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
