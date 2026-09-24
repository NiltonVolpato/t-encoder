// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

//! Hardware driver for the Texas Instruments DRV2605 haptic motor driver.

#![no_std]

use embedded_hal_async::i2c::I2c;

/// Standard 7-bit I2C slave address for DRV2605.
pub const DEFAULT_I2C_ADDRESS: u8 = 0x5A;

// Register definitions
pub const REG_STATUS: u8 = 0x00;
pub const REG_MODE: u8 = 0x01;
pub const REG_RTPIN: u8 = 0x02;
pub const REG_LIBRARY: u8 = 0x03;
pub const REG_WAVESEQ1: u8 = 0x04;
pub const REG_WAVESEQ2: u8 = 0x05;
pub const REG_GO: u8 = 0x0C;
pub const REG_OVERDRIVE: u8 = 0x0D;
pub const REG_SUSTAINPOS: u8 = 0x0E;
pub const REG_SUSTAINNEG: u8 = 0x0F;
pub const REG_BREAK: u8 = 0x10;
pub const REG_AUDIOMAX: u8 = 0x13;
pub const REG_FEEDBACK: u8 = 0x1A;
pub const REG_CONTROL3: u8 = 0x1D;

/// Operating modes for the DRV2605.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Mode {
    InternalTrigger = 0x00,
    ExternalTriggerEdge = 0x01,
    ExternalTriggerLevel = 0x02,
    PwmAnalog = 0x03,
    AudioToVibe = 0x04,
    RealTimePlayback = 0x05,
    Diagnostics = 0x06,
    AutoCalibration = 0x07,
}

/// Waveform ROM library selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Library {
    Empty = 0,
    ErmLibraryA = 1,
    ErmLibraryB = 2,
    ErmLibraryC = 3,
    ErmLibraryD = 4,
    ErmLibraryE = 5,
    Lra = 6,
}

/// DRV2605 haptic driver over async I2C.
pub struct Drv2605<I2C> {
    i2c: I2C,
    address: u8,
}

impl<I2C> Drv2605<I2C> {
    /// Creates a new driver instance using the default I2C address (`0x5A`).
    pub const fn new(i2c: I2C) -> Self {
        Self { i2c, address: DEFAULT_I2C_ADDRESS }
    }

    /// Creates a new driver instance with a custom I2C address.
    pub const fn with_address(i2c: I2C, address: u8) -> Self {
        Self { i2c, address }
    }

    /// Releases the underlying I2C peripheral.
    pub fn release(self) -> I2C {
        self.i2c
    }
}

impl<I2C: I2c> Drv2605<I2C> {
    /// Reads a single 8-bit register.
    pub async fn read_register(&mut self, reg: u8) -> Result<u8, I2C::Error> {
        let mut buf = [0u8; 1];
        self.i2c.write_read(self.address, &[reg], &mut buf).await?;
        Ok(buf[0])
    }

    /// Writes a single 8-bit register.
    pub async fn write_register(&mut self, reg: u8, val: u8) -> Result<(), I2C::Error> {
        self.i2c.write(self.address, &[reg, val]).await
    }

    /// Initializes the DRV2605 for ERM open-loop playback as configured on the Waveshare board.
    pub async fn init(&mut self) -> Result<(), I2C::Error> {
        // 1. Take out of standby by writing 0x00 to MODE
        self.write_register(REG_MODE, Mode::InternalTrigger as u8).await?;

        // 2. Clear real-time playback input
        self.write_register(REG_RTPIN, 0x00).await?;

        // 3. Clear waveform sequencer
        self.write_register(REG_WAVESEQ1, 1).await?;
        self.write_register(REG_WAVESEQ2, 0).await?;

        // 4. Default timing offsets
        self.write_register(REG_OVERDRIVE, 0x00).await?;
        self.write_register(REG_SUSTAINPOS, 0x00).await?;
        self.write_register(REG_SUSTAINNEG, 0x00).await?;
        self.write_register(REG_BREAK, 0x00).await?;
        self.write_register(REG_AUDIOMAX, 0x64).await?;

        // 5. Select ROM Library 5 (ERM Library E)
        self.select_library(Library::ErmLibraryE).await?;

        // 6. ERM Open-Loop configuration
        let feedback = self.read_register(REG_FEEDBACK).await?;
        self.write_register(REG_FEEDBACK, feedback & 0x7F).await?; // Clear N_ERM_LRA bit

        let control3 = self.read_register(REG_CONTROL3).await?;
        self.write_register(REG_CONTROL3, control3 | 0x20).await?; // Set ERM_OPEN_LOOP bit

        Ok(())
    }

    /// Selects the waveform ROM library.
    pub async fn select_library(&mut self, lib: Library) -> Result<(), I2C::Error> {
        self.write_register(REG_LIBRARY, lib as u8).await
    }

    /// Sets the operational mode.
    pub async fn set_mode(&mut self, mode: Mode) -> Result<(), I2C::Error> {
        self.write_register(REG_MODE, mode as u8).await
    }

    /// Programs a single waveform slot (0..=7).
    pub async fn set_waveform(&mut self, slot: u8, effect_id: u8) -> Result<(), I2C::Error> {
        if slot < 8 {
            self.write_register(REG_WAVESEQ1 + slot, effect_id).await?;
        }
        Ok(())
    }

    /// Starts waveform playback.
    pub async fn go(&mut self) -> Result<(), I2C::Error> {
        self.write_register(REG_GO, 1).await
    }

    /// Stops waveform playback immediately.
    pub async fn stop(&mut self) -> Result<(), I2C::Error> {
        self.write_register(REG_GO, 0).await
    }

    /// Convenient helper to play a single effect immediately.
    pub async fn play_effect(&mut self, effect_id: u8) -> Result<(), I2C::Error> {
        self.set_waveform(0, effect_id).await?;
        self.set_waveform(1, 0).await?; // End of sequence
        self.go().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockI2c {
        regs: [u8; 32],
    }

    impl MockI2c {
        fn new() -> Self {
            Self { regs: [0; 32] }
        }
    }

    impl embedded_hal_async::i2c::ErrorType for MockI2c {
        type Error = core::convert::Infallible;
    }

    impl embedded_hal_async::i2c::I2c for MockI2c {
        async fn transaction(
            &mut self,
            _address: u8,
            operations: &mut [embedded_hal_async::i2c::Operation<'_>],
        ) -> Result<(), Self::Error> {
            let mut last_reg = 0usize;
            for op in operations {
                match op {
                    embedded_hal_async::i2c::Operation::Write(buf) => {
                        if !buf.is_empty() {
                            last_reg = buf[0] as usize;
                            if buf.len() > 1 && last_reg < self.regs.len() {
                                self.regs[last_reg] = buf[1];
                            }
                        }
                    }
                    embedded_hal_async::i2c::Operation::Read(buf) => {
                        if !buf.is_empty() && last_reg < self.regs.len() {
                            buf[0] = self.regs[last_reg];
                        }
                    }
                }
            }
            Ok(())
        }
    }

    fn block_on<F: core::future::Future>(f: F) -> F::Output {
        let mut cx = core::task::Context::from_waker(core::task::Waker::noop());
        let mut pinned = core::pin::pin!(f);
        loop {
            match pinned.as_mut().poll(&mut cx) {
                core::task::Poll::Ready(val) => return val,
                core::task::Poll::Pending => panic!("Future remained pending"),
            }
        }
    }

    #[test]
    fn test_init_and_play() {
        block_on(async {
            let mock = MockI2c::new();
            let mut drv = Drv2605::new(mock);

            drv.init().await.unwrap();
            assert_eq!(drv.read_register(REG_MODE).await.unwrap(), 0x00);
            assert_eq!(drv.read_register(REG_LIBRARY).await.unwrap(), Library::ErmLibraryE as u8);

            drv.play_effect(47).await.unwrap();
            assert_eq!(drv.read_register(REG_WAVESEQ1).await.unwrap(), 47);
            assert_eq!(drv.read_register(REG_WAVESEQ2).await.unwrap(), 0);
            assert_eq!(drv.read_register(REG_GO).await.unwrap(), 1);
        });
    }
}
