// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

#![no_std]

extern crate alloc;

pub mod codec;
pub mod coprocessor_generated;

pub use codec::{CodecError, FrameAccumulator, decode_packet, encode_packet};
pub use coprocessor_generated::coprocessor_proto as proto;
pub use flatbuffers;

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
