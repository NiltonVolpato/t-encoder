//! `espielberg` — Host-side driver library for directing the T-Encoder-Pro.
//!
//! Exposes a film-director-themed API:
//! - [`Director`]: The controller holding the connection to the stage.
//! - [`Director::action`]: "Lights, camera, action!" — opens connection to device set.
//! - [`Director::take`]: Captures a frame from the live set.
//! - [`Director::cut`]: "Cut!" — closes connection and releases serial port.
//! - [`Shot`]: The captured frame ($390\times390$ RGB565), with helpers to save raw or PNG.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::time::Duration;

use base64::prelude::*;
use image::{ImageBuffer, Rgb};
pub use protocol::{
    CommandResponse, CueCommand, DeviceEvent, DeviceMessage, IncomingCommand, LogRecord,
};
use rmcp::schemars;
use rmcp::schemars::JsonSchema;
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

    /// JSON serialization or deserialization error.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// Base64 decoding error.
    #[error("base64 decode error: {0}")]
    Base64(#[from] base64::DecodeError),

    /// Image encoding or processing error.
    #[error("image error: {0}")]
    Image(#[from] image::ImageError),

    /// Command execution failure returned by device.
    #[error("command failed: {0}")]
    CommandFailed(String),

    /// Operation timed out.
    #[error("timed out: {0}")]
    TimedOut(String),

    /// Invalid screenshot or compressed data payload.
    #[error("invalid payload: {0}")]
    InvalidPayload(String),
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

/// Swipe direction for touch gesture cues.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SwipeDirection {
    /// Swipe right-to-left across the panel (back).
    Left,
    /// Swipe left-to-right across the panel.
    Right,
    /// Swipe bottom-to-top across the panel (exit).
    Up,
    /// Swipe top-to-bottom across the panel.
    Down,
}

impl SwipeDirection {
    /// Returns the lowercase string identifier for the direction.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Up => "up",
            Self::Down => "down",
        }
    }
}

/// A cue given by the director to the device set (event injection).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cue {
    /// Rotate dial by delta detents (+1 CW, -1 CCW).
    Rotate(i32),
    /// Dial button short press.
    ShortPress,
    /// Dial button long press.
    LongPress,
    /// Touch tap at screen coordinates (x, y).
    Tap { x: i32, y: i32 },
    /// Touch swipe across the panel.
    Swipe(SwipeDirection),
}

impl From<Cue> for CueCommand {
    fn from(cue: Cue) -> Self {
        match cue {
            Cue::Rotate(delta) => CueCommand::Rotate { delta },
            Cue::ShortPress => CueCommand::Press,
            Cue::LongPress => CueCommand::LongPress,
            Cue::Tap { x, y } => CueCommand::Tap { x, y },
            Cue::Swipe(direction) => CueCommand::Swipe {
                direction: match direction {
                    SwipeDirection::Left => protocol::SwipeDirection::Left,
                    SwipeDirection::Right => protocol::SwipeDirection::Right,
                    SwipeDirection::Up => protocol::SwipeDirection::Up,
                    SwipeDirection::Down => protocol::SwipeDirection::Down,
                },
            },
        }
    }
}

/// Decompresses TGA-style run-length encoded 16-bit pixels into raw bytes.
///
/// # Errors
/// Returns an error if the compressed stream is truncated or the decompressed
/// size does not match `expected_bytes`.
pub fn decompress_tga_rle(
    compressed: &[u8],
    expected_bytes: usize,
) -> Result<Vec<u8>, EspielbergError> {
    let mut raw = Vec::with_capacity(expected_bytes);
    let mut i = 0;
    while i < compressed.len() {
        let hdr = compressed.get(i).copied().unwrap_or(0);
        i = i.saturating_add(1);
        if hdr & 0x80 != 0 {
            // Run packet: (hdr & 0x7F) + 1 repeats of 2-byte pixel
            let count = usize::from(hdr & 0x7F).saturating_add(1);
            let px_end = i.saturating_add(2);
            let Some(px) = compressed.get(i..px_end) else {
                return Err(EspielbergError::InvalidPayload(
                    "truncated TGA run packet".to_string(),
                ));
            };
            i = px_end;
            for _ in 0..count {
                raw.extend_from_slice(px);
            }
        } else {
            // Literal packet: hdr + 1 uncompressed 2-byte pixels follow
            let count = usize::from(hdr).saturating_add(1);
            let bytes_len = count.saturating_mul(2);
            let lit_end = i.saturating_add(bytes_len);
            let Some(lit) = compressed.get(i..lit_end) else {
                return Err(EspielbergError::InvalidPayload(
                    "truncated TGA literal packet".to_string(),
                ));
            };
            raw.extend_from_slice(lit);
            i = lit_end;
        }
    }
    if raw.len() != expected_bytes {
        return Err(EspielbergError::InvalidPayload(format!(
            "decompressed size mismatch: expected {expected_bytes}, got {}",
            raw.len()
        )));
    }
    Ok(raw)
}

impl From<SwipeDirection> for protocol::SwipeDirection {
    fn from(d: SwipeDirection) -> Self {
        match d {
            SwipeDirection::Left => protocol::SwipeDirection::Left,
            SwipeDirection::Right => protocol::SwipeDirection::Right,
            SwipeDirection::Up => protocol::SwipeDirection::Up,
            SwipeDirection::Down => protocol::SwipeDirection::Down,
        }
    }
}

impl From<protocol::SwipeDirection> for SwipeDirection {
    fn from(d: protocol::SwipeDirection) -> Self {
        match d {
            protocol::SwipeDirection::Left => SwipeDirection::Left,
            protocol::SwipeDirection::Right => SwipeDirection::Right,
            protocol::SwipeDirection::Up => SwipeDirection::Up,
            protocol::SwipeDirection::Down => SwipeDirection::Down,
        }
    }
}

/// The director holding the active session on the device set.
pub struct Director {
    port_name: String,
    writer: Option<Box<dyn SerialPort>>,
    reader: Option<BufReader<Box<dyn SerialPort>>>,
    buffered_events: VecDeque<DeviceEvent>,
}

impl Director {
    fn writer_mut(&mut self) -> Result<&mut (dyn SerialPort + 'static), EspielbergError> {
        match self.writer.as_deref_mut() {
            Some(w) => Ok(w),
            None => Err(EspielbergError::Serial(serialport::Error::new(
                serialport::ErrorKind::NoDevice,
                "serial port is closed",
            ))),
        }
    }

    /// Reads the next NDJSON message from the device, transparently logging any LogRecord.
    fn read_message(&mut self) -> Result<DeviceMessage, EspielbergError> {
        let reader = self.reader.as_mut().ok_or_else(|| {
            EspielbergError::Serial(serialport::Error::new(
                serialport::ErrorKind::NoDevice,
                "serial port is closed",
            ))
        })?;

        let mut line = String::new();
        loop {
            line.clear();
            let bytes_read = reader.read_line(&mut line)?;
            if bytes_read == 0 {
                return Err(EspielbergError::TimedOut(
                    "EOF reading serial stream".to_string(),
                ));
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            match serde_json::from_str::<DeviceMessage>(trimmed) {
                Ok(DeviceMessage::Log(log)) => {
                    tracing::info!(target: "device", "[{}] {}: {}", log.level, log.target, log.msg);
                    continue;
                }
                Ok(msg) => return Ok(msg),
                Err(err) => {
                    tracing::debug!("ignoring non-json line: '{trimmed}' ({err})");
                    continue;
                }
            }
        }
    }

    /// "Lights, camera, action!" — opens connection to the set on `port_name`.
    ///
    /// # Errors
    /// Returns an error if the serial port cannot be opened.
    pub fn action(port_name: &str) -> Result<Self, EspielbergError> {
        let port = serialport::new(port_name, 115_200)
            .timeout(Duration::from_millis(4000))
            .open()?;

        let reader_port = port.try_clone()?;
        Ok(Self {
            port_name: port_name.to_string(),
            writer: Some(port),
            reader: Some(BufReader::new(reader_port)),
            buffered_events: VecDeque::new(),
        })
    }

    /// Captures a live frame from the set ("Take!").
    ///
    /// # Errors
    /// Returns an error if communication fails or decoding fails.
    pub fn take(&mut self) -> Result<Shot, EspielbergError> {
        let cmd = IncomingCommand::Screenshot;
        let mut line = serde_json::to_string(&cmd)?;
        line.push('\n');

        let writer = self.writer_mut()?;
        writer.write_all(line.as_bytes())?;
        writer.flush()?;

        loop {
            match self.read_message()? {
                DeviceMessage::Screenshot(shot_msg) => {
                    let compressed = BASE64_STANDARD.decode(&shot_msg.data)?;
                    let expected_bytes = usize::try_from(shot_msg.width)
                        .unwrap_or(0)
                        .saturating_mul(usize::try_from(shot_msg.height).unwrap_or(0))
                        .saturating_mul(2);
                    let raw_bytes = decompress_tga_rle(&compressed, expected_bytes)?;
                    return Ok(Shot::new(shot_msg.width, shot_msg.height, raw_bytes));
                }
                DeviceMessage::Response(resp) if !resp.ok => {
                    return Err(EspielbergError::CommandFailed(
                        resp.error
                            .unwrap_or_else(|| "screenshot failed".to_string()),
                    ));
                }
                DeviceMessage::Event(ev) => {
                    self.buffered_events.push_back(ev);
                }
                _ => {}
            }
        }
    }

    /// Delivers a cue to the stage (event injection).
    ///
    /// # Errors
    /// Returns an error if writing to the serial port fails.
    pub fn cue(&mut self, cue: Cue) -> Result<(), EspielbergError> {
        let cmd = IncomingCommand::Cue { cue: cue.into() };
        let mut line = serde_json::to_string(&cmd)?;
        line.push('\n');

        let writer = self.writer_mut()?;
        writer.write_all(line.as_bytes())?;
        writer.flush()?;

        loop {
            match self.read_message()? {
                DeviceMessage::Response(resp) => {
                    if resp.ok {
                        return Ok(());
                    }
                    return Err(EspielbergError::CommandFailed(
                        resp.error.unwrap_or_else(|| "command failed".to_string()),
                    ));
                }
                DeviceMessage::Event(ev) => {
                    self.buffered_events.push_back(ev);
                }
                _ => {}
            }
        }
    }

    /// Waits for an event emitted by the device set within `timeout`.
    ///
    /// # Errors
    /// Returns an error if the timeout elapses or serial I/O fails.
    pub fn wait_for_event(&mut self, timeout: Duration) -> Result<DeviceEvent, EspielbergError> {
        if let Some(ev) = self.buffered_events.pop_front() {
            return Ok(ev);
        }

        let start = std::time::Instant::now();
        while start.elapsed() < timeout {
            if let DeviceMessage::Event(ev) = self.read_message()? {
                return Ok(ev);
            }
        }
        Err(EspielbergError::TimedOut(
            "timed out waiting for event".to_string(),
        ))
    }

    /// Convenience helper to cue a rotary dial turn.
    ///
    /// # Errors
    /// Returns an error if writing to the serial port fails.
    pub fn rotate(&mut self, delta: i32) -> Result<(), EspielbergError> {
        self.cue(Cue::Rotate(delta))
    }

    /// Convenience helper to cue a button short press.
    ///
    /// # Errors
    /// Returns an error if writing to the serial port fails.
    pub fn press(&mut self) -> Result<(), EspielbergError> {
        self.cue(Cue::ShortPress)
    }

    /// Convenience helper to cue a button long press.
    ///
    /// # Errors
    /// Returns an error if writing to the serial port fails.
    pub fn long_press(&mut self) -> Result<(), EspielbergError> {
        self.cue(Cue::LongPress)
    }

    /// Convenience helper to cue a touch tap.
    ///
    /// # Errors
    /// Returns an error if writing to the serial port fails.
    pub fn tap(&mut self, x: i32, y: i32) -> Result<(), EspielbergError> {
        self.cue(Cue::Tap { x, y })
    }

    /// Convenience helper to cue a touch swipe.
    ///
    /// # Errors
    /// Returns an error if writing to the serial port fails.
    pub fn swipe(&mut self, direction: SwipeDirection) -> Result<(), EspielbergError> {
        self.cue(Cue::Swipe(direction))
    }

    /// Reboots the device set via software reset, waits for reboot, and reconnects.
    ///
    /// # Errors
    /// Returns an error if writing to or reopening the serial port fails.
    pub fn reset(&mut self) -> Result<(), EspielbergError> {
        if let Some(mut w) = self.writer.take() {
            let cmd = IncomingCommand::Reset;
            let mut line = serde_json::to_string(&cmd).unwrap_or_default();
            line.push('\n');
            let _ = w.write_all(line.as_bytes());
            let _ = w.flush();
            drop(w);
        }
        self.reader.take();
        self.buffered_events.clear();

        // Wait for device to reboot and USB-Serial-JTAG to re-enumerate
        let start = std::time::Instant::now();
        let timeout = Duration::from_secs(5);
        std::thread::sleep(Duration::from_millis(500));

        let port = loop {
            match serialport::new(&self.port_name, 115_200)
                .timeout(Duration::from_millis(4000))
                .open()
            {
                Ok(p) => break p,
                Err(_e) if start.elapsed() < timeout => {
                    std::thread::sleep(Duration::from_millis(150));
                }
                Err(e) => return Err(e.into()),
            }
        };

        let reader_port = port.try_clone()?;
        self.writer = Some(port);
        self.reader = Some(BufReader::new(reader_port));

        // Wait for Boot ready event
        let boot_timeout = Duration::from_secs(6);
        let boot_start = std::time::Instant::now();
        while boot_start.elapsed() < boot_timeout {
            if let Ok(DeviceEvent::Boot { ready: true }) =
                self.wait_for_event(Duration::from_millis(500))
            {
                return Ok(());
            }
        }

        Ok(())
    }

    /// "Cut!" — ends the shoot, closing the serial connection and freeing the port.
    ///
    /// # Errors
    /// Returns an error if flushing fails.
    pub fn cut(mut self) -> Result<(), EspielbergError> {
        if let Some(mut w) = self.writer.take() {
            let _ = w.flush();
        }
        self.reader.take();
        self.buffered_events.clear();
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

    #[test]
    fn test_decompress_tga_rle() {
        // Test Run packet: 3 repeats of pixel [0x12, 0x34]
        // Header: 0x80 | (3 - 1) = 0x82
        let compressed = [0x82, 0x12, 0x34];
        let decomp = decompress_tga_rle(&compressed, 6).expect("decompress run");
        assert_eq!(decomp, &[0x12, 0x34, 0x12, 0x34, 0x12, 0x34]);

        // Test Literal packet: 2 uncompressed pixels [0xAA, 0xBB] and [0xCC, 0xDD]
        // Header: (2 - 1) = 0x01
        let compressed_lit = [0x01, 0xAA, 0xBB, 0xCC, 0xDD];
        let decomp_lit = decompress_tga_rle(&compressed_lit, 4).expect("decompress literal");
        assert_eq!(decomp_lit, &[0xAA, 0xBB, 0xCC, 0xDD]);

        // Test Mixed packets: 2 repeats of [0x11, 0x22], then 1 literal [0x33, 0x44]
        let mixed = [0x81, 0x11, 0x22, 0x00, 0x33, 0x44];
        let decomp_mixed = decompress_tga_rle(&mixed, 6).expect("decompress mixed");
        assert_eq!(decomp_mixed, &[0x11, 0x22, 0x11, 0x22, 0x33, 0x44]);
    }
}
