//! The `kind` tag distinguishing legacy vs managed session records.
//!
//! Why: `session_list` and `session_status` tag every emitted record with a
//! `kind` field so callers can tell a legacy in-process registry session apart
//! from a managed (`SessionManager`) one (#1946/#1976). The tag used to be
//! ad-hoc `"legacy"`/`"managed"` string literals scattered across four call
//! sites in `mcp_backend`; a closed-set enum makes the two valid values
//! authoritative while keeping the emitted JSON byte-identical. It lives in its
//! own module (rather than inline in `mcp_backend`) so that file — already at
//! its line-cap budget — does not grow.
//! What: [`SessionRecordKind`], rendered via [`as_str`](SessionRecordKind::as_str).
//! Test: the `mcp_backend` `session_list_*` / `session_status_*` tests assert
//! the emitted `kind` string unchanged.

use std::fmt;

/// Which session store a listed/queried record originated from.
///
/// Why: makes the two valid `kind` tags a closed set instead of bare literals.
/// What: [`Legacy`](SessionRecordKind::Legacy) for the in-process `DaemonState`
/// registry, [`Managed`](SessionRecordKind::Managed) for the `SessionManager`
/// store; both render as exactly `legacy` / `managed`.
/// Test: covered by the `mcp_backend` `session_list_*` / `session_status_*`
/// assertions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionRecordKind {
    /// A session from the legacy in-process `DaemonState` registry.
    Legacy,
    /// A session from the managed `SessionManager` store.
    Managed,
}

impl SessionRecordKind {
    /// The wire token for this record kind (`legacy` / `managed`).
    ///
    /// Why: the JSON `kind` field must stay byte-identical to the previous
    /// string literals.
    /// What: maps each variant to its lowercase token.
    /// Test: covered by the `mcp_backend` `session_*` assertions.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            SessionRecordKind::Legacy => "legacy",
            SessionRecordKind::Managed => "managed",
        }
    }
}

impl fmt::Display for SessionRecordKind {
    /// Render the wire token (see [`SessionRecordKind::as_str`]).
    ///
    /// Why: lets callers use `{kind}` / `kind.to_string()`.
    /// What: writes [`SessionRecordKind::as_str`].
    /// Test: `wire_tokens_are_stable`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin the exact wire tokens the JSON `kind` field must emit.
    #[test]
    fn wire_tokens_are_stable() {
        assert_eq!(SessionRecordKind::Legacy.as_str(), "legacy");
        assert_eq!(SessionRecordKind::Managed.as_str(), "managed");
        assert_eq!(SessionRecordKind::Legacy.to_string(), "legacy");
        assert_eq!(SessionRecordKind::Managed.to_string(), "managed");
    }
}
