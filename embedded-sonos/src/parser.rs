//! XML and SOAP response parsers using `xml-no-std`.

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use xml_no_std::reader::{EventReader, ParserConfig, XmlEvent};

use crate::error::{Result, SonosError};
use crate::model::{
    HouseholdTopology, TrackInfo, TransportState, ZoneGroup, ZoneMember, extract_ip_from_location,
    parse_duration_seconds,
};

/// Check if the SOAP XML response contains a Fault, and return a `SonosError::Upnp` if so.
pub fn check_soap_fault(xml_bytes: &[u8]) -> Result<()> {
    let config = ParserConfig::new()
        .trim_whitespace(true)
        .ignore_comments(true);
    let mut reader = EventReader::new_with_config(xml_bytes.iter(), config);

    let mut is_fault = false;
    let mut fault_string = String::new();
    let mut error_code = None;
    let mut current_tag = String::new();

    while let Ok(event) = reader.next() {
        match event {
            XmlEvent::StartElement { name, .. } => {
                if name.local_name == "Fault" {
                    is_fault = true;
                }
                current_tag = name.local_name;
            }
            XmlEvent::Characters(text) => {
                if is_fault {
                    if current_tag == "faultstring" {
                        fault_string = text;
                    } else if current_tag == "errorCode" {
                        error_code = text.parse::<u16>().ok();
                    }
                }
            }
            XmlEvent::EndDocument => break,
            _ => {}
        }
    }

    if is_fault {
        return Err(SonosError::Upnp {
            code: error_code.unwrap_or(500),
            description: fault_string,
        });
    }

    Ok(())
}

/// Extract a single named XML property from a SOAP response.
pub fn extract_xml_property(xml_bytes: &[u8], target_tag: &str) -> Result<String> {
    check_soap_fault(xml_bytes)?;

    let config = ParserConfig::new()
        .trim_whitespace(true)
        .ignore_comments(true);
    let mut reader = EventReader::new_with_config(xml_bytes.iter(), config);

    let mut inside_target = false;

    while let Ok(event) = reader.next() {
        match event {
            XmlEvent::StartElement { name, .. } => {
                if name.local_name.eq_ignore_ascii_case(target_tag) {
                    inside_target = true;
                }
            }
            XmlEvent::Characters(text) => {
                if inside_target {
                    return Ok(text);
                }
            }
            XmlEvent::EndElement { name } => {
                if name.local_name.eq_ignore_ascii_case(target_tag) {
                    inside_target = false;
                }
            }
            XmlEvent::EndDocument => break,
            _ => {}
        }
    }

    Err(SonosError::MissingField("Requested tag not found in XML"))
}

/// Parse `GetTransportInfoResponse` into `TransportState`.
pub fn parse_transport_info(xml_bytes: &[u8]) -> Result<TransportState> {
    let state_str = extract_xml_property(xml_bytes, "CurrentTransportState")?;
    TransportState::parse(&state_str).ok_or(SonosError::Parse("Unrecognized CurrentTransportState"))
}

/// Parse `GetVolumeResponse` or `SetRelativeVolumeResponse` into volume level (0..=100).
pub fn parse_volume_response(xml_bytes: &[u8]) -> Result<u16> {
    let vol_str = extract_xml_property(xml_bytes, "CurrentVolume")
        .or_else(|_| extract_xml_property(xml_bytes, "NewVolume"))?;
    vol_str
        .parse::<u16>()
        .map_err(|_| SonosError::Parse("Invalid volume integer"))
}

/// Parse `GetMuteResponse` into boolean mute state.
pub fn parse_mute_response(xml_bytes: &[u8]) -> Result<bool> {
    let mute_str = extract_xml_property(xml_bytes, "CurrentMute")?;
    match mute_str.as_str() {
        "1" | "true" | "TRUE" => Ok(true),
        "0" | "false" | "FALSE" => Ok(false),
        _ => Err(SonosError::Parse("Invalid boolean mute value")),
    }
}

/// Parse `GetPositionInfoResponse` into `TrackInfo`.
pub fn parse_position_info(xml_bytes: &[u8]) -> Result<Option<TrackInfo>> {
    check_soap_fault(xml_bytes)?;

    let config = ParserConfig::new()
        .trim_whitespace(true)
        .ignore_comments(true);
    let mut reader = EventReader::new_with_config(xml_bytes.iter(), config);

    let mut track_no = 0u32;
    let mut track_duration = 0u32;
    let mut elapsed = 0u32;
    let mut track_uri = String::new();
    let mut track_meta_xml = String::new();
    let mut current_tag = String::new();

    while let Ok(event) = reader.next() {
        match event {
            XmlEvent::StartElement { name, .. } => {
                current_tag = name.local_name;
            }
            XmlEvent::Characters(text) => {
                if current_tag == "Track" {
                    track_no = text.parse::<u32>().unwrap_or(0);
                } else if current_tag == "TrackDuration" {
                    track_duration = parse_duration_seconds(&text).unwrap_or(0);
                } else if current_tag == "RelTime" {
                    elapsed = parse_duration_seconds(&text).unwrap_or(0);
                } else if current_tag == "TrackURI" {
                    track_uri = text;
                } else if current_tag == "TrackMetaData" {
                    track_meta_xml = text;
                }
            }
            XmlEvent::EndDocument => break,
            _ => {}
        }
    }

    if track_meta_xml.is_empty() || track_meta_xml.eq_ignore_ascii_case("NOT_IMPLEMENTED") {
        return Ok(None);
    }

    // Parse inner unescaped DIDL-Lite metadata.
    let parsed = parse_didl_lite(
        &track_meta_xml,
        track_no,
        track_duration,
        elapsed,
        &track_uri,
    )?;
    Ok(Some(parsed))
}

/// Parse inner DIDL-Lite XML metadata (already entity-decoded).
pub fn parse_didl_lite(
    didl_xml: &str,
    track_number: u32,
    duration_seconds: u32,
    elapsed_seconds: u32,
    fallback_uri: &str,
) -> Result<TrackInfo> {
    let config = ParserConfig::new()
        .trim_whitespace(true)
        .ignore_comments(true);
    let mut reader = EventReader::new_with_config(didl_xml.as_bytes().iter(), config);

    let mut title = None;
    let mut artist = None;
    let mut album = None;
    let mut album_art_uri = None;
    let mut res_uri = None;
    let mut current_tag = String::new();

    while let Ok(event) = reader.next() {
        match event {
            XmlEvent::StartElement { name, .. } => {
                current_tag = name.local_name;
            }
            XmlEvent::Characters(text) => {
                if current_tag == "title" && title.is_none() {
                    title = Some(text);
                } else if (current_tag == "creator" || current_tag == "artist") && artist.is_none()
                {
                    artist = Some(text);
                } else if current_tag == "album" && album.is_none() {
                    album = Some(text);
                } else if current_tag == "albumArtURI" && album_art_uri.is_none() {
                    album_art_uri = Some(text);
                } else if current_tag == "res" && res_uri.is_none() {
                    res_uri = Some(text);
                }
            }
            XmlEvent::EndDocument => break,
            _ => {}
        }
    }

    let uri = res_uri.unwrap_or_else(|| fallback_uri.to_string());
    let title = title.unwrap_or_else(|| "Unknown Title".to_string());

    Ok(TrackInfo {
        track_number,
        duration_seconds,
        elapsed_seconds,
        title,
        artist,
        album,
        album_art_uri,
        uri,
    })
}

/// Parse `GetZoneGroupStateResponse` into full `HouseholdTopology`.
pub fn parse_zone_group_state(xml_bytes: &[u8]) -> Result<HouseholdTopology> {
    let state_xml = extract_xml_property(xml_bytes, "ZoneGroupState")?;
    parse_inner_zone_group_state(&state_xml)
}

/// Parse inner unescaped `ZoneGroupState` XML.
pub fn parse_inner_zone_group_state(state_xml: &str) -> Result<HouseholdTopology> {
    let config = ParserConfig::new()
        .trim_whitespace(true)
        .ignore_comments(true);
    let mut reader = EventReader::new_with_config(state_xml.as_bytes().iter(), config);

    let mut groups = Vec::new();
    let mut current_group_id = String::new();
    let mut current_coordinator_uuid = String::new();
    let mut current_members = Vec::new();

    while let Ok(event) = reader.next() {
        match event {
            XmlEvent::StartElement {
                name, attributes, ..
            } => {
                if name.local_name == "ZoneGroup" {
                    current_members.clear();
                    current_group_id.clear();
                    current_coordinator_uuid.clear();

                    for attr in attributes {
                        if attr.name.local_name == "ID" {
                            current_group_id = attr.value;
                        } else if attr.name.local_name == "Coordinator" {
                            current_coordinator_uuid = attr.value;
                        }
                    }
                } else if name.local_name == "ZoneGroupMember" {
                    let mut uuid = String::new();
                    let mut zone_name = String::new();
                    let mut location = String::new();
                    let mut invisible = false;

                    for attr in attributes {
                        if attr.name.local_name == "UUID" {
                            uuid = attr.value;
                        } else if attr.name.local_name == "ZoneName" {
                            zone_name = attr.value;
                        } else if attr.name.local_name == "Location" {
                            location = attr.value;
                        } else if attr.name.local_name == "Invisible" {
                            invisible = attr.value == "1";
                        }
                    }

                    if let Some(ip) = extract_ip_from_location(&location) {
                        let is_coordinator = uuid == current_coordinator_uuid;
                        current_members.push(ZoneMember {
                            uuid,
                            name: zone_name,
                            location_url: location,
                            ip,
                            is_coordinator,
                            invisible,
                        });
                    }
                }
            }
            XmlEvent::EndElement { name } => {
                if name.local_name == "ZoneGroup" {
                    let coordinator_ip = current_members
                        .iter()
                        .find(|m| m.uuid == current_coordinator_uuid)
                        .map(|m| m.ip);

                    // Name of the group is the coordinator's room name (or first non-invisible member).
                    let group_name = current_members
                        .iter()
                        .find(|m| m.uuid == current_coordinator_uuid)
                        .map_or_else(
                            || {
                                current_members
                                    .iter()
                                    .find(|m| !m.invisible)
                                    .map_or_else(|| "Unknown".to_string(), |m| m.name.clone())
                            },
                            |m| m.name.clone(),
                        );

                    groups.push(ZoneGroup {
                        id: current_group_id.clone(),
                        coordinator_uuid: current_coordinator_uuid.clone(),
                        coordinator_ip,
                        name: group_name,
                        members: current_members.clone(),
                    });
                }
            }
            XmlEvent::EndDocument => break,
            _ => {}
        }
    }

    Ok(HouseholdTopology { groups })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;
    use core::net::Ipv4Addr;

    #[test]
    fn test_parse_transport_info_playing() {
        let xml = b"<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body><u:GetTransportInfoResponse xmlns:u=\"urn:schemas-upnp-org:service:AVTransport:1\"><CurrentTransportState>PLAYING</CurrentTransportState><CurrentTransportStatus>OK</CurrentTransportStatus><CurrentSpeed>1</CurrentSpeed></u:GetTransportInfoResponse></s:Body></s:Envelope>";
        let state = parse_transport_info(xml).unwrap();
        assert_eq!(state, TransportState::Playing);
    }

    #[test]
    fn test_parse_soap_fault() {
        let xml = b"<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body><s:Fault><faultcode>s:Client</faultcode><faultstring>UPnPError</faultstring><detail><UPnPError xmlns=\"urn:schemas-upnp-org:control-1-0\"><errorCode>401</errorCode></UPnPError></detail></s:Fault></s:Body></s:Envelope>";
        let err = parse_transport_info(xml).unwrap_err();
        match err {
            SonosError::Upnp { code, description } => {
                assert_eq!(code, 401);
                assert_eq!(description, "UPnPError");
            }
            other => panic!("Expected UPnP error, got: {other:?}"),
        }
    }

    #[test]
    fn test_parse_volume_and_mute() {
        let vol_xml = b"<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body><u:GetVolumeResponse xmlns:u=\"urn:schemas-upnp-org:service:RenderingControl:1\"><CurrentVolume>35</CurrentVolume></u:GetVolumeResponse></s:Body></s:Envelope>";
        assert_eq!(parse_volume_response(vol_xml).unwrap(), 35);

        let mute_xml = b"<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body><u:GetMuteResponse xmlns:u=\"urn:schemas-upnp-org:service:RenderingControl:1\"><CurrentMute>1</CurrentMute></u:GetMuteResponse></s:Body></s:Envelope>";
        assert!(parse_mute_response(mute_xml).unwrap());
    }

    #[test]
    fn test_parse_position_info_fixture() {
        let xml = include_bytes!("../../../ref/sonor/position-info.out");
        let track_opt = parse_position_info(xml).unwrap();
        let track = track_opt.expect("Should have track metadata");

        assert_eq!(track.track_number, 11);
        assert_eq!(track.duration_seconds, 481);
        assert_eq!(track.elapsed_seconds, 113);
        assert_eq!(track.title, "Untitled - Or the Evening Redness in the West");
        assert_eq!(track.artist.as_deref(), Some("Kerala Dust"));
        assert_eq!(track.album.as_deref(), Some("Light, West"));
        assert!(
            track
                .album_art_uri
                .unwrap()
                .starts_with("/getaa?s=1&u=x-sonos-spotify")
        );
    }

    #[test]
    fn test_parse_zone_group_state_fixtures() {
        let state_xml = r#"<ZoneGroupState><ZoneGroups><ZoneGroup Coordinator="RINCON_B8E937D9C81601400" ID="RINCON_B8E937D9C81601400:0"><ZoneGroupMember UUID="RINCON_B8E937D9C81601400" Location="http://192.168.9.105:1400/xml/device_description.xml" ZoneName="Kitchen" Configuration="1" SoftwareVersion="86.8-78270" /></ZoneGroup><ZoneGroup Coordinator="RINCON_347E5CF8A88601400" ID="RINCON_000E58BC2F5C01400:171"><ZoneGroupMember UUID="RINCON_347E5CF8A88601400" Location="http://192.168.9.190:1400/xml/device_description.xml" ZoneName="Office" Configuration="1" /></ZoneGroup></ZoneGroups></ZoneGroupState>"#;

        let topology = parse_inner_zone_group_state(state_xml).unwrap();
        assert_eq!(topology.groups.len(), 2);

        let kitchen = topology.find_group_by_name("Kitchen").unwrap();
        assert_eq!(
            kitchen.coordinator_ip,
            Some(Ipv4Addr::new(192, 168, 9, 105))
        );
        assert_eq!(kitchen.members.len(), 1);

        let office = topology.find_group_by_name("Office").unwrap();
        assert_eq!(office.coordinator_ip, Some(Ipv4Addr::new(192, 168, 9, 190)));

        assert_eq!(
            topology.find_coordinator_ip_for_zone("Kitchen"),
            Some(Ipv4Addr::new(192, 168, 9, 105))
        );
        assert_eq!(
            topology.find_coordinator_ip_for_zone("Office"),
            Some(Ipv4Addr::new(192, 168, 9, 190))
        );
        assert_eq!(topology.find_coordinator_ip_for_zone("Bedroom"), None);
    }
}
