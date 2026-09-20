// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

//! Hardware peripheral description and mapping for the LilyGO T-Encoder Pro board.

use esp_hal::peripherals::*;

/// Peripherals required to drive the CO5300 AMOLED display over QSPI with DMA.
pub struct DisplayPeripherals {
    pub spi: SPI2<'static>,
    pub dma_channel: DMA_CH0<'static>,
    pub cs: GPIO10<'static>,
    pub sck: GPIO12<'static>,
    pub sio0: GPIO11<'static>,
    pub sio1: GPIO13<'static>,
    pub sio2: GPIO7<'static>,
    pub sio3: GPIO14<'static>,
    pub power_en: GPIO3<'static>,
    pub reset_pin: GPIO4<'static>,
}

/// Peripherals required for the CHSC5816 capacitive touch screen over I2C.
pub struct TouchPeripherals {
    pub i2c: I2C0<'static>,
    pub sda: GPIO5<'static>,
    pub scl: GPIO6<'static>,
    pub int: GPIO9<'static>,
    pub rst: GPIO8<'static>,
}

/// Peripherals required for the PCNT quadrature rotary encoder and push button.
pub struct EncoderPeripherals {
    pub pcnt: PCNT<'static>,
    pub pin_a: GPIO1<'static>,
    pub pin_b: GPIO2<'static>,
    pub button: GPIO0<'static>,
}

/// Peripherals required for the buzzer / haptic feedback.
pub struct BuzzerPeripherals {
    pub ledc: LEDC<'static>,
    pub pin: GPIO17<'static>,
}

/// Hardware peripherals serviced by Core 0 (inputs, profiling, haptics).
pub struct Core0Peripherals {
    pub touch: TouchPeripherals,
    pub encoder: EncoderPeripherals,
    pub buzzer: BuzzerPeripherals,
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

/// Complete peripheral map of the LilyGO T-Encoder Pro board.
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
                touch: TouchPeripherals {
                    i2c: p.I2C0,
                    sda: p.GPIO5,
                    scl: p.GPIO6,
                    int: p.GPIO9,
                    rst: p.GPIO8,
                },
                encoder: EncoderPeripherals {
                    pcnt: p.PCNT,
                    pin_a: p.GPIO1,
                    pin_b: p.GPIO2,
                    button: p.GPIO0,
                },
                buzzer: BuzzerPeripherals {
                    ledc: p.LEDC,
                    pin: p.GPIO17,
                },
                profiler_timer: p.TIMG1,
            },
            core1: Core1Peripherals {
                display: DisplayPeripherals {
                    spi: p.SPI2,
                    dma_channel: p.DMA_CH0,
                    cs: p.GPIO10,
                    sck: p.GPIO12,
                    sio0: p.GPIO11,
                    sio1: p.GPIO13,
                    sio2: p.GPIO7,
                    sio3: p.GPIO14,
                    power_en: p.GPIO3,
                    reset_pin: p.GPIO4,
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
