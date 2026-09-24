// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

//! Hardware peripheral description and mapping for the Waveshare ESP32-S3-Knob-Touch-LCD-1.8.

use esp_hal::peripherals::*;

/// Peripherals required to drive the SH8601 circular display over QSPI with DMA.
pub struct DisplayPeripherals {
    pub spi: SPI2<'static>,
    pub dma_channel: DMA_CH0<'static>,
    pub cs: GPIO14<'static>,
    pub sck: GPIO13<'static>,
    pub sio0: GPIO15<'static>,
    pub sio1: GPIO16<'static>,
    pub sio2: GPIO17<'static>,
    pub sio3: GPIO18<'static>,
    pub reset_pin: GPIO21<'static>,
    pub backlight: GPIO47<'static>,
    pub ledc: LEDC<'static>,
}

/// Peripherals required for the CST816D touch screen and DRV2605 haptics over shared I2C0.
pub struct I2c0Peripherals {
    pub i2c: I2C0<'static>,
    pub sda: GPIO11<'static>,
    pub scl: GPIO12<'static>,
    pub touch_int: GPIO9<'static>,
    pub touch_rst: GPIO10<'static>,
}

/// Peripherals required for the PCNT quadrature rotary encoder.
pub struct EncoderPeripherals {
    pub pcnt: PCNT<'static>,
    pub pin_a: GPIO8<'static>,
    pub pin_b: GPIO7<'static>,
    pub io_mux: IO_MUX<'static>,
}

/// Hardware peripherals serviced by Core 0 (inputs, profiling, haptics).
pub struct Core0Peripherals {
    pub i2c0: I2c0Peripherals,
    pub encoder: EncoderPeripherals,
    pub profiler_timer: TIMG1<'static>,
}

/// Hardware peripherals serviced by Core 1 (display rendering and DMA flushing).
pub struct Core1Peripherals {
    pub display: DisplayPeripherals,
}

/// Core system peripherals (timers, interrupts, multi-core control, memory).
pub struct SystemPeripherals {
    pub psram: PSRAM<'static>,
    pub timg0: TIMG0<'static>,
    pub sw_interrupt: SW_INTERRUPT<'static>,
    pub cpu_ctrl: CPU_CTRL<'static>,
}

/// Complete peripheral map of the Waveshare ESP32-S3-Knob-Touch-LCD-1.8 board.
pub struct Board {
    pub core0: Core0Peripherals,
    pub core1: Core1Peripherals,
    pub system: SystemPeripherals,
}

impl Board {
    /// Constructs the Board peripheral tree from the raw HAL peripherals.
    pub fn new(p: Peripherals) -> Self {
        Self {
            core0: Core0Peripherals {
                i2c0: I2c0Peripherals {
                    i2c: p.I2C0,
                    sda: p.GPIO11,
                    scl: p.GPIO12,
                    touch_int: p.GPIO9,
                    touch_rst: p.GPIO10,
                },
                encoder: EncoderPeripherals {
                    pcnt: p.PCNT,
                    pin_a: p.GPIO8,
                    pin_b: p.GPIO7,
                    io_mux: p.IO_MUX,
                },
                profiler_timer: p.TIMG1,
            },
            core1: Core1Peripherals {
                display: DisplayPeripherals {
                    spi: p.SPI2,
                    dma_channel: p.DMA_CH0,
                    cs: p.GPIO14,
                    sck: p.GPIO13,
                    sio0: p.GPIO15,
                    sio1: p.GPIO16,
                    sio2: p.GPIO17,
                    sio3: p.GPIO18,
                    reset_pin: p.GPIO21,
                    backlight: p.GPIO47,
                    ledc: p.LEDC,
                },
            },
            system: SystemPeripherals {
                psram: p.PSRAM,
                timg0: p.TIMG0,
                sw_interrupt: p.SW_INTERRUPT,
                cpu_ctrl: p.CPU_CTRL,
            },
        }
    }
}
