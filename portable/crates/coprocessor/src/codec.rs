// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

extern crate alloc;

use alloc::vec::Vec;

/// Errors that can occur during packet decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodecError {
    /// Frame had no delimiter or was completely empty.
    EmptyFrame,
    /// COBS byte decoding failed (corrupted stuffing bytes).
    CobsDecodeFailed,
    /// Decoded frame is shorter than the 8-byte header ([CRC32: 4B][SizePrefix: 4B]).
    PacketTooShort,
    /// CRC32 checksum mismatch.
    CrcMismatch { expected: u32, actual: u32 },
    /// Size prefix does not match the actual FlatBuffers payload length.
    LengthMismatch { expected: usize, actual: usize },
    /// Frame exceeded the maximum allowed buffer size.
    BufferOverflow,
}

/// Minimum length of an unencoded packet: 4 bytes CRC32 + 4 bytes SizePrefix.
pub const MIN_UNENCODED_HEADER_LEN: usize = 8;

/// Encodes a size-prefixed FlatBuffer payload into a framed, COBS-encoded wire packet.
///
/// Output format: `[ COBS( [CRC32: 4B LE] [size_prefixed_data] ) ] [ 0x00 ]`
pub fn encode_packet(size_prefixed_flatbuffer: &[u8]) -> Vec<u8> {
    let crc = crc32fast::hash(size_prefixed_flatbuffer);
    let mut unencoded = Vec::with_capacity(4 + size_prefixed_flatbuffer.len());
    unencoded.extend_from_slice(&crc.to_le_bytes());
    unencoded.extend_from_slice(size_prefixed_flatbuffer);

    let mut encoded = cobs::encode_vec(&unencoded);
    encoded.push(0x00);
    encoded
}

/// Decodes a COBS-encoded wire frame (with or without trailing 0x00) and validates its CRC32 and size prefix.
///
/// Returns `Ok(size_prefixed_payload)` on success, which includes the 4-byte size prefix
/// ready for FlatBuffers `size_prefixed_root_as_*`.
pub fn decode_packet<'a>(frame: &[u8], decoded_buf: &'a mut [u8]) -> Result<&'a [u8], CodecError> {
    let frame_to_decode = match frame.strip_suffix(&[0x00]) {
        Some(stripped) => stripped,
        None => frame,
    };

    if frame_to_decode.is_empty() {
        return Err(CodecError::EmptyFrame);
    }

    let report =
        cobs::decode(frame_to_decode, decoded_buf).map_err(|_| CodecError::CobsDecodeFailed)?;
    let decoded_len = report.frame_size();

    if decoded_len < MIN_UNENCODED_HEADER_LEN {
        return Err(CodecError::PacketTooShort);
    }

    let expected_crc =
        u32::from_le_bytes(decoded_buf[0..4].try_into().map_err(|_| CodecError::PacketTooShort)?);

    let payload = &decoded_buf[4..decoded_len];
    let actual_crc = crc32fast::hash(payload);
    if expected_crc != actual_crc {
        return Err(CodecError::CrcMismatch { expected: expected_crc, actual: actual_crc });
    }

    let expected_size =
        u32::from_le_bytes(payload[0..4].try_into().map_err(|_| CodecError::PacketTooShort)?)
            as usize;

    let actual_size = payload.len() - 4;
    if expected_size != actual_size {
        return Err(CodecError::LengthMismatch { expected: expected_size, actual: actual_size });
    }

    Ok(payload)
}

/// A streaming byte accumulator that collects incoming UART bytes and yields
/// decoded packets as 0x00 delimiters are encountered.
pub struct FrameAccumulator<const CAPACITY: usize> {
    buffer: [u8; CAPACITY],
    write_pos: usize,
    decode_scratch: [u8; CAPACITY],
}

impl<const CAPACITY: usize> Default for FrameAccumulator<CAPACITY> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const CAPACITY: usize> FrameAccumulator<CAPACITY> {
    pub const fn new() -> Self {
        Self { buffer: [0u8; CAPACITY], write_pos: 0, decode_scratch: [0u8; CAPACITY] }
    }

    /// Resets the accumulator state, discarding any partially accumulated frame.
    pub fn reset(&mut self) {
        self.write_pos = 0;
    }

    /// Pushes a single byte from the UART stream.
    ///
    /// If the byte is `0x00`, attempts to decode the accumulated frame.
    /// Returns `Some(Ok(payload))` if a valid packet was finished,
    /// `Some(Err(e))` if a delimiter was hit but decoding/CRC failed,
    /// or `None` if more bytes are needed.
    pub fn push_byte(&mut self, byte: u8) -> Option<Result<&[u8], CodecError>> {
        if byte == 0x00 {
            if self.write_pos == 0 {
                // Empty sentinel delimiter, ignore
                return None;
            }
            let frame = &self.buffer[..self.write_pos];
            let result = decode_packet(frame, &mut self.decode_scratch);
            self.write_pos = 0;
            return Some(result);
        }

        if self.write_pos >= CAPACITY {
            // Buffer overflow, drop current accumulator and resynchronize on next 0x00
            self.write_pos = 0;
            return Some(Err(CodecError::BufferOverflow));
        }

        self.buffer[self.write_pos] = byte;
        self.write_pos += 1;
        None
    }
}
