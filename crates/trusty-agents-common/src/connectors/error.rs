//! [`ConnectorError`] — the typed failure surface every [`super::WorkstreamConnector`]
//! implementation returns.
//!
//! Why: this crate is a stable library surface (no `unwrap`/`anyhow` in
//! library code — see the workspace CLAUDE.md convention already followed by
//! `agents::manifest::ManifestError`). A closed `thiserror` enum lets the
//! lead agent (DOC-44 Layer 2, not built in this Phase 1 ticket) branch on
//! failure kind — "escalate to the user" vs. "retry" vs. "this backend simply
//! can't do that" — without string-matching error messages.
//! What: five variants covering the failure modes both the tm and tcode
//! backends can hit: an unknown session id, a malformed/backend-mismatched
//! request, an operation the backend never implements (tcode's `delegate`),
//! a transport-level failure (the HTTP/JSON-RPC call itself failed), and a
//! generic backend-reported failure (the daemon understood the request and
//! rejected it for a domain reason that isn't one of the above).
//! Test: `error::tests` covers `Display` formatting for every variant.

/// Failure returned by any [`super::WorkstreamConnector`] method.
///
/// Why: see module docs — one closed enum shared by every backend so a
/// caller (today: tests; DOC-44 Phase 5: the lead agent) can match on
/// failure kind uniformly across tm and tcode.
/// What: `NotFound` — the session id is unknown to the backend.
/// `InvalidRequest` — the caller-supplied request was malformed, or (per the
/// `CreateSessionReq::backend` asymmetry, DOC-44 §5.2/locked decision 4)
/// carried the wrong [`super::BackendParams`] variant for this backend.
/// `NotSupported` — the backend has no surface for this operation at all
/// (tcode's `delegate`, see that method's docs). `Transport` — the
/// underlying HTTP/JSON-RPC call itself failed (connection refused, timeout,
/// malformed response body). `Backend` — the remote daemon understood the
/// request and returned a domain-level rejection that isn't better
/// classified above.
/// Test: `error::tests::display_messages_are_stable`.
#[derive(Debug, thiserror::Error)]
pub enum ConnectorError {
    /// The session id is unknown to this backend.
    #[error("session not found: {0}")]
    NotFound(String),
    /// The request was malformed, or carried a [`super::BackendParams`]
    /// variant this backend does not implement.
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    /// This backend has no implementation of the requested operation at all
    /// (as opposed to rejecting a specific call) — see tcode's `delegate`.
    #[error("operation not supported by this backend: {0}")]
    NotSupported(String),
    /// The HTTP/JSON-RPC call to the backend daemon itself failed (network,
    /// timeout, or an unparseable response body).
    #[error("transport error: {0}")]
    Transport(String),
    /// The backend daemon understood the request and rejected it for a
    /// domain reason not covered by the other variants.
    #[error("backend error: {0}")]
    Backend(String),
}

impl ConnectorError {
    /// True when this error means "the session id does not exist."
    ///
    /// Why: callers (tests, and eventually the lead agent) frequently need
    /// to distinguish "gone" from every other failure without a `matches!`
    /// at every call site.
    /// What: `true` only for [`ConnectorError::NotFound`].
    /// Test: `error::tests::is_not_found_only_matches_not_found_variant`.
    pub fn is_not_found(&self) -> bool {
        matches!(self, ConnectorError::NotFound(_))
    }
}

#[cfg(test)]
mod tests {
    use super::ConnectorError;

    #[test]
    fn display_messages_are_stable() {
        assert_eq!(
            ConnectorError::NotFound("abc".into()).to_string(),
            "session not found: abc"
        );
        assert_eq!(
            ConnectorError::InvalidRequest("bad shape".into()).to_string(),
            "invalid request: bad shape"
        );
        assert_eq!(
            ConnectorError::NotSupported("delegate".into()).to_string(),
            "operation not supported by this backend: delegate"
        );
        assert_eq!(
            ConnectorError::Transport("connection refused".into()).to_string(),
            "transport error: connection refused"
        );
        assert_eq!(
            ConnectorError::Backend("rejected".into()).to_string(),
            "backend error: rejected"
        );
    }

    #[test]
    fn is_not_found_only_matches_not_found_variant() {
        assert!(ConnectorError::NotFound("x".into()).is_not_found());
        assert!(!ConnectorError::InvalidRequest("x".into()).is_not_found());
        assert!(!ConnectorError::NotSupported("x".into()).is_not_found());
        assert!(!ConnectorError::Transport("x".into()).is_not_found());
        assert!(!ConnectorError::Backend("x".into()).is_not_found());
    }
}
