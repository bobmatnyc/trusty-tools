//! [`EngineError`]: [`crate::tui_client::CodeEngine`]'s unified error type.
//!
//! Why: `CodeEngine`'s `TuiEngine` methods return `anyhow::Result` (the
//! trait's signature, `crates/trusty-code-tui/src/engine.rs`), but the engine's
//! own internal helpers need a concrete, matchable error type — both so
//! tests can assert on WHICH failure occurred and so `?` composes cleanly
//! across the client's submodules before the outer `TuiEngine` method converts
//! the final error into `anyhow::Error` (automatic via `anyhow`'s blanket
//! `From<E: std::error::Error>` impl).
//! What: one variant per failure class this client can observe over the
//! daemon's Unix socket (#6637): a transport-level failure, a JSON-RPC error
//! envelope, a response whose shape this client did not expect, an event
//! stream that ended without a terminal session event, and "no active session"
//! (a caller-usage error, not a transport one — `handle_input` before `setup`
//! completed).
//!
//! The HTTP-shaped variants are gone with the transport. `Discovery` went with
//! `TCODE_DAEMON_URL` and the `http_addr` file — the socket path is derived,
//! not discovered. `Status` went with HTTP status codes, which a framed
//! JSON-RPC response does not have: every daemon-side refusal now arrives as
//! [`EngineError::Rpc`] carrying a code, and every transport failure as
//! [`EngineError::Transport`].
//!
//! Test: exercised indirectly through every `tui_client` submodule's own
//! tests and `tests/tui_client_engine.rs`.

use std::path::PathBuf;

use serde_json::Value;
use trusty_common::uds::UdsRpcError;

/// See module docs.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// The socket could not be dialled, written to, or read from — including
    /// the hardening checks `connect_hardened` runs before a byte is written.
    ///
    /// `Box`ed because [`UdsRpcError`] carries a `PathBuf` and a boxed
    /// response of its own, and clippy's `result_large_err` reads the unboxed
    /// enum as too large to return by value.
    #[error("rpc over {} failed: {source}", socket.display())]
    Transport {
        /// Socket that failed.
        socket: PathBuf,
        /// Why it failed.
        #[source]
        source: Box<UdsRpcError>,
    },

    /// No daemon is answering on the socket, and this client will not start
    /// one — the caller (`tcode tui`'s auto-spawn) decides that.
    #[error("no tcode daemon is answering on {}", socket.display())]
    NoDaemon {
        /// Socket nothing answered on.
        socket: PathBuf,
    },

    /// The daemon closed an event stream before this client observed a
    /// terminal event (`SessionDone`/`SessionCancelled`) — and every reconnect
    /// attempt (bounded by `SESSION_STREAM_MAX_RECONNECTS`) hit the same
    /// premature close. Distinct from [`Self::Transport`]: nothing about the
    /// individual dial failed, only the STREAM never reached a terminal state.
    #[error("event stream on {} closed repeatedly with no terminal session event observed", socket.display())]
    StreamClosed {
        /// Socket whose stream kept ending early.
        socket: PathBuf,
    },

    /// The daemon's response body didn't have the shape this client expected
    /// (e.g. `session.create`'s result missing an `id` field) — a
    /// daemon/client version-skew symptom, not a transport failure.
    #[error("malformed response from daemon: {0}")]
    Malformed(String),

    /// The daemon's JSON-RPC error envelope (verbatim: code + message +
    /// optional `data`).
    #[error("daemon returned an error ({code}): {message}")]
    Rpc {
        /// JSON-RPC error code.
        code: i32,
        /// The daemon's own message.
        message: String,
        #[allow(dead_code)]
        // surfaced for callers that want to inspect it; not read internally yet
        /// The error's optional structured payload.
        data: Option<Value>,
    },

    /// `handle_input`/`cancel_session` was called before `setup` minted a
    /// session (or after a session was never successfully created) — a
    /// caller-sequencing error, never a transport failure.
    #[error("no active tcode session — `setup` must complete before sending input")]
    NoSession,
}
