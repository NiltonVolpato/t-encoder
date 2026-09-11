//! High-level and stream-level Sonos client API.

extern crate alloc;

use alloc::vec::Vec;
use core::net::Ipv4Addr;
use embedded_io_async::{Read, Write as AsyncWrite};

use crate::error::{Result, SonosError};
use crate::http::{
    read_response_body, read_response_headers, send_get_request, send_soap_request,
    stream_response_body,
};
use crate::model::{HouseholdTopology, TrackInfo, TransportState, VolumeChannel};
use crate::parser::{
    parse_mute_response, parse_position_info, parse_transport_info, parse_volume_response,
    parse_zone_group_state,
};
use crate::soap::Action;

/// Default Sonos UPnP HTTP port.
pub const SONOS_DEFAULT_PORT: u16 = 1400;

/// Execute a SOAP action over an active bidirectional stream.
///
/// Sends the action request, checks the HTTP status and SOAP Faults,
/// and returns the response body bytes.
pub async fn call_action<S>(
    stream: &mut S,
    host: Ipv4Addr,
    port: u16,
    action: &Action<'_>,
) -> Result<Vec<u8>>
where
    S: Read + AsyncWrite,
{
    send_soap_request(stream, host, port, action).await?;

    let mut header_buf = [0u8; 1024];
    let (headers, leftover_len) = read_response_headers(stream, &mut header_buf).await?;
    let leftover = header_buf
        .get(..leftover_len)
        .ok_or(SonosError::BufferTooSmall)?;

    let body = read_response_body(stream, leftover, &headers, 64 * 1024).await?;

    if headers.status_code != 200 && headers.status_code != 500 {
        return Err(SonosError::Http {
            status_code: headers.status_code,
            message: "Sonos returned unexpected HTTP status code",
        });
    }

    Ok(body)
}

/// Start or resume playback on the speaker.
pub async fn play<S: Read + AsyncWrite>(stream: &mut S, host: Ipv4Addr, port: u16) -> Result<()> {
    let body = call_action(stream, host, port, &Action::Play).await?;
    crate::parser::check_soap_fault(&body)
}

/// Pause playback on the speaker.
pub async fn pause<S: Read + AsyncWrite>(stream: &mut S, host: Ipv4Addr, port: u16) -> Result<()> {
    let body = call_action(stream, host, port, &Action::Pause).await?;
    crate::parser::check_soap_fault(&body)
}

/// Stop playback on the speaker.
pub async fn stop<S: Read + AsyncWrite>(stream: &mut S, host: Ipv4Addr, port: u16) -> Result<()> {
    let body = call_action(stream, host, port, &Action::Stop).await?;
    crate::parser::check_soap_fault(&body)
}

/// Skip to next track.
pub async fn next<S: Read + AsyncWrite>(stream: &mut S, host: Ipv4Addr, port: u16) -> Result<()> {
    let body = call_action(stream, host, port, &Action::Next).await?;
    crate::parser::check_soap_fault(&body)
}

/// Skip to previous track.
pub async fn previous<S: Read + AsyncWrite>(
    stream: &mut S,
    host: Ipv4Addr,
    port: u16,
) -> Result<()> {
    let body = call_action(stream, host, port, &Action::Previous).await?;
    crate::parser::check_soap_fault(&body)
}

/// Seek to a time offset within the current track.
pub async fn seek_time<S: Read + AsyncWrite>(
    stream: &mut S,
    host: Ipv4Addr,
    port: u16,
    seconds: u32,
) -> Result<()> {
    let body = call_action(stream, host, port, &Action::SeekTime { seconds }).await?;
    crate::parser::check_soap_fault(&body)
}

/// Seek to a specific track number in queue.
pub async fn seek_track<S: Read + AsyncWrite>(
    stream: &mut S,
    host: Ipv4Addr,
    port: u16,
    track_number: u32,
) -> Result<()> {
    let body = call_action(stream, host, port, &Action::SeekTrack { track_number }).await?;
    crate::parser::check_soap_fault(&body)
}

/// Query current transport playback state (`Playing`, `PausedPlayback`, `Stopped`, `Transitioning`).
pub async fn get_transport_info<S: Read + AsyncWrite>(
    stream: &mut S,
    host: Ipv4Addr,
    port: u16,
) -> Result<TransportState> {
    let body = call_action(stream, host, port, &Action::GetTransportInfo).await?;
    parse_transport_info(&body)
}

/// Query current track metadata and position info.
pub async fn get_position_info<S: Read + AsyncWrite>(
    stream: &mut S,
    host: Ipv4Addr,
    port: u16,
) -> Result<Option<TrackInfo>> {
    let body = call_action(stream, host, port, &Action::GetPositionInfo).await?;
    parse_position_info(&body)
}

/// Query volume level for a channel (0..=100).
pub async fn get_volume<S: Read + AsyncWrite>(
    stream: &mut S,
    host: Ipv4Addr,
    port: u16,
    channel: VolumeChannel,
) -> Result<u16> {
    let body = call_action(stream, host, port, &Action::GetVolume { channel }).await?;
    parse_volume_response(&body)
}

/// Set volume level for a channel (0..=100).
pub async fn set_volume<S: Read + AsyncWrite>(
    stream: &mut S,
    host: Ipv4Addr,
    port: u16,
    channel: VolumeChannel,
    volume: u16,
) -> Result<()> {
    let body = call_action(stream, host, port, &Action::SetVolume { channel, volume }).await?;
    crate::parser::check_soap_fault(&body)
}

/// Adjust volume level relatively (+/- delta). Returns new volume.
pub async fn set_relative_volume<S: Read + AsyncWrite>(
    stream: &mut S,
    host: Ipv4Addr,
    port: u16,
    channel: VolumeChannel,
    adjustment: i16,
) -> Result<u16> {
    let body = call_action(
        stream,
        host,
        port,
        &Action::SetRelativeVolume {
            channel,
            adjustment,
        },
    )
    .await?;
    parse_volume_response(&body)
}

/// Query mute state on a channel.
pub async fn get_mute<S: Read + AsyncWrite>(
    stream: &mut S,
    host: Ipv4Addr,
    port: u16,
    channel: VolumeChannel,
) -> Result<bool> {
    let body = call_action(stream, host, port, &Action::GetMute { channel }).await?;
    parse_mute_response(&body)
}

/// Set mute state on a channel.
pub async fn set_mute<S: Read + AsyncWrite>(
    stream: &mut S,
    host: Ipv4Addr,
    port: u16,
    channel: VolumeChannel,
    mute: bool,
) -> Result<()> {
    let body = call_action(stream, host, port, &Action::SetMute { channel, mute }).await?;
    crate::parser::check_soap_fault(&body)
}

/// Query entire household zone group topology.
pub async fn get_zone_group_state<S: Read + AsyncWrite>(
    stream: &mut S,
    host: Ipv4Addr,
    port: u16,
) -> Result<HouseholdTopology> {
    let body = call_action(stream, host, port, &Action::GetZoneGroupState).await?;
    parse_zone_group_state(&body)
}

/// Stream raw album art bytes from the speaker into an async writer.
pub async fn stream_album_art<S: Read + AsyncWrite, W: AsyncWrite>(
    stream: &mut S,
    host: Ipv4Addr,
    port: u16,
    album_art_uri: &str,
    writer: &mut W,
) -> Result<usize> {
    send_get_request(stream, host, port, album_art_uri).await?;

    let mut header_buf = [0u8; 1024];
    let (headers, leftover_len) = read_response_headers(stream, &mut header_buf).await?;

    if headers.status_code != 200 {
        return Err(SonosError::Http {
            status_code: headers.status_code,
            message: "Failed to download album art image",
        });
    }

    let leftover = header_buf
        .get(..leftover_len)
        .ok_or(SonosError::BufferTooSmall)?;
    stream_response_body(stream, leftover, &headers, writer).await
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;

    /// A simple in-memory mock async stream for testing client logic.
    struct MockStream {
        incoming: Vec<u8>,
        read_pos: usize,
        written: Vec<u8>,
    }

    impl MockStream {
        fn new(response: &[u8]) -> Self {
            Self {
                incoming: response.to_vec(),
                read_pos: 0,
                written: Vec::new(),
            }
        }
    }

    impl embedded_io_async::ErrorType for MockStream {
        type Error = embedded_io_async::ErrorKind;
    }

    impl Read for MockStream {
        async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
            if self.read_pos >= self.incoming.len() {
                return Ok(0);
            }
            let available = self.incoming.len() - self.read_pos;
            let n = available.min(buf.len());
            buf[..n].copy_from_slice(&self.incoming[self.read_pos..self.read_pos + n]);
            self.read_pos += n;
            Ok(n)
        }
    }

    impl AsyncWrite for MockStream {
        async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
            self.written.extend_from_slice(buf);
            Ok(buf.len())
        }

        async fn flush(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_mock_play_request() {
        let response = b"HTTP/1.1 200 OK\r\nContent-Length: 150\r\nConnection: close\r\n\r\n<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body><u:PlayResponse xmlns:u=\"urn:schemas-upnp-org:service:AVTransport:1\"/></s:Body></s:Envelope>";
        let mut stream = MockStream::new(response);
        let host = Ipv4Addr::new(192, 168, 9, 105);

        play(&mut stream, host, 1400).await.unwrap();

        let req_str = core::str::from_utf8(&stream.written).unwrap();
        assert!(req_str.starts_with("POST /MediaRenderer/AVTransport/Control HTTP/1.1\r\n"));
        assert!(
            req_str.contains("SOAPAction: \"urn:schemas-upnp-org:service:AVTransport:1#Play\"")
        );
        assert!(req_str.contains("<u:Play xmlns:u=\"urn:schemas-upnp-org:service:AVTransport:1\"><InstanceID>0</InstanceID><Speed>1</Speed></u:Play>"));
    }

    #[tokio::test]
    async fn test_mock_get_transport_info() {
        let response = b"HTTP/1.1 200 OK\r\nContent-Length: 260\r\nConnection: close\r\n\r\n<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body><u:GetTransportInfoResponse xmlns:u=\"urn:schemas-upnp-org:service:AVTransport:1\"><CurrentTransportState>PAUSED_PLAYBACK</CurrentTransportState></u:GetTransportInfoResponse></s:Body></s:Envelope>";
        let mut stream = MockStream::new(response);
        let host = Ipv4Addr::new(192, 168, 9, 105);

        let state = get_transport_info(&mut stream, host, 1400).await.unwrap();
        assert_eq!(state, TransportState::PausedPlayback);
    }
}
