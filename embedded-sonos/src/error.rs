//! Error types for `embedded-sonos`.

extern crate alloc;

use alloc::string::String;
use core::fmt;

/// Result alias for `embedded-sonos` operations.
pub type Result<T, E = SonosError> = core::result::Result<T, E>;

/// Error variants encountered when communicating with a Sonos speaker.
#[derive(Debug)]
pub enum SonosError {
    /// UPnP SOAP error returned by the speaker.
    Upnp {
        /// UPnP error code (e.g. 701 for action not permitted).
        code: u16,
        /// Error description if provided in SOAP Fault.
        description: String,
    },
    /// HTTP protocol or status code error.
    Http {
        /// HTTP status code.
        status_code: u16,
        /// Reason or error message.
        message: &'static str,
    },
    /// XML parsing failure.
    Xml(String),
    /// Underflow or network I/O error.
    Io(embedded_io_async::ErrorKind),
    /// Data formatting error (e.g. unexpected time string or integer).
    Parse(&'static str),
    /// An expected field or element was absent from the response.
    MissingField(&'static str),
    /// Destination buffer is too small to complete the operation.
    BufferTooSmall,
}

impl fmt::Display for SonosError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Upnp { code, description } => {
                write!(f, "UPnP error {code}: {description}")
            }
            Self::Http {
                status_code,
                message,
            } => {
                write!(f, "HTTP error {status_code}: {message}")
            }
            Self::Xml(err) => write!(f, "XML parsing error: {err}"),
            Self::Io(kind) => write!(f, "I/O error: {kind:?}"),
            Self::Parse(msg) => write!(f, "Data parse error: {msg}"),
            Self::MissingField(field) => write!(f, "Missing expected field: {field}"),
            Self::BufferTooSmall => write!(f, "Buffer too small"),
        }
    }
}

impl core::error::Error for SonosError {}

impl From<embedded_io_async::ErrorKind> for SonosError {
    fn from(kind: embedded_io_async::ErrorKind) -> Self {
        Self::Io(kind)
    }
}

impl From<xml_no_std::reader::Error> for SonosError {
    fn from(err: xml_no_std::reader::Error) -> Self {
        use alloc::string::ToString;
        Self::Xml(err.to_string())
    }
}
