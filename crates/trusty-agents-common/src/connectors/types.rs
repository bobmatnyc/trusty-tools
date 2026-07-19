//! Portable value types for [`super::WorkstreamConnector`] — request/response
//! shapes both the tm and tcode backends speak (DOC-44 §5.2/§5.4).
//!
//! Why: DOC-44 §1.2/§4.1 requires the tm and tcode backends to be reachable
//! through ONE trait without forcing them to be symmetric — tm provisions a
//! git worktree per session and attaches via a tmux shell command; tcode
//! creates a session inside an existing local project and attaches via a
//! ring-buffer replay + SSE stream. Rather than lossy-flatten that asymmetry
//! into a single struct (which would leave half the fields meaningless for
//! either backend), the two points of genuine divergence — session creation
//! and attach — are modelled as explicit enums (`BackendParams`,
//! `AttachHandle`); everything else (list/status/send/delegate) is symmetric
//! enough to share one shape.
//! What: [`CreateSessionReq`] (common core + [`BackendParams`] extension),
//! [`SessionInfo`] (list-entry summary), [`SessionStatus`] (point-lookup
//! detail), [`AttachHandle`] (the tm-vs-tcode attach asymmetry),
//! [`AgentSpec`]/[`DelegateHandle`] (the `delegate` operation's request and
//! response).
//! Test: `types::tests` covers construction, the `BackendParams`/
//! `AttachHandle` variant matching, and serde round-trips for the wire types.

use serde::{Deserialize, Serialize};

/// Per-backend session-creation parameters (DOC-44 locked decision 4).
///
/// Why: tm provisions an isolated git worktree (needs `repo_url`/`git_ref`/
/// `runtime`/`ephemeral`); tcode binds to an existing local project directory
/// (needs `project`). Forcing one struct with all fields optional would let a
/// caller build a request that is valid for neither backend and silently
/// drop data; a backend rejects the variant it does not implement with
/// [`super::ConnectorError::InvalidRequest`] instead.
/// What: `Tm` mirrors the daemon's `SpawnRequest` provisioning fields
/// (`crates/trusty-mpm/src/daemon/managed_routes/mod.rs`); `Tcode` carries
/// the existing local project path `session.create` requires.
/// Test: `types::tests::backend_params_variants_are_distinct`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendParams {
    /// tm: provision a new isolated worktree session.
    Tm {
        /// Repository URL to provision the session workspace from.
        repo_url: String,
        /// Git branch or ref to check out.
        git_ref: String,
        /// Optional runtime selector (`"claude-code"` | `"tcode"`).
        runtime: Option<String>,
        /// `true` tags the session as ephemeral (test/throwaway, eligible
        /// for bulk teardown + age-based auto-reap).
        ephemeral: bool,
    },
    /// tcode: bind a new session to an existing local project directory.
    Tcode {
        /// Path to the existing local project the session is scoped to.
        project: std::path::PathBuf,
    },
}

/// Common core session-creation request, plus the per-backend extension.
///
/// Why: `task`/`name_hint`/`agent` are meaningful to both backends
/// (tm: task description + tmux name hint; tcode: `session.create`'s
/// `task`/`agent` params) — see DOC-44 locked decision 4.
/// What: `name_hint` is tm-specific in practice (tcode has no equivalent
/// concept and ignores it); `agent` is tcode-specific in practice (tm's
/// spawn request has no `agent` field and ignores it). Both stay on the
/// common core rather than moving into `BackendParams` because a caller
/// that doesn't know which backend it's talking to can still fill them in
/// harmlessly — the receiving backend ignores what it doesn't use rather
/// than erroring.
/// Test: `types::tests::create_session_req_round_trips_backend_params`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateSessionReq {
    /// Human-readable task description for the session.
    pub task: String,
    /// Optional name hint (tm: overrides the auto-generated tmux name).
    pub name_hint: Option<String>,
    /// Optional agent name (tcode: `session.create`'s `agent` param).
    pub agent: Option<String>,
    /// The per-backend extension — see [`BackendParams`].
    pub backend: BackendParams,
}

/// One session as returned by `list_sessions`.
///
/// Why: both backends already expose a flat, string-typed summary
/// (tm's `SessionSummary`, tcode's `Session`) — this is the intersection a
/// caller can render without knowing which backend produced it.
/// What: `state` deliberately stays a raw backend-native string rather than
/// a shared enum — tm's lifecycle vocabulary (`"active"`, `"stopped"`,
/// `"errored"`, `"provisioning"`, `"decommissioned"`, …) and tcode's
/// (`"created"`, `"running"`, `"cancelled"`, `"finished"`, `"failed"`,
/// `"deadline_exceeded"`) do not have a 1:1 mapping; collapsing them into one
/// enum would either lose information or invent states neither backend has.
/// `task` is optional because a legacy tm record may omit it.
/// Test: `types::tests::session_info_serde_round_trip`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    /// Backend-native session id.
    pub id: String,
    /// Display name (tm: tmux session name; tcode: mirrors `task`, see that
    /// backend's connector for why — tcode sessions have no separate name).
    pub name: String,
    /// Backend-native lifecycle state string — see the struct docs.
    pub state: String,
    /// Task description, when the backend records one.
    pub task: Option<String>,
}

/// One session's detail as returned by `session_status`.
///
/// Why: a point lookup needs slightly more than the list summary (a pending
/// decision, when the backend surfaces one) without pulling the full
/// backend-native record.
/// What: `state` uses the same backend-native vocabulary as
/// [`SessionInfo::state`] — see that field's docs.
/// Test: `types::tests::session_status_serde_round_trip`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionStatus {
    /// Backend-native session id.
    pub id: String,
    /// Backend-native lifecycle state string.
    pub state: String,
    /// A pending decision question the session is blocked on, if the
    /// backend surfaces one (tm only; tcode always reports `None`).
    pub pending_decision: Option<String>,
}

/// The asymmetric result of `attach` (DOC-44 locked decision 2).
///
/// Why: tm's attach surface is a tmux shell command an operator runs in
/// their own terminal (`GET .../attach-cmd`); tcode's is a ring-buffer
/// replay plus a live SSE stream URL (`session.attach`). These are not two
/// encodings of the same concept — forcing them into one shape (e.g. always
/// returning a URL, with tm's being a fake `tmux://` scheme) would invent a
/// protocol neither backend speaks. The enum keeps each backend's real
/// contract visible to the caller.
/// What: `ShellCommand` — a fully-formed command line the caller (a human
/// operator, or code that shells out) runs directly, e.g.
/// `"tmux attach -t tmpm-a1b2c3"`. `EventStream` — the replayed ring-buffer
/// events plus the URL a caller GETs (as SSE) for live events.
/// Test: `types::tests::attach_handle_variants_are_distinct`.
#[derive(Debug, Clone, PartialEq)]
pub enum AttachHandle {
    /// tm: a `tmux attach -t <name>` command string the caller runs.
    ShellCommand(String),
    /// tcode: ring-buffer replay plus the SSE stream URL for live events.
    EventStream {
        /// The session id the stream belongs to.
        session_id: String,
        /// URL to `GET` (as Server-Sent Events) for live events.
        stream_url: String,
        /// Ring-buffer replay of recent events, oldest first.
        replayed_events: Vec<serde_json::Value>,
    },
}

/// Request payload for `delegate` — spawn a sub-agent within a session.
///
/// Why: shared by both backends' `delegate` signature even though only tm
/// implements it (tcode returns
/// [`super::ConnectorError::NotSupported`] — see that backend's docs).
/// What: `agent_name` names the sub-agent persona/role; `task` is its
/// instruction; `tier` is an optional model-tier hint (tm: `"haiku"` |
/// `"sonnet"` | `"opus"`, mirroring `agent_delegate`'s MCP tool signature).
/// Test: `types::tests::agent_spec_construction`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSpec {
    /// Name of the agent/persona to delegate to.
    pub agent_name: String,
    /// Task instruction for the delegated agent.
    pub task: String,
    /// Optional model-tier hint (backend-specific vocabulary).
    pub tier: Option<String>,
}

/// Result of a successful `delegate` call.
///
/// Why: the caller needs a handle to correlate this delegation with later
/// status/audit lookups (DOC-44 §3.5's audit trail, future work) without the
/// connector owning any state itself (connectors are stateless — DOC-44
/// §2.1).
/// What: `delegate_id` is the backend-assigned tracking id (tm:
/// `Delegation::id`, a UUID); `note` carries any backend-supplied
/// human-readable context (tm: the circuit-breaker state at delegation
/// time).
/// Test: `types::tests::delegate_handle_construction`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelegateHandle {
    /// Backend-assigned id tracking this delegation.
    pub delegate_id: String,
    /// Optional backend-supplied human-readable context.
    pub note: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_params_variants_are_distinct() {
        let tm = BackendParams::Tm {
            repo_url: "https://example/repo".into(),
            git_ref: "main".into(),
            runtime: None,
            ephemeral: false,
        };
        let tcode = BackendParams::Tcode {
            project: std::path::PathBuf::from("/tmp/proj"),
        };
        assert!(matches!(tm, BackendParams::Tm { .. }));
        assert!(matches!(tcode, BackendParams::Tcode { .. }));
    }

    #[test]
    fn create_session_req_round_trips_backend_params() {
        let req = CreateSessionReq {
            task: "fix the bug".into(),
            name_hint: Some("hint".into()),
            agent: None,
            backend: BackendParams::Tcode {
                project: std::path::PathBuf::from("/tmp/proj"),
            },
        };
        assert_eq!(req.task, "fix the bug");
        assert!(matches!(req.backend, BackendParams::Tcode { .. }));
    }

    #[test]
    fn session_info_serde_round_trip() {
        let info = SessionInfo {
            id: "abc".into(),
            name: "tmpm-abc".into(),
            state: "active".into(),
            task: Some("do the thing".into()),
        };
        let json = serde_json::to_string(&info).unwrap();
        let back: SessionInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(info, back);
    }

    #[test]
    fn session_status_serde_round_trip() {
        let status = SessionStatus {
            id: "abc".into(),
            state: "running".into(),
            pending_decision: None,
        };
        let json = serde_json::to_string(&status).unwrap();
        let back: SessionStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(status, back);
    }

    #[test]
    fn attach_handle_variants_are_distinct() {
        let shell = AttachHandle::ShellCommand("tmux attach -t x".into());
        let stream = AttachHandle::EventStream {
            session_id: "abc".into(),
            stream_url: "/sessions/abc/events".into(),
            replayed_events: vec![],
        };
        assert!(matches!(shell, AttachHandle::ShellCommand(_)));
        assert!(matches!(stream, AttachHandle::EventStream { .. }));
    }

    #[test]
    fn agent_spec_construction() {
        let spec = AgentSpec {
            agent_name: "research".into(),
            task: "find the bug".into(),
            tier: Some("opus".into()),
        };
        assert_eq!(spec.agent_name, "research");
        assert_eq!(spec.tier.as_deref(), Some("opus"));
    }

    #[test]
    fn delegate_handle_construction() {
        let handle = DelegateHandle {
            delegate_id: "uuid-1".into(),
            note: None,
        };
        assert_eq!(handle.delegate_id, "uuid-1");
        assert!(handle.note.is_none());
    }
}
