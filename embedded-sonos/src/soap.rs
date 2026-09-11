//! SOAP envelope serialization and action definitions.

use crate::model::VolumeChannel;
use core::fmt::{self, Write};
use embedded_io_async::Error;

/// Helper to measure formatted length without allocating.
pub struct ByteCounter {
    count: usize,
}

impl ByteCounter {
    /// Create a new counter initialized to zero.
    #[must_use]
    pub const fn new() -> Self {
        Self { count: 0 }
    }

    /// Return total bytes counted.
    #[must_use]
    pub const fn count(&self) -> usize {
        self.count
    }
}

impl Default for ByteCounter {
    fn default() -> Self {
        Self::new()
    }
}

impl Write for ByteCounter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.count = match self.count.checked_add(s.len()) {
            Some(sum) => sum,
            None => return Err(fmt::Error),
        };
        Ok(())
    }
}

/// A UPnP service identifier on a Sonos speaker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    /// AVTransport service for playback control and track metadata.
    AvTransport,
    /// RenderingControl service for volume and equalization.
    RenderingControl,
    /// ZoneGroupTopology service for household room and group topology.
    ZoneGroupTopology,
}

impl Service {
    /// Return the service URN.
    #[must_use]
    pub const fn urn(self) -> &'static str {
        match self {
            Self::AvTransport => "urn:schemas-upnp-org:service:AVTransport:1",
            Self::RenderingControl => "urn:schemas-upnp-org:service:RenderingControl:1",
            Self::ZoneGroupTopology => "urn:schemas-upnp-org:service:ZoneGroupTopology:1",
        }
    }

    /// Return the relative control endpoint URL.
    #[must_use]
    pub const fn control_url(self) -> &'static str {
        match self {
            Self::AvTransport => "/MediaRenderer/AVTransport/Control",
            Self::RenderingControl => "/MediaRenderer/RenderingControl/Control",
            Self::ZoneGroupTopology => "/ZoneGroupTopology/Control",
        }
    }

    /// Return the relative GENA event subscription endpoint URL.
    #[must_use]
    pub const fn event_sub_url(self) -> &'static str {
        match self {
            Self::AvTransport => "/MediaRenderer/AVTransport/Event",
            Self::RenderingControl => "/MediaRenderer/RenderingControl/Event",
            Self::ZoneGroupTopology => "/ZoneGroupTopology/Event",
        }
    }
}

/// A specific action to be executed on a Sonos speaker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action<'a> {
    /// Start or resume playback.
    Play,
    /// Pause playback.
    Pause,
    /// Stop playback.
    Stop,
    /// Skip to the next track in the queue.
    Next,
    /// Skip to the previous track in the queue.
    Previous,
    /// Seek to a relative time position within the current track.
    SeekTime {
        /// Target position in seconds from start of track.
        seconds: u32,
    },
    /// Seek to a specific 1-based track number in the queue.
    SeekTrack {
        /// 1-based track number.
        track_number: u32,
    },
    /// Query current transport status and state.
    GetTransportInfo,
    /// Query current track metadata and elapsed time.
    GetPositionInfo,
    /// Query current volume level on specified channel.
    GetVolume {
        /// Audio channel.
        channel: VolumeChannel,
    },
    /// Set volume level on specified channel.
    SetVolume {
        /// Audio channel.
        channel: VolumeChannel,
        /// Desired volume level (0..=100).
        volume: u16,
    },
    /// Adjust volume level relative to current level.
    SetRelativeVolume {
        /// Audio channel.
        channel: VolumeChannel,
        /// Adjustment delta (e.g. -2, +5).
        adjustment: i16,
    },
    /// Query mute status on specified channel.
    GetMute {
        /// Audio channel.
        channel: VolumeChannel,
    },
    /// Set mute status on specified channel.
    SetMute {
        /// Audio channel.
        channel: VolumeChannel,
        /// Desired mute status.
        mute: bool,
    },
    /// Query household zone group topology and speaker IPs.
    GetZoneGroupState,
    /// Arbitrary custom action.
    Custom {
        /// Service containing the action.
        service: Service,
        /// Action name.
        action_name: &'a str,
        /// XML payload inside the action tag.
        payload: &'a str,
    },
}

impl Action<'_> {
    /// Target service for this action.
    #[must_use]
    pub const fn service(&self) -> Service {
        match self {
            Self::Play
            | Self::Pause
            | Self::Stop
            | Self::Next
            | Self::Previous
            | Self::SeekTime { .. }
            | Self::SeekTrack { .. }
            | Self::GetTransportInfo
            | Self::GetPositionInfo => Service::AvTransport,

            Self::GetVolume { .. }
            | Self::SetVolume { .. }
            | Self::SetRelativeVolume { .. }
            | Self::GetMute { .. }
            | Self::SetMute { .. } => Service::RenderingControl,

            Self::GetZoneGroupState => Service::ZoneGroupTopology,

            Self::Custom { service, .. } => *service,
        }
    }

    /// Action name string.
    #[must_use]
    pub const fn action_name(&self) -> &str {
        match self {
            Self::Play => "Play",
            Self::Pause => "Pause",
            Self::Stop => "Stop",
            Self::Next => "Next",
            Self::Previous => "Previous",
            Self::SeekTime { .. } | Self::SeekTrack { .. } => "Seek",
            Self::GetTransportInfo => "GetTransportInfo",
            Self::GetPositionInfo => "GetPositionInfo",
            Self::GetVolume { .. } => "GetVolume",
            Self::SetVolume { .. } => "SetVolume",
            Self::SetRelativeVolume { .. } => "SetRelativeVolume",
            Self::GetMute { .. } => "GetMute",
            Self::SetMute { .. } => "SetMute",
            Self::GetZoneGroupState => "GetZoneGroupState",
            Self::Custom { action_name, .. } => action_name,
        }
    }

    /// Write action payload XML tags into a formatter.
    pub fn write_payload<W: Write>(&self, w: &mut W) -> fmt::Result {
        match self {
            Self::Play => {
                write!(w, "<InstanceID>0</InstanceID><Speed>1</Speed>")
            }
            Self::Pause
            | Self::Stop
            | Self::Next
            | Self::Previous
            | Self::GetTransportInfo
            | Self::GetPositionInfo => {
                write!(w, "<InstanceID>0</InstanceID>")
            }
            Self::SeekTime { seconds } => {
                let (hours, minutes, rem_seconds) = {
                    let h = seconds / 3600;
                    let m = (seconds % 3600) / 60;
                    let s = seconds % 60;
                    (h, m, s)
                };
                write!(
                    w,
                    "<InstanceID>0</InstanceID><Unit>REL_TIME</Unit><Target>{hours:02}:{minutes:02}:{rem_seconds:02}</Target>"
                )
            }
            Self::SeekTrack { track_number } => {
                write!(
                    w,
                    "<InstanceID>0</InstanceID><Unit>TRACK_NR</Unit><Target>{track_number}</Target>"
                )
            }
            Self::GetVolume { channel } | Self::GetMute { channel } => {
                write!(
                    w,
                    "<InstanceID>0</InstanceID><Channel>{}</Channel>",
                    channel.as_str()
                )
            }
            Self::SetVolume { channel, volume } => {
                write!(
                    w,
                    "<InstanceID>0</InstanceID><Channel>{}</Channel><DesiredVolume>{volume}</DesiredVolume>",
                    channel.as_str()
                )
            }
            Self::SetRelativeVolume {
                channel,
                adjustment,
            } => {
                write!(
                    w,
                    "<InstanceID>0</InstanceID><Channel>{}</Channel><Adjustment>{adjustment}</Adjustment>",
                    channel.as_str()
                )
            }
            Self::SetMute { channel, mute } => {
                let mute_val = u8::from(*mute);
                write!(
                    w,
                    "<InstanceID>0</InstanceID><Channel>{}</Channel><DesiredMute>{mute_val}</DesiredMute>",
                    channel.as_str()
                )
            }
            Self::GetZoneGroupState => Ok(()),
            Self::Custom { payload, .. } => w.write_str(payload),
        }
    }

    /// Write full SOAP envelope body into a formatter.
    pub fn write_soap_envelope<W: Write>(&self, w: &mut W) -> fmt::Result {
        let s_urn = self.service().urn();
        let a_name = self.action_name();

        w.write_str(
            "<?xml version=\"1.0\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body><u:",
        )?;
        w.write_str(a_name)?;
        w.write_str(" xmlns:u=\"")?;
        w.write_str(s_urn)?;
        w.write_str("\">")?;
        self.write_payload(w)?;
        w.write_str("</u:")?;
        w.write_str(a_name)?;
        w.write_str("></s:Body></s:Envelope>")
    }

    /// Write full SOAP envelope body asynchronously directly to an async writer.
    pub async fn write_soap_envelope_async<W: embedded_io_async::Write>(
        &self,
        w: &mut W,
    ) -> Result<(), crate::error::SonosError> {
        let s_urn = self.service().urn();
        let a_name = self.action_name();

        w.write_all(
            b"<?xml version=\"1.0\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body><u:",
        )
        .await
        .map_err(|e| crate::error::SonosError::Io(e.kind()))?;

        w.write_all(a_name.as_bytes())
            .await
            .map_err(|e| crate::error::SonosError::Io(e.kind()))?;

        w.write_all(b" xmlns:u=\"")
            .await
            .map_err(|e| crate::error::SonosError::Io(e.kind()))?;

        w.write_all(s_urn.as_bytes())
            .await
            .map_err(|e| crate::error::SonosError::Io(e.kind()))?;

        w.write_all(b"\">")
            .await
            .map_err(|e| crate::error::SonosError::Io(e.kind()))?;

        let mut payload_buf = [0u8; 512];
        let mut cursor = BufferCursor::new(&mut payload_buf);
        self.write_payload(&mut cursor)
            .map_err(|_| crate::error::SonosError::BufferTooSmall)?;

        w.write_all(cursor.written())
            .await
            .map_err(|e| crate::error::SonosError::Io(e.kind()))?;

        w.write_all(b"</u:")
            .await
            .map_err(|e| crate::error::SonosError::Io(e.kind()))?;

        w.write_all(a_name.as_bytes())
            .await
            .map_err(|e| crate::error::SonosError::Io(e.kind()))?;

        w.write_all(b"></s:Body></s:Envelope>")
            .await
            .map_err(|e| crate::error::SonosError::Io(e.kind()))?;

        Ok(())
    }

    /// Calculate exact byte length of the SOAP envelope.
    #[must_use]
    pub fn envelope_length(&self) -> usize {
        let mut counter = ByteCounter::new();
        let _ = self.write_soap_envelope(&mut counter);
        counter.count()
    }
}

/// Simple cursor writing into a stack byte buffer.
pub struct BufferCursor<'a> {
    buf: &'a mut [u8],
    pos: usize,
}

impl<'a> BufferCursor<'a> {
    /// Create a new cursor wrapping buffer.
    #[must_use]
    pub fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    /// Bytes written so far.
    #[must_use]
    pub fn written(&self) -> &[u8] {
        self.buf.get(..self.pos).unwrap_or(&[])
    }
}

impl Write for BufferCursor<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let bytes = s.as_bytes();
        let remaining = self.buf.len().saturating_sub(self.pos);
        if bytes.len() > remaining {
            return Err(fmt::Error);
        }
        let Some(end) = self.pos.checked_add(bytes.len()) else {
            return Err(fmt::Error);
        };
        if let Some(dest) = self.buf.get_mut(self.pos..end) {
            dest.copy_from_slice(bytes);
            self.pos = end;
            Ok(())
        } else {
            Err(fmt::Error)
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;
    use alloc::string::String;

    #[test]
    fn test_play_action_envelope() {
        let action = Action::Play;
        let mut body = String::new();
        action.write_soap_envelope(&mut body).unwrap();

        assert_eq!(body.len(), action.envelope_length());
        assert!(body.contains("<u:Play xmlns:u=\"urn:schemas-upnp-org:service:AVTransport:1\">"));
        assert!(body.contains("<InstanceID>0</InstanceID><Speed>1</Speed>"));
        assert!(body.ends_with("</u:Play></s:Body></s:Envelope>"));
    }

    #[test]
    fn test_set_volume_envelope() {
        let action = Action::SetVolume {
            channel: VolumeChannel::Master,
            volume: 25,
        };
        let mut body = String::new();
        action.write_soap_envelope(&mut body).unwrap();

        assert_eq!(body.len(), action.envelope_length());
        assert!(
            body.contains(
                "<u:SetVolume xmlns:u=\"urn:schemas-upnp-org:service:RenderingControl:1\">"
            )
        );
        assert!(body.contains("<Channel>Master</Channel><DesiredVolume>25</DesiredVolume>"));
    }

    #[test]
    fn test_seek_time_envelope() {
        let action = Action::SeekTime { seconds: 125 };
        let mut body = String::new();
        action.write_soap_envelope(&mut body).unwrap();

        assert_eq!(body.len(), action.envelope_length());
        assert!(body.contains("<Unit>REL_TIME</Unit><Target>00:02:05</Target>"));
    }

    #[test]
    fn test_get_zone_group_state_envelope() {
        let action = Action::GetZoneGroupState;
        let mut body = String::new();
        action.write_soap_envelope(&mut body).unwrap();

        assert_eq!(body.len(), action.envelope_length());
        assert!(body.contains("<u:GetZoneGroupState xmlns:u=\"urn:schemas-upnp-org:service:ZoneGroupTopology:1\"></u:GetZoneGroupState>"));
    }
}
