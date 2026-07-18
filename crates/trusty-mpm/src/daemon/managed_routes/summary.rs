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

use std::collections::HashMap;
use std::path::PathBuf;

use axum::http::StatusCode;

use crate::core::session_assets::{session_asset_staleness_with_catalog, session_plan};
use crate::core::update_check::CatalogHashes;
use crate::session_manager::{
    InjectionStatus, ManagedSessionId, ManagedSessionState, SessionRecord,
};

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
        stale_assets: false,
    }
}

/// Whether a record's state is one [`session_assets_stale`] is worth probing.
///
/// Why: shared by [`record_to_summary_checked`] and [`checked_summaries`] so
/// the two never disagree on which states get the asset-staleness probe.
/// Provisioning workspaces have not deployed yet (every managed artifact would
/// spuriously read as "new"/stale) and decommissioned ones have no workspace
/// left to probe — both would only add noise, not signal.
/// What: `true` for `Active`, `Stopped`, and `Errored`.
/// Test: `checked_summaries_flags_stale_assets_only_for_relevant_states`.
fn probe_staleness_for(state: &ManagedSessionState) -> bool {
    matches!(
        state,
        ManagedSessionState::Active | ManagedSessionState::Stopped | ManagedSessionState::Errored
    )
}

/// Compute `stale_assets` for every record in `records`, sharing the
/// catalog-side compose/hash work across ALL of them (issue #2444 review,
/// MEDIUM finding: `checked_summaries` originally called
/// `session_assets_stale` — a full, independent catalog recompose — once PER
/// SESSION on every `tm sessions ls`, recomposing the same ~40+ catalog
/// agents redundantly for every session sharing the default source).
///
/// Why: a staleness comparison splits into an expensive CATALOG-side half
/// (composing agents, reading skill bodies — identical for every session
/// sharing the same resolved `(agent_source, skill_source)` pair) and a
/// cheap DEPLOYED-side half (this session's own manifest + on-disk files).
/// Grouping records by their resolved source pair via
/// [`crate::core::session_assets::session_plan`] and computing
/// [`CatalogHashes::compute`] ONCE per distinct pair — which collapses to a
/// SINGLE compute for the common case where every session resolves the
/// shared default bundled/catalog source, and stays exactly correct for the
/// rare session carrying its own project-level manifest override — removes
/// the N-times recompose entirely. Both the manifest resolution and the
/// catalog compose are filesystem-bound, so this whole function is
/// synchronous/blocking; callers run it via [`tokio::task::spawn_blocking`].
/// What: returns `record.id -> stale` for every record passed in (typically
/// pre-filtered to [`probe_staleness_for`]'s syncable subset by the caller).
/// Test: `checked_summaries_stale_assets_independent_per_session_sharing_one_catalog`,
/// `checked_summaries_flags_stale_assets_only_for_relevant_states` in
/// `super::tests`.
fn stale_assets_for_many(records: Vec<SessionRecord>) -> HashMap<ManagedSessionId, bool> {
    let mut cache: HashMap<(PathBuf, PathBuf), CatalogHashes> = HashMap::new();
    let mut result = HashMap::with_capacity(records.len());
    for record in records {
        let (fw, plan) = session_plan(&record);
        let key = (plan.agent_source.clone(), plan.skill_source.clone());
        let catalog = cache
            .entry(key)
            .or_insert_with(|| CatalogHashes::compute(&plan.agent_source, &plan.skill_source));
        let stale = session_asset_staleness_with_catalog(&fw, &plan, catalog).stale;
        result.insert(record.id, stale);
    }
    result
}

/// Run [`stale_assets_for_many`] on the blocking pool for a single record —
/// the `record_to_summary_checked` (single-session GET) call site, which has
/// no batching benefit (N=1) but still needs the same blocking-pool handoff
/// [`checked_summaries`] uses for the multi-session path.
async fn probe_stale_assets(record: SessionRecord) -> bool {
    let id = record.id;
    tokio::task::spawn_blocking(move || stale_assets_for_many(vec![record]))
        .await
        .unwrap_or_default()
        .get(&id)
        .copied()
        .unwrap_or(false)
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
    if probe_staleness_for(&r.state) {
        summary.stale_assets = probe_stale_assets(r.clone()).await;
    }
    summary
}

/// [`record_to_summary`] for MANY records, running each `unresumable` probe
/// concurrently rather than one-at-a-time (#2595 review, MEDIUM finding 4).
///
/// Why: `list_managed_sessions` originally awaited [`record_to_summary_checked`]
/// sequentially in a `for` loop — for a fleet of N sessions that serializes N
/// `tokio::fs::try_exists`-backed round trips even though they are entirely
/// independent. `is_unresumable` already short-circuits with NO I/O for any
/// state other than `Stopped`/`Errored` (the state gate), so only that subset
/// ever pays the probe cost — fan out exactly those via `tokio::task::JoinSet`
/// (unconditional `tokio` dependency; the workspace's optional `futures` crate
/// is feature-gated and not assumed available to every daemon route) rather
/// than every record.
/// What: builds every summary synchronously first via [`record_to_summary`]
/// (cheap, no I/O — preserves `records`' order), then spawns one
/// `is_unresumable` task per `Stopped`/`Errored` record (each tagged with its
/// original index so `JoinSet`'s out-of-completion-order yield can never
/// reorder the response), and patches each result back into its slot as
/// tasks complete. A panicking probe task is dropped silently — that summary
/// simply keeps its `record_to_summary` default (`unresumable: false`)
/// rather than failing the whole list response.
/// Test: `checked_summaries_preserves_input_order_and_flags_only_dead_sessions`
/// in `super::tests`; end-to-end coverage via `list_marks_dead_stopped_session_unresumable`,
/// `list_leaves_live_and_healthy_stopped_sessions_unmarked` in
/// `tests/session_manager_mvp.rs`.
pub(super) async fn checked_summaries(records: &[SessionRecord]) -> Vec<SessionSummary> {
    let mut summaries: Vec<SessionSummary> = records.iter().map(record_to_summary).collect();

    let mut probes = tokio::task::JoinSet::new();
    for (idx, r) in records.iter().enumerate() {
        if matches!(
            r.state,
            ManagedSessionState::Stopped | ManagedSessionState::Errored
        ) {
            let r = r.clone();
            probes.spawn(async move {
                let unresumable = crate::session_manager::resume_workdir::is_unresumable(&r).await;
                (idx, unresumable)
            });
        }
    }
    while let Some(res) = probes.join_next().await {
        if let Ok((idx, unresumable)) = res {
            summaries[idx].unresumable = unresumable;
        }
    }

    // Second, independent pass for the #2444 asset-staleness probe. Unlike
    // `unresumable` above, this is a SINGLE `spawn_blocking` call over every
    // syncable record rather than one task per record — `stale_assets_for_many`
    // shares the expensive catalog-side compose across all of them (issue
    // #2444 review MEDIUM finding), which a per-record `JoinSet` fan-out
    // would defeat (each task would redundantly recompose the same catalog).
    let syncable: Vec<SessionRecord> = records
        .iter()
        .filter(|r| probe_staleness_for(&r.state))
        .cloned()
        .collect();
    if !syncable.is_empty() {
        let stale_map = tokio::task::spawn_blocking(move || stale_assets_for_many(syncable))
            .await
            .unwrap_or_default();
        for (idx, r) in records.iter().enumerate() {
            if let Some(&stale) = stale_map.get(&r.id) {
                summaries[idx].stale_assets = stale;
            }
        }
    }

    summaries
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
