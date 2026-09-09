//! `espielberg` — Host-side driver library for directing the T-Encoder-Pro.
//!
//! Exposes a film-director-themed API:
//! - [`Director`]: The controller holding the connection to the stage.
//! - [`Director::action`]: "Lights, camera, action!" — opens connection to device set.
//! - [`Director::take`]: Captures a frame from the live set.
//! - [`Director::cut`]: "Cut!" — closes connection and releases serial port.
//! - [`Shot`]: The captured frame ($390\times390$ RGB565), with helpers to save raw or PNG.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::time::Duration;

use image::{ImageBuffer, Rgb};
use serialport::SerialPort;
use thiserror::Error;

/// Error type for `espielberg` operations.
#[derive(Debug, Error)]
pub enum EspielbergError {
    /// Serial port communication error.
    #[error("serial port error: {0}")]
    Serial(#[from] serialport::Error),

    /// Standard I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Image encoding or processing error.
    #[error("image error: {0}")]
    Image(#[from] image::ImageError),

    /// Device returned an unexpected or malformed header.
    #[error("invalid header from device: {0}")]
    InvalidHeader(String),

    /// Incomplete payload received from device.
    #[error("incomplete frame: expected {expected} bytes, received {actual} bytes")]
    IncompletePayload { expected: usize, actual: usize },
}

/// A captured frame from the T-Encoder-Pro screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shot {
    width: u32,
    height: u32,
    raw_rgb565: Vec<u8>,
}

impl Shot {
    /// Creates a new shot with given dimensions and raw RGB565 bytes.
    #[must_use]
    pub fn new(width: u32, height: u32, raw_rgb565: Vec<u8>) -> Self {
        Self {
            width,
            height,
            raw_rgb565,
        }
    }

    /// Frame width in pixels.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Frame height in pixels.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// Raw big-endian RGB565 pixel bytes.
    #[must_use]
    pub fn raw(&self) -> &[u8] {
        &self.raw_rgb565
    }

    /// Saves the raw uncompressed RGB565 framebuffer bytes to disk.
    ///
    /// # Errors
    /// Returns an I/O error if writing fails.
    pub fn save_raw(&self, path: impl AsRef<Path>) -> std::io::Result<()> {
        std::fs::write(path, &self.raw_rgb565)
    }

    /// Converts the big-endian RGB565 payload to an 8-bit RGB image buffer.
    #[must_use]
    pub fn to_rgb_image(&self) -> ImageBuffer<Rgb<u8>, Vec<u8>> {
        let mut img = ImageBuffer::new(self.width, self.height);

        for (chunk, pixel) in self.raw_rgb565.chunks_exact(2).zip(img.pixels_mut()) {
            let b0 = chunk[0];
            let b1 = chunk[1];
            let raw16 = u16::from_be_bytes([b0, b1]);

            // RGB565: 5 bits Red, 6 bits Green, 5 bits Blue
            let r5 = ((raw16 >> 11) & 0x1F) as u8;
            let g6 = ((raw16 >> 5) & 0x3F) as u8;
            let b5 = (raw16 & 0x1F) as u8;

            // Expand to 8-bit color depth
            let r8 = (r5 << 3) | (r5 >> 2);
            let g8 = (g6 << 2) | (g6 >> 4);
            let b8 = (b5 << 3) | (b5 >> 2);

            *pixel = Rgb([r8, g8, b8]);
        }

        img
    }

    /// Converts the shot to standard PNG image bytes in memory.
    ///
    /// # Errors
    /// Returns an error if PNG encoding fails.
    pub fn to_png_bytes(&self) -> Result<Vec<u8>, EspielbergError> {
        let img = self.to_rgb_image();
        let mut bytes = Vec::new();
        let mut cursor = std::io::Cursor::new(&mut bytes);
        img.write_to(&mut cursor, image::ImageFormat::Png)?;
        Ok(bytes)
    }

    /// Saves the shot as a PNG image to disk.
    ///
    /// # Errors
    /// Returns an error if encoding or disk write fails.
    pub fn save_png(&self, path: impl AsRef<Path>) -> Result<(), EspielbergError> {
        let img = self.to_rgb_image();
        img.save(path)?;
        Ok(())
    }
}

/// The director holding the active session on the device set.
pub struct Director {
    port: Box<dyn SerialPort>,
}

impl Director {
    /// "Lights, camera, action!" — opens connection to the set on `port_name`.
    ///
    /// # Errors
    /// Returns an error if the serial port cannot be opened.
    pub fn action(port_name: &str) -> Result<Self, EspielbergError> {
        let mut port = serialport::new(port_name, 115_200)
            .timeout(Duration::from_millis(4000))
            .open()?;

        // Send a wake-up newline to trigger wait_for_connection if needed
        let _ = port.write_all(b"\r\n");
        let _ = port.flush();
        std::thread::sleep(Duration::from_millis(100));

        Ok(Self { port })
    }

    /// Captures a live frame from the set ("Take!").
    ///
    /// # Errors
    /// Returns an error if communication fails, the header is invalid, or the payload is incomplete.
    pub fn take(&mut self) -> Result<Shot, EspielbergError> {
        // Clear any stale buffered bytes
        let mut discard = [0u8; 1024];
        while let Ok(n) = self.port.read(&mut discard) {
            if n == 0 {
                break;
            }
        }

        // Send screenshot command
        self.port.write_all(b"screenshot\r\n")?;
        self.port.flush()?;

        let mut reader = BufReader::new(&mut self.port);
        let mut header_line = String::new();

        // Read until we find the "SCREENSHOT <width> <height> <len>" header
        loop {
            header_line.clear();
            let bytes_read = reader.read_line(&mut header_line)?;
            if bytes_read == 0 {
                return Err(EspielbergError::InvalidHeader(
                    "EOF reached while waiting for SCREENSHOT header".to_string(),
                ));
            }
            let trimmed = header_line.trim();
            if trimmed.starts_with("SCREENSHOT") {
                break;
            }
        }

        // Parse header fields: "SCREENSHOT 390 390 304200"
        let parts: Vec<&str> = header_line.split_whitespace().collect();
        if parts.len() < 4 {
            return Err(EspielbergError::InvalidHeader(format!(
                "malformed header: '{header_line}'"
            )));
        }

        let width: u32 = parts[1].parse().map_err(|_| {
            EspielbergError::InvalidHeader(format!("invalid width: '{}'", parts[1]))
        })?;
        let height: u32 = parts[2].parse().map_err(|_| {
            EspielbergError::InvalidHeader(format!("invalid height: '{}'", parts[2]))
        })?;
        let expected_bytes: usize = parts[3].parse().map_err(|_| {
            EspielbergError::InvalidHeader(format!("invalid length: '{}'", parts[3]))
        })?;

        // Read exact payload bytes
        let mut raw_bytes = vec![0u8; expected_bytes];
        reader.read_exact(&mut raw_bytes)?;

        Ok(Shot::new(width, height, raw_bytes))
    }

    /// "Cut!" — ends the shoot, closing the serial connection and freeing the port.
    ///
    /// # Errors
    /// Returns an error if flushing fails.
    pub fn cut(mut self) -> Result<(), EspielbergError> {
        let _ = self.port.flush();
        drop(self.port);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shot_converts_rgb565_to_rgb888_correctly() {
        // Red: 0xF800, Green: 0x07E0, Blue: 0x001F, White: 0xFFFF, Black: 0x0000
        let red_be = 0xF800u16.to_be_bytes();
        let green_be = 0x07E0u16.to_be_bytes();
        let blue_be = 0x001Fu16.to_be_bytes();
        let white_be = 0xFFFFu16.to_be_bytes();

        let raw = vec![
            red_be[0],
            red_be[1],
            green_be[0],
            green_be[1],
            blue_be[0],
            blue_be[1],
            white_be[0],
            white_be[1],
        ];

        let shot = Shot::new(2, 2, raw);
        let img = shot.to_rgb_image();

        assert_eq!(img.width(), 2);
        assert_eq!(img.height(), 2);

        // Pixel (0, 0): Pure Red (255, 0, 0)
        let p_red = img.get_pixel(0, 0);
        assert_eq!(p_red[0], 255);
        assert_eq!(p_red[1], 0);
        assert_eq!(p_red[2], 0);

        // Pixel (1, 0): Pure Green (0, 255, 0)
        let p_green = img.get_pixel(1, 0);
        assert_eq!(p_green[0], 0);
        assert_eq!(p_green[1], 255);
        assert_eq!(p_green[2], 0);

        // Pixel (0, 1): Pure Blue (0, 0, 255)
        let p_blue = img.get_pixel(0, 1);
        assert_eq!(p_blue[0], 0);
        assert_eq!(p_blue[1], 0);
        assert_eq!(p_blue[2], 255);

        // Pixel (1, 1): Pure White (255, 255, 255)
        let p_white = img.get_pixel(1, 1);
        assert_eq!(p_white[0], 255);
        assert_eq!(p_white[1], 255);
        assert_eq!(p_white[2], 255);
    }

    #[test]
    fn shot_encodes_png_bytes() {
        let raw = vec![0u8; 390 * 390 * 2];
        let shot = Shot::new(390, 390, raw);
        let png = shot.to_png_bytes().expect("PNG encoding succeeds");

        // Check PNG signature: 0x89 0x50 0x4E 0x47 0x0D 0x0A 0x1A 0x0A
        assert_eq!(&png[0..8], &[137, 80, 78, 71, 13, 10, 26, 10]);
    }
}
