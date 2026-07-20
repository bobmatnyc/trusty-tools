//! Session domain model: the daemon-owned first-class object (#2054, Axiom 4).
//!
//! Why: the vision spec's Axiom 4 ("The Daemon OWNS Sessions; CLI Attaches
//! Over the API") requires a session to be a real object living inside the
//! daemon — not a tmux pane, not a PTY, not something the CLI reconstructs
//! from scrollback. This module defines that object's shape; ownership,
//! storage, and the ring buffer live in `super::registry`.
//! What: [`Session`] (the wire/domain type returned by
//! `session.create`/`session.list`/`session.status`) and [`SessionStatus`]
//! (its lifecycle states, per §12 Session Durability: M1 has no queueing —
//! a session moves directly from `Created` to `Running` — and no
//! persistence, so `Failed` here means "the daemon observed a failure", not
//! "recovered from one after a restart").
//! Test: `model::tests::*`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Lifecycle status of a [`Session`].
///
/// Why: a small, closed set of states the registry can transition between
/// and that a client can match on reliably (the wire string is stable —
/// `#[serde(rename_all = "snake_case")]` — independent of Rust variant
/// naming).
/// What: `Created` -> `Running` happens immediately on `session.create` in
/// M1 (there is no queue to sit in ahead of real task execution, which
/// lands with `task.*` in #2056). `Cancelled`/`Finished`/`Failed`/
/// `DeadlineExceeded` are the four terminal states; `session.cancel` is the
/// only one #2054 can drive directly (`Finished`/`Failed`/`DeadlineExceeded`
/// are set by `task::executor::run_and_record` when the PM's `AgentLoop`
/// resolves). `DeadlineExceeded` (#2207) is distinct from `Failed`: it means
/// the run's configured wall-clock budget elapsed before the PM reached a
/// terminal state, NOT that the loop or LLM call errored — a run that hit
/// this status may have made real, useful progress.
/// Test: `model::tests::session_status_serialises_snake_case`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Created,
    Running,
    Cancelled,
    Finished,
    Failed,
    DeadlineExceeded,
}

impl SessionStatus {
    /// Return the stable wire string for this status (matches the serde
    /// `rename_all = "snake_case"` representation).
    ///
    /// Why: `crate::events::Event::SessionStatusChanged`'s `status` field is
    /// a plain `String`, not a `SessionStatus`, so registry code needs the
    /// wire string without round-tripping through `serde_json`.
    /// What: a `&'static str` per variant.
    /// Test: `model::tests::as_str_matches_serde_rename`.
    pub fn as_str(&self) -> &'static str {
        match self {
            SessionStatus::Created => "created",
            SessionStatus::Running => "running",
            SessionStatus::Cancelled => "cancelled",
            SessionStatus::Finished => "finished",
            SessionStatus::Failed => "failed",
            SessionStatus::DeadlineExceeded => "deadline_exceeded",
        }
    }

    /// Returns `true` when this status is terminal (the session will never
    /// transition again).
    ///
    /// Why: `session.cancel` and any future completion handler need to
    /// reject transitions out of a terminal state (e.g. cancelling an
    /// already-finished session is a no-op error, not a silent overwrite).
    /// What: `true` for `Cancelled`/`Finished`/`Failed`/`DeadlineExceeded`;
    /// `false` otherwise.
    /// Test: `model::tests::is_terminal_covers_terminal_states`.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            SessionStatus::Cancelled
                | SessionStatus::Finished
                | SessionStatus::Failed
                | SessionStatus::DeadlineExceeded
        )
    }
}

/// A daemon-owned session (vision spec Axiom 4).
///
/// Why: the wire type every `session.*` method (except `send`/`attach`/
/// `detach`, which acknowledge rather than return a session) sends back to
/// the caller — `session.create`, `session.list`, `session.status`.
/// What: `id` is a UUIDv4 string; `agent`/`project` are optional because
/// `session.create` does not require either in M1 (no real agent-loop
/// wiring yet); `created_at` is UTC. `mode` (#2059) is `None` until
/// `task.run` resolves and sets it (`SessionRegistry::set_mode`) — a session
/// that has never run a task has no mode, mirroring the same "`None` means
/// not yet run" convention `session::transcript::TranscriptRecord` already
/// uses for `cost_usd`.
///
/// `binding` is the session's TYPED project binding (spec DOC-39 §5.5) — the
/// single object `task.run` and `session.create` now converge on. `project`
/// survives as the human display label but is no longer an independent source
/// of truth: it is DERIVED from `binding` (`ProjectBinding::label`) and so can
/// never again disagree with it. That derivation is the whole reconciliation —
/// previously `session.create` took a free-form label that was never validated
/// and never bound, while `task.run` took a path the label knew nothing about.
/// Test: `model::tests::session_round_trips_through_json`,
/// `model::tests::projectless_session_round_trips`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub task: String,
    pub agent: Option<String>,
    /// Display label, derived from `binding` — see the struct docs. Kept for
    /// wire compatibility with existing `session.list`/`session.status`
    /// consumers that read a project name.
    pub project: Option<String>,
    pub status: SessionStatus,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub mode: Option<crate::mode::HarnessMode>,
    /// The typed three-state project binding. Defaults to projectless so an
    /// older persisted/serialized `Session` without the field still reads back.
    #[serde(default)]
    pub binding: crate::binding::ProjectBinding,
    /// (Issue #3298, DOC-48 §4.1) The workstream this session is bound to, if
    /// any. `None` means projectless/unbound — a fully valid state (DOC-39
    /// §4.2), not a missing one. Set exactly once, at creation
    /// (`session::protocol::create` / `task::protocol::task_run` resolve
    /// either an explicit `workstream_id` param or §4.2's ambient
    /// active-workstream default before calling
    /// `SessionRegistry::bind_workstream`); there is no setter that changes
    /// it afterward, mirroring `binding`'s own immutability convention — see
    /// `SessionRegistry::bind_workstream`'s docs for the enforcement point.
    /// `#[serde(default)]` keeps an older persisted/serialized `Session`
    /// (predating this field) reading back as unbound.
    #[serde(default)]
    pub workstream_id: Option<crate::workstreams::model::WorkstreamId>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The wire representation of every status must be the documented
    /// snake_case string.
    #[test]
    fn session_status_serialises_snake_case() {
        assert_eq!(
            serde_json::to_value(SessionStatus::Created).unwrap(),
            json!("created")
        );
        assert_eq!(
            serde_json::to_value(SessionStatus::Running).unwrap(),
            json!("running")
        );
        assert_eq!(
            serde_json::to_value(SessionStatus::Cancelled).unwrap(),
            json!("cancelled")
        );
        assert_eq!(
            serde_json::to_value(SessionStatus::Finished).unwrap(),
            json!("finished")
        );
        assert_eq!(
            serde_json::to_value(SessionStatus::Failed).unwrap(),
            json!("failed")
        );
        assert_eq!(
            serde_json::to_value(SessionStatus::DeadlineExceeded).unwrap(),
            json!("deadline_exceeded")
        );
    }

    /// `as_str` must match the serde wire representation exactly.
    #[test]
    fn as_str_matches_serde_rename() {
        for status in [
            SessionStatus::Created,
            SessionStatus::Running,
            SessionStatus::Cancelled,
            SessionStatus::Finished,
            SessionStatus::Failed,
            SessionStatus::DeadlineExceeded,
        ] {
            let via_str = status.as_str();
            let via_serde = serde_json::to_value(status).unwrap();
            assert_eq!(json!(via_str), via_serde);
        }
    }

    /// Only `Cancelled`/`Finished`/`Failed`/`DeadlineExceeded` are terminal.
    #[test]
    fn is_terminal_covers_terminal_states() {
        assert!(!SessionStatus::Created.is_terminal());
        assert!(!SessionStatus::Running.is_terminal());
        assert!(SessionStatus::Cancelled.is_terminal());
        assert!(SessionStatus::Finished.is_terminal());
        assert!(SessionStatus::Failed.is_terminal());
        assert!(SessionStatus::DeadlineExceeded.is_terminal());
    }

    /// A `Session` must round-trip through JSON with the expected shape.
    #[test]
    fn session_round_trips_through_json() {
        let session = Session {
            id: "s-1".to_string(),
            task: "do the thing".to_string(),
            agent: Some("engineer".to_string()),
            project: None,
            binding: crate::binding::ProjectBinding::None,
            status: SessionStatus::Running,
            created_at: Utc::now(),
            mode: Some(crate::mode::HarnessMode::DailyDriver),
            workstream_id: None,
        };
        let value = serde_json::to_value(&session).unwrap();
        assert_eq!(value["id"], "s-1");
        assert_eq!(value["status"], "running");
        assert_eq!(value["agent"], "engineer");
        assert!(value["project"].is_null());
        assert_eq!(value["mode"], "daily-driver");

        let back: Session = serde_json::from_value(value).unwrap();
        assert_eq!(back.id, session.id);
        assert_eq!(back.status, session.status);
        assert_eq!(back.mode, session.mode);
    }
}

#[cfg(test)]
mod binding_model_tests {
    use super::*;

    /// A projectless `Session` must round-trip, and a payload with NO `binding`
    /// field at all must read back as projectless rather than failing.
    #[test]
    fn projectless_session_round_trips() {
        let session = Session {
            id: "s-1".to_string(),
            task: "t".to_string(),
            agent: None,
            project: None,
            binding: crate::binding::ProjectBinding::None,
            status: SessionStatus::Running,
            created_at: Utc::now(),
            mode: None,
            workstream_id: None,
        };
        let value = serde_json::to_value(&session).expect("serialize");
        assert_eq!(value["binding"]["state"], "projectless");

        let back: Session = serde_json::from_value(value).expect("deserialize");
        assert_eq!(back.binding, crate::binding::ProjectBinding::None);

        // An older payload predating the `binding` field must still read back.
        let legacy = serde_json::json!({
            "id": "s-2", "task": "t", "agent": null, "project": null,
            "status": "running", "created_at": Utc::now(),
        });
        let back: Session =
            serde_json::from_value(legacy).expect("legacy payload must deserialize");
        assert_eq!(
            back.binding,
            crate::binding::ProjectBinding::None,
            "a missing binding field must default to projectless"
        );
        assert_eq!(
            back.workstream_id, None,
            "a missing workstream_id field must default to unbound"
        );
    }

    /// (Issue #3298) A `Session` bound to a workstream must round-trip that
    /// binding through JSON.
    #[test]
    fn workstream_bound_session_round_trips() {
        let ws_id = crate::workstreams::model::WorkstreamId::new();
        let session = Session {
            id: "s-1".to_string(),
            task: "t".to_string(),
            agent: None,
            project: None,
            binding: crate::binding::ProjectBinding::None,
            status: SessionStatus::Running,
            created_at: Utc::now(),
            mode: None,
            workstream_id: Some(ws_id),
        };
        let value = serde_json::to_value(&session).expect("serialize");
        assert_eq!(value["workstream_id"], ws_id.to_string());

        let back: Session = serde_json::from_value(value).expect("deserialize");
        assert_eq!(back.workstream_id, Some(ws_id));
    }
}
