// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

#![no_std]

extern crate alloc;

pub mod codec;
#[allow(clippy::all)]
pub mod coprocessor_generated;

pub use codec::{CodecError, FrameAccumulator, decode_packet, encode_packet};
pub use coprocessor_generated::coprocessor_proto as proto;
pub use flatbuffers;

/// Battery telemetry status.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct BatteryStatus {
    pub millivolts: u32,
    pub percent: u8,
    pub is_plugged: bool,
}

impl BatteryStatus {
    #[must_use]
    pub const fn new(millivolts: u32, percent: u8, is_plugged: bool) -> Self {
        Self { millivolts, percent, is_plugged }
    }
}

impl From<BatteryStatus> for proto::BatteryStatusArgs {
    fn from(b: BatteryStatus) -> Self {
        proto::BatteryStatusArgs {
            millivolts: b.millivolts,
            percent: b.percent,
            is_plugged: b.is_plugged,
        }
    }
}

impl From<proto::BatteryStatus<'_>> for BatteryStatus {
    fn from(b: proto::BatteryStatus<'_>) -> Self {
        Self { millivolts: b.millivolts(), percent: b.percent(), is_plugged: b.is_plugged() }
    }
}

/// Parses a size-prefixed FlatBuffer ResponseEnvelope payload.
pub fn parse_response_envelope(
    payload: &[u8],
) -> Result<proto::ResponseEnvelope<'_>, flatbuffers::InvalidFlatbuffer> {
    flatbuffers::size_prefixed_root::<proto::ResponseEnvelope>(payload)
}

/// Parses a size-prefixed FlatBuffer RequestEnvelope payload.
pub fn parse_request_envelope(
    payload: &[u8],
) -> Result<proto::RequestEnvelope<'_>, flatbuffers::InvalidFlatbuffer> {
    flatbuffers::size_prefixed_root::<proto::RequestEnvelope>(payload)
}

#[cfg(test)]
mod tests;
