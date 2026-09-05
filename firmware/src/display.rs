// Adapted from rust-enc (https://github.com/rayslava/rust-enc), MIT licensed.
// Copyright (c) rayslava. Copied from enc-app, which is a binary crate and so
// cannot be reached by path dependency; diverges from upstream from here on.

//! CO5300 QSPI display bring-up: power rails, reset, the QSPI DMA bus, and the
//! [`enc_co5300`] driver.
//!
//! The bus uses **hardware chip-select**: each CO5300 pixel chunk is a
//! self-contained `0x32`/`0x003C00` (WRMC write-continue) transaction, so CS
//! toggling per transfer matches the controller protocol.

use embedded_hal::delay::DelayNs;
use enc_co5300::{Co5300, Co5300Bus};
use esp_hal::Blocking;
use esp_hal::dma::{DmaRxBuf, DmaTxBuf};
use esp_hal::dma_buffers;
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_hal::peripherals::{
    DMA_CH0, GPIO3, GPIO4, GPIO7, GPIO10, GPIO11, GPIO12, GPIO13, GPIO14, SPI2,
};
use esp_hal::spi::Mode;
use esp_hal::spi::master::{Address, Command, Config as SpiConfig, DataMode, Spi, SpiDmaBus};
use esp_hal::time::Rate;

/// CO5300 QSPI transport over an esp-hal half-duplex SPI DMA bus.
pub struct QspiBus {
    spi: SpiDmaBus<'static, Blocking>,
}

impl Co5300Bus for QspiBus {
    type Error = esp_hal::spi::Error;

    fn write_command(&mut self, reg: u8, data: &[u8]) -> Result<(), Self::Error> {
        self.spi.half_duplex_write(
            DataMode::Single,
            Command::_8Bit(0x02, DataMode::Single),
            Address::_24Bit(u32::from(reg) << 8, DataMode::Single),
            0,
            data,
        )
    }

    fn write_pixels(&mut self, data: &[u8]) -> Result<(), Self::Error> {
        self.spi.half_duplex_write(
            DataMode::Quad,
            Command::_8Bit(0x32, DataMode::Single),
            Address::_24Bit(0x00_3c00, DataMode::Single),
            0,
            data,
        )
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

/// A live display: the driver plus the power/reset pins that must stay held
/// for the panel to remain powered.
pub struct Display {
    pub driver: Co5300<QspiBus>,
    _en: Output<'static>,
    _rst: Output<'static>,
}

/// Per-transfer DMA staging size (bytes); also bounds the pixel chunk.
pub const DMA_CHUNK: usize = 4096;

/// Why display bring-up failed.
#[derive(Debug)]
pub enum DisplayInitError {
    /// DMA descriptor/buffer setup failed.
    DmaBuffer,
    /// SPI peripheral configuration was rejected.
    Spi(esp_hal::spi::master::ConfigError),
    /// The CO5300 init sequence failed on the bus.
    Controller(esp_hal::spi::Error),
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

    // Reset pulse: HIGH 10ms, LOW 200ms, HIGH 200ms (per vendor timing).
    delay.delay_ms(10);
    rst.set_low();
    delay.delay_ms(200);
    rst.set_high();
    delay.delay_ms(200);

    let (rx_buffer, rx_descriptors, tx_buffer, tx_descriptors) = dma_buffers!(DMA_CHUNK);
    let dma_rx =
        DmaRxBuf::new(rx_descriptors, rx_buffer).map_err(|_| DisplayInitError::DmaBuffer)?;
    let dma_tx =
        DmaTxBuf::new(tx_descriptors, tx_buffer).map_err(|_| DisplayInitError::DmaBuffer)?;

    let spi = Spi::new(
        spi2,
        SpiConfig::default()
            .with_frequency(Rate::from_mhz(40))
            .with_mode(Mode::_0),
    )
    .map_err(DisplayInitError::Spi)?
    .with_sck(pins.sclk)
    .with_sio0(pins.sio0)
    .with_sio1(pins.sio1)
    .with_sio2(pins.sio2)
    .with_sio3(pins.sio3)
    .with_cs(pins.cs)
    .with_dma(dma)
    .with_buffers(dma_rx, dma_tx);

    let width = u16::try_from(enc_config::display::WIDTH).unwrap_or(390);
    let height = u16::try_from(enc_config::display::HEIGHT).unwrap_or(390);
    let mut driver = Co5300::new(QspiBus { spi }, width, height, 0, 0);
    driver.init(delay).map_err(DisplayInitError::Controller)?;

    Ok(Display {
        driver,
        _en: en,
        _rst: rst,
    })
}
