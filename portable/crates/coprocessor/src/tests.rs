// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

use alloc::vec;

use super::*;

#[test]
fn test_roundtrip_request_heartbeat() {
    let mut builder = flatbuffers::FlatBufferBuilder::new();
    let heartbeat = proto::Heartbeat::create(
        &mut builder,
        &proto::HeartbeatArgs { uptime_ms: 123456789, heap_free: 250000 },
    );
    let envelope = proto::RequestEnvelope::create(
        &mut builder,
        &proto::RequestEnvelopeArgs {
            message_type: proto::Request::Heartbeat,
            message: Some(heartbeat.as_union_value()),
        },
    );
    builder.finish_size_prefixed(envelope, None);

    let finished_data = builder.finished_data();
    let wire_packet = encode_packet(finished_data);

    assert_eq!(*wire_packet.last().unwrap(), 0x00);

    let mut scratch = [0u8; 1024];
    let decoded = decode_packet(&wire_packet, &mut scratch).expect("Decoding must succeed");

    let parsed = proto::size_prefixed_root_as_request_envelope(decoded)
        .expect("Flatbuffers parse must succeed");
    assert_eq!(parsed.message_type(), proto::Request::Heartbeat);
    let hb = parsed.message_as_heartbeat().expect("Must have heartbeat");
    assert_eq!(hb.uptime_ms(), 123456789);
    assert_eq!(hb.heap_free(), 250000);
}

#[test]
fn test_roundtrip_response_wifi_status() {
    let mut builder = flatbuffers::FlatBufferBuilder::new();
    let ip_str = builder.create_string("192.168.1.105");
    let ssid_str = builder.create_string("SanFrancisco-Guest");
    let wifi_status = proto::WifiStatus::create(
        &mut builder,
        &proto::WifiStatusArgs {
            connected: true,
            ip: Some(ip_str),
            ssid: Some(ssid_str),
            rssi: -58,
        },
    );
    let envelope = proto::ResponseEnvelope::create(
        &mut builder,
        &proto::ResponseEnvelopeArgs {
            message_type: proto::Response::WifiStatus,
            message: Some(wifi_status.as_union_value()),
        },
    );
    builder.finish_size_prefixed(envelope, None);

    let wire_packet = encode_packet(builder.finished_data());
    let mut scratch = [0u8; 1024];
    let decoded = decode_packet(&wire_packet, &mut scratch).expect("Decoding must succeed");

    let parsed = flatbuffers::size_prefixed_root::<proto::ResponseEnvelope>(decoded)
        .expect("Flatbuffers parse must succeed");
    assert_eq!(parsed.message_type(), proto::Response::WifiStatus);
    let status = parsed.message_as_wifi_status().expect("Must have WifiStatus");
    assert!(status.connected());
    assert_eq!(status.ip(), Some("192.168.1.105"));
    assert_eq!(status.ssid(), Some("SanFrancisco-Guest"));
    assert_eq!(status.rssi(), -58);
}

#[test]
fn test_crc_corruption_detection() {
    let mut builder = flatbuffers::FlatBufferBuilder::new();
    let hb = proto::Heartbeat::create(
        &mut builder,
        &proto::HeartbeatArgs { uptime_ms: 42, heap_free: 1000 },
    );
    let envelope = proto::RequestEnvelope::create(
        &mut builder,
        &proto::RequestEnvelopeArgs {
            message_type: proto::Request::Heartbeat,
            message: Some(hb.as_union_value()),
        },
    );
    builder.finish_size_prefixed(envelope, None);

    let mut wire_packet = encode_packet(builder.finished_data());

    // Corrupt one non-delimiter byte in the encoded frame
    if wire_packet[2] == 0xFF {
        wire_packet[2] = 0x01;
    } else {
        wire_packet[2] = 0xFF;
    }

    let mut scratch = [0u8; 1024];
    let result = decode_packet(&wire_packet, &mut scratch);
    assert!(result.is_err(), "Corrupted wire packet must be rejected by COBS or CRC");
}

#[test]
fn test_stream_accumulator_resynchronization() {
    let mut accumulator: FrameAccumulator<1024> = FrameAccumulator::new();

    // 1. Send valid packet 1
    let mut builder = flatbuffers::FlatBufferBuilder::new();
    let hb1 = proto::Heartbeat::create(
        &mut builder,
        &proto::HeartbeatArgs { uptime_ms: 100, heap_free: 5000 },
    );
    let env1 = proto::RequestEnvelope::create(
        &mut builder,
        &proto::RequestEnvelopeArgs {
            message_type: proto::Request::Heartbeat,
            message: Some(hb1.as_union_value()),
        },
    );
    builder.finish_size_prefixed(env1, None);
    let packet1 = encode_packet(builder.finished_data());

    let mut received1 = None;
    for &b in &packet1 {
        if let Some(res) = accumulator.push_byte(b) {
            received1 = Some(res.map(|p| p.to_vec()));
        }
    }
    assert!(received1.is_some() && received1.unwrap().is_ok());

    // 2. Inject noisy garbage bytes followed by 0x00 delimiter
    let garbage = vec![0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0, 0x00];
    let mut garbage_res = None;
    for &b in &garbage {
        if let Some(res) = accumulator.push_byte(b) {
            garbage_res = Some(res.map(|p| p.to_vec()));
        }
    }
    // Must fail decoding gracefully
    assert!(garbage_res.is_some() && garbage_res.unwrap().is_err());

    // 3. Immediately send valid packet 2 — accumulator must recover completely!
    let mut builder2 = flatbuffers::FlatBufferBuilder::new();
    let hb2 = proto::Heartbeat::create(
        &mut builder2,
        &proto::HeartbeatArgs { uptime_ms: 200, heap_free: 6000 },
    );
    let env2 = proto::RequestEnvelope::create(
        &mut builder2,
        &proto::RequestEnvelopeArgs {
            message_type: proto::Request::Heartbeat,
            message: Some(hb2.as_union_value()),
        },
    );
    builder2.finish_size_prefixed(env2, None);
    let packet2 = encode_packet(builder2.finished_data());

    let mut received2 = None;
    for &b in &packet2 {
        if let Some(res) = accumulator.push_byte(b) {
            received2 = Some(res.map(|p| p.to_vec()));
        }
    }
    assert!(
        received2.is_some() && received2.as_ref().unwrap().is_ok(),
        "Accumulator must successfully lock onto the valid frame after garbage"
    );
    let payload = received2.unwrap().unwrap();
    let parsed = proto::size_prefixed_root_as_request_envelope(&payload).unwrap();
    let hb = parsed.message_as_heartbeat().unwrap();
    assert_eq!(hb.uptime_ms(), 200);
    assert_eq!(hb.heap_free(), 6000);
}

#[test]
fn test_roundtrip_provisioning_messages() {
    // 1. Test StartProvisioning request
    let mut builder = flatbuffers::FlatBufferBuilder::new();
    let start_req = proto::StartProvisioning::create(
        &mut builder,
        &proto::StartProvisioningArgs { timeout_seconds: 180 },
    );
    let env = proto::RequestEnvelope::create(
        &mut builder,
        &proto::RequestEnvelopeArgs {
            message_type: proto::Request::StartProvisioning,
            message: Some(start_req.as_union_value()),
        },
    );
    builder.finish_size_prefixed(env, None);
    let packet = encode_packet(builder.finished_data());

    let mut decoded = [0u8; 256];
    let payload = decode_packet(&packet, &mut decoded).unwrap();
    let parsed = proto::size_prefixed_root_as_request_envelope(payload).unwrap();
    assert_eq!(parsed.message_type(), proto::Request::StartProvisioning);
    let start = parsed.message_as_start_provisioning().unwrap();
    assert_eq!(start.timeout_seconds(), 180);

    // 2. Test ProvisioningStatus response
    let mut resp_builder = flatbuffers::FlatBufferBuilder::new();
    let prov_status = proto::ProvisioningStatus::create(
        &mut resp_builder,
        &proto::ProvisioningStatusArgs { state: proto::ProvisioningState::Active },
    );
    let resp_env = proto::ResponseEnvelope::create(
        &mut resp_builder,
        &proto::ResponseEnvelopeArgs {
            message_type: proto::Response::ProvisioningStatus,
            message: Some(prov_status.as_union_value()),
        },
    );
    resp_builder.finish_size_prefixed(resp_env, None);
    let resp_packet = encode_packet(resp_builder.finished_data());

    let mut resp_decoded = [0u8; 256];
    let resp_payload = decode_packet(&resp_packet, &mut resp_decoded).unwrap();
    let parsed_resp = crate::parse_response_envelope(resp_payload).unwrap();
    assert_eq!(parsed_resp.message_type(), proto::Response::ProvisioningStatus);
    let status = parsed_resp.message_as_provisioning_status().unwrap();
    assert_eq!(status.state(), proto::ProvisioningState::Active);
}

#[test]
fn test_roundtrip_hello() {
    // 1. Request::Hello
    let mut builder = flatbuffers::FlatBufferBuilder::new();
    let hello = proto::Hello::create(&mut builder, &proto::HelloArgs {});
    let env = proto::RequestEnvelope::create(
        &mut builder,
        &proto::RequestEnvelopeArgs {
            message_type: proto::Request::Hello,
            message: Some(hello.as_union_value()),
        },
    );
    builder.finish_size_prefixed(env, None);
    let packet = encode_packet(builder.finished_data());

    let mut decoded = [0u8; 128];
    let payload = decode_packet(&packet, &mut decoded).unwrap();
    let parsed = proto::size_prefixed_root_as_request_envelope(payload).unwrap();
    assert_eq!(parsed.message_type(), proto::Request::Hello);
    assert!(parsed.message_as_hello().is_some());

    // 2. Response::Hello
    let mut resp_builder = flatbuffers::FlatBufferBuilder::new();
    let resp_hello = proto::Hello::create(&mut resp_builder, &proto::HelloArgs {});
    let resp_env = proto::ResponseEnvelope::create(
        &mut resp_builder,
        &proto::ResponseEnvelopeArgs {
            message_type: proto::Response::Hello,
            message: Some(resp_hello.as_union_value()),
        },
    );
    resp_builder.finish_size_prefixed(resp_env, None);
    let resp_packet = encode_packet(resp_builder.finished_data());

    let mut resp_decoded = [0u8; 128];
    let resp_payload = decode_packet(&resp_packet, &mut resp_decoded).unwrap();
    let parsed_resp = crate::parse_response_envelope(resp_payload).unwrap();
    assert_eq!(parsed_resp.message_type(), proto::Response::Hello);
    assert!(parsed_resp.message_as_hello().is_some());
}

#[test]
fn test_roundtrip_time_sync() {
    let mut builder = flatbuffers::FlatBufferBuilder::new();
    let time_sync = proto::TimeSync::create(
        &mut builder,
        &proto::TimeSyncArgs { epoch_seconds: 1790596867, subsec_micros: 123456 },
    );
    let env = proto::ResponseEnvelope::create(
        &mut builder,
        &proto::ResponseEnvelopeArgs {
            message_type: proto::Response::TimeSync,
            message: Some(time_sync.as_union_value()),
        },
    );
    builder.finish_size_prefixed(env, None);
    let packet = encode_packet(builder.finished_data());

    let mut decoded = [0u8; 128];
    let payload = decode_packet(&packet, &mut decoded).unwrap();
    let parsed = crate::parse_response_envelope(payload).unwrap();
    assert_eq!(parsed.message_type(), proto::Response::TimeSync);
    let sync = parsed.message_as_time_sync().unwrap();
    assert_eq!(sync.epoch_seconds(), 1790596867);
    assert_eq!(sync.subsec_micros(), 123456);
}
