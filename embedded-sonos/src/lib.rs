//! # embedded-sonos
//!
//! A lightweight, `no_std`-compatible, asynchronous Sonos controller library
//! designed for embedded devices (like ESP32) and host applications.
//!
//! ## Overview
//!
//! - **SOAP Control**: Control playback (`Play`, `Pause`, `Next`, `Previous`, `Seek`) and volume.
//! - **Topology Discovery**: Discover all rooms, groups, and coordinators from a single seed IP address.
//! - **DIDL-Lite Metadata**: Extract title, artist, album, duration, and album art URIs.
//! - **GENA Eventing**: Subscribe to real-time state change notifications.
//! - **Pure Embedded Transport**: Operates over any `embedded-io-async` byte stream (`embassy-net`, Tokio, etc.).

#![no_std]
#![forbid(unsafe_code)]
#![expect(
    clippy::missing_errors_doc,
    clippy::module_name_repetitions,
    clippy::doc_markdown
)]

extern crate alloc;

pub mod client;
pub mod error;
pub mod event;
pub mod http;
pub mod model;
pub mod parser;
pub mod soap;

pub use client::{
    SONOS_DEFAULT_PORT, call_action, get_mute, get_position_info, get_transport_info, get_volume,
    get_zone_group_state, next, pause, play, previous, seek_time, seek_track, set_mute,
    set_relative_volume, set_volume, stop, stream_album_art,
};
pub use error::{Result, SonosError};
pub use event::{
    Subscription, handle_notify_request, parse_notify_body, renew_subscription, subscribe,
    unsubscribe,
};
pub use http::{
    ResponseHeaders, read_response_body, read_response_headers, send_get_request,
    send_resubscribe_request, send_soap_request, send_subscribe_request, send_unsubscribe_request,
    stream_response_body,
};
pub use model::{
    HouseholdTopology, PlayMode, SonosEvent, TrackInfo, TransportState, VolumeChannel, ZoneGroup,
    ZoneMember, extract_ip_from_location, parse_duration_seconds,
};
pub use parser::{
    check_soap_fault, extract_xml_property, parse_didl_lite, parse_mute_response,
    parse_position_info, parse_transport_info, parse_volume_response, parse_zone_group_state,
};
pub use soap::{Action, Service};
