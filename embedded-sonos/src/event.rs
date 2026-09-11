//! GENA event subscriptions and notification handling.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use core::net::Ipv4Addr;
use embedded_io_async::{Error, Read, Write as AsyncWrite};
use xml_no_std::reader::{EventReader, ParserConfig, XmlEvent};

use crate::error::{Result, SonosError};
use crate::http::{
    read_response_body, read_response_headers, send_resubscribe_request, send_subscribe_request,
    send_unsubscribe_request,
};
use crate::model::{SonosEvent, TransportState, VolumeChannel};
use crate::parser::{extract_xml_property, parse_didl_lite};
use crate::soap::Service;

/// Represents an active UPnP GENA event subscription.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subscription {
    /// Unique subscription ID (UUID header returned by Sonos).
    pub sid: String,
    /// Granted duration in seconds before subscription expires.
    pub timeout_seconds: u32,
}

/// Subscribe to events on a specific Sonos service.
pub async fn subscribe<S: Read + AsyncWrite>(
    stream: &mut S,
    host: Ipv4Addr,
    port: u16,
    service: Service,
    callback_url: &str,
    requested_timeout_seconds: u32,
) -> Result<Subscription> {
    send_subscribe_request(
        stream,
        host,
        port,
        service.event_sub_url(),
        callback_url,
        requested_timeout_seconds,
    )
    .await?;

    let mut header_buf = [0u8; 1024];
    let (headers, _) = read_response_headers(stream, &mut header_buf).await?;

    if headers.status_code != 200 {
        return Err(SonosError::Http {
            status_code: headers.status_code,
            message: "Failed to subscribe to Sonos events",
        });
    }

    let sid = headers.sid.ok_or(SonosError::MissingField("SID"))?;
    let timeout_seconds = headers.timeout_seconds.unwrap_or(requested_timeout_seconds);

    Ok(Subscription {
        sid,
        timeout_seconds,
    })
}

/// Renew an active event subscription. Returns updated granted timeout.
pub async fn renew_subscription<S: Read + AsyncWrite>(
    stream: &mut S,
    host: Ipv4Addr,
    port: u16,
    service: Service,
    sid: &str,
    requested_timeout_seconds: u32,
) -> Result<u32> {
    send_resubscribe_request(
        stream,
        host,
        port,
        service.event_sub_url(),
        sid,
        requested_timeout_seconds,
    )
    .await?;

    let mut header_buf = [0u8; 1024];
    let (headers, _) = read_response_headers(stream, &mut header_buf).await?;

    if headers.status_code != 200 {
        return Err(SonosError::Http {
            status_code: headers.status_code,
            message: "Failed to renew Sonos event subscription",
        });
    }

    Ok(headers.timeout_seconds.unwrap_or(requested_timeout_seconds))
}

/// Cancel an active event subscription.
pub async fn unsubscribe<S: Read + AsyncWrite>(
    stream: &mut S,
    host: Ipv4Addr,
    port: u16,
    service: Service,
    sid: &str,
) -> Result<()> {
    send_unsubscribe_request(stream, host, port, service.event_sub_url(), sid).await?;

    let mut header_buf = [0u8; 1024];
    let (headers, _) = read_response_headers(stream, &mut header_buf).await?;

    if headers.status_code != 200 {
        return Err(SonosError::Http {
            status_code: headers.status_code,
            message: "Failed to unsubscribe from Sonos events",
        });
    }

    Ok(())
}

/// Parse an incoming UPnP `NOTIFY` XML body and return all decoded state change events.
pub fn parse_notify_body(xml_bytes: &[u8]) -> Result<Vec<SonosEvent>> {
    let mut events = Vec::new();

    // 1. Check for ZoneGroupTopology events
    if extract_xml_property(xml_bytes, "ZoneGroupState").is_ok_and(|s| !s.is_empty()) {
        events.push(SonosEvent::TopologyChanged);
    }

    // 2. Check for AVTransport or RenderingControl LastChange events
    if let Ok(last_change_xml) = extract_xml_property(xml_bytes, "LastChange") {
        parse_last_change_events(&last_change_xml, &mut events);
    }

    Ok(events)
}

/// Parse the inner `<Event>` XML within a `<LastChange>` tag.
fn parse_last_change_events(last_change_xml: &str, events: &mut Vec<SonosEvent>) {
    let config = ParserConfig::new()
        .trim_whitespace(true)
        .ignore_comments(true);
    let mut reader = EventReader::new_with_config(last_change_xml.as_bytes().iter(), config);

    while let Ok(event) = reader.next() {
        match event {
            XmlEvent::StartElement {
                name, attributes, ..
            } => {
                let tag = name.local_name.as_str();

                if tag == "TransportState" {
                    for attr in attributes {
                        if attr.name.local_name == "val" {
                            events.extend(
                                TransportState::parse(&attr.value)
                                    .map(|state| SonosEvent::TransportStateChanged { state }),
                            );
                        }
                    }
                } else if tag == "CurrentTrackMetaData" {
                    for attr in attributes {
                        if attr.name.local_name == "val" {
                            if attr.value.is_empty()
                                || attr.value.eq_ignore_ascii_case("NOT_IMPLEMENTED")
                            {
                                events.push(SonosEvent::TrackChanged { track: None });
                            } else if let Ok(track) = parse_didl_lite(&attr.value, 0, 0, 0, "") {
                                events.push(SonosEvent::TrackChanged { track: Some(track) });
                            }
                        }
                    }
                } else if tag == "Volume" {
                    let mut channel = VolumeChannel::Master;
                    let mut volume = None;

                    for attr in attributes {
                        if attr.name.local_name == "channel" {
                            if attr.value.eq_ignore_ascii_case("LF") {
                                channel = VolumeChannel::LeftFront;
                            } else if attr.value.eq_ignore_ascii_case("RF") {
                                channel = VolumeChannel::RightFront;
                            } else {
                                channel = VolumeChannel::Master;
                            }
                        } else if attr.name.local_name == "val" {
                            volume = attr.value.parse::<u16>().ok();
                        }
                    }

                    if let Some(vol) = volume {
                        events.push(SonosEvent::VolumeChanged {
                            channel,
                            volume: vol,
                        });
                    }
                } else if tag == "Mute" {
                    let mut channel = VolumeChannel::Master;
                    let mut mute = None;

                    for attr in attributes {
                        if attr.name.local_name == "channel" {
                            if attr.value.eq_ignore_ascii_case("LF") {
                                channel = VolumeChannel::LeftFront;
                            } else if attr.value.eq_ignore_ascii_case("RF") {
                                channel = VolumeChannel::RightFront;
                            } else {
                                channel = VolumeChannel::Master;
                            }
                        } else if attr.name.local_name == "val" {
                            mute = match attr.value.as_str() {
                                "1" | "true" => Some(true),
                                "0" | "false" => Some(false),
                                _ => None,
                            };
                        }
                    }

                    if let Some(m) = mute {
                        events.push(SonosEvent::MuteChanged { channel, mute: m });
                    }
                }
            }
            XmlEvent::EndDocument => break,
            _ => {}
        }
    }
}

/// Process an incoming HTTP `NOTIFY` connection: parses headers & body, sends `200 OK`,
/// and returns the decoded `SonosEvent` instances.
pub async fn handle_notify_request<R: Read, W: AsyncWrite>(
    reader: &mut R,
    writer: &mut W,
    header_buf: &mut [u8],
) -> Result<Vec<SonosEvent>> {
    let (headers, leftover_len) = read_response_headers(reader, header_buf).await?;
    let leftover = header_buf
        .get(..leftover_len)
        .ok_or(SonosError::BufferTooSmall)?;

    let body = read_response_body(reader, leftover, &headers, 32 * 1024).await?;

    // Respond to Sonos with HTTP 200 OK
    writer
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
        .await
        .map_err(|e| SonosError::Io(e.kind()))?;

    parse_notify_body(&body)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_avtransport_last_change() {
        let notify_xml = r#"<e:propertyset xmlns:e="urn:schemas-upnp-org:event-1-0"><e:property><LastChange>&lt;Event xmlns=&quot;urn:schemas-upnp-org:metadata-1-0/AVT/&quot;&gt;&lt;InstanceID val=&quot;0&quot;&gt;&lt;TransportState val=&quot;PLAYING&quot;/&gt;&lt;CurrentTrackMetaData val=&quot;&amp;lt;DIDL-Lite xmlns:dc=&amp;quot;http://purl.org/dc/elements/1.1/&amp;quot; xmlns:upnp=&amp;quot;urn:schemas-upnp-org:metadata-1-0/upnp/&amp;quot;&amp;gt;&amp;lt;item&amp;gt;&amp;lt;dc:title&amp;gt;Test Song&amp;lt;/dc:title&amp;gt;&amp;lt;dc:creator&amp;gt;Test Artist&amp;lt;/dc:creator&amp;gt;&amp;lt;/item&amp;gt;&amp;lt;/DIDL-Lite&amp;gt;&quot;/&gt;&lt;/InstanceID&gt;&lt;/Event&gt;</LastChange></e:property></e:propertyset>"#;

        let events = parse_notify_body(notify_xml.as_bytes()).unwrap();
        assert_eq!(events.len(), 2);

        assert_eq!(
            events[0],
            SonosEvent::TransportStateChanged {
                state: TransportState::Playing
            }
        );

        match &events[1] {
            SonosEvent::TrackChanged { track: Some(t) } => {
                assert_eq!(t.title, "Test Song");
                assert_eq!(t.artist.as_deref(), Some("Test Artist"));
            }
            other => panic!("Expected TrackChanged, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_rendering_control_last_change() {
        let notify_xml = r#"<e:propertyset xmlns:e="urn:schemas-upnp-org:event-1-0"><e:property><LastChange>&lt;Event xmlns=&quot;urn:schemas-upnp-org:metadata-1-0/RCS/&quot;&gt;&lt;InstanceID val=&quot;0&quot;&gt;&lt;Volume channel=&quot;Master&quot; val=&quot;42&quot;/&gt;&lt;Mute channel=&quot;Master&quot; val=&quot;0&quot;/&gt;&lt;/InstanceID&gt;&lt;/Event&gt;</LastChange></e:property></e:propertyset>"#;

        let events = parse_notify_body(notify_xml.as_bytes()).unwrap();
        assert_eq!(events.len(), 2);

        assert_eq!(
            events[0],
            SonosEvent::VolumeChanged {
                channel: VolumeChannel::Master,
                volume: 42,
            }
        );

        assert_eq!(
            events[1],
            SonosEvent::MuteChanged {
                channel: VolumeChannel::Master,
                mute: false,
            }
        );
    }

    #[test]
    fn test_parse_topology_notify() {
        let notify_xml = r#"<e:propertyset xmlns:e="urn:schemas-upnp-org:event-1-0"><e:property><ZoneGroupState>&lt;ZoneGroups&gt;&lt;/ZoneGroups&gt;</ZoneGroupState></e:property></e:propertyset>"#;

        let events = parse_notify_body(notify_xml.as_bytes()).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0], SonosEvent::TopologyChanged);
    }
}
