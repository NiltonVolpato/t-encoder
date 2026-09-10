//! Shared wire types for the T-Encoder-Pro NDJSON communication protocol.

#![no_std]

extern crate alloc;

use alloc::string::String;

/// Swipe direction for touch gestures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwipeDirection {
    /// Swipe right-to-left across the panel (back).
    Left,
    /// Swipe left-to-right across the panel.
    Right,
    /// Swipe bottom-to-top across the panel (exit).
    Up,
    /// Swipe top-to-bottom across the panel.
    Down,
}

impl SwipeDirection {
    /// Lowercase string representation.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Up => "up",
            Self::Down => "down",
        }
    }
}

/// A specific touch or mechanical input cue to inject into the device.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum CueCommand {
    /// Turn rotary dial by delta steps.
    Rotate { delta: i32 },
    /// Button short press.
    Press,
    /// Button long press.
    LongPress,
    /// Screen touch tap at coordinates.
    Tap { x: i32, y: i32 },
    /// Screen touch swipe in direction.
    Swipe { direction: SwipeDirection },
}

/// Incoming commands sent from host to device.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum IncomingCommand {
    /// Cue an input event.
    #[serde(rename = "cue")]
    Cue {
        /// The specific input action and arguments.
        #[serde(flatten)]
        cue: CueCommand,
    },
    /// Request a full framebuffer screenshot.
    Screenshot,
    /// Trigger a software reset.
    Reset,
}

/// A structured log record emitted by the device.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LogRecord {
    /// Microseconds elapsed since device boot.
    pub ts_us: u64,
    /// Log level string (e.g. "INFO", "WARN", "ERROR").
    pub level: String,
    /// Logging target module.
    pub target: String,
    /// Formatted log message body.
    pub msg: String,
}

/// High-level device events emitted to the host for event-driven testing and telemetry.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "name", rename_all = "snake_case")]
pub enum DeviceEvent {
    /// Device finished initialization and entered main loop.
    Boot { ready: bool },
    /// Buzzer/haptic tone played.
    Buzzer { freq_hz: u32, duration_ms: u32 },
    /// Active screen or launcher selection changed.
    ViewChanged {
        screen: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        card: Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        app: Option<String>,
    },
}

/// Command execution response status.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CommandResponse {
    /// Whether command succeeded.
    pub ok: bool,
    /// Microseconds elapsed on device since boot when the command was processed.
    #[serde(default)]
    pub ts_us: u64,
    /// Optional error description on failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Complete screenshot frame representation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ScreenshotMessage {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Microseconds elapsed on device since boot when the screenshot was captured.
    #[serde(default)]
    pub ts_us: u64,
    /// Base64 encoded big-endian RGB565 pixel payload.
    pub data: String,
}

/// Outgoing message from device to host over NDJSON.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DeviceMessage {
    /// Diagnostic log line.
    Log(LogRecord),
    /// Domain or lifecycle event.
    Event(DeviceEvent),
    /// Framebuffer capture.
    Screenshot(ScreenshotMessage),
    /// Command acknowledgement or error.
    Response(CommandResponse),
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn test_cue_rotation_serialization() {
        let cmd = IncomingCommand::Cue {
            cue: CueCommand::Rotate { delta: -2 },
        };
        let s = serde_json::to_string(&cmd).unwrap();
        assert_eq!(s, r#"{"cmd":"cue","action":"rotate","delta":-2}"#);

        let parsed: IncomingCommand = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed, cmd);
    }

    #[test]
    fn test_cue_swipe_serialization() {
        let cmd = IncomingCommand::Cue {
            cue: CueCommand::Swipe {
                direction: SwipeDirection::Left,
            },
        };
        let s = serde_json::to_string(&cmd).unwrap();
        assert_eq!(s, r#"{"cmd":"cue","action":"swipe","direction":"left"}"#);

        let parsed: IncomingCommand = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed, cmd);
    }

    #[test]
    fn test_device_event_boot_serialization() {
        let event = DeviceMessage::Event(DeviceEvent::Boot { ready: true });
        let s = serde_json::to_string(&event).unwrap();
        assert_eq!(s, r#"{"type":"event","name":"boot","ready":true}"#);

        let parsed: DeviceMessage = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed, event);
    }

    #[test]
    fn test_device_event_buzzer_serialization() {
        let event = DeviceMessage::Event(DeviceEvent::Buzzer {
            freq_hz: 440,
            duration_ms: 50,
        });
        let s = serde_json::to_string(&event).unwrap();
        assert_eq!(
            s,
            r#"{"type":"event","name":"buzzer","freq_hz":440,"duration_ms":50}"#
        );

        let parsed: DeviceMessage = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed, event);
    }

    #[test]
    fn test_device_log_serialization() {
        let log = DeviceMessage::Log(LogRecord {
            ts_us: 100_000,
            level: "INFO".to_string(),
            target: "main".to_string(),
            msg: "boot: ready".to_string(),
        });
        let s = serde_json::to_string(&log).unwrap();
        assert_eq!(
            s,
            r#"{"type":"log","ts_us":100000,"level":"INFO","target":"main","msg":"boot: ready"}"#
        );

        let parsed: DeviceMessage = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed, log);
    }

    #[test]
    fn test_serde_json_roundtrip() {
        let cmd = IncomingCommand::Cue {
            cue: CueCommand::Rotate { delta: 3 },
        };
        let s = serde_json::to_string(&cmd).unwrap();
        assert_eq!(s, r#"{"cmd":"cue","action":"rotate","delta":3}"#);

        let parsed: IncomingCommand = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed, cmd);
    }
}
