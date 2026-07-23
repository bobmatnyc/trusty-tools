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
    InjectionStatus, ManagedSessionId, ManagedSessionState, ManagedTmuxDriver, NumberedSlot,
    SessionRecord,
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
        // #3531: captured once, here, BEFORE any later display-only
        // reconciliation (`reconcile_live_state`) can overwrite `state` above
        // — see `SessionSummary::persisted_state`'s doc for why resume/restart
        // decisions must key off this instead.
        persisted_state: r.state.to_string(),
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
        // `attached` is a live-tmux reconciliation only the list handler
        // computes (see [`super::list_managed_sessions`]); every other summary
        // builder leaves it at its `false` default.
        attached: false,
        // `slot`/`deleted` (#3034) are meaningful only on the numbered `tm ls`
        // listing surface; every other caller of `record_to_summary` leaves
        // them at their "not applicable" defaults.
        slot: 0,
        deleted: false,
    }
}

/// Reconcile each summary's displayed `state`/`attached` against LIVE tmux.
///
/// Why: a record's PERSISTED `state` can read `stopped` after a daemon restart
/// (before the reconcile pass runs) even though its tmux session is alive and a
/// client is attached — the `tm ls` picker then wrongly showed EVERY session
/// `(stopped)` and offered a destructive `restart`. Real tmux is the
/// authoritative signal, so the list handler probes it once and corrects the
/// DISPLAYED state here (read-time reconciliation, mirroring how
/// `unresumable`/`stale_assets` are computed without mutating the store).
/// What: for each summary whose record is in the transient `Active`/`Stopped`
/// pair, sets `state` to `"active"` when its tmux is confirmed live (else
/// `"stopped"`), and sets `attached` when its session is in `attached`.
/// Terminal states (`Decommissioned`/`Deleted`), `Provisioning`, and `Errored`
/// are left as persisted — a live tmux name must never resurrect a
/// deleted/decommissioned record's label, and `errored`/`provisioning` carry
/// information a bare liveness probe would erase.
///
/// Pane-scoped liveness (#3714): a bare `live.contains(name)` proves only that
/// SOME tmux session currently uses this NAME — under the #3692 duplicate-name
/// condition, a stale record's own pane can be long gone while an UNRELATED
/// live session (owned by a DIFFERENT record) still happens to share its
/// name. Trusting the name alone there resurrected the stale record's
/// DISPLAYED `state` to `"active"` while its `persisted_state` correctly kept
/// reading `"stopped"` — the exact same-record `state`/`persisted_state`
/// contradiction the issue reported (two consecutive `tm` runs seconds apart
/// read as flip-flopping between "already active" and "stopped, will kill
/// pane" for the identical command). When a record has a captured `pane_id`,
/// liveness is proven only once [`ManagedTmuxDriver::pane_exists`] confirms
/// THAT SPECIFIC pane still lives inside a session named `name` — tying the
/// check to the record's own tmux identity rather than a name string. A
/// legacy record with no captured `pane_id` falls back to the name-only
/// check; no stronger signal exists for it. Pane probes only run for records
/// whose name IS in `live` (short-circuited by `&&`), so a fleet where most
/// sessions are genuinely stopped pays no extra tmux round-trips.
/// Test: `reconcile_live_state_flips_stopped_to_active_when_alive`,
/// `reconcile_live_state_leaves_terminal_states`,
/// `reconcile_live_state_leaves_persisted_state_untouched`,
/// `reconcile_live_state_prefers_pane_scoped_liveness_over_name_membership`,
/// `reconcile_live_state_legacy_record_without_pane_id_uses_name_only` in
/// `super::tests`; end-to-end coverage in the `session_lifecycle` integration
/// suite.
pub(super) fn reconcile_live_state(
    tmux: &dyn ManagedTmuxDriver,
    summaries: &mut [SessionSummary],
    records: &[SessionRecord],
    live: &std::collections::HashSet<String>,
    attached: &std::collections::HashSet<String>,
) {
    for (summary, record) in summaries.iter_mut().zip(records.iter()) {
        let name = &record.tmux_name;
        let name_live = live.contains(name);
        let is_live = match record.pane_id.as_deref() {
            Some(pane_id) => name_live && tmux.pane_exists(name, pane_id),
            None => name_live,
        };
        summary.attached = is_live && attached.contains(name);
        if matches!(
            record.state,
            ManagedSessionState::Active | ManagedSessionState::Stopped
        ) {
            summary.state = if is_live { "active" } else { "stopped" }.to_string();
        }
    }
}

/// Probe live tmux ONCE and reconcile every summary's displayed state in place.
///
/// Why: both `list_managed_sessions` (the fleet list) AND `get_managed_session`
/// (the single-record fetch that `tm session resume <id>` reads) must reconcile
/// the persisted state against real tmux — otherwise the highest-consequence
/// path (`resume`) can act on a stale `stopped` for a live session and
/// destructively recreate its pane (code-critic HIGH). Centralising the probe
/// here keeps the two call sites a single line each and, crucially, keeps the
/// FAIL-CLOSED contract in ONE place: a transient tmux enumeration error must
/// NOT collapse to "zero live sessions" (which would reconcile every Active
/// record to `stopped` and offer fleet-wide destructive restarts). On error we
/// SKIP reconciliation entirely and serve the persisted state unchanged.
/// What: calls [`ManagedTmuxDriver::list_sessions`]; on `Ok`, gathers the live +
/// attached name sets and applies [`reconcile_live_state`]; on `Err`, logs a
/// warning and leaves `summaries` at their persisted state.
/// Test: `reconcile_against_tmux_fails_closed_on_enumeration_error` in
/// `super::tests` (the fail-closed path); the success path is covered by
/// `reconcile_live_state_flips_stopped_to_active_when_alive` and end-to-end by
/// the `session_lifecycle` integration suite.
pub(super) fn reconcile_against_tmux(
    tmux: &dyn ManagedTmuxDriver,
    summaries: &mut [SessionSummary],
    records: &[SessionRecord],
) {
    match tmux.list_sessions() {
        Ok(names) => {
            let live: std::collections::HashSet<String> = names.into_iter().collect();
            let attached: std::collections::HashSet<String> =
                tmux.attached_session_names().into_iter().collect();
            reconcile_live_state(tmux, summaries, records, &live, &attached);
        }
        Err(e) => {
            tracing::warn!(
                "tmux session enumeration failed ({e}); serving persisted session \
                 state WITHOUT live reconciliation (fail-closed — never treat a \
                 probe error as 'all sessions stopped')"
            );
        }
    }
}

/// Build a tombstone `SessionSummary` for a deleted slot (issue #3034).
///
/// Why: `tm ls`'s stable numbering shows a `-- deleted --` placeholder for a
/// slot whose session record no longer exists in the store, so operators
/// never mistake a shifted neighbor for the session they meant. This is the
/// single place that shape is constructed so every numbered-listing caller
/// renders an identical, unambiguously-empty row.
/// What: every string/option field is blank/`None`; `state` is the literal
/// `"deleted"` — distinct from `"decommissioned"`, so the CLI's
/// decommissioned-only tombstone filter (`is_live_session_state`) never hides
/// this row from the default (non-`--all`) view; `slot` is the caller-supplied
/// stable number and `deleted` is always `true`.
/// Test: `list_assigns_stable_slot_numbers_and_tombstones_deleted_one`
/// (integration test, `tests/session_manager_slots.rs`).
pub(super) fn tombstone_summary(slot: u32) -> SessionSummary {
    SessionSummary {
        id: String::new(),
        name: String::new(),
        state: "deleted".to_string(),
        persisted_state: String::new(),
        workspace_path: None,
        repo_url: None,
        branch: None,
        created_at: String::new(),
        last_activity_at: None,
        pending_decision: None,
        proposed_default: None,
        source_id: None,
        task: None,
        cwd: None,
        claude_session_id: None,
        deliverable_id: None,
        pane_id: None,
        injection_status: None,
        unresumable: false,
        stale_assets: false,
        attached: false,
        slot,
        deleted: true,
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

/// Build the numbered `tm ls` summaries for one listing call (issue #3034).
///
/// Why: `list_managed_sessions` combines the daemon's stable slot assignment
/// with the existing async `unresumable`/`stale_assets` probes
/// ([`checked_summaries`]), the LIVE-tmux `state`/`attached` reconciliation
/// ([`reconcile_against_tmux`], #3302/#3531 — a running/attached session must
/// never read `(stopped)` just because its persisted state went stale), and
/// the `?source_id=` filter, in the order that keeps all three correct:
/// filter the numbered rows FIRST (so a session outside the filter never pays
/// the probe cost), THEN probe and reconcile only the live records that
/// remain, THEN stitch each summary back to its slot number — while a
/// tombstoned row skips both entirely and gets a blank [`tombstone_summary`]
/// instead.
/// What: takes the full [`NumberedSlot`] list (already observed against the
/// registry for every record — filtered or not, by the caller), a tmux driver
/// for the live-state reconciliation, plus an optional `source_id` filter;
/// returns one [`SessionSummary`] per surviving row, in slot order. The
/// stable `slot` field is the identity every by-number CLI surface resolves
/// against, so it is set on the row AFTER reconciliation/probing overwrite
/// `state`/`attached`/`unresumable`/`stale_assets` — sort/filter on the CLI
/// side only ever reorders or subsets this returned `Vec`, it never
/// recomputes `slot`.
/// Test: `list_assigns_stable_slot_numbers_and_tombstones_deleted_one`
/// (integration test, `tests/session_manager_slots.rs`).
pub(super) async fn numbered_summaries(
    numbered: Vec<NumberedSlot>,
    tmux: &dyn ManagedTmuxDriver,
    source_id_filter: Option<&str>,
) -> Vec<SessionSummary> {
    let filtered: Vec<NumberedSlot> = numbered
        .into_iter()
        .filter(|n| source_id_filter.is_none_or(|sid| n.source_id.as_deref() == Some(sid)))
        .collect();
    let live_records: Vec<SessionRecord> =
        filtered.iter().filter_map(|n| n.record.clone()).collect();
    let mut live_summaries = checked_summaries(&live_records).await;
    reconcile_against_tmux(tmux, &mut live_summaries, &live_records);
    let mut live_summaries = live_summaries.into_iter();
    filtered
        .into_iter()
        .map(|n| match n.record {
            Some(_) => {
                let mut s = live_summaries
                    .next()
                    .expect("live_summaries has one entry per Some(record) row, same order");
                s.slot = n.slot;
                s
            }
            None => tombstone_summary(n.slot),
        })
        .collect()
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
