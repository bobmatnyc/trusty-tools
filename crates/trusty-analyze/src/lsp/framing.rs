//! LSP base-protocol framing: `Content-Length` headers around a JSON body.
//!
//! Why: a stdio pipe delivers bytes in arbitrary chunks. One read can hold half
//! a header or two whole messages, so framing must be a buffer, not a
//! line reader (#9078).
//! What: [`FrameDecoder`] accumulates bytes and yields whole messages; [`encode`]
//! writes one. A malformed or oversized frame is a [`LspError::Protocol`],
//! because a byte stream that lost its framing cannot be resynchronised.
//! Test: `framing_round_trips_a_message`,
//! `framing_reassembles_a_frame_split_across_reads`,
//! `framing_yields_two_messages_from_one_read`, `framing_rejects_malformed_frames`.

use serde_json::Value;

use super::LspError;

/// The largest body this client accepts (64 MiB).
///
/// Why: a corrupt length must not make the client allocate without bound. A
/// `textDocument/references` answer over a large workspace stays far below this.
pub const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

/// The longest header block accepted before its blank-line terminator.
const MAX_HEADER_BYTES: usize = 8 * 1024;

const TERMINATOR: &[u8] = b"\r\n\r\n";

/// Serialise `msg` as one framed LSP message.
///
/// Test: `framing_round_trips_a_message`.
pub fn encode(msg: &Value) -> Result<Vec<u8>, LspError> {
    let body = serde_json::to_vec(msg)
        .map_err(|e| LspError::Protocol(format!("cannot encode message: {e}")))?;
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(&body);
    Ok(out)
}

/// Incremental decoder over a byte stream.
///
/// Why: see the module doc. What: [`extend`](Self::extend) appends bytes and
/// [`next_message`](Self::next_message) pops one complete message, or `None`
/// until more bytes arrive.
/// Test: `framing_reassembles_a_frame_split_across_reads`.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buf: Vec<u8>,
}

impl FrameDecoder {
    /// Append bytes read from the stream.
    pub fn extend(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Bytes held that do not yet form a whole message.
    pub fn buffered(&self) -> usize {
        self.buf.len()
    }

    /// Pop the next whole message.
    ///
    /// # Errors
    /// [`LspError::Protocol`] for a missing or non-numeric `Content-Length`, a
    /// header line without a colon, an oversized header block or body, or a
    /// body that is not JSON.
    ///
    /// Test: `framing_rejects_malformed_frames`,
    /// `framing_rejects_an_unterminated_header_block`.
    pub fn next_message(&mut self) -> Result<Option<Value>, LspError> {
        let Some(end) = self
            .buf
            .windows(TERMINATOR.len())
            .position(|w| w == TERMINATOR)
        else {
            if self.buf.len() > MAX_HEADER_BYTES {
                return Err(protocol(format!(
                    "no header terminator within {MAX_HEADER_BYTES} bytes"
                )));
            }
            return Ok(None);
        };
        if end > MAX_HEADER_BYTES {
            return Err(protocol(format!("header block of {end} bytes")));
        }
        let len = content_length(&self.buf[..end])?;
        let body_start = end + TERMINATOR.len();
        if self.buf.len() < body_start + len {
            return Ok(None);
        }
        let frame: Vec<u8> = self.buf.drain(..body_start + len).collect();
        let value = serde_json::from_slice(&frame[body_start..])
            .map_err(|e| protocol(format!("frame body is not JSON: {e}")))?;
        Ok(Some(value))
    }
}

fn content_length(header: &[u8]) -> Result<usize, LspError> {
    let header =
        std::str::from_utf8(header).map_err(|_| protocol("header block is not UTF-8".into()))?;
    let mut len = None;
    for line in header.split("\r\n") {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| protocol(format!("header line without a colon: {line:?}")))?;
        if name.trim().eq_ignore_ascii_case("content-length") {
            let n = value
                .trim()
                .parse::<usize>()
                .map_err(|_| protocol(format!("bad Content-Length: {:?}", value.trim())))?;
            len = Some(n);
        }
    }
    let len = len.ok_or_else(|| protocol("missing Content-Length header".into()))?;
    if len > MAX_FRAME_BYTES {
        return Err(protocol(format!(
            "frame of {len} bytes exceeds the {MAX_FRAME_BYTES}-byte limit"
        )));
    }
    Ok(len)
}

fn protocol(reason: String) -> LspError {
    LspError::Protocol(reason)
}
