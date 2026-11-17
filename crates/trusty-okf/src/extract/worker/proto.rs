//! Appendix B wire format: frames and messages (SPEC-OKF-01 Appendix B).
//!
//! Why: the host must never trust a byte the worker sends. Every frame is
//! length-checked before it is read, every reply is checked against the
//! request it answers, and every message has one fixed shape.
//! What: a frame is a 4-byte big-endian length, then that many bytes of
//! JSON header; an `extract` frame is followed by `byte_len` raw input
//! bytes, which are not counted in the length (#10746 S10: the spec leaves
//! the header/body split open; this is the resolution). [`read_frame`]
//! refuses a declared length over its cap without reading it.
//! [`host_hello`], [`extract_request`] and [`shutdown`] build host frames;
//! [`parse_hello`] and [`parse_reply`] check worker frames.
//! Test: `proto_round_trip_and_refusals`.
//!
//! # Spec References
//! - SPEC-OKF-01 Appendix B "Transport", "Messages", "Error codes".

use std::io::Read;

use serde::Deserialize;
use serde_json::{Value, json};

/// The protocol versions this host speaks (Appendix B).
pub const PROTOCOL_MIN: u32 = 1;
/// The newest protocol version this host speaks.
pub const PROTOCOL_MAX: u32 = 1;

/// The host version sent in `hello`: this crate's version. A literal,
/// because B3 bars the compile-time variable macro from `src/`; the test
/// `host_version_matches_the_package_version` pins it to `Cargo.toml`.
pub const HOST_VERSION: &str = "0.1.0";

/// Worker error codes a reply may carry (Appendix B, "raised by worker").
const WORKER_CODES: [&str; 3] = ["unsupported_format", "no_text_layer", "extract_error"];

/// The longest worker error message kept (DOC-55 E4: bounded, no excerpt).
const MESSAGE_CAP: usize = 256;

/// Why reading one frame stopped.
#[derive(Debug)]
pub(crate) enum FrameRead {
    /// A whole frame.
    Frame(Vec<u8>),
    /// The declared length exceeds the cap; nothing more was read.
    TooLarge(u64),
    /// The stream ended at a frame boundary or inside a frame.
    Eof,
}

/// Read one frame of at most `cap` header bytes from `r`.
pub(crate) fn read_frame(r: &mut dyn Read, cap: u64) -> std::io::Result<FrameRead> {
    let mut len = [0u8; 4];
    if !read_full(r, &mut len)? {
        return Ok(FrameRead::Eof);
    }
    let n = u64::from(u32::from_be_bytes(len));
    if n > cap {
        return Ok(FrameRead::TooLarge(n));
    }
    let mut buf = vec![0u8; n as usize];
    if !read_full(r, &mut buf)? {
        return Ok(FrameRead::Eof);
    }
    Ok(FrameRead::Frame(buf))
}

/// Fill `buf`; `false` when the stream ends first.
fn read_full(r: &mut dyn Read, buf: &mut [u8]) -> std::io::Result<bool> {
    let mut at = 0;
    while at < buf.len() {
        match r.read(&mut buf[at..]) {
            Ok(0) => return Ok(false),
            Ok(k) => at += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}

fn frame(header: &Value, body: &[u8]) -> Vec<u8> {
    let h = header.to_string().into_bytes();
    let mut out = Vec::with_capacity(4 + h.len() + body.len());
    out.extend_from_slice(&(h.len() as u32).to_be_bytes());
    out.extend_from_slice(&h);
    out.extend_from_slice(body);
    out
}

/// The host `hello` frame.
pub(crate) fn host_hello() -> Vec<u8> {
    frame(
        &json!({
            "type": "hello",
            "protocol_min": PROTOCOL_MIN,
            "protocol_max": PROTOCOL_MAX,
            "host_version": HOST_VERSION,
        }),
        &[],
    )
}

/// The `shutdown` frame.
pub(crate) fn shutdown() -> Vec<u8> {
    frame(&json!({ "type": "shutdown" }), &[])
}

/// Per-item output limits sent with each request.
#[derive(Debug, Clone, Copy)]
pub(crate) struct OutputLimits {
    pub output_bytes: u64,
    pub fields_bytes: u64,
    pub notes: usize,
}

/// One `extract` frame: header, then the raw bytes.
pub(crate) fn extract_request(
    request_id: u64,
    hint: &crate::extract::TypeHint,
    bytes: &[u8],
    sha256: &str,
    limits: OutputLimits,
) -> Vec<u8> {
    let magic: String = bytes.iter().take(8).map(|b| format!("{b:02x}")).collect();
    frame(
        &json!({
            "type": "extract",
            "request_id": request_id,
            "type_hint": { "mime": hint.mime, "extension": hint.extension, "magic": magic },
            "byte_len": bytes.len(),
            "sha256": sha256,
            "limits": {
                "output_bytes": limits.output_bytes,
                "fields_bytes": limits.fields_bytes,
                "notes": limits.notes,
            },
        }),
        bytes,
    )
}

/// The worker's `hello` reply (Appendix B).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkerHello {
    #[serde(rename = "type")]
    pub kind: String,
    pub protocol: u32,
    pub worker_version: String,
    pub xberg_version: String,
    pub config_hash: String,
    pub formats: Vec<String>,
    pub deterministic: bool,
    pub features: Vec<String>,
    /// The active OS sandbox, or `none`; a cross-check only (§2.16 r7.1).
    pub sandbox: String,
}

/// A protocol violation: the item (or handshake) fails `protocol_error`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Violation(pub String);

/// Parse the first worker frame; anything but a well-formed `hello` is a
/// violation (§2.16 "Version negotiation").
pub(crate) fn parse_hello(frame: &[u8]) -> Result<WorkerHello, Violation> {
    let hello: WorkerHello = serde_json::from_slice(frame)
        .map_err(|e| Violation(format!("first frame is not a hello: {e}")))?;
    if hello.kind != "hello" {
        return Err(Violation(format!("first frame is {:?}", hello.kind)));
    }
    Ok(hello)
}

/// A checked worker reply to one `extract` request.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Reply {
    /// `ok`: text, fields as YAML and notes.
    Ok {
        text: String,
        fields: serde_yaml::Mapping,
        notes: Vec<String>,
    },
    /// `error` with a worker code and a bounded message.
    Error { code: String, message: String },
}

/// Why a reply was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReplyFault {
    /// Bad JSON, a wrong `request_id`, a mismatched `input_sha256`, an
    /// unknown message or code.
    Protocol(String),
    /// Text, fields or notes over the request's limits.
    TooLarge(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OkMsg {
    #[serde(rename = "type")]
    _kind: String,
    request_id: u64,
    input_sha256: String,
    text: String,
    #[serde(default)]
    fields: serde_json::Map<String, Value>,
    #[serde(default)]
    notes: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    provenance: Option<Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ErrMsg {
    #[serde(rename = "type")]
    _kind: String,
    request_id: u64,
    code: String,
    message: String,
}

/// Check one reply against its request: `request_id`, the echoed input
/// SHA-256, the message shape, the worker code set and the output limits.
pub(crate) fn parse_reply(
    frame: &[u8],
    request_id: u64,
    sha256: &str,
    limits: OutputLimits,
) -> Result<Reply, ReplyFault> {
    let bad = |m: String| ReplyFault::Protocol(m);
    let v: Value = serde_json::from_slice(frame).map_err(|e| bad(format!("bad JSON: {e}")))?;
    match v.get("type").and_then(Value::as_str) {
        Some("ok") => {
            let m: OkMsg = serde_json::from_value(v).map_err(|e| bad(format!("bad ok: {e}")))?;
            if m.request_id != request_id {
                return Err(bad(format!("request_id {} answered", m.request_id)));
            }
            if m.input_sha256 != sha256 {
                return Err(bad("input_sha256 does not match the input".into()));
            }
            if m.text.len() as u64 > limits.output_bytes {
                return Err(ReplyFault::TooLarge(format!("text {} bytes", m.text.len())));
            }
            let fields_len = Value::Object(m.fields.clone()).to_string().len() as u64;
            if fields_len > limits.fields_bytes || m.notes.len() > limits.notes {
                return Err(ReplyFault::TooLarge(
                    "fields or notes over the limit".into(),
                ));
            }
            let fields = match serde_yaml::to_value(&m.fields) {
                Ok(serde_yaml::Value::Mapping(map)) => map,
                _ => return Err(bad("fields are not a mapping".into())),
            };
            Ok(Reply::Ok {
                text: m.text,
                fields,
                notes: m.notes,
            })
        }
        Some("error") => {
            let m: ErrMsg =
                serde_json::from_value(v).map_err(|e| bad(format!("bad error: {e}")))?;
            if m.request_id != request_id {
                return Err(bad(format!("request_id {} answered", m.request_id)));
            }
            if !WORKER_CODES.contains(&m.code.as_str()) {
                return Err(bad(format!("unknown worker code {:?}", m.code)));
            }
            let message: String = m.message.chars().take(MESSAGE_CAP).collect();
            Ok(Reply::Error {
                code: m.code,
                message,
            })
        }
        other => Err(bad(format!("unexpected message {other:?}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIM: OutputLimits = OutputLimits {
        output_bytes: 8,
        fields_bytes: 64,
        notes: 1,
    };

    fn json_frame(v: Value) -> Vec<u8> {
        v.to_string().into_bytes()
    }

    /// Why: every worker byte is untrusted; each refusal must hold.
    /// What: a frame over its cap is refused unread; a good `ok` parses;
    /// a wrong ID, a wrong hash, an unknown code, oversize text and a
    /// non-`hello` first frame are each refused.
    /// Test: self-contained.
    #[test]
    fn proto_round_trip_and_refusals() {
        let f = shutdown();
        let mut r: &[u8] = &f;
        assert!(matches!(
            read_frame(&mut r, 64).unwrap(),
            FrameRead::Frame(_)
        ));
        let mut r: &[u8] = &f;
        assert!(matches!(
            read_frame(&mut r, 2).unwrap(),
            FrameRead::TooLarge(_)
        ));
        let mut r: &[u8] = &f[..6];
        assert!(matches!(read_frame(&mut r, 64).unwrap(), FrameRead::Eof));

        let ok = |id: u64, sha: &str, text: &str| {
            json_frame(json!({"type":"ok","request_id":id,"input_sha256":sha,"text":text}))
        };
        assert!(matches!(
            parse_reply(&ok(1, "h", "abc"), 1, "h", LIM),
            Ok(Reply::Ok { .. })
        ));
        assert!(matches!(
            parse_reply(&ok(2, "h", "abc"), 1, "h", LIM),
            Err(ReplyFault::Protocol(_))
        ));
        assert!(matches!(
            parse_reply(&ok(1, "x", "abc"), 1, "h", LIM),
            Err(ReplyFault::Protocol(_))
        ));
        assert!(matches!(
            parse_reply(&ok(1, "h", "abcdefghi"), 1, "h", LIM),
            Err(ReplyFault::TooLarge(_))
        ));
        let err = json_frame(json!({"type":"error","request_id":1,"code":"boom","message":""}));
        assert!(matches!(
            parse_reply(&err, 1, "h", LIM),
            Err(ReplyFault::Protocol(_))
        ));
        assert!(parse_hello(&ok(1, "h", "")).is_err());
        assert!(parse_hello(b"not json").is_err());
    }
}
