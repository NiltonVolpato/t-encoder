// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

//! Board Support Package (BSP) for LilyGO T-Encoder Pro (ESP32-S3).

pub mod buzzer;
pub mod display;
pub mod platform;
pub mod rotary;
pub mod touch;

pub use buzzer::signal_feedback;

use esp_hal::delay::Delay;
use esp_hal::dma::{DmaRxBuf, DmaTxBuf};
use esp_hal::dma_buffers;
use esp_hal::i2c::master::{BusTimeout, Config as I2cConfig, I2c};
use esp_hal::spi::Mode;
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::time::Rate;

use display::{Co5300, DMA_CHUNK_SIZE};
pub use platform::{EspPlatform, WindowHolder, run_event_loop};
use rotary::Rotary;
use touch::Chsc5816;

pub struct Bsp {
    pub window: WindowHolder,
    pub display: Co5300,
    pub touch: Option<Chsc5816>,
    pub rotary: Rotary,
}

/// Hardware peripherals required to initialize the Board Support Package (display, touch, rotary).
pub struct BspPeripherals {
    pub spi2: esp_hal::peripherals::SPI2<'static>,
    pub dma_ch0: esp_hal::peripherals::DMA_CH0<'static>,
    pub i2c0: esp_hal::peripherals::I2C0<'static>,
    pub pcnt: esp_hal::peripherals::PCNT<'static>,
    pub gpio0: esp_hal::peripherals::GPIO0<'static>,
    pub gpio1: esp_hal::peripherals::GPIO1<'static>,
    pub gpio2: esp_hal::peripherals::GPIO2<'static>,
    pub gpio3: esp_hal::peripherals::GPIO3<'static>,
    pub gpio4: esp_hal::peripherals::GPIO4<'static>,
    pub gpio5: esp_hal::peripherals::GPIO5<'static>,
    pub gpio6: esp_hal::peripherals::GPIO6<'static>,
    pub gpio7: esp_hal::peripherals::GPIO7<'static>,
    pub gpio8: esp_hal::peripherals::GPIO8<'static>,
    pub gpio9: esp_hal::peripherals::GPIO9<'static>,
    pub gpio10: esp_hal::peripherals::GPIO10<'static>,
    pub gpio11: esp_hal::peripherals::GPIO11<'static>,
    pub gpio12: esp_hal::peripherals::GPIO12<'static>,
    pub gpio13: esp_hal::peripherals::GPIO13<'static>,
    pub gpio14: esp_hal::peripherals::GPIO14<'static>,
}

impl Bsp {
    /// Initializes display, touch, rotary, and registers the Slint platform.
    pub fn init(peripherals: BspPeripherals) -> Self {
        let mut delay = Delay::new();

        // 1. Initialize SPI2 with QSPI mode and DMA for CO5300 AMOLED
        let (rx_buffer, rx_descriptors, tx_buffer, tx_descriptors) = dma_buffers!(DMA_CHUNK_SIZE);
        let dma_rx_buf = DmaRxBuf::new(rx_descriptors, rx_buffer).unwrap();
        let dma_tx_buf = DmaTxBuf::new(tx_descriptors, tx_buffer).unwrap();

        let spi = Spi::new(
            peripherals.spi2,
            SpiConfig::default()
                .with_frequency(Rate::from_mhz(40))
                .with_mode(Mode::_0),
        )
        .unwrap()
        .with_sio0(peripherals.gpio11)
        .with_sio1(peripherals.gpio13)
        .with_sio2(peripherals.gpio7)
        .with_sio3(peripherals.gpio14)
        .with_cs(peripherals.gpio10)
        .with_sck(peripherals.gpio12)
        .with_dma(peripherals.dma_ch0)
        .with_buffers(dma_rx_buf, dma_tx_buf);

        let mut display = Co5300::new(spi, peripherals.gpio3, peripherals.gpio4);
        if let Err(e) = display.init(&mut delay) {
            defmt::error!("Failed to initialize CO5300 display: {:?}", defmt::Debug2Format(&e));
        } else {
            defmt::info!("CO5300 display initialized successfully");
        }

        // 2. Initialize I2C0 for CHSC5816 touch controller
        let touch = match I2c::new(
            peripherals.i2c0,
            I2cConfig::default()
                .with_frequency(Rate::from_khz(400))
                .with_timeout(BusTimeout::Maximum),
        ) {
            Ok(i2c) => {
                let i2c = i2c.with_sda(peripherals.gpio5).with_scl(peripherals.gpio6);
                let mut touch_dev = Chsc5816::new(i2c, peripherals.gpio9, peripherals.gpio8);
                if let Err(_e) = touch_dev.init(&mut delay) {
                    defmt::warn!("CHSC5816 touch init failed");
                    None
                } else {
                    defmt::info!("CHSC5816 touch initialized successfully");
                    Some(touch_dev)
                }
            }
            Err(_) => None,
        };

        // 3. Initialize PCNT quadrature rotary encoder and button
        let rotary = Rotary::new(
            peripherals.pcnt,
            peripherals.gpio1,
            peripherals.gpio2,
            peripherals.gpio0,
        );

        // 4. Set Slint platform
        let (platform, window) = EspPlatform::new();
        slint::platform::set_platform(alloc::boxed::Box::new(platform))
            .expect("Slint platform already set");

        Self {
            window,
            display,
            touch,
            rotary,
        }
    }
}
