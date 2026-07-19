//! [`LedgerError`] — the typed failure surface for [`super::WorkstreamLedger`]
//! and [`super::LedgerRecovery`] (issue #3008, twin Phase 2, DOC-44 §8 "Phase
//! 2: Workstream Ledger & Heterogeneous Registry").
//!
//! Why: mirrors the precedent already set in this crate —
//! `connectors::ConnectorError` and `agents::manifest::ManifestError` are both
//! closed `thiserror` enums so a caller (eventually the DOC-44 Layer 2 lead
//! agent, not built by this ticket) can branch on failure kind instead of
//! string-matching. `trusty-agents-common` is a library surface: no
//! `unwrap`/`anyhow` in its public API (workspace CLAUDE.md convention).
//! What: `NotFound` — the requested workstream id isn't in the ledger.
//! `ImmutableField` — a mutation attempted to change a field the DOC-44 §4.1
//! "no mid-life migration" rule locks after creation (today: only
//! `assigned_harness`). `Io`/`Json` — filesystem or (de)serialization
//! failure reading/writing the ledger file. `NotSupported` — a
//! [`super::LedgerRecovery`] backend has no working implementation yet (see
//! [`super::TrustyMemoryRecovery`]).
//! Test: `error::tests` covers `Display` formatting for every variant.

/// Failure returned by [`super::WorkstreamLedger`] or [`super::LedgerRecovery`].
///
/// Why/What: see module docs.
/// Test: `error::tests::display_messages_are_stable`.
#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    /// No workstream with this id exists in the ledger.
    #[error("workstream not found: {0}")]
    NotFound(String),
    /// A mutation attempted to change a field that is immutable after
    /// creation (DOC-44 §4.1: tool assignment is locked at creation time).
    #[error("attempted to mutate immutable field `{0}` after creation")]
    ImmutableField(&'static str),
    /// Filesystem access (read/write/rename/create_dir_all) failed.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// JSON (de)serialization of the ledger file failed.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    /// The operation is not implemented by this backend yet.
    #[error("unsupported: {0}")]
    NotSupported(String),
}

#[cfg(test)]
mod tests {
    use super::LedgerError;

    #[test]
    fn display_messages_are_stable() {
        assert_eq!(
            LedgerError::NotFound("ws-1".into()).to_string(),
            "workstream not found: ws-1"
        );
        assert_eq!(
            LedgerError::ImmutableField("assigned_harness").to_string(),
            "attempted to mutate immutable field `assigned_harness` after creation"
        );
        assert_eq!(
            LedgerError::NotSupported("not wired yet".into()).to_string(),
            "unsupported: not wired yet"
        );
        let io_err: LedgerError = std::io::Error::new(std::io::ErrorKind::NotFound, "gone").into();
        assert!(io_err.to_string().starts_with("io error:"));
        let json_err: LedgerError = serde_json::from_str::<serde_json::Value>("{bad")
            .unwrap_err()
            .into();
        assert!(json_err.to_string().starts_with("json error:"));
    }
}
