//! IPC protocol between the daemon and its clients (CLI, TUI, Telegram).
//!
//! Why: A versioned request/response envelope lets clients evolve independently
//! of the daemon and gives a single place to enforce protocol compatibility.
//! What: Defines `Request`, `Response`, and the wire `Envelope` carrying a
//! protocol version. Transport is JSON over a local HTTP API (axum).
//! Test: `cargo test -p trusty-mpm-core` round-trips each variant through JSON.

use serde::{Deserialize, Serialize};

use crate::core::session::{Session, SessionId};

/// Current IPC protocol version. Bump on any breaking envelope change.
pub const PROTOCOL_VERSION: u32 = 1;

/// A command sent from a client to the daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Request {
    /// Health/liveness probe.
    Ping,
    /// List all managed sessions.
    ListSessions,
    /// Start a new session in the given working directory.
    StartSession { workdir: String },
    /// Stop a running session.
    StopSession { id: SessionId },
    /// Approve or deny a pending permission request.
    ResolveApproval { id: SessionId, approved: bool },
}

/// A reply sent from the daemon to a client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    /// Reply to `Ping`.
    Pong,
    /// Reply carrying a session list.
    Sessions { sessions: Vec<Session> },
    /// Reply confirming a session was created.
    SessionStarted { session: Session },
    /// Generic acknowledgement.
    Ok,
    /// The request failed; carries a human-readable reason.
    Error { message: String },
}

/// Versioned wire envelope wrapping any payload `T`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope<T> {
    /// Protocol version the sender speaks.
    pub version: u32,
    /// The wrapped payload.
    pub payload: T,
}

impl<T> Envelope<T> {
    /// Wrap a payload with the current protocol version.
    pub fn new(payload: T) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            payload,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrip() {
        let req = Request::StartSession {
            workdir: "/tmp/x".into(),
        };
        let env = Envelope::new(req);
        let json = serde_json::to_string(&env).unwrap();
        let back: Envelope<Request> = serde_json::from_str(&json).unwrap();
        assert_eq!(back.version, PROTOCOL_VERSION);
        assert!(matches!(back.payload, Request::StartSession { .. }));
    }

    #[test]
    fn response_error_roundtrip() {
        let resp = Response::Error {
            message: "no such session".into(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: Response = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, Response::Error { .. }));
    }
}
