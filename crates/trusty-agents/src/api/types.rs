//! Canonical JSON response envelope for PM output (#151).
//!
//! Why: Existing trusty-agents writes free-form text to stdout, making it hard for
//! HTTP clients, GUIs, or downstream tools to consume results. `PmResponse`
//! gives every workflow/agent run a single stable JSON shape that carries
//! narrative + metadata + per-phase breakdown + file list + errors.
//! What: Serde-derived structs used by `--json` CLI output (Phase 1), the
//! HTTP API server (Phase 2), and the `ompm` thin client (Phase 3).
//! Test: `cargo test api::types` exercises the round-trip serialization and
//! the `PmResponse::running`/`from_workflow` constructors.

use serde::{Deserialize, Serialize};

/// Canonical response envelope returned by every PM/workflow invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PmResponse {
    /// uuid v4 for this response (unique per run).
    pub id: String,
    /// ISO8601 timestamp of when this response was produced.
    pub timestamp: String,
    #[serde(rename = "type")]
    pub response_type: PmResponseType,
    pub status: PmStatus,
    /// Human-readable summary. Printed verbatim in non-JSON mode to preserve
    /// the pre-#151 stdout behavior.
    pub narrative: String,
    pub metadata: PmMetadata,
    pub phases: Vec<PhaseResult>,
    /// Relative paths of files written into `out_dir` (if any).
    pub files_modified: Vec<String>,
    pub out_dir: Option<String>,
    /// Collected error strings. Empty for `status = success`.
    pub errors: Vec<String>,
    /// #149: Live phase progress streamed by the workflow engine. Each entry
    /// is appended as a phase starts / completes / fails so HTTP pollers
    /// (notably the Tauri UI) can render real-time progress without waiting
    /// for the workflow to finish.
    ///
    /// Why: Workflows run for 20–70 minutes; without intermediate progress
    /// the UI shows a spinner with no detail. Surfacing per-phase status,
    /// elapsed seconds, and cost lets the UI display the same live timeline
    /// the terminal sees via `ProgressReporter`.
    /// What: Empty `Vec` for the running placeholder; appended to as phases
    /// complete. The terminal `done` envelope embeds the same list for
    /// completeness.
    /// Test: `phase_progress_serializes_to_json`.
    #[serde(default)]
    pub phases_completed: Vec<PhaseProgress>,
    /// #3737 (per-message chat attribution, epic #3052): the name of the
    /// specialist agent that actually produced this answer, when the turn
    /// delegated away from the caller. `None` for a turn the caller answered
    /// itself (no delegation fired). Populated server-side from the session's
    /// last `AgentSpawned`/`PmDelegating` event (`api/server/handlers.rs`).
    ///
    /// Why: chat attribution must reflect who ANSWERED, not who was asked —
    /// e.g. base "Assistant" delegating a weather question to Izzie must label
    /// the bubble "Izzie", not "Assistant". This carries the server-authoritative
    /// responder identity so the GUI can overwrite its request-time speaker
    /// stamp on `task-complete`. It is an agent NAME (roster id); the GUI maps
    /// it to a display name via the `/api/agents` catalog.
    /// What: `Option<String>`; `skip_serializing_if` keeps the wire shape
    /// byte-identical to pre-#3737 for the common (no-delegation) case, and
    /// `default` keeps deserializing older persisted `tasks.json` snapshots.
    /// Test: `responder_agent_defaults_to_none_and_omits_when_absent`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub responder_agent: Option<String>,
}

/// One entry in the live progress stream returned by `GET /api/task/:id`.
///
/// Why (#149): The Tauri UI polls task status and needs a stable, growing
/// list of phase events to render a "research → plan → code → qa → observe"
/// timeline. Reusing `PhaseResult` doesn't fit because that struct carries
/// final token counts; live progress only needs status, elapsed, cost, and
/// an optional human note (e.g. "35/35 passed").
/// What: Plain serializable struct; status is a free-form string (`"running"`,
/// `"done"`, `"failed"`) so the UI can render arbitrary phase states without
/// requiring strict enum versioning.
/// Test: `phase_progress_serializes_to_json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PhaseProgress {
    pub name: String,
    /// `"running"`, `"done"`, or `"failed"`.
    pub status: String,
    pub elapsed_secs: f32,
    pub cost_usd: f32,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PmResponseType {
    WorkflowResult,
    AgentResponse,
    TaskSubmitted,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PmStatus {
    Success,
    Partial,
    Failed,
    Running,
    /// #3063: the client aborted this task via `DELETE /api/task/:id` (or it
    /// was swept up by `POST /api/clear-context`) before it reached a
    /// natural terminal state. Distinct from `Failed` so pollers can tell
    /// "the client stopped this" apart from "the workflow itself errored".
    Cancelled,
}

impl PmStatus {
    /// Stable lowercase string form used in `SessionDone` events and the
    /// cancel-response JSON body.
    ///
    /// Why: The status->string mapping was duplicated at two call sites in
    /// `api::server::handlers` (one per intent-routing branch). Centralizing
    /// it here means a new `PmStatus` variant is a single compile error to
    /// fix instead of two, and keeps `Failed` mapping to the wire value
    /// `"error"` (not `"failed"`, which is the pre-existing convention)
    /// consistent everywhere.
    /// What: `Success`->"success", `Failed`->"error", `Partial`->"partial",
    /// `Running`->"running", `Cancelled`->"cancelled".
    /// Test: `pm_status_as_str_matches_wire_values`.
    pub fn as_str(&self) -> &'static str {
        match self {
            PmStatus::Success => "success",
            PmStatus::Failed => "error",
            PmStatus::Partial => "partial",
            PmStatus::Running => "running",
            PmStatus::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PmMetadata {
    pub processing_time_ms: u64,
    pub total_tokens_in: u64,
    pub total_tokens_out: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub total_cost_usd: f64,
    pub model: Option<String>,
    pub workflow: Option<String>,
    pub build_number: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseResult {
    pub name: String,
    pub status: PmStatus,
    pub duration_ms: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub summary: String,
}

impl PmResponse {
    /// Construct a `running` placeholder used when the HTTP server accepts a
    /// task but the workflow hasn't finished yet. The `id` here is the
    /// task id the client will poll for.
    ///
    /// Why: Phase 2 — server returns a placeholder on `POST /api/task` and
    /// updates it in place when the background task completes.
    /// What: Minimal envelope with `type=task_submitted`, `status=running`.
    /// Test: `running_is_well_formed`.
    pub fn running(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            timestamp: now_iso8601(),
            response_type: PmResponseType::TaskSubmitted,
            status: PmStatus::Running,
            narrative: String::new(),
            metadata: PmMetadata::default(),
            phases: Vec::new(),
            files_modified: Vec::new(),
            out_dir: None,
            errors: Vec::new(),
            phases_completed: Vec::new(),
            responder_agent: None,
        }
    }

    /// Construct a terminal `error` envelope.
    pub fn error(id: impl Into<String>, msg: impl Into<String>) -> Self {
        let m = msg.into();
        Self {
            id: id.into(),
            timestamp: now_iso8601(),
            response_type: PmResponseType::Error,
            status: PmStatus::Failed,
            narrative: m.clone(),
            metadata: PmMetadata::default(),
            phases: Vec::new(),
            files_modified: Vec::new(),
            out_dir: None,
            errors: vec![m],
            phases_completed: Vec::new(),
            responder_agent: None,
        }
    }

    /// Construct a terminal `cancelled` envelope (#3063).
    ///
    /// Why: `DELETE /api/task/:id` needs a stable terminal shape distinct
    /// from `error()` so pollers can tell "the client aborted this" apart
    /// from "the workflow itself failed" — `status` is the authoritative
    /// field; `type` stays `Error` since no caller branches on `type` today
    /// (see the #3063 PR body for the rationale) and adding a dedicated
    /// `PmResponseType` variant would be surface area with no consumer yet.
    /// What: Minimal envelope with `status = Cancelled` and a fixed
    /// narrative; no phases/files/errors since the task never produced any.
    /// Test: `cancelled_is_well_formed`.
    pub fn cancelled(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            timestamp: now_iso8601(),
            response_type: PmResponseType::Error,
            status: PmStatus::Cancelled,
            narrative: "Task cancelled by client request".to_string(),
            metadata: PmMetadata::default(),
            phases: Vec::new(),
            files_modified: Vec::new(),
            out_dir: None,
            errors: Vec::new(),
            phases_completed: Vec::new(),
            responder_agent: None,
        }
    }
}

/// Return the current UTC time formatted as ISO8601.
///
/// Why: chrono is already a transitive dep; centralizing the format keeps
/// timestamps consistent across producers.
/// What: RFC3339 with second precision.
/// Test: `iso8601_has_t_separator`.
pub(crate) fn now_iso8601() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_is_well_formed() {
        let r = PmResponse::running("abc");
        assert_eq!(r.id, "abc");
        assert_eq!(r.status, PmStatus::Running);
        assert_eq!(r.response_type, PmResponseType::TaskSubmitted);
        assert!(r.errors.is_empty());
    }

    #[test]
    fn round_trip_serializes_type_field() {
        let r = PmResponse::running("id1");
        let j = serde_json::to_string(&r).unwrap();
        assert!(j.contains("\"type\":\"task_submitted\""), "got: {j}");
        assert!(j.contains("\"status\":\"running\""), "got: {j}");
        let back: PmResponse = serde_json::from_str(&j).unwrap();
        assert_eq!(back.id, "id1");
    }

    #[test]
    fn error_populates_errors_vec() {
        let r = PmResponse::error("id", "boom");
        assert_eq!(r.status, PmStatus::Failed);
        assert_eq!(r.errors, vec!["boom".to_string()]);
        assert_eq!(r.narrative, "boom");
    }

    #[test]
    fn phase_progress_serializes_to_json() {
        let p = PhaseProgress {
            name: "research".into(),
            status: "done".into(),
            elapsed_secs: 42.5,
            cost_usd: 0.08,
            note: Some("ok".into()),
        };
        let j = serde_json::to_string(&p).unwrap();
        assert!(j.contains("\"name\":\"research\""), "got: {j}");
        assert!(j.contains("\"status\":\"done\""), "got: {j}");
        assert!(j.contains("\"elapsed_secs\":42.5"), "got: {j}");
        assert!(j.contains("\"cost_usd\":0.08"), "got: {j}");
        assert!(j.contains("\"note\":\"ok\""), "got: {j}");
        // Round-trips back to the same struct.
        let back: PhaseProgress = serde_json::from_str(&j).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn pm_response_running_includes_empty_phases_completed() {
        let r = PmResponse::running("abc");
        assert!(r.phases_completed.is_empty());
    }

    /// #3737: `responder_agent` defaults to `None`, is OMITTED from the wire
    /// when absent (so the common no-delegation case is byte-identical to the
    /// pre-#3737 shape), and round-trips when present (the delegated case the
    /// GUI reads to relabel a bubble to the agent that actually answered).
    #[test]
    fn responder_agent_defaults_to_none_and_omits_when_absent() {
        let r = PmResponse::running("abc");
        assert_eq!(r.responder_agent, None);
        let j = serde_json::to_string(&r).unwrap();
        assert!(
            !j.contains("responder_agent"),
            "absent responder must be omitted from the wire, got: {j}"
        );

        let mut delegated = PmResponse::running("abc");
        delegated.responder_agent = Some("izzie".to_string());
        let j2 = serde_json::to_string(&delegated).unwrap();
        assert!(j2.contains("\"responder_agent\":\"izzie\""), "got: {j2}");
        let back: PmResponse = serde_json::from_str(&j2).unwrap();
        assert_eq!(back.responder_agent.as_deref(), Some("izzie"));

        // An older snapshot without the field still deserializes.
        let legacy = r#"{"id":"x","timestamp":"t","type":"task_submitted","status":"running","narrative":"","metadata":{"processing_time_ms":0,"total_tokens_in":0,"total_tokens_out":0,"cache_read_tokens":0,"cache_creation_tokens":0,"total_cost_usd":0.0,"model":null,"workflow":null,"build_number":null},"phases":[],"files_modified":[],"out_dir":null,"errors":[]}"#;
        let parsed: PmResponse = serde_json::from_str(legacy).unwrap();
        assert_eq!(parsed.responder_agent, None);
    }

    #[test]
    fn iso8601_has_t_separator() {
        let s = now_iso8601();
        assert!(
            s.contains('T'),
            "expected ISO8601 with T separator, got {s}"
        );
    }

    #[test]
    fn cancelled_is_well_formed() {
        let r = PmResponse::cancelled("abc");
        assert_eq!(r.id, "abc");
        assert_eq!(r.status, PmStatus::Cancelled);
        assert!(r.errors.is_empty(), "cancellation is not an error");
        let j = serde_json::to_string(&r).unwrap();
        assert!(j.contains("\"status\":\"cancelled\""), "got: {j}");
    }

    #[test]
    fn pm_status_as_str_matches_wire_values() {
        assert_eq!(PmStatus::Success.as_str(), "success");
        assert_eq!(PmStatus::Failed.as_str(), "error");
        assert_eq!(PmStatus::Partial.as_str(), "partial");
        assert_eq!(PmStatus::Running.as_str(), "running");
        assert_eq!(PmStatus::Cancelled.as_str(), "cancelled");
    }
}
