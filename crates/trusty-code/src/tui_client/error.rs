//! [`EngineError`]: [`crate::tui_client::CodeEngine`]'s unified error type.
//!
//! Why: `CodeEngine`'s `TuiEngine` methods return `anyhow::Result` (the
//! trait's signature, `crates/trusty-tui/src/engine.rs`), but the engine's
//! own internal helpers need a concrete, matchable error type — both so
//! tests can assert on WHICH failure occurred (discovery vs. transport vs.
//! an RPC error envelope) and so `?` composes cleanly across the
//! discovery/rpc/sse submodules before the outer `TuiEngine` method converts
//! the final error into `anyhow::Error` (automatic via `anyhow`'s blanket
//! `From<E: std::error::Error>` impl).
//! What: one variant per failure class this client can observe: daemon
//! discovery failure, an HTTP-transport-level failure (DNS/connect/decode),
//! a non-2xx HTTP status without a JSON-RPC error envelope (e.g. `502`), a
//! JSON-RPC error envelope from the daemon, and "no active session" (a
//! caller-usage error, not a transport one — `handle_input` before `setup`
//! completed).
//! Test: exercised indirectly through every `tui_client` submodule's own
//! tests and `tests/tui_client_engine.rs`.

use serde_json::Value;

use super::discovery::DiscoveryError;

/// See module docs.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// No live daemon could be found (see [`DiscoveryError`] for the
    /// specific reason — no candidate source, or a candidate that didn't
    /// answer the liveness ping).
    #[error(transparent)]
    Discovery(#[from] DiscoveryError),

    /// A `reqwest`-level failure: DNS/connect/TLS, a request timeout, or a
    /// response-body decode failure. Carries the URL for context.
    #[error("request to {url} failed: {source}")]
    Transport {
        url: String,
        #[source]
        source: reqwest::Error,
    },

    /// The daemon answered with a non-2xx HTTP status that carries no
    /// JSON-RPC error envelope to parse (the SSE routes return a bare HTTP
    /// status on failure, unlike `POST /rpc`, which always returns `200`
    /// with the error carried in the envelope — see
    /// `crate::serve::http::rpc_handler`'s docs).
    #[error("request to {url} returned HTTP {status}")]
    Status {
        url: String,
        status: reqwest::StatusCode,
    },

    /// The daemon's `POST /rpc` response body didn't have the shape this
    /// client expected (e.g. `session.create`'s result missing an `id`
    /// field) — a daemon/client version-skew symptom, not a transport
    /// failure.
    #[error("malformed response from daemon: {0}")]
    Malformed(String),

    /// The daemon's JSON-RPC error envelope for a `POST /rpc` call
    /// (verbatim: code + message + optional `data`).
    #[error("daemon returned an error ({code}): {message}")]
    Rpc {
        code: i32,
        message: String,
        #[allow(dead_code)]
        // surfaced for callers that want to inspect it; not read internally yet
        data: Option<Value>,
    },

    /// `handle_input`/`cancel_session` was called before `setup` minted a
    /// session (or after a session was never successfully created) — a
    /// caller-sequencing error, never a transport failure.
    #[error("no active tcode session — `setup` must complete before sending input")]
    NoSession,
}
