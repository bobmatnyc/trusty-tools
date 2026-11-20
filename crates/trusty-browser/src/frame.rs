//! Length-framed JSON, the wire format of every Trusty Browser socket.
//!
//! Why: the extension, native host and broker exchange frames whose bytes
//! Python 0.7.0 defines (`protocol.py` `read_frame`/`write_frame`), so the
//! Rust broker must accept and produce exactly the same bytes.
//! What: a frame is a 4-byte native-endian length then a compact JSON object
//! of 1..=1 MiB. Encoding uses Python's default `ensure_ascii=True`.
//! Test: `every_captured_frame_reencodes_byte_identically`,
//! `frame_limits_and_shapes_are_refused` in `tests/parity_frames.rs`.

use std::io::{self, Read, Write};

use crate::pyjson::Json;

/// Largest frame body, in bytes (`MAX_FRAME`).
pub const MAX_FRAME: usize = 1024 * 1024;
/// Header size (`struct.Struct("=I").size`).
pub const HEADER: usize = 4;

/// Why a frame could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    /// Size out of `1..=MAX_FRAME`, a non-object body, or a non-finite float:
    /// the broker answers `{"ok":false,"error":"InvalidFrame"}`.
    #[error("InvalidFrame")]
    Invalid,
    /// The body is not JSON (Python raises `JSONDecodeError`, not a refusal).
    #[error("frame body is not JSON: {0}")]
    Malformed(#[from] serde_json::Error),
    /// The stream failed or ended early.
    #[error("frame I/O: {0}")]
    Io(#[from] io::Error),
}

/// Encode one message as header plus body.
///
/// Why: the only writer of wire bytes. What: compact `ensure_ascii` JSON;
/// a body over `MAX_FRAME` or a NaN/inf float is `FrameError::Invalid`.
/// An empty object still yields a 2-byte body, so length 0 never leaves here.
/// Test: `every_captured_frame_reencodes_byte_identically`.
pub fn encode(message: &Json) -> Result<Vec<u8>, FrameError> {
    encode_as(message, true)
}

/// Encode with an explicit `ensure_ascii`; `false` gives the UTF-8 form the
/// extension and test clients send (`JSON.stringify`, `ensure_ascii=False`).
pub fn encode_as(message: &Json, ensure_ascii: bool) -> Result<Vec<u8>, FrameError> {
    let body = message
        .dumps(ensure_ascii)
        .map_err(|_| FrameError::Invalid)?;
    if body.len() > MAX_FRAME {
        return Err(FrameError::Invalid);
    }
    let size = u32::try_from(body.len()).map_err(|_| FrameError::Invalid)?;
    let mut out = Vec::with_capacity(HEADER + body.len());
    out.extend_from_slice(&size.to_ne_bytes());
    out.extend_from_slice(body.as_bytes());
    Ok(out)
}

/// Validate a header and return the body length it announces.
pub fn body_len(header: [u8; HEADER]) -> Result<usize, FrameError> {
    let size = u32::from_ne_bytes(header) as usize;
    if size == 0 || size > MAX_FRAME {
        return Err(FrameError::Invalid);
    }
    Ok(size)
}

/// Parse a body; it must be a JSON object.
pub fn decode_body(body: &[u8]) -> Result<Json, FrameError> {
    match Json::parse(body)? {
        frame @ Json::Object(_) => Ok(frame),
        _ => Err(FrameError::Invalid),
    }
}

/// Decode one complete frame held in `bytes` (header plus exactly one body).
pub fn decode(bytes: &[u8]) -> Result<Json, FrameError> {
    let (header, body) = bytes.split_at_checked(HEADER).ok_or(FrameError::Invalid)?;
    let header: [u8; HEADER] = header.try_into().map_err(|_| FrameError::Invalid)?;
    if body_len(header)? != body.len() {
        return Err(FrameError::Invalid);
    }
    decode_body(body)
}

/// Read one frame from a blocking stream.
pub fn read_frame(reader: &mut impl Read) -> Result<Json, FrameError> {
    let mut header = [0u8; HEADER];
    reader.read_exact(&mut header)?;
    let mut body = vec![0u8; body_len(header)?];
    reader.read_exact(&mut body)?;
    decode_body(&body)
}

/// Write one frame to a blocking stream.
pub fn write_frame(writer: &mut impl Write, message: &Json) -> Result<(), FrameError> {
    writer.write_all(&encode(message)?)?;
    writer.flush()?;
    Ok(())
}
