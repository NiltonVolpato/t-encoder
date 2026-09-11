//! Data models and domain types for Sonos devices.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use core::net::Ipv4Addr;

/// Current transport playback state of a Sonos speaker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportState {
    /// Playback is stopped.
    Stopped,
    /// Currently playing media.
    Playing,
    /// Playback is paused.
    PausedPlayback,
    /// In transition between states (buffering, loading, etc.).
    Transitioning,
}

impl TransportState {
    /// Parse a transport state string from a Sonos response.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        if s.eq_ignore_ascii_case("PLAYING") {
            Some(Self::Playing)
        } else if s.eq_ignore_ascii_case("PAUSED_PLAYBACK") {
            Some(Self::PausedPlayback)
        } else if s.eq_ignore_ascii_case("STOPPED") {
            Some(Self::Stopped)
        } else if s.eq_ignore_ascii_case("TRANSITIONING") {
            Some(Self::Transitioning)
        } else {
            None
        }
    }

    /// Return string representation as required by Sonos SOAP API.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Playing => "PLAYING",
            Self::PausedPlayback => "PAUSED_PLAYBACK",
            Self::Stopped => "STOPPED",
            Self::Transitioning => "TRANSITIONING",
        }
    }
}

/// Sonos playback repeat and shuffle modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlayMode {
    /// Standard sequential playback without repeat.
    #[default]
    Normal,
    /// Repeat the entire queue or playlist.
    RepeatAll,
    /// Repeat the current track indefinitely.
    RepeatOne,
    /// Shuffle playlist without repeating tracks.
    ShuffleNoRepeat,
    /// Shuffle playlist and repeat when complete.
    Shuffle,
    /// Shuffle and repeat the current track.
    ShuffleRepeatOne,
}

impl PlayMode {
    /// Parse a play mode string from a Sonos response.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        if s.eq_ignore_ascii_case("NORMAL") {
            Some(Self::Normal)
        } else if s.eq_ignore_ascii_case("REPEAT_ALL") {
            Some(Self::RepeatAll)
        } else if s.eq_ignore_ascii_case("REPEAT_ONE") {
            Some(Self::RepeatOne)
        } else if s.eq_ignore_ascii_case("SHUFFLE_NOREPEAT") {
            Some(Self::ShuffleNoRepeat)
        } else if s.eq_ignore_ascii_case("SHUFFLE") {
            Some(Self::Shuffle)
        } else if s.eq_ignore_ascii_case("SHUFFLE_REPEAT_ONE") {
            Some(Self::ShuffleRepeatOne)
        } else {
            None
        }
    }

    /// Return string representation as required by Sonos SOAP API.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "NORMAL",
            Self::RepeatAll => "REPEAT_ALL",
            Self::RepeatOne => "REPEAT_ONE",
            Self::ShuffleNoRepeat => "SHUFFLE_NOREPEAT",
            Self::Shuffle => "SHUFFLE",
            Self::ShuffleRepeatOne => "SHUFFLE_REPEAT_ONE",
        }
    }
}

/// Volume audio channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VolumeChannel {
    /// Master speaker volume.
    #[default]
    Master,
    /// Left front audio channel.
    LeftFront,
    /// Right front audio channel.
    RightFront,
}

impl VolumeChannel {
    /// Return string representation as required by Sonos RenderingControl API.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Master => "Master",
            Self::LeftFront => "LF",
            Self::RightFront => "RF",
        }
    }
}

/// Metadata and position information for a track currently loaded on the speaker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackInfo {
    /// 1-based index of the track in current playlist or queue.
    pub track_number: u32,
    /// Total duration of the track in seconds.
    pub duration_seconds: u32,
    /// Elapsed playback time in seconds.
    pub elapsed_seconds: u32,
    /// Track title.
    pub title: String,
    /// Track artist or creator, if available.
    pub artist: Option<String>,
    /// Track album title, if available.
    pub album: Option<String>,
    /// Relative or absolute URI to album cover art.
    pub album_art_uri: Option<String>,
    /// Media URI (e.g. `x-sonos-spotify:...`).
    pub uri: String,
}

/// An individual speaker in a Sonos household.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZoneMember {
    /// Unique RINCON device identifier.
    pub uuid: String,
    /// User-visible name of the room or zone (e.g. "Kitchen", "Living Room").
    pub name: String,
    /// Full HTTP location URL of the device description (e.g. `http://192.168.9.105:1400/xml/device_description.xml`).
    pub location_url: String,
    /// IPv4 address extracted from location URL.
    pub ip: Ipv4Addr,
    /// True if this member is the coordinator of its zone group.
    pub is_coordinator: bool,
    /// True if this speaker is an invisible satellite (e.g., surround or sub).
    pub invisible: bool,
}

/// A group of one or more Sonos speakers synchronized together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZoneGroup {
    /// Unique group identifier (e.g. `RINCON_...:171`).
    pub id: String,
    /// UUID of the group coordinator speaker.
    pub coordinator_uuid: String,
    /// IPv4 address of the group coordinator.
    pub coordinator_ip: Option<Ipv4Addr>,
    /// Name of the zone group (derived from coordinator room name).
    pub name: String,
    /// All members of this group, including satellites.
    pub members: Vec<ZoneMember>,
}

/// Full topology of all rooms and groups in a Sonos household.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HouseholdTopology {
    /// All active zone groups in the household.
    pub groups: Vec<ZoneGroup>,
}

impl HouseholdTopology {
    /// Find a group by room/zone name (case-insensitive).
    #[must_use]
    pub fn find_group_by_name(&self, name: &str) -> Option<&ZoneGroup> {
        self.groups
            .iter()
            .find(|g| g.name.eq_ignore_ascii_case(name))
    }

    /// Find the coordinator IP address responsible for controlling a given room.
    #[must_use]
    pub fn find_coordinator_ip_for_zone(&self, zone_name: &str) -> Option<Ipv4Addr> {
        for group in &self.groups {
            let has_member = group
                .members
                .iter()
                .any(|m| m.name.eq_ignore_ascii_case(zone_name));
            if has_member {
                return group.coordinator_ip;
            }
        }
        None
    }
}

/// Real-time event notifications received via UPnP GENA push.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SonosEvent {
    /// Playback state changed.
    TransportStateChanged {
        /// New transport state.
        state: TransportState,
    },
    /// Currently playing track metadata changed.
    TrackChanged {
        /// Updated track info, or `None` if stopped or unparseable.
        track: Option<TrackInfo>,
    },
    /// Volume changed on a specific channel.
    VolumeChanged {
        /// Channel adjusted.
        channel: VolumeChannel,
        /// New volume level (0..=100).
        volume: u16,
    },
    /// Mute state changed.
    MuteChanged {
        /// Channel adjusted.
        channel: VolumeChannel,
        /// True if muted.
        mute: bool,
    },
    /// Group membership or coordinators changed.
    TopologyChanged,
}

/// Parse a time string formatted as `H:MM:SS` or `MM:SS` into total seconds.
#[must_use]
pub fn parse_duration_seconds(s: &str) -> Option<u32> {
    if s.is_empty() || s.eq_ignore_ascii_case("not_implemented") {
        return None;
    }

    let mut parts = s.split(':');
    let first = parts.next()?.parse::<u32>().ok()?;
    let second = parts.next();
    let third = parts.next();
    if parts.next().is_some() {
        return None;
    }

    match (second, third) {
        (None, None) => Some(first),
        (Some(sec_part), None) => {
            let seconds = sec_part.parse::<u32>().ok()?;
            let minutes = first;
            minutes.checked_mul(60)?.checked_add(seconds)
        }
        (Some(min_part), Some(sec_part)) => {
            let hours = first;
            let minutes = min_part.parse::<u32>().ok()?;
            let seconds = sec_part.parse::<u32>().ok()?;
            let total_hours = hours.checked_mul(3600)?;
            let total_minutes = minutes.checked_mul(60)?;
            total_hours.checked_add(total_minutes)?.checked_add(seconds)
        }
        (None, Some(_)) => None,
    }
}

/// Extract IPv4 address from a UPnP location URL (e.g. `http://192.168.9.105:1400/...`).
#[must_use]
pub fn extract_ip_from_location(location: &str) -> Option<Ipv4Addr> {
    let after_scheme = location.strip_prefix("http://")?;
    let host_part = match after_scheme.find('/') {
        Some(pos) => &after_scheme[..pos],
        None => after_scheme,
    };
    let ip_str = match host_part.find(':') {
        Some(pos) => &host_part[..pos],
        None => host_part,
    };
    ip_str.parse::<Ipv4Addr>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_duration_seconds() {
        assert_eq!(parse_duration_seconds("0:08:01"), Some(481));
        assert_eq!(parse_duration_seconds("0:00:00"), Some(0));
        assert_eq!(parse_duration_seconds("1:30:15"), Some(5415));
        assert_eq!(parse_duration_seconds("04:20"), Some(260));
        assert_eq!(parse_duration_seconds("NOT_IMPLEMENTED"), None);
        assert_eq!(parse_duration_seconds(""), None);
        assert_eq!(parse_duration_seconds("invalid"), None);
    }

    #[test]
    fn test_extract_ip_from_location() {
        assert_eq!(
            extract_ip_from_location("http://192.168.9.105:1400/xml/device_description.xml"),
            Some(Ipv4Addr::new(192, 168, 9, 105))
        );
        assert_eq!(
            extract_ip_from_location("http://10.0.0.1/desc.xml"),
            Some(Ipv4Addr::new(10, 0, 0, 1))
        );
        assert_eq!(extract_ip_from_location("ftp://192.168.1.1"), None);
        assert_eq!(extract_ip_from_location("invalid"), None);
    }

    #[test]
    fn test_transport_state_parse_and_as_str() {
        assert_eq!(
            TransportState::parse("PLAYING"),
            Some(TransportState::Playing)
        );
        assert_eq!(
            TransportState::parse("playing"),
            Some(TransportState::Playing)
        );
        assert_eq!(
            TransportState::parse("PAUSED_PLAYBACK"),
            Some(TransportState::PausedPlayback)
        );
        assert_eq!(
            TransportState::parse("STOPPED"),
            Some(TransportState::Stopped)
        );
        assert_eq!(
            TransportState::parse("TRANSITIONING"),
            Some(TransportState::Transitioning)
        );
        assert_eq!(TransportState::parse("UNKNOWN"), None);

        assert_eq!(TransportState::Playing.as_str(), "PLAYING");
        assert_eq!(TransportState::PausedPlayback.as_str(), "PAUSED_PLAYBACK");
    }

    #[test]
    fn test_play_mode_parse_and_as_str() {
        assert_eq!(PlayMode::parse("NORMAL"), Some(PlayMode::Normal));
        assert_eq!(PlayMode::parse("REPEAT_ALL"), Some(PlayMode::RepeatAll));
        assert_eq!(PlayMode::parse("SHUFFLE"), Some(PlayMode::Shuffle));
        assert_eq!(PlayMode::parse("unknown"), None);

        assert_eq!(PlayMode::RepeatAll.as_str(), "REPEAT_ALL");
    }
}
