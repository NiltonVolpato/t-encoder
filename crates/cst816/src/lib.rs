// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

//! Hardware driver for the CST816 family of capacitive touch controllers.

#![no_std]

use embedded_hal_async::i2c::I2c;

/// Standard 7-bit I2C slave address for the CST816.
pub const DEFAULT_I2C_ADDRESS: u8 = 0x15;

/// Gesture detected by the CST816 controller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Gesture {
    None,
    SlideDown,
    SlideUp,
    SlideLeft,
    SlideRight,
    SingleClick,
    DoubleClick,
    LongPress,
    Unknown(u8),
}

impl From<u8> for Gesture {
    fn from(value: u8) -> Self {
        match value {
            0x00 => Self::None,
            0x01 => Self::SlideDown,
            0x02 => Self::SlideUp,
            0x03 => Self::SlideLeft,
            0x04 => Self::SlideRight,
            0x05 => Self::SingleClick,
            0x0B => Self::DoubleClick,
            0x0C => Self::LongPress,
            other => Self::Unknown(other),
        }
    }
}

/// Physical contact state of the touch point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum TouchState {
    Down,
    Up,
    Contact,
}

/// Decoded touch report from the CST816.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct TouchPoint {
    pub x: u16,
    pub y: u16,
    pub gesture: Gesture,
    pub state: TouchState,
}

/// Parses a 7-byte raw report read from register `0x00`.
pub fn parse_touch_report(buf: &[u8; 7]) -> Option<TouchPoint> {
    let gesture = Gesture::from(buf[0]);
    let points = buf[2];

    if points == 0 && gesture == Gesture::None {
        return None;
    }

    let state = match buf[3] >> 6 {
        0x00 => TouchState::Down,
        0x01 => TouchState::Up,
        _ => TouchState::Contact,
    };

    let x = (((buf[3] & 0x0F) as u16) << 8) | (buf[4] as u16);
    let y = (((buf[5] & 0x0F) as u16) << 8) | (buf[6] as u16);

    Some(TouchPoint { x, y, gesture, state })
}

/// CST816 touch driver over async I2C.
pub struct Cst816<I2C> {
    i2c: I2C,
    address: u8,
}

impl<I2C> Cst816<I2C> {
    /// Creates a new driver instance using the default I2C address (`0x15`).
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

impl<I2C: I2c> Cst816<I2C> {
    /// Reads and parses the current touch report.
    pub async fn read_touch(&mut self) -> Result<Option<TouchPoint>, I2C::Error> {
        let mut buf = [0u8; 7];
        self.i2c.write_read(self.address, &[0x00], &mut buf).await?;
        Ok(parse_touch_report(&buf))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_no_touch() {
        let buf = [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        assert_eq!(parse_touch_report(&buf), None);
    }

    #[test]
    fn test_parse_single_touch_down() {
        // Gesture: None (0x00), 1 finger, 1 point, state Down (0x00 << 6), x=180, y=240
        let buf = [0x00, 0x01, 0x01, 0x00, 180, 0x00, 240];
        let report = parse_touch_report(&buf).expect("should parse report");
        assert_eq!(report.x, 180);
        assert_eq!(report.y, 240);
        assert_eq!(report.gesture, Gesture::None);
        assert_eq!(report.state, TouchState::Down);
    }

    #[test]
    fn test_parse_gesture_swipe_right() {
        // Gesture: SlideRight (0x04)
        let buf = [0x04, 0x01, 0x01, 0x80, 100, 0x00, 200];
        let report = parse_touch_report(&buf).expect("should parse report");
        assert_eq!(report.gesture, Gesture::SlideRight);
        assert_eq!(report.state, TouchState::Contact);
    }

    #[test]
    fn test_high_coordinate_bits() {
        // x = 359 = 0x0167, y = 359 = 0x0167, state Contact (0x80)
        let buf = [0x00, 0x01, 0x01, 0x81, 0x67, 0x01, 0x67];
        let report = parse_touch_report(&buf).expect("should parse report");
        assert_eq!(report.x, 359);
        assert_eq!(report.y, 359);
        assert_eq!(report.state, TouchState::Contact);
    }
}
