//! Record → wire-shape conversion helpers shared across the managed-session
//! route handlers.
//!
//! Why: `managed_routes/mod.rs` was at the 500-SLOC production cap; these
//! conversion helpers are pure and self-contained, so extracting them (rather
//! than a handler) keeps `mod.rs` focused on request/response shapes and
//! routing while leaving every call site's import path unchanged (all three
//! non-JSON helpers are re-exported through `mod.rs` at their original
//! visibility).
//! What: [`record_to_json`] (MCP wire shape, crate-public), [`record_to_summary`]
//! (HTTP wire shape), [`attach_cmd_for`], and [`parse_id`].
//! Test: `serializers_include_source_id` in `super::tests`; the handler tests
//! throughout `managed_routes` exercise these indirectly.

use axum::http::StatusCode;

use crate::session_manager::{InjectionStatus, ManagedSessionId, SessionRecord};

use super::SessionSummary;

/// Serialize a [`SessionRecord`] to the flat JSON shape the MCP tools return.
///
/// Why: the MCP tools return JSON values (not axum responses); reusing the same
/// field set as `SpawnResponse`/[`SessionSummary`] keeps the MCP and HTTP
/// payloads consistent for the driver skill. `source_id` is included so MCP
/// callers can filter or reconnect by project identity, matching what the HTTP
/// `GET /api/v1/sessions/managed` path already exposes (#1733).
/// What: maps the record to a JSON object including the derived `attach_cmd`
/// and `source_id` (null when the session has no project identity).
/// Test: `serializers_include_source_id` unit test in `super::tests`; also
/// covered by `crate::daemon::mcp_session` tests that assert echoed fields.
pub fn record_to_json(r: &SessionRecord) -> serde_json::Value {
    serde_json::json!({
        "id": r.id.to_string(),
        "name": r.tmux_name,
        "state": r.state.to_string(),
        "workspace_path": r.workspace_path.as_ref().map(|p| p.to_string_lossy().to_string()),
        "cwd": r.cwd.to_string_lossy().to_string(),
        "task": r.task,
        "repo_url": r.repo_url,
        "branch": r.branch,
        "created_at": r.created_at.to_rfc3339(),
        "last_activity_at": r.last_activity_at.map(|t| t.to_rfc3339()),
        "attach_cmd": attach_cmd_for(&r.tmux_name),
        "runtime": r.runtime.as_str(),
        "pending_decision": r.pending_decision,
        "proposed_default": r.proposed_default,
        "source_id": r.source_id,
        "deliverable_id": r.deliverable_id.map(|id| id.to_string()),
        "pane_id": r.pane_id,
        "injection_status": injection_status_wire(r.injection_status),
    })
}

/// Map [`InjectionStatus`] to its wire form, `None` for "never attempted"
/// (#2364).
///
/// Why: shared by [`record_to_json`] and [`record_to_summary`] so the MCP and
/// HTTP wire shapes cannot drift on what "no injection happened" looks like —
/// both omit the field (JSON `null`/absent) rather than emitting the literal
/// `"not_applicable"` string, keeping the field silent for the (common)
/// sessions injection never applies to.
/// What: `NotApplicable` → `None`; every other variant → `Some(<snake_case>)`
/// via [`InjectionStatus`]'s `Display`.
/// Test: `injection_status_wire_omits_not_applicable`,
/// `injection_status_wire_stringifies_other_variants` in `super::tests`.
fn injection_status_wire(status: InjectionStatus) -> Option<String> {
    match status {
        InjectionStatus::NotApplicable => None,
        other => Some(other.to_string()),
    }
}

/// Convert a [`SessionRecord`] into a wire [`SessionSummary`].
///
/// Why: the API exposes a flat, string-typed summary so clients don't depend on
/// the internal record shape.
/// What: maps every record field to its serialized form. `unresumable` always
/// starts `false` here — it requires an async filesystem probe
/// (`session_manager::resume_workdir::is_unresumable`), which this function
/// cannot perform since it is a pure/sync conversion shared by handlers that
/// have no reason to pay that cost (spawn/reactivate/decommission are never
/// mid-flight through the dead-workspace predicate). The list/get handlers
/// (#2595) overwrite the field on the returned value when they can await the
/// probe.
/// Test: covered by the list/get handler tests.
pub(super) fn record_to_summary(r: &SessionRecord) -> SessionSummary {
    SessionSummary {
        id: r.id.to_string(),
        name: r.tmux_name.clone(),
        state: r.state.to_string(),
        workspace_path: r
            .workspace_path
            .as_ref()
            .map(|p| p.to_string_lossy().to_string()),
        repo_url: r.repo_url.clone(),
        branch: r.branch.clone(),
        created_at: r.created_at.to_rfc3339(),
        last_activity_at: r.last_activity_at.map(|t| t.to_rfc3339()),
        pending_decision: r.pending_decision.clone(),
        proposed_default: r.proposed_default.clone(),
        source_id: r.source_id.clone(),
        task: Some(r.task.clone()),
        cwd: Some(r.cwd.to_string_lossy().to_string()),
        claude_session_id: r.claude_session_id.clone(),
        deliverable_id: r.deliverable_id.map(|id| id.to_string()),
        pane_id: r.pane_id.clone(),
        injection_status: injection_status_wire(r.injection_status),
        unresumable: false,
    }
}

/// [`record_to_summary`] plus the async `unresumable` probe (#2595).
///
/// Why: `list_managed_sessions`/`get_managed_session` are the two handlers
/// that can afford the filesystem probe (both already `.await` the session
/// manager). Folding "convert, then overwrite the flag" into one call here —
/// rather than repeating it inline at each handler — keeps `mod.rs`'s handler
/// bodies a single line each (`mod.rs` sits at its 500-SLOC production cap).
/// What: [`record_to_summary`], then overwrites `unresumable` via
/// `session_manager::resume_workdir::is_unresumable`.
/// Test: `list_marks_dead_stopped_session_unresumable`,
/// `list_leaves_live_and_healthy_stopped_sessions_unmarked` in `super::tests`.
pub(super) async fn record_to_summary_checked(r: &SessionRecord) -> SessionSummary {
    let mut summary = record_to_summary(r);
    summary.unresumable = crate::session_manager::resume_workdir::is_unresumable(r).await;
    summary
}

/// Build the tmux attach command string for a session.
///
/// Why: clients need the exact attach command without hardcoding the convention.
/// What: returns `tmux attach-session -t <name>`.
/// Test: attach-cmd handler test.
pub(super) fn attach_cmd_for(tmux_name: &str) -> String {
    format!("tmux attach-session -t {tmux_name}")
}

/// Parse a UUID path segment into a [`ManagedSessionId`].
///
/// Why: handlers receive the id as a string; an invalid UUID must produce a 400
/// rather than a 404 or panic.
/// What: parses the string into a UUID, mapping failure to a `400` tuple.
/// Test: covered by handler tests that pass an invalid id.
pub(super) fn parse_id(id_str: &str) -> Result<ManagedSessionId, (StatusCode, String)> {
    id_str
        .parse::<uuid::Uuid>()
        .map(ManagedSessionId::from)
        .map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                format!("invalid session id: {id_str}"),
            )
        })
}
