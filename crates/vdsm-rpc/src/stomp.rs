//! STOMP 1.2 frame types, serializer, and parser.
//!
//! ovirt-engine wraps its JSON-RPC requests in STOMP frames over the
//! same TLS connection vdsmd already terminates. This module is the
//! framing layer: bytes ↔ [`Frame`] structs. It does not know about
//! JSON-RPC; the next layer up (the server's connection handler) reads
//! STOMP frames, extracts the JSON body, dispatches via
//! [`crate::dispatch::Dispatcher`], and sends a `MESSAGE` frame back.
//!
//! Spec we follow: [STOMP 1.2](https://stomp.github.io/stomp-specification-1.2.html).
//! The two non-obvious bits:
//!
//!   - **Header escaping**: STOMP 1.2 escapes `\r`, `\n`, `:`, and `\\`
//!     in header values as `\\r`, `\\n`, `\\c`, `\\\\` respectively.
//!     The CONNECT frame is the only exception (treated as 1.0-style).
//!     We escape on serialize and unescape on parse uniformly; engine's
//!     CONNECT frame has no special characters so this is harmless.
//!   - **Heartbeats**: peers may emit bare `\n` or `\r\n` between frames
//!     for keepalive. We skip leading newlines before each parse.

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub command: String,
    /// Headers in insertion order. STOMP says first header wins on
    /// duplicate keys; [`Frame::header`] returns the first match.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Frame {
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    pub fn with_header(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.headers.push((k.into(), v.into()));
        self
    }

    pub fn with_body(mut self, b: impl Into<Vec<u8>>) -> Self {
        self.body = b.into();
        self
    }

    /// Returns the first header value matching `name` (case-sensitive
    /// per STOMP 1.2 section 1.4).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.command.len() + 64 + self.body.len());
        out.extend_from_slice(self.command.as_bytes());
        out.push(b'\n');
        for (k, v) in &self.headers {
            out.extend_from_slice(escape_header(k).as_bytes());
            out.push(b':');
            out.extend_from_slice(escape_header(v).as_bytes());
            out.push(b'\n');
        }
        // If a content-length header is missing AND the body contains
        // a NUL byte, parsers will truncate. The caller is responsible
        // for adding `content-length` for binary bodies; for JSON we
        // can rely on NUL-free UTF-8 and skip the header.
        out.push(b'\n');
        out.extend_from_slice(&self.body);
        out.push(0);
        out
    }
}

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("malformed frame: {0}")]
    Malformed(&'static str),
    #[error("invalid utf-8 in command or header: {0}")]
    Utf8(#[from] std::str::Utf8Error),
    #[error("declared content-length {declared} disagrees with body terminator at {actual}")]
    ContentLengthMismatch { declared: usize, actual: usize },
    #[error("invalid content-length header: {0}")]
    BadContentLength(String),
}

/// Try to parse one STOMP frame from `input`.
///
/// Returns:
///   - `Ok(Some((frame, remaining)))` — parsed a complete frame; `remaining`
///     is the unparsed tail (possibly the start of the next frame).
///   - `Ok(None)` — `input` is incomplete; caller should buffer more bytes
///     and retry.
///   - `Err(_)` — malformed frame; the caller should close the connection
///     (per STOMP, malformed frames are unrecoverable).
///
/// Skips leading heartbeat bytes (`\n` or `\r\n`) before the frame.
pub fn parse_frame(input: &[u8]) -> Result<Option<(Frame, &[u8])>, ParseError> {
    let mut cursor = skip_heartbeats(input);
    if cursor.is_empty() {
        return Ok(None);
    }

    // Command line.
    let Some(line_end) = memchr(b'\n', cursor) else {
        return Ok(None);
    };
    let command_line = strip_cr(&cursor[..line_end]);
    if command_line.is_empty() {
        return Err(ParseError::Malformed("empty command line"));
    }
    let command = std::str::from_utf8(command_line)?.to_string();
    cursor = &cursor[line_end + 1..];

    // Header lines until blank line.
    let mut headers: Vec<(String, String)> = Vec::new();
    let mut content_length: Option<usize> = None;
    loop {
        let Some(line_end) = memchr(b'\n', cursor) else {
            return Ok(None);
        };
        let raw_line = strip_cr(&cursor[..line_end]);
        cursor = &cursor[line_end + 1..];
        if raw_line.is_empty() {
            break;
        }
        let Some(colon) = raw_line.iter().position(|&b| b == b':') else {
            return Err(ParseError::Malformed("header line missing ':'"));
        };
        let key_raw = &raw_line[..colon];
        let val_raw = &raw_line[colon + 1..];
        let key = unescape_header(std::str::from_utf8(key_raw)?);
        let val = unescape_header(std::str::from_utf8(val_raw)?);
        if key == "content-length" && content_length.is_none() {
            content_length = Some(
                val.parse()
                    .map_err(|_| ParseError::BadContentLength(val.clone()))?,
            );
        }
        headers.push((key, val));
    }

    // Body.
    let (body, remaining) = match content_length {
        Some(n) => {
            // n bytes of body, then mandatory NUL.
            if cursor.len() < n + 1 {
                return Ok(None);
            }
            let body = cursor[..n].to_vec();
            if cursor[n] != 0 {
                return Err(ParseError::ContentLengthMismatch {
                    declared: n,
                    actual: cursor[..n].iter().position(|&b| b == 0).unwrap_or(n),
                });
            }
            (body, &cursor[n + 1..])
        }
        None => {
            let Some(nul) = memchr(0, cursor) else {
                return Ok(None);
            };
            (cursor[..nul].to_vec(), &cursor[nul + 1..])
        }
    };

    Ok(Some((
        Frame {
            command,
            headers,
            body,
        },
        remaining,
    )))
}

/// Escape a header value per STOMP 1.2 section 3.1: `\r` `\n` `:` `\\`
/// become `\\r` `\\n` `\\c` `\\\\`.
fn escape_header(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\r' => out.push_str("\\r"),
            '\n' => out.push_str("\\n"),
            ':' => out.push_str("\\c"),
            other => out.push(other),
        }
    }
    out
}

/// Reverse of [`escape_header`]. Unknown escapes (`\\x` for any other x)
/// are passed through verbatim — STOMP says clients SHOULD only emit
/// the four documented escapes, but lenient parsing avoids breakage.
fn unescape_header(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('\\') => out.push('\\'),
                Some('r') => out.push('\r'),
                Some('n') => out.push('\n'),
                Some('c') => out.push(':'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn skip_heartbeats(input: &[u8]) -> &[u8] {
    let mut i = 0;
    while i < input.len() {
        match input[i] {
            b'\n' => i += 1,
            b'\r' if i + 1 < input.len() && input[i + 1] == b'\n' => i += 2,
            _ => break,
        }
    }
    &input[i..]
}

fn strip_cr(line: &[u8]) -> &[u8] {
    if line.last() == Some(&b'\r') {
        &line[..line.len() - 1]
    } else {
        line
    }
}

fn memchr(needle: u8, haystack: &[u8]) -> Option<usize> {
    haystack.iter().position(|&b| b == needle)
}

/// Convenience builders for the handful of frames the JSON-RPC dispatcher
/// will routinely emit. Kept small on purpose; full STOMP coverage is not
/// needed because we never originate `CONNECT` / `SUBSCRIBE` frames as a
/// server.
pub mod build {
    use super::Frame;

    /// `CONNECTED` reply to a client `CONNECT` / `STOMP` frame.
    pub fn connected(version: &str, server: &str, session: Option<&str>) -> Frame {
        let mut f = Frame::new("CONNECTED")
            .with_header("version", version)
            .with_header("server", server)
            .with_header("heart-beat", "0,0");
        if let Some(s) = session {
            f = f.with_header("session", s);
        }
        f
    }

    /// `MESSAGE` frame carrying a JSON-RPC response body.
    pub fn message(destination: &str, subscription: &str, body: Vec<u8>) -> Frame {
        let len = body.len().to_string();
        Frame::new("MESSAGE")
            .with_header("destination", destination)
            .with_header("subscription", subscription)
            .with_header("content-type", "application/json")
            .with_header("content-length", len)
            .with_body(body)
    }

    /// `ERROR` frame, used to refuse a malformed CONNECT or unparseable
    /// body before closing the connection.
    pub fn error(message: &str, body: &str) -> Frame {
        Frame::new("ERROR")
            .with_header("message", message)
            .with_header("content-type", "text/plain")
            .with_header("content-length", body.len().to_string())
            .with_body(body.as_bytes().to_vec())
    }

    /// `RECEIPT` ack for a client SEND that requested one via `receipt:`.
    pub fn receipt(receipt_id: &str) -> Frame {
        Frame::new("RECEIPT").with_header("receipt-id", receipt_id)
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_minimal_command() {
        let f = Frame::new("DISCONNECT");
        let bytes = f.to_bytes();
        assert_eq!(bytes, b"DISCONNECT\n\n\0");
        let (parsed, rest) = parse_frame(&bytes).unwrap().unwrap();
        assert_eq!(parsed, f);
        assert!(rest.is_empty());
    }

    #[test]
    fn roundtrip_connect() {
        let f = Frame::new("CONNECT")
            .with_header("accept-version", "1.2")
            .with_header("host", "vdsmd")
            .with_header("heart-beat", "0,0");
        let bytes = f.to_bytes();
        let (parsed, _) = parse_frame(&bytes).unwrap().unwrap();
        assert_eq!(parsed, f);
        assert_eq!(parsed.header("accept-version"), Some("1.2"));
        assert_eq!(parsed.header("missing"), None);
    }

    #[test]
    fn roundtrip_send_with_json_body() {
        let body = br#"{"jsonrpc":"2.0","id":1,"method":"Host.ping2"}"#.to_vec();
        let f = Frame::new("SEND")
            .with_header("destination", "/queue/_local/vdsm/requests")
            .with_header("content-type", "application/json")
            .with_header("content-length", body.len().to_string())
            .with_body(body.clone());
        let bytes = f.to_bytes();
        let (parsed, rest) = parse_frame(&bytes).unwrap().unwrap();
        assert_eq!(parsed.command, "SEND");
        assert_eq!(parsed.body, body);
        assert!(rest.is_empty());
    }

    #[test]
    fn header_escape_roundtrip() {
        let f = Frame::new("MESSAGE")
            .with_header("subject", "weird:value\nnext\\line\r\n");
        let bytes = f.to_bytes();
        let (parsed, _) = parse_frame(&bytes).unwrap().unwrap();
        assert_eq!(parsed.header("subject"), Some("weird:value\nnext\\line\r\n"));
    }

    #[test]
    fn heartbeat_bytes_are_skipped() {
        let f = Frame::new("CONNECT").with_header("host", "vdsmd");
        let mut wire = vec![b'\n', b'\n', b'\r', b'\n']; // three heartbeats
        wire.extend_from_slice(&f.to_bytes());
        let (parsed, _) = parse_frame(&wire).unwrap().unwrap();
        assert_eq!(parsed, f);
    }

    #[test]
    fn incomplete_returns_none_not_error() {
        // Truncated mid-headers.
        let raw = b"CONNECT\nhost:vdsm";
        assert!(parse_frame(raw).unwrap().is_none());
        // Truncated mid-body.
        let raw = b"SEND\ncontent-length:10\n\n12345";
        assert!(parse_frame(raw).unwrap().is_none());
        // Truncated before terminating NUL (no content-length).
        let raw = b"SEND\n\n{\"x\":1}";
        assert!(parse_frame(raw).unwrap().is_none());
    }

    #[test]
    fn parses_two_frames_in_one_buffer() {
        let f1 = Frame::new("CONNECT").with_header("host", "a");
        let f2 = Frame::new("DISCONNECT");
        let mut buf = f1.to_bytes();
        buf.extend_from_slice(&f2.to_bytes());
        let (p1, rest) = parse_frame(&buf).unwrap().unwrap();
        assert_eq!(p1, f1);
        let (p2, rest) = parse_frame(rest).unwrap().unwrap();
        assert_eq!(p2, f2);
        assert!(rest.is_empty());
    }

    #[test]
    fn content_length_pins_body_length_even_with_embedded_nul() {
        // Body is "ab\0cd" (5 bytes including a NUL); without
        // content-length the parser would stop at the inner NUL.
        let body = b"ab\0cd".to_vec();
        let f = Frame::new("SEND")
            .with_header("destination", "/q")
            .with_header("content-length", body.len().to_string())
            .with_body(body.clone());
        let bytes = f.to_bytes();
        let (parsed, rest) = parse_frame(&bytes).unwrap().unwrap();
        assert_eq!(parsed.body, body);
        assert!(rest.is_empty());
    }

    #[test]
    fn malformed_header_rejected() {
        let raw = b"SEND\nno-colon-here\n\n\0";
        assert!(parse_frame(raw).is_err());
    }

    #[test]
    fn build_helpers_produce_parseable_frames() {
        let connected = build::connected("1.2", "vdsm-rs/0.1.0", Some("abc"));
        let bytes = connected.to_bytes();
        let (parsed, _) = parse_frame(&bytes).unwrap().unwrap();
        assert_eq!(parsed.command, "CONNECTED");
        assert_eq!(parsed.header("version"), Some("1.2"));
        assert_eq!(parsed.header("session"), Some("abc"));

        let msg = build::message("/q/r", "sub-1", b"{}".to_vec());
        let bytes = msg.to_bytes();
        let (parsed, _) = parse_frame(&bytes).unwrap().unwrap();
        assert_eq!(parsed.body, b"{}");
        assert_eq!(parsed.header("content-length"), Some("2"));
    }
}
