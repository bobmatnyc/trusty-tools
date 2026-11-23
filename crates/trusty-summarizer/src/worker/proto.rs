//! The converter worker's wire format, shared by host and worker.
//!
//! Why: the host (this library) and the worker binary
//! (`trusty-summarizer-convert`) are separate processes; one definition keeps
//! them in step.
//!
//! What: newline-delimited JSON over the worker's stdin and stdout.
//!
//! 1. On start the worker writes one [`Hello`] line.
//! 2. The host writes a [`Request`] line, then exactly `len` raw bytes.
//! 3. The worker answers with one [`Response`] line carrying the same `id`.
//!
//! Steps 2 and 3 repeat until the host closes stdin; the worker then exits 0.
//! The worker writes nothing else to stdout; diagnostics go to stderr.
//! Test: `wire_types_round_trip`

use serde::{Deserialize, Serialize};

/// The protocol version; part of [`CONVERTER_NAME`].
pub const PROTOCOL: u32 = 1;

/// The Xberg release this build links in its worker: `1.3.10`
/// (`workers/summarizer-convert`, rustc 1.98). The `xberg14` feature names a
/// former Xberg 1.4 build this repository does not carry. See the README,
/// "Build".
#[cfg(not(feature = "xberg14"))]
pub const XBERG_VERSION: &str = "1.3.10";
/// The Xberg release this build links in its worker. See the README, "Build".
#[cfg(feature = "xberg14")]
pub const XBERG_VERSION: &str = "1.4.0";

/// A reader's name for this build, used in messages: `Xberg 1.3.10 build` or
/// `Xberg 1.4 build`.
#[cfg(not(feature = "xberg14"))]
pub const BUILD_NAME: &str = "Xberg 1.3.10 build";
/// A reader's name for this build, used in messages.
#[cfg(feature = "xberg14")]
pub const BUILD_NAME: &str = "Xberg 1.4 build";

/// The converter name the host records and puts in cache keys. It names the
/// Xberg version and the protocol, so a worker whose output can change gets
/// a new name, and text cached by one build is never served by the other.
/// The worker reports it in [`Hello`]; the host refuses a worker that
/// reports another name.
#[cfg(not(feature = "xberg14"))]
pub const CONVERTER_NAME: &str = "xberg-1.3.10+w1";
/// The converter name; see the other build's doc above.
#[cfg(feature = "xberg14")]
pub const CONVERTER_NAME: &str = "xberg-1.4.0+w1";

/// The name the host records for converted text: the worker's
/// [`CONVERTER_NAME`] plus the version of the host's clean-up of that text
/// (`t2`: DOCX `(style_name: …)` labels removed). A new clean-up gets a new
/// name, so text cached by an older host is not reused.
#[cfg(not(feature = "xberg14"))]
pub const HOST_CONVERTER_NAME: &str = "xberg-1.3.10+w1+t2";
/// The host's converter name; see the other build's doc above.
#[cfg(feature = "xberg14")]
pub const HOST_CONVERTER_NAME: &str = "xberg-1.4.0+w1+t2";

/// Names the build a worker's converter name comes from, for the refusal
/// message: `Xberg 1.3.10 build`, `Xberg 1.4 build`, or `unknown build`.
///
/// Test: `builds_are_named_from_converter_names`
pub fn build_of(converter: &str) -> &'static str {
    match converter.split('+').next() {
        Some("xberg-1.3.10") => "Xberg 1.3.10 build",
        Some("xberg-1.4.0") => "Xberg 1.4 build",
        _ => "unknown build",
    }
}

/// The worker binary's file name (without a platform suffix).
pub const WORKER_BIN: &str = "trusty-summarizer-convert";

/// The worker's first line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    /// Always [`WORKER_BIN`].
    pub hello: String,
    /// The worker's [`CONVERTER_NAME`].
    pub converter: String,
    /// The worker's [`PROTOCOL`].
    pub protocol: u32,
}

impl Hello {
    /// The hello this build of the protocol expects.
    pub fn current() -> Self {
        Hello {
            hello: WORKER_BIN.into(),
            converter: CONVERTER_NAME.into(),
            protocol: PROTOCOL,
        }
    }
}

/// One conversion request header; `len` raw bytes follow it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    /// Echoed in the response.
    pub id: u64,
    /// The detected format name, e.g. `pdf`.
    pub format: String,
    /// Byte count of the document that follows.
    pub len: u64,
}

/// The answer to one [`Request`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Response {
    /// The document's text.
    Ok {
        /// The request's id.
        id: u64,
        /// Extracted plain text; may be empty.
        text: String,
    },
    /// The worker could not convert the document.
    Error {
        /// The request's id.
        id: u64,
        /// A short code, e.g. `unsupported_format` or `parse`.
        kind: String,
        /// What went wrong.
        message: String,
    },
}

impl Response {
    /// The request id this answers.
    pub fn id(&self) -> u64 {
        match self {
            Response::Ok { id, .. } | Response::Error { id, .. } => *id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_types_round_trip() {
        let ok = Response::Ok {
            id: 7,
            text: "a\nb".into(),
        };
        let line = serde_json::to_string(&ok).unwrap();
        assert!(!line.contains('\n'), "a response is one line: {line}");
        assert!(line.contains(r#""status":"ok""#));
        assert_eq!(serde_json::from_str::<Response>(&line).unwrap(), ok);
        let hello = serde_json::to_string(&Hello::current()).unwrap();
        assert_eq!(
            serde_json::from_str::<Hello>(&hello).unwrap(),
            Hello::current()
        );
    }

    #[test]
    fn builds_are_named_from_converter_names() {
        assert_eq!(build_of(CONVERTER_NAME), BUILD_NAME);
        assert_eq!(build_of(HOST_CONVERTER_NAME), BUILD_NAME);
        assert!(CONVERTER_NAME.contains(XBERG_VERSION));
        assert_eq!(build_of("xberg-1.3.10+w1"), "Xberg 1.3.10 build");
        assert_eq!(build_of("xberg-1.4.0+w1"), "Xberg 1.4 build");
        assert_eq!(build_of("other"), "unknown build");
    }
}
