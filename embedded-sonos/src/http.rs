//! Embedded HTTP/1.1 wire codec over `embedded-io-async`.

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write;
use core::net::Ipv4Addr;
use embedded_io_async::{Error, Read, Write as AsyncWrite};

use crate::error::{Result, SonosError};
use crate::soap::{Action, BufferCursor};

/// Parsed HTTP response headers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseHeaders {
    /// HTTP status code (e.g. 200, 500).
    pub status_code: u16,
    /// Content length in bytes, if header present.
    pub content_length: Option<usize>,
    /// True if `Transfer-Encoding: chunked` was specified.
    pub is_chunked: bool,
    /// Subscription ID (SID) returned by GENA SUBSCRIBE.
    pub sid: Option<String>,
    /// Timeout in seconds granted by GENA SUBSCRIBE.
    pub timeout_seconds: Option<u32>,
}

/// Send a SOAP action POST request over an async writer.
pub async fn send_soap_request<W: AsyncWrite>(
    writer: &mut W,
    host: Ipv4Addr,
    port: u16,
    action: &Action<'_>,
) -> Result<()> {
    let envelope_len = action.envelope_length();
    let service_urn = action.service().urn();
    let control_url = action.service().control_url();
    let action_name = action.action_name();

    // Format headers into a small stack buffer.
    let mut header_buf = [0u8; 512];
    let mut cursor = BufferCursor::new(&mut header_buf);
    write!(
        cursor,
        "POST {control_url} HTTP/1.1\r\nHost: {host}:{port}\r\nContent-Type: text/xml; charset=\"utf-8\"\r\nSOAPAction: \"{service_urn}#{action_name}\"\r\nContent-Length: {envelope_len}\r\nConnection: close\r\n\r\n"
    )
    .map_err(|_| SonosError::BufferTooSmall)?;

    let header_bytes = cursor.written();
    writer
        .write_all(header_bytes)
        .await
        .map_err(|e| SonosError::Io(e.kind()))?;

    // Write SOAP envelope directly into the stream asynchronously.
    action.write_soap_envelope_async(writer).await?;

    Ok(())
}

/// Send an HTTP GET request (e.g. for album art or XML description).
pub async fn send_get_request<W: AsyncWrite>(
    writer: &mut W,
    host: Ipv4Addr,
    port: u16,
    path: &str,
) -> Result<()> {
    let mut header_buf = [0u8; 512];
    let mut cursor = BufferCursor::new(&mut header_buf);
    write!(
        cursor,
        "GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n"
    )
    .map_err(|_| SonosError::BufferTooSmall)?;

    let header_bytes = cursor.written();
    writer
        .write_all(header_bytes)
        .await
        .map_err(|e| SonosError::Io(e.kind()))?;

    Ok(())
}

/// Send a GENA SUBSCRIBE request.
pub async fn send_subscribe_request<W: AsyncWrite>(
    writer: &mut W,
    host: Ipv4Addr,
    port: u16,
    event_sub_url: &str,
    callback_url: &str,
    timeout_seconds: u32,
) -> Result<()> {
    let mut header_buf = [0u8; 512];
    let mut cursor = BufferCursor::new(&mut header_buf);
    write!(
        cursor,
        "SUBSCRIBE {event_sub_url} HTTP/1.1\r\nHost: {host}:{port}\r\nCallback: <{callback_url}>\r\nNT: upnp:event\r\nTimeout: Second-{timeout_seconds}\r\nConnection: close\r\n\r\n"
    )
    .map_err(|_| SonosError::BufferTooSmall)?;

    let header_bytes = cursor.written();
    writer
        .write_all(header_bytes)
        .await
        .map_err(|e| SonosError::Io(e.kind()))?;

    Ok(())
}

/// Send a GENA renewal SUBSCRIBE request.
pub async fn send_resubscribe_request<W: AsyncWrite>(
    writer: &mut W,
    host: Ipv4Addr,
    port: u16,
    event_sub_url: &str,
    sid: &str,
    timeout_seconds: u32,
) -> Result<()> {
    let mut header_buf = [0u8; 512];
    let mut cursor = BufferCursor::new(&mut header_buf);
    write!(
        cursor,
        "SUBSCRIBE {event_sub_url} HTTP/1.1\r\nHost: {host}:{port}\r\nSID: {sid}\r\nTimeout: Second-{timeout_seconds}\r\nConnection: close\r\n\r\n"
    )
    .map_err(|_| SonosError::BufferTooSmall)?;

    let header_bytes = cursor.written();
    writer
        .write_all(header_bytes)
        .await
        .map_err(|e| SonosError::Io(e.kind()))?;

    Ok(())
}

/// Send a GENA UNSUBSCRIBE request.
pub async fn send_unsubscribe_request<W: AsyncWrite>(
    writer: &mut W,
    host: Ipv4Addr,
    port: u16,
    event_sub_url: &str,
    sid: &str,
) -> Result<()> {
    let mut header_buf = [0u8; 512];
    let mut cursor = BufferCursor::new(&mut header_buf);
    write!(
        cursor,
        "UNSUBSCRIBE {event_sub_url} HTTP/1.1\r\nHost: {host}:{port}\r\nSID: {sid}\r\nConnection: close\r\n\r\n"
    )
    .map_err(|_| SonosError::BufferTooSmall)?;

    let header_bytes = cursor.written();
    writer
        .write_all(header_bytes)
        .await
        .map_err(|e| SonosError::Io(e.kind()))?;

    Ok(())
}

/// Read HTTP response headers from a reader until `\r\n\r\n`.
/// Returns the parsed headers and any leftover body bytes already read into `header_buf`.
pub async fn read_response_headers<R: Read>(
    reader: &mut R,
    header_buf: &mut [u8],
) -> Result<(ResponseHeaders, usize)> {
    let mut total_read = 0;
    let header_end_offset;

    loop {
        if total_read >= header_buf.len() {
            return Err(SonosError::BufferTooSmall);
        }

        let slice_to_read = header_buf
            .get_mut(total_read..)
            .ok_or(SonosError::BufferTooSmall)?;

        let n = reader
            .read(slice_to_read)
            .await
            .map_err(|e| SonosError::Io(e.kind()))?;

        if n == 0 {
            return Err(SonosError::Http {
                status_code: 0,
                message: "Connection closed while reading HTTP headers",
            });
        }

        total_read = match total_read.checked_add(n) {
            Some(sum) => sum,
            None => return Err(SonosError::BufferTooSmall),
        };

        let filled_headers = header_buf
            .get(..total_read)
            .ok_or(SonosError::BufferTooSmall)?;
        if let Some(pos) = find_header_end(filled_headers) {
            header_end_offset = pos;
            break;
        }
    }

    let header_bytes = header_buf
        .get(..header_end_offset)
        .ok_or(SonosError::BufferTooSmall)?;
    let header_str = core::str::from_utf8(header_bytes)
        .map_err(|_| SonosError::Parse("HTTP headers are not valid UTF-8"))?;

    let parsed = parse_headers_str(header_str)?;
    let Some(body_start) = header_end_offset.checked_add(4) else {
        return Err(SonosError::BufferTooSmall);
    };
    let leftover_len = total_read.saturating_sub(body_start);

    // Shift leftover body bytes to the beginning of header_buf so caller can use them.
    if leftover_len > 0 {
        header_buf.copy_within(body_start..total_read, 0);
    }

    Ok((parsed, leftover_len))
}

/// Read entire HTTP response body into an allocated `Vec<u8>`.
pub async fn read_response_body<R: Read>(
    reader: &mut R,
    initial_bytes: &[u8],
    headers: &ResponseHeaders,
    max_bytes: usize,
) -> Result<Vec<u8>> {
    let capacity = headers.content_length.unwrap_or(initial_bytes.len());
    let mut body = Vec::with_capacity(capacity.min(max_bytes));
    let mut writer = VecWriter(&mut body, max_bytes);
    let mut prefixed = PrefixedRead::new(initial_bytes, reader);

    if headers.is_chunked {
        stream_chunked_body(&mut prefixed, &mut writer).await?;
    } else {
        stream_fixed_body(&mut prefixed, headers.content_length, &mut writer).await?;
    }

    Ok(body)
}

/// Stream response body directly into a writer sink (e.g. for album art).
pub async fn stream_response_body<R: Read, W: AsyncWrite>(
    reader: &mut R,
    initial_bytes: &[u8],
    headers: &ResponseHeaders,
    writer: &mut W,
) -> Result<usize> {
    let mut prefixed = PrefixedRead::new(initial_bytes, reader);
    if headers.is_chunked {
        stream_chunked_body(&mut prefixed, writer).await
    } else {
        stream_fixed_body(&mut prefixed, headers.content_length, writer).await
    }
}

/// A reader that consumes from a prefix buffer before falling back to an underlying stream.
struct PrefixedRead<'a, R> {
    prefix: &'a [u8],
    reader: &'a mut R,
}

impl<'a, R: Read> PrefixedRead<'a, R> {
    fn new(prefix: &'a [u8], reader: &'a mut R) -> Self {
        Self { prefix, reader }
    }
}

impl<R: Read> embedded_io_async::ErrorType for PrefixedRead<'_, R> {
    type Error = R::Error;
}

impl<R: Read> Read for PrefixedRead<'_, R> {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        if !self.prefix.is_empty() {
            let n = self.prefix.len().min(buf.len());
            if let (Some(dst), Some(src)) = (buf.get_mut(..n), self.prefix.get(..n)) {
                dst.copy_from_slice(src);
            }
            self.prefix = self.prefix.get(n..).unwrap_or(&[]);
            return Ok(n);
        }
        self.reader.read(buf).await
    }
}

/// Adapter allowing `Vec<u8>` to act as an `AsyncWrite` sink with a maximum limit.
struct VecWriter<'a>(&'a mut Vec<u8>, usize);

impl embedded_io_async::ErrorType for VecWriter<'_> {
    type Error = embedded_io_async::ErrorKind;
}

impl AsyncWrite for VecWriter<'_> {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        if self.0.len().saturating_add(buf.len()) > self.1 {
            return Err(embedded_io_async::ErrorKind::OutOfMemory);
        }
        self.0.extend_from_slice(buf);
        Ok(buf.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// Stream non-chunked body bytes.
async fn stream_fixed_body<R: Read, W: AsyncWrite>(
    reader: &mut R,
    content_length: Option<usize>,
    writer: &mut W,
) -> Result<usize> {
    let mut total_written = 0;
    let mut buf = [0u8; 1024];

    while match content_length {
        Some(expected) => total_written < expected,
        None => true,
    } {
        let max_to_read = match content_length {
            Some(expected) => expected.saturating_sub(total_written).min(buf.len()),
            None => buf.len(),
        };

        let slice_to_read = buf
            .get_mut(..max_to_read)
            .ok_or(SonosError::BufferTooSmall)?;
        let n = reader
            .read(slice_to_read)
            .await
            .map_err(|e| SonosError::Io(e.kind()))?;

        if n == 0 {
            break;
        }

        let written_slice = buf.get(..n).ok_or(SonosError::BufferTooSmall)?;
        writer
            .write_all(written_slice)
            .await
            .map_err(|e| SonosError::Io(e.kind()))?;

        total_written = match total_written.checked_add(n) {
            Some(sum) => sum,
            None => return Err(SonosError::BufferTooSmall),
        };
    }

    Ok(total_written)
}

/// Stream HTTP chunked transfer encoding body bytes.
async fn stream_chunked_body<R: Read, W: AsyncWrite>(
    reader: &mut R,
    writer: &mut W,
) -> Result<usize> {
    let mut total_written: usize = 0;
    let mut line_buf = [0u8; 64];

    loop {
        let line_len = read_crlf_line(reader, &mut line_buf).await?;
        if line_len < 2 {
            return Err(SonosError::Parse("Truncated chunk header"));
        }

        let chunk_header_bytes = line_buf
            .get(..line_len.saturating_sub(2))
            .ok_or(SonosError::Parse("Invalid chunk header length"))?;
        let line_str = core::str::from_utf8(chunk_header_bytes)
            .map_err(|_| SonosError::Parse("Invalid chunk header UTF-8"))?;
        let hex_str = line_str.split(';').next().unwrap_or("").trim();
        let chunk_size = usize::from_str_radix(hex_str, 16)
            .map_err(|_| SonosError::Parse("Invalid chunk length hex"))?;

        if chunk_size == 0 {
            // Read trailing empty CRLF
            let _ = read_crlf_line(reader, &mut line_buf).await?;
            break;
        }

        let mut remaining = chunk_size;
        let mut buf = [0u8; 1024];

        while remaining > 0 {
            let to_read = remaining.min(buf.len());
            let slice_to_read = buf.get_mut(..to_read).ok_or(SonosError::BufferTooSmall)?;
            let n = reader
                .read(slice_to_read)
                .await
                .map_err(|e| SonosError::Io(e.kind()))?;

            if n == 0 {
                return Err(SonosError::Http {
                    status_code: 0,
                    message: "Unexpected EOF inside chunk data",
                });
            }

            let written_slice = buf.get(..n).ok_or(SonosError::BufferTooSmall)?;
            writer
                .write_all(written_slice)
                .await
                .map_err(|e| SonosError::Io(e.kind()))?;

            total_written = match total_written.checked_add(n) {
                Some(sum) => sum,
                None => return Err(SonosError::BufferTooSmall),
            };
            remaining -= n;
        }

        // Consume chunk trailing CRLF
        let cr = read_one_byte(reader).await?;
        let lf = read_one_byte(reader).await?;
        if cr != Some(b'\r') || lf != Some(b'\n') {
            return Err(SonosError::Parse("Expected CRLF after chunk data"));
        }
    }

    Ok(total_written)
}

/// Read single byte from stream.
async fn read_one_byte<R: Read>(reader: &mut R) -> Result<Option<u8>> {
    let mut b = [0u8; 1];
    match reader.read(&mut b).await {
        Ok(0) => Ok(None),
        Ok(1) => Ok(Some(b[0])),
        Ok(_) => unreachable!(),
        Err(e) => Err(SonosError::Io(e.kind())),
    }
}

/// Read a line ending in CRLF into `buf`.
async fn read_crlf_line<R: Read>(reader: &mut R, buf: &mut [u8]) -> Result<usize> {
    let mut pos = 0;
    while pos < buf.len() {
        match read_one_byte(reader).await? {
            Some(b) => {
                let slot = buf.get_mut(pos).ok_or(SonosError::BufferTooSmall)?;
                *slot = b;
                pos += 1;
                if buf.get(pos.saturating_sub(2)..pos) == Some(b"\r\n") {
                    return Ok(pos);
                }
            }
            None => return Ok(pos),
        }
    }
    Err(SonosError::BufferTooSmall)
}

/// Find index of `\r\n\r\n` sequence in slice.
fn find_header_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|w| w == b"\r\n\r\n")
}

/// Parse HTTP status line and header lines.
fn parse_headers_str(s: &str) -> Result<ResponseHeaders> {
    let mut lines = s.split("\r\n");
    let status_line = lines
        .next()
        .ok_or(SonosError::Parse("Missing HTTP status line"))?;

    // Example: "HTTP/1.1 200 OK"
    let mut parts = status_line.split_whitespace();
    let _version = parts.next();
    let status_code = parts
        .next()
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or(SonosError::Parse("Invalid HTTP status code"))?;

    let mut content_length = None;
    let mut is_chunked = false;
    let mut sid = None;
    let mut timeout_seconds = None;

    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some((name, val)) = line.split_once(':') {
            let name = name.trim();
            let val = val.trim();

            if name.eq_ignore_ascii_case("Content-Length") {
                content_length = val.parse::<usize>().ok();
            } else if name.eq_ignore_ascii_case("Transfer-Encoding") {
                if val.eq_ignore_ascii_case("chunked") {
                    is_chunked = true;
                }
            } else if name.eq_ignore_ascii_case("SID") {
                sid = Some(val.to_string());
            } else if name.eq_ignore_ascii_case("Timeout") {
                timeout_seconds = val
                    .strip_prefix("Second-")
                    .and_then(|sec| sec.parse::<u32>().ok());
            }
        }
    }

    Ok(ResponseHeaders {
        status_code,
        content_length,
        is_chunked,
        sid,
        timeout_seconds,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_headers_200_ok() {
        let raw = "HTTP/1.1 200 OK\r\nCONTENT-LENGTH: 399\r\nCONTENT-TYPE: text/xml; charset=\"utf-8\"\r\nConnection: close\r\n\r\n";
        let headers = parse_headers_str(raw).unwrap();
        assert_eq!(headers.status_code, 200);
        assert_eq!(headers.content_length, Some(399));
        assert_eq!(headers.sid, None);
    }

    #[test]
    fn test_parse_headers_subscribe_response() {
        let raw = "HTTP/1.1 200 OK\r\nServer: Linux UPnP/1.0 Sonos/86.8\r\nSID: uuid:RINCON_B8E937D9C81601400_sub123\r\nTIMEOUT: Second-1800\r\nContent-Length: 0\r\n\r\n";
        let headers = parse_headers_str(raw).unwrap();
        assert_eq!(headers.status_code, 200);
        assert_eq!(headers.content_length, Some(0));
        assert_eq!(
            headers.sid.as_deref(),
            Some("uuid:RINCON_B8E937D9C81601400_sub123")
        );
        assert_eq!(headers.timeout_seconds, Some(1800));
    }

    #[test]
    fn test_parse_headers_500_fault() {
        let raw = "HTTP/1.1 500 Internal Server Error\r\nCONTENT-LENGTH: 347\r\n\r\n";
        let headers = parse_headers_str(raw).unwrap();
        assert_eq!(headers.status_code, 500);
        assert_eq!(headers.content_length, Some(347));
    }
}
