// Adapted from rust-enc (https://github.com/rayslava/rust-enc), MIT licensed.
// Copyright (c) rayslava. Copied from enc-app, which is a binary crate and so
// cannot be reached by path dependency; diverges from upstream from here on.

//! CO5300 QSPI display bring-up: power rails, reset, the QSPI DMA bus, and the
//! [`enc_co5300`] driver.
//!
//! The bus talks to `SpiDma` directly rather than through `SpiDmaBus`, so the
//! DMA buffer is ours to fill: a dirty rectangle's rows are packed into it
//! straight from the framebuffer, strides and all, and only then does the
//! transfer start. `SpiDmaBus` would have copied a contiguous slice into that
//! same buffer for us, which is no use when the rows are scattered.
//!
//! Chip-select is driven **in software**: CS falls once at the start of a panel
//! transaction and rises once at its end. A payload larger than the buffer
//! simply fills it again, and only the first transfer carries the command and
//! address phases — so however many transfers it takes, the panel sees one
//! continuous `0x32`/`0x003C00` (WRMC write-continue) write.

use embedded_hal::delay::DelayNs;
use enc_co5300::{Co5300, Co5300Bus};
use esp_hal::Blocking;
use esp_hal::dma::DmaTxBuf;
use esp_hal::dma_tx_buffer;
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_hal::peripherals::{
    DMA_CH0, GPIO3, GPIO4, GPIO7, GPIO10, GPIO11, GPIO12, GPIO13, GPIO14, SPI2,
};
use esp_hal::spi::Mode;
use esp_hal::spi::master::{Address, Command, Config as SpiConfig, DataMode, Spi, SpiDma};
use esp_hal::time::Rate;

/// The SPI peripheral and the DMA buffer it writes from. `half_duplex_write`
/// consumes both and `wait` hands them back, so they travel together.
struct Port {
    spi: SpiDma<'static, Blocking>,
    tx: DmaTxBuf,
}

/// Why a bus transfer failed.
#[derive(Debug)]
pub enum BusError {
    /// The SPI/DMA pair never came back from a transfer. Unreachable in
    /// practice — the only path that moves it out puts it back on both
    /// outcomes — but it keeps that move honest without an `unwrap`.
    Unavailable,
    /// The transfer itself was rejected.
    Spi(esp_hal::spi::Error),
}

/// CO5300 QSPI transport over a raw `SpiDma` plus the DMA buffer it fills.
pub struct QspiBus {
    port: Option<Port>,
    cs: Output<'static>,
}

impl QspiBus {
    /// Runs one panel transaction: CS low, the payload, CS high — a single CS
    /// transition however many DMA transfers the payload needs.
    fn transaction<'a>(
        &mut self,
        data_mode: DataMode,
        cmd: Command,
        address: Address,
        payload: impl Iterator<Item = &'a [u8]>,
    ) -> Result<(), BusError> {
        self.cs.set_low();
        let result = self.write_payload(data_mode, cmd, address, payload);
        self.cs.set_high();
        result
    }

    /// Fills the DMA buffer from `payload` and transfers it, repeating until
    /// the payload runs out. Split out of [`Self::transaction`] so `?` cannot
    /// skip raising CS.
    fn write_payload<'a>(
        &mut self,
        data_mode: DataMode,
        cmd: Command,
        address: Address,
        mut payload: impl Iterator<Item = &'a [u8]>,
    ) -> Result<(), BusError> {
        // Taken by the first transfer; every later one continues the same
        // transaction with neither command nor address.
        let mut opening = Some((cmd, address));
        // Tail of a piece that did not fit in the last buffer-full.
        let mut carry: &[u8] = &[];
        loop {
            let port = self.port.as_mut().ok_or(BusError::Unavailable)?;
            let filled = fill(port.tx.as_mut_slice(), &mut payload, &mut carry);
            if filled == 0 {
                break;
            }
            let (cmd, address) = opening.take().unwrap_or((Command::None, Address::None));
            self.transfer(data_mode, cmd, address, filled)?;
        }
        // An empty payload is still a transaction: command and address go out
        // on their own (this is how most of the init sequence is written).
        if let Some((cmd, address)) = opening {
            self.transfer(data_mode, cmd, address, 0)?;
        }
        Ok(())
    }

    /// Clocks out the first `len` bytes of the DMA buffer and waits for the
    /// transfer to finish, so the buffer is free to refill on return.
    fn transfer(
        &mut self,
        data_mode: DataMode,
        cmd: Command,
        address: Address,
        len: usize,
    ) -> Result<(), BusError> {
        let Port { spi, tx } = self.port.take().ok_or(BusError::Unavailable)?;
        match spi.half_duplex_write(data_mode, cmd, address, 0, len, tx) {
            Ok(transfer) => {
                let (spi, tx) = transfer.wait();
                self.port = Some(Port { spi, tx });
                Ok(())
            }
            Err((e, spi, tx)) => {
                self.port = Some(Port { spi, tx });
                Err(BusError::Spi(e))
            }
        }
    }

    /// Streams window pixels as one transaction, the payload arriving in as
    /// many pieces as the caller's rows.
    fn write_pixel_rows<'a>(
        &mut self,
        rows: impl Iterator<Item = &'a [u8]>,
    ) -> Result<(), BusError> {
        self.transaction(
            DataMode::Quad,
            Command::_8Bit(0x32, DataMode::Single),
            Address::_24Bit(0x00_3c00, DataMode::Single),
            rows,
        )
    }
}

/// Packs as much of `pieces` into `dst` as fits, starting with `carry` — the
/// tail of a piece the last call could not finish — and leaving any new tail
/// there. Returns how many bytes landed.
fn fill<'a>(
    dst: &mut [u8],
    pieces: &mut impl Iterator<Item = &'a [u8]>,
    carry: &mut &'a [u8],
) -> usize {
    let mut filled = 0;
    while filled < dst.len() {
        if carry.is_empty() {
            match pieces.next() {
                Some(piece) => *carry = piece,
                None => break,
            }
        }
        let (Some(room), Some(src)) = (dst.get_mut(filled..), carry.get(..)) else {
            break;
        };
        let take = room.len().min(src.len());
        let (Some(target), Some(head)) = (room.get_mut(..take), src.get(..take)) else {
            break;
        };
        target.copy_from_slice(head);
        *carry = carry.get(take..).unwrap_or_default();
        filled = filled.saturating_add(take);
    }
    filled
}

impl Co5300Bus for QspiBus {
    type Error = BusError;

    fn write_command(&mut self, reg: u8, data: &[u8]) -> Result<(), Self::Error> {
        self.transaction(
            DataMode::Single,
            Command::_8Bit(0x02, DataMode::Single),
            Address::_24Bit(u32::from(reg) << 8, DataMode::Single),
            core::iter::once(data),
        )
    }

    fn write_pixels(&mut self, data: &[u8]) -> Result<(), Self::Error> {
        self.write_pixel_rows(core::iter::once(data))
    }
}

/// Lets [`Display`] keep the bus and borrow it to a driver per call, instead of
/// the driver owning it and walling it off.
impl Co5300Bus for &mut QspiBus {
    type Error = BusError;

    fn write_command(&mut self, reg: u8, data: &[u8]) -> Result<(), Self::Error> {
        (**self).write_command(reg, data)
    }

    fn write_pixels(&mut self, data: &[u8]) -> Result<(), Self::Error> {
        (**self).write_pixels(data)
    }
}

/// Pins the CO5300 needs, grouped to keep the bring-up signature readable.
pub struct DisplayPins {
    pub en: GPIO3<'static>,
    pub rst: GPIO4<'static>,
    pub cs: GPIO10<'static>,
    pub sclk: GPIO12<'static>,
    pub sio0: GPIO11<'static>,
    pub sio1: GPIO13<'static>,
    pub sio2: GPIO7<'static>,
    pub sio3: GPIO14<'static>,
}

/// A live display: the bus, the panel geometry, and the power/reset pins that
/// must stay held for the panel to remain powered.
pub struct Display {
    bus: QspiBus,
    width: u16,
    height: u16,
    _en: Output<'static>,
    _rst: Output<'static>,
}

/// DMA buffer size (bytes) — one buffer-full is one DMA transfer, and a longer
/// payload just refills it inside the same CS assertion. 16 KiB is well under
/// the 32736-byte per-transfer limit and takes a full frame in ~19 transfers.
const TX_BUF_BYTES: usize = 16 * 1024;

/// Using 40 MHz QSPI clock following the vendor firmware's 40 MHz setting.
/// Earlier we used 30 MHz to eliminate some tearing and it seemed to have
/// improved, but there may be no causal relationship between the two.
const QSPI_CLOCK_MHZ: u32 = 40;

/// Why display bring-up failed.
#[derive(Debug)]
pub enum DisplayInitError {
    /// DMA descriptor/buffer setup failed.
    DmaBuffer,
    /// SPI peripheral configuration was rejected.
    Spi(esp_hal::spi::master::ConfigError),
    /// The CO5300 init sequence failed on the bus.
    Controller(BusError),
}

/// Why a flush did not reach the panel.
#[derive(Debug)]
pub enum FlushError {
    /// The rectangle leaves the panel, or the framebuffer is too short for it.
    OutOfRange,
    /// A DMA transfer failed.
    Spi(BusError),
}

/// Lets `ui` deliver a frame without naming this type: the renderer owns the
/// framebuffer and pushes changed rectangles here.
impl ui::Panel for Display {
    type Error = FlushError;

    fn flush(&mut self, rect: ui::DirtyRect, framebuffer: &[u8]) -> Result<(), FlushError> {
        self.flush_rect(rect.x, rect.y, rect.w, rect.h, framebuffer)
    }
}

impl Display {
    /// The CO5300 driver over a borrowed bus. `Co5300` is a plain wrapper over
    /// the bus and the panel geometry with no state of its own, so building one
    /// per call is free.
    fn driver(&mut self) -> Co5300<&mut QspiBus> {
        Co5300::new(&mut self.bus, self.width, self.height, 0, 0)
    }

    /// Flushes the rectangle `(x, y, w, h)` of `fb` — a full-frame,
    /// panel-order RGB565 buffer — to the panel.
    ///
    /// Only the rectangle's own pixels are sent: its rows are packed into the
    /// DMA buffer straight from the framebuffer, so a narrow rectangle costs
    /// its own pixels instead of every line it touches. A rectangle bigger than
    /// the buffer refills it, all inside one chip-select assertion.
    ///
    /// # Errors
    /// [`FlushError::OutOfRange`] if the rectangle leaves the panel or runs
    /// past the end of `fb`; [`FlushError::Spi`] if a transfer fails.
    pub fn flush_rect(
        &mut self,
        x: u16,
        y: u16,
        w: u16,
        h: u16,
        fb: &[u8],
    ) -> Result<(), FlushError> {
        if w == 0 || h == 0 {
            return Ok(());
        }
        let right = x.checked_add(w).ok_or(FlushError::OutOfRange)?;
        let bottom = y.checked_add(h).ok_or(FlushError::OutOfRange)?;
        if right > self.width || bottom > self.height {
            return Err(FlushError::OutOfRange);
        }

        let stride = usize::from(self.width).saturating_mul(2);
        let top = usize::from(y).saturating_mul(stride);
        let end = usize::from(bottom).saturating_mul(stride);
        let band = fb.get(top..end).ok_or(FlushError::OutOfRange)?;

        self.driver()
            .set_window(x, y, w, h)
            .map_err(FlushError::Spi)?;

        if x == 0 && w == self.width {
            self.bus.write_pixel_rows(core::iter::once(band))
        } else {
            // `left..right` is inside every row, `right` being at most `width`,
            // so `filter_map` never actually drops one.
            let left = usize::from(x).saturating_mul(2);
            let right = usize::from(right).saturating_mul(2);
            self.bus.write_pixel_rows(
                band.chunks_exact(stride)
                    .filter_map(|row| row.get(left..right)),
            )
        }
        .map_err(FlushError::Spi)
    }
}

/// Brings up the CO5300: powers the panel, pulses reset, configures the QSPI
/// DMA bus, and runs the controller init sequence.
///
/// # Errors
/// Returns a [`DisplayInitError`] if DMA buffer setup, SPI configuration, or
/// the controller init sequence fails.
pub fn init(
    spi2: SPI2<'static>,
    dma: DMA_CH0<'static>,
    pins: DisplayPins,
    delay: &mut impl DelayNs,
) -> Result<Display, DisplayInitError> {
    let en = Output::new(pins.en, Level::High, OutputConfig::default());
    let mut rst = Output::new(pins.rst, Level::High, OutputConfig::default());
    // CS is ours, not the SPI peripheral's (no `.with_cs` below), so that one
    // transaction is one CS transition regardless of how many DMA transfers it
    // takes. Idles high.
    let cs = Output::new(pins.cs, Level::High, OutputConfig::default());

    // Reset pulse: HIGH 10ms, LOW 200ms, HIGH 200ms (per vendor timing).
    delay.delay_ms(10);
    rst.set_low();
    delay.delay_ms(200);
    rst.set_high();
    delay.delay_ms(200);

    // TX only: nothing is ever read back from the panel. Staying on `SpiDma`
    // (no `with_buffers`) means no `DmaRxBuf` has to be conjured up for a
    // direction this bus never uses.
    let tx = dma_tx_buffer!(TX_BUF_BYTES).map_err(|_| DisplayInitError::DmaBuffer)?;

    let spi = Spi::new(
        spi2,
        SpiConfig::default()
            .with_frequency(Rate::from_mhz(QSPI_CLOCK_MHZ))
            .with_mode(Mode::_0),
    )
    .map_err(DisplayInitError::Spi)?
    .with_sck(pins.sclk)
    .with_sio0(pins.sio0)
    .with_sio1(pins.sio1)
    .with_sio2(pins.sio2)
    .with_sio3(pins.sio3)
    .with_dma(dma);

    let width = u16::try_from(enc_config::display::WIDTH).unwrap_or(390);
    let height = u16::try_from(enc_config::display::HEIGHT).unwrap_or(390);
    let mut display = Display {
        bus: QspiBus {
            port: Some(Port { spi, tx }),
            cs,
        },
        width,
        height,
        _en: en,
        _rst: rst,
    };
    display
        .driver()
        .init(delay)
        .map_err(DisplayInitError::Controller)?;

    Ok(display)
}
