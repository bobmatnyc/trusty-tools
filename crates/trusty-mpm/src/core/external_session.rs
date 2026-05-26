//! External (non-trusty-mpm) tmux session model.
//!
//! Why: trusty-mpm should observe and manage *every* tmux session on the host,
//! not just the ones it spawned. The dashboard needs to tell apart sessions
//! the daemon created (`tmpm-*` / `trusty-mpm-*`) from pre-existing ones so it
//! can offer to "adopt" the latter for oversight without touching them.
//! What: [`SessionOrigin`] classifies a session by name prefix, and
//! [`ExternalSession`] is the structured, origin-tagged view of one tmux
//! session row used by the `GET /tmux/sessions` endpoint.
//! Test: `cargo test -p trusty-mpm-core external_session` covers the prefix
//! classification and the JSON round-trip.

use serde::{Deserialize, Serialize};

/// Where a tmux session came from, by name convention.
///
/// Why: the daemon treats its own sessions and externally-created ones
/// differently — only external sessions need an explicit "adopt" step before
/// oversight applies.
/// What: [`TrustyMpm`](SessionOrigin::TrustyMpm) for `tmpm-*` / `trusty-mpm-*`
/// names (this covers both the random `tmpm-<adjective>-<noun>` form and the
/// folder-derived `tmpm-<folder>` form), [`External`](SessionOrigin::External)
/// for everything else.
/// Test: `classifies_trusty_mpm_prefixes`, `classifies_external`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SessionOrigin {
    /// A session created by trusty-mpm (name starts with `tmpm-`/`trusty-mpm-`).
    TrustyMpm,
    /// A session that pre-existed or was created outside trusty-mpm.
    External,
}

impl SessionOrigin {
    /// Classify a tmux session by its name.
    ///
    /// Why: the daemon's session driver only has the tmux name to work from;
    /// a single classifier keeps the prefix convention in one place.
    /// What: returns [`TrustyMpm`](SessionOrigin::TrustyMpm) when `name` starts
    /// with `tmpm-` or `trusty-mpm-`, else [`External`](SessionOrigin::External).
    /// Test: `classifies_trusty_mpm_prefixes`, `classifies_external`.
    pub fn classify(name: &str) -> Self {
        if name.starts_with("tmpm-") || name.starts_with("trusty-mpm-") {
            Self::TrustyMpm
        } else {
            Self::External
        }
    }

    /// Lowercase wire label for this origin (`"trusty_mpm"` / `"external"`).
    pub fn label(&self) -> &'static str {
        match self {
            Self::TrustyMpm => "trusty_mpm",
            Self::External => "external",
        }
    }
}

/// An origin-tagged view of one tmux session.
///
/// Why: the universal-session dashboard lists every tmux session with enough
/// metadata to decide whether trusty-mpm already manages it.
/// What: the session name, its [`SessionOrigin`], whether a client is
/// attached, and the creation epoch.
/// Test: `external_session_json_roundtrip`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ExternalSession {
    /// tmux session name.
    pub name: String,
    /// Whether trusty-mpm created the session or it is external.
    pub origin: SessionOrigin,
    /// Whether a tmux client is currently attached.
    pub attached: bool,
    /// Unix epoch seconds the session was created.
    pub created: i64,
}

impl ExternalSession {
    /// Build an origin-tagged session view, classifying `name` automatically.
    ///
    /// Why: callers parsing `tmux list-sessions` rows have the raw fields; this
    /// constructor classifies the origin so they need not.
    /// What: stores the fields and sets `origin` via [`SessionOrigin::classify`].
    /// Test: `external_session_classifies_on_construction`.
    pub fn new(name: impl Into<String>, attached: bool, created: i64) -> Self {
        let name = name.into();
        let origin = SessionOrigin::classify(&name);
        Self {
            name,
            origin,
            attached,
            created,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_trusty_mpm_prefixes() {
        assert_eq!(
            SessionOrigin::classify("tmpm-brave-otter"),
            SessionOrigin::TrustyMpm
        );
        assert_eq!(
            SessionOrigin::classify("trusty-mpm-abc123"),
            SessionOrigin::TrustyMpm
        );
    }

    #[test]
    fn classifies_external() {
        assert_eq!(SessionOrigin::classify("work"), SessionOrigin::External);
        assert_eq!(SessionOrigin::classify("0"), SessionOrigin::External);
        // A name that merely contains the substring is still external.
        assert_eq!(
            SessionOrigin::classify("my-tmpm-thing"),
            SessionOrigin::External
        );
    }

    #[test]
    fn origin_labels_are_stable() {
        assert_eq!(SessionOrigin::TrustyMpm.label(), "trusty_mpm");
        assert_eq!(SessionOrigin::External.label(), "external");
    }

    #[test]
    fn external_session_classifies_on_construction() {
        let internal = ExternalSession::new("tmpm-x", true, 100);
        assert_eq!(internal.origin, SessionOrigin::TrustyMpm);
        let external = ExternalSession::new("vim", false, 200);
        assert_eq!(external.origin, SessionOrigin::External);
    }

    #[test]
    fn external_session_json_roundtrip() {
        let session = ExternalSession::new("trusty-mpm-1", true, 1_700_000_000);
        let json = serde_json::to_string(&session).unwrap();
        let back: ExternalSession = serde_json::from_str(&json).unwrap();
        assert_eq!(back, session);
    }
}
