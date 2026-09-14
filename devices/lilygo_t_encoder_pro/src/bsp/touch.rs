// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

//! CHSC5816 capacitive touch controller on I2C for LilyGO T-Encoder Pro.

use esp_hal::Blocking;
use esp_hal::delay::Delay;
use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull};
use esp_hal::i2c::master::I2c;
use esp_hal::peripherals::{GPIO8, GPIO9};

const TOUCH_ADDRESS: u8 = 0x2E;
const REG_POINT: u32 = 0x2000_002C;
const REG_BOOT_STATE: u32 = 0x2000_0018;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TouchEvent {
    Down,
    Move,
    Up,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TouchPoint {
    pub x: u16,
    pub y: u16,
    pub event: TouchEvent,
}

pub struct Chsc5816 {
    i2c: I2c<'static, Blocking>,
    _int: Input<'static>,
    rst: Output<'static>,
}

impl Chsc5816 {
    pub fn new(
        i2c: I2c<'static, Blocking>,
        int_pin: GPIO9<'static>,
        rst_pin: GPIO8<'static>,
    ) -> Self {
        let int = Input::new(int_pin, InputConfig::default().with_pull(Pull::Up));
        let rst = Output::new(rst_pin, Level::High, OutputConfig::default());
        Self {
            i2c,
            _int: int,
            rst,
        }
    }

    pub fn init(&mut self, delay: &mut Delay) -> Result<(), esp_hal::i2c::master::Error> {
        self.pulse_reset(delay);
        let _ = self.i2c.write(TOUCH_ADDRESS, &[
            (REG_BOOT_STATE >> 24) as u8,
            (REG_BOOT_STATE >> 16) as u8,
            (REG_BOOT_STATE >> 8) as u8,
            REG_BOOT_STATE as u8,
            0, 0, 0, 0,
        ]);
        self.pulse_reset(delay);
        delay.delay_millis(100);
        self.pulse_reset(delay);
        delay.delay_millis(50);
        Ok(())
    }

    fn pulse_reset(&mut self, delay: &mut Delay) {
        self.rst.set_low();
        delay.delay_millis(5);
        self.rst.set_high();
        delay.delay_millis(10);
    }

    /// Reads the current touch point from the controller.
    pub fn read(&mut self) -> Result<Option<TouchPoint>, esp_hal::i2c::master::Error> {
        let mut buf = [0u8; 8];
        let addr_bytes = REG_POINT.to_be_bytes();
        self.i2c.write(TOUCH_ADDRESS, &addr_bytes)?;
        self.i2c.read(TOUCH_ADDRESS, &mut buf)?;

        let [_status, fingers, x_l8, y_l8, _z, hi, id_event, _p2] = buf;
        let event = match (id_event >> 4) & 0x0F {
            0 => TouchEvent::Down,
            8 => TouchEvent::Move,
            4 => TouchEvent::Up,
            _ => return Ok(None),
        };

        if fingers == 0 && event != TouchEvent::Up {
            return Ok(None);
        }

        let x = (u16::from(hi & 0x0F) << 8) | u16::from(x_l8);
        let y = (u16::from(hi >> 4) << 8) | u16::from(y_l8);

        Ok(Some(TouchPoint { x, y, event }))
    }
}
