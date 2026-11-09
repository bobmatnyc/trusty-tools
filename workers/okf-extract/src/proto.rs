//! The worker side of the Appendix B wire format (SPEC-OKF-01 Appendix B).
//!
//! Why: the host and the worker must agree on every byte of a frame, and the
//! worker must not let a bad frame make it allocate without bound.
//! What: a frame is a 4-byte big-endian length, then that many bytes of JSON
//! header; an `extract` header is followed by `byte_len` raw input bytes that
//! the length does not count (the S10 client's resolution, `proto.rs` in
//! trusty-okf). [`read_request`] refuses a header over [`HEADER_CAP`] and an
//! input over [`INPUT_CAP`] before it allocates either. [`write_frame`] writes
//! one header-only frame and flushes it.
//! Test: `frames_round_trip_and_caps_hold`, `requests_parse_by_type`.
//!
//! # Spec References
//! - SPEC-OKF-01 Appendix B "Transport", "Messages", "Default limits".

use std::io::{self, Read, Write};

use serde::Deserialize;
use serde_json::Value;

/// The one protocol version this worker speaks.
pub const PROTOCOL: u32 = 1;

/// The largest header the host sends: its frames are small JSON objects.
pub const HEADER_CAP: u32 = 64 * 1024;

/// The compiled-in input ceiling (Appendix B "Default limits": 64 MiB).
pub const INPUT_CAP: u64 = 64 * 1024 * 1024;

/// The per-item output limits the host sends with each request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct Limits {
    /// Text bytes.
    pub output_bytes: u64,
    /// `fields` bytes as JSON.
    pub fields_bytes: u64,
    /// Number of notes.
    pub notes: usize,
}

/// The host's hint about the input's type.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct TypeHint {
    /// A MIME type, when the source reports one.
    #[serde(default)]
    pub mime: Option<String>,
    /// The lowercase extension without the dot; empty when none.
    #[serde(default)]
    pub extension: String,
    /// The first bytes of the input as hex.
    #[serde(default)]
    pub magic: String,
}

/// One `extract` request header.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ExtractHeader {
    /// Echoed in the reply.
    pub request_id: u64,
    /// The type hint.
    pub type_hint: TypeHint,
    /// The number of raw bytes after the header.
    pub byte_len: u64,
    /// The host's SHA-256 of the bytes. The worker echoes its own hash, not
    /// this one, so the host can check the bytes arrived intact.
    #[allow(dead_code)]
    pub sha256: String,
    /// Output limits for the reply.
    pub limits: Limits,
}

/// A host message.
#[derive(Debug, PartialEq, Eq)]
pub enum Request {
    /// `hello` with the host's protocol range.
    Hello {
        /// The oldest version the host speaks.
        protocol_min: u32,
        /// The newest version the host speaks.
        protocol_max: u32,
    },
    /// `extract` and its input bytes.
    Extract(ExtractHeader, Vec<u8>),
    /// `shutdown`.
    Shutdown,
}

#[derive(Deserialize)]
struct HelloMsg {
    protocol_min: u32,
    protocol_max: u32,
}

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

/// Fill `buf`; `false` when the stream ends before the first byte.
fn read_full(r: &mut dyn Read, buf: &mut [u8]) -> io::Result<bool> {
    let mut at = 0;
    while at < buf.len() {
        match r.read(&mut buf[at..]) {
            Ok(0) if at == 0 => return Ok(false),
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(k) => at += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}

/// Read the next host message. `Ok(None)` when stdin ends at a frame
/// boundary (the host closed it). Errors: a short frame, a header over
/// [`HEADER_CAP`], bad JSON, an unknown type, or an input over [`INPUT_CAP`].
pub fn read_request(r: &mut dyn Read) -> io::Result<Option<Request>> {
    let mut len = [0u8; 4];
    if !read_full(r, &mut len)? {
        return Ok(None);
    }
    let n = u32::from_be_bytes(len);
    if n > HEADER_CAP {
        return Err(invalid(format!("header of {n} bytes over the cap")));
    }
    let mut header = vec![0u8; n as usize];
    if !read_full(r, &mut header)? {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    let v: Value =
        serde_json::from_slice(&header).map_err(|e| invalid(format!("bad header JSON: {e}")))?;
    let kind = v.get("type").and_then(Value::as_str).unwrap_or_default();
    match kind {
        "hello" => {
            let h: HelloMsg =
                serde_json::from_value(v).map_err(|e| invalid(format!("bad hello: {e}")))?;
            Ok(Some(Request::Hello {
                protocol_min: h.protocol_min,
                protocol_max: h.protocol_max,
            }))
        }
        "extract" => {
            let h: ExtractHeader =
                serde_json::from_value(v).map_err(|e| invalid(format!("bad extract: {e}")))?;
            if h.byte_len > INPUT_CAP {
                return Err(invalid(format!(
                    "input of {} bytes over the cap",
                    h.byte_len
                )));
            }
            let mut bytes = vec![0u8; h.byte_len as usize];
            if !read_full(r, &mut bytes)? {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            Ok(Some(Request::Extract(h, bytes)))
        }
        "shutdown" => Ok(Some(Request::Shutdown)),
        other => Err(invalid(format!("unknown message {other:?}"))),
    }
}

/// Write one header-only frame and flush it.
pub fn write_frame(w: &mut dyn Write, header: &Value) -> io::Result<()> {
    let h = serde_json::to_vec(header)?;
    let n = u32::try_from(h.len()).map_err(|_| invalid("reply over 4 GiB".into()))?;
    w.write_all(&n.to_be_bytes())?;
    w.write_all(&h)?;
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn frame(header: &Value, body: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        write_frame(&mut out, header).unwrap();
        out.extend_from_slice(body);
        out
    }

    /// Why: a frame the host writes must parse, and a frame over a cap must
    /// be refused before the worker allocates it.
    /// What: a written frame reads back; a header over `HEADER_CAP`, an
    /// input over `INPUT_CAP`, a short body and an unknown type each fail;
    /// an empty stream is a clean end.
    /// Test: self-contained.
    #[test]
    fn frames_round_trip_and_caps_hold() {
        let mut r: &[u8] = &[];
        assert!(read_request(&mut r).unwrap().is_none());

        let mut big = (HEADER_CAP + 1).to_be_bytes().to_vec();
        big.extend_from_slice(b"{}");
        assert!(read_request(&mut big.as_slice()).is_err());

        let over = json!({"type":"extract","request_id":1,"type_hint":{},
            "byte_len": INPUT_CAP + 1,"sha256":"h",
            "limits":{"output_bytes":1,"fields_bytes":1,"notes":1}});
        assert!(read_request(&mut frame(&over, &[]).as_slice()).is_err());

        let short = json!({"type":"extract","request_id":1,"type_hint":{},
            "byte_len": 4,"sha256":"h",
            "limits":{"output_bytes":1,"fields_bytes":1,"notes":1}});
        assert!(read_request(&mut frame(&short, b"ab").as_slice()).is_err());

        let unknown = json!({"type":"ok"});
        assert!(read_request(&mut frame(&unknown, &[]).as_slice()).is_err());
    }

    /// Why: each host message has one shape.
    /// What: `hello`, `extract` with its body and `shutdown` parse in turn
    /// from one stream; the stream then ends cleanly.
    /// Test: self-contained.
    #[test]
    fn requests_parse_by_type() {
        let mut s = frame(
            &json!({"type":"hello","protocol_min":1,"protocol_max":3,"host_version":"0.1.0"}),
            &[],
        );
        s.extend(frame(
            &json!({"type":"extract","request_id":7,
                "type_hint":{"mime":null,"extension":"pdf","magic":"25504446"},
                "byte_len":3,"sha256":"h",
                "limits":{"output_bytes":10,"fields_bytes":20,"notes":2}}),
            b"abc",
        ));
        s.extend(frame(&json!({"type":"shutdown"}), &[]));
        let mut r = s.as_slice();
        assert_eq!(
            read_request(&mut r).unwrap(),
            Some(Request::Hello {
                protocol_min: 1,
                protocol_max: 3
            })
        );
        match read_request(&mut r).unwrap() {
            Some(Request::Extract(h, b)) => {
                assert_eq!((h.request_id, h.type_hint.extension.as_str()), (7, "pdf"));
                assert_eq!(h.limits.notes, 2);
                assert_eq!(b, b"abc");
            }
            other => panic!("expected extract, got {other:?}"),
        }
        assert_eq!(read_request(&mut r).unwrap(), Some(Request::Shutdown));
        assert_eq!(read_request(&mut r).unwrap(), None);
    }
}
