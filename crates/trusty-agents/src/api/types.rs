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

    #[test]
    fn iso8601_has_t_separator() {
        let s = now_iso8601();
        assert!(
            s.contains('T'),
            "expected ISO8601 with T separator, got {s}"
        );
    }
}
