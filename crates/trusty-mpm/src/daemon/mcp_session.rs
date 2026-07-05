//! Daemon-side implementation of the nine session-lifecycle MCP tools
//! (#1221 + the two fleet-wide #1508 teardown verbs + #2012's `session_delete`).
//!
//! Why: the MCP `StateBackend` (in `mcp_backend.rs`) must service the new
//! session-lifecycle tools, but inlining their bodies there would push that file
//! over the 500-SLOC production cap. More importantly, these tools must be THIN
//! WRAPPERS over the existing [`crate::session_manager::SessionManager`]
//! lifecycle ops that the HTTP `…/managed/*` routes already use — not a parallel
//! reimplementation — so MCP and HTTP behave identically. This module holds the
//! wrapping logic; `session_new` delegates to the shared
//! [`crate::daemon::managed_routes::spawn_managed`] used by the HTTP handler too.
//! What: nine free async functions — the seven per-session ops (`session_new`,
//! `session_stop`, `session_resume`, `session_decommission`, `session_delete`,
//! `session_activity`, `session_send`) plus the two fleet-wide #1508 verbs
//! (`session_decommission_ephemeral`, `session_prune`) — each taking
//! `&Arc<DaemonState>` plus parsed arguments and returning the same JSON shapes
//! the HTTP routes return, or a human-readable error string.
//! Test: `cargo test -p trusty-mpm daemon::mcp_session` plus the dispatch-level
//! tests in `crate::mcp`.

use std::sync::Arc;

use serde_json::{Value, json};

use crate::daemon::managed_routes::{SpawnParams, record_to_json, spawn_managed};
use crate::daemon::state::DaemonState;
use crate::session_manager::{ManagedError, ManagedSessionId, PruneFilter};

/// Parse a managed-session id string into a [`ManagedSessionId`].
///
/// Why: every single-id tool must reject a non-UUID with a clear message rather
/// than a confusing downstream "not found".
/// What: parses `raw` as a UUID, mapping failure to a descriptive error string.
/// Test: `parse_managed_id_rejects_garbage` in the `tests` module.
fn parse_managed_id(raw: &str) -> Result<ManagedSessionId, String> {
    raw.parse::<uuid::Uuid>()
        .map(ManagedSessionId::from)
        .map_err(|_| format!("`{raw}` is not a valid managed session id (expected a UUID)"))
}

/// Map a [`ManagedError`] to a human-readable tool error string.
///
/// Why: the MCP tool result carries a flat string; mapping here keeps every
/// wrapper's error path uniform and avoids leaking `Debug` formatting.
/// What: renders the error via its `Display` impl (which already carries
/// actionable context for each variant).
/// Test: exercised by the not-found paths in the `tests` module.
fn managed_err(e: ManagedError) -> String {
    e.to_string()
}

/// Spawn a new managed session (`session_new` tool).
///
/// Why: gives the driver a typed spawn path; delegates to the shared
/// [`spawn_managed`] helper so the MCP and HTTP spawn flows are identical.
/// What: validates the optional `runtime` selector, provisions the workspace,
/// creates the tmux host, launches the harness, and returns the new record as
/// JSON (id, tmux name, workspace path, state, attach command, runtime). The
/// optional `ephemeral` flag (#1508) tags a test/throwaway session for the
/// bulk-teardown + auto-reap paths.
/// Test: `session_new_invalid_runtime_errors` (unit, no tmux needed).
pub async fn session_new(
    state: &Arc<DaemonState>,
    repo_url: &str,
    git_ref: &str,
    task: &str,
    name_hint: Option<&str>,
    runtime: Option<&str>,
    ephemeral: Option<bool>,
) -> Result<Value, String> {
    let params = SpawnParams {
        repo_url: repo_url.to_string(),
        git_ref: git_ref.to_string(),
        task: task.to_string(),
        name_hint: name_hint.map(str::to_string),
        runtime: runtime.map(str::to_string),
        ephemeral,
        // This IS the MCP tool surface (#1836/#1837): every call here is
        // subject to the off-by-default gate + registry allowlist.
        mcp_initiated: true,
    };
    let record = spawn_managed(state, params).await?;
    Ok(record_to_json(&record))
}

/// Stop a session's runtime, keeping its workspace (`session_stop` tool).
///
/// Why: thin wrapper over [`crate::session_manager::SessionManager::stop`].
/// What: parses the id, calls `stop`, returns the updated record as JSON.
/// Test: `session_stop_unknown_id_errors` in the `tests` module.
pub async fn session_stop(state: &Arc<DaemonState>, session_id: &str) -> Result<Value, String> {
    let id = parse_managed_id(session_id)?;
    let mgr = state.session_manager().await;
    mgr.stop(&id)
        .await
        .map(|r| record_to_json(&r))
        .map_err(managed_err)
}

/// Resume a stopped session in its existing workspace (`session_resume` tool).
///
/// Why: thin wrapper that mirrors the HTTP resume handler — it both resumes the
/// record AND re-spawns the runtime so the session is actually live again.
/// What: parses the id, delegates to
/// [`crate::daemon::managed_routes::resume_managed`], and renders its typed
/// [`crate::daemon::managed_routes::ResumeManagedError`] to a flat string via
/// `Display` for the MCP tool result (the not-found variant's string still
/// contains the literal "not found" the dispatch tests assert on). On success
/// returns the record as JSON.
/// Test: `session_resume_unknown_id_errors` in the `tests` module.
pub async fn session_resume(state: &Arc<DaemonState>, session_id: &str) -> Result<Value, String> {
    let id = parse_managed_id(session_id)?;
    let record = crate::daemon::managed_routes::resume_managed(state, &id)
        .await
        .map_err(|e| e.to_string())?;
    Ok(record_to_json(&record))
}

/// Permanently tear down a session (`session_decommission` tool).
///
/// Why: thin wrapper over
/// [`crate::session_manager::SessionManager::decommission`].
/// What: parses the id, calls `decommission`, returns the tombstone record.
/// Test: `session_decommission_unknown_id_errors` in the `tests` module.
pub async fn session_decommission(
    state: &Arc<DaemonState>,
    session_id: &str,
) -> Result<Value, String> {
    let id = parse_managed_id(session_id)?;
    let mgr = state.session_manager().await;
    mgr.decommission(&id)
        .await
        .map(|(r, _workspace_removed)| record_to_json(&r))
        .map_err(managed_err)
}

/// Hard-delete a session's RECORD from the store (`session_delete` tool, #2012).
///
/// Why: thin wrapper over
/// [`crate::session_manager::SessionManager::delete_record`] — distinct from
/// `session_decommission` (stops the runtime and may remove the workspace, but
/// always leaves a `Decommissioned` tombstone). This permanently drops the
/// record itself for an operator who wants a mis-provisioned or stale record
/// gone outright. Fail-closed: refuses a RUNNING (`Active`/`Provisioning`)
/// session unless `force` is `true`.
/// What: parses the id, calls `delete_record(&id, force)`, and returns the
/// pre-deletion record as JSON with `deleted: true` appended. NEVER touches the
/// workspace directory on disk (store-only operation).
/// Test: `session_delete_unknown_id_errors`, `session_delete_refuses_running`,
/// `session_delete_force_bypasses_guard` in the `tests` module.
pub async fn session_delete(
    state: &Arc<DaemonState>,
    session_id: &str,
    force: bool,
) -> Result<Value, String> {
    let id = parse_managed_id(session_id)?;
    let mgr = state.session_manager().await;
    let record = mgr.delete_record(&id, force).await.map_err(managed_err)?;
    let mut value = record_to_json(&record);
    if let Some(obj) = value.as_object_mut() {
        obj.insert("deleted".to_string(), Value::Bool(true));
    }
    Ok(value)
}

/// Inspect a session's recent activity (`session_activity` tool).
///
/// Why: the driver needs raw pane content (always) plus lifecycle fields so it
/// can reason about session state without an LLM key — matching the HTTP
/// `…/activity` route's no-key contract.
/// What: parses the id, captures the last `lines` pane lines, reports
/// `runtime_active` and the pending-decision fields. The LLM classification
/// overlay is intentionally NOT invoked here (it requires the activity monitor
/// and a key); the raw pane is sufficient for the driver's own inference. A
/// missing tmux pane degrades to an empty `raw_pane`, never an error.
/// Test: `session_activity_unknown_id_errors` in the `tests` module.
pub async fn session_activity(
    state: &Arc<DaemonState>,
    session_id: &str,
    lines: u32,
) -> Result<Value, String> {
    let id = parse_managed_id(session_id)?;
    let mgr = state.session_manager().await;
    let record = mgr.get(&id).await.map_err(managed_err)?;
    // `lines` arrives as u32 from the MCP `session_activity` contract; the
    // `capture_pane` param is `usize` (aligned with the observe path). u32→usize
    // never narrows on supported platforms; saturate defensively just in case.
    let raw_pane = mgr
        .capture_pane(&id, usize::try_from(lines).unwrap_or(usize::MAX))
        .await
        .unwrap_or_default();
    let runtime_active = mgr.tmux_driver().session_exists(&record.tmux_name);
    Ok(json!({
        "id": record.id.to_string(),
        "name": record.tmux_name,
        "state": record.state.to_string(),
        "raw_pane": raw_pane,
        "lines": lines,
        "runtime_active": runtime_active,
        "pending_decision": record.pending_decision,
        "proposed_default": record.proposed_default,
    }))
}

/// Send a line of text into a session's pane (`session_send` tool).
///
/// Why: thin wrapper over
/// [`crate::session_manager::SessionManager::send_input`].
/// What: parses the id, resolves the tmux name (for the confirmation), injects
/// `text`, and returns `{ id, sent, tmux_name }`.
/// Test: `session_send_unknown_id_errors` in the `tests` module.
pub async fn session_send(
    state: &Arc<DaemonState>,
    session_id: &str,
    text: &str,
) -> Result<Value, String> {
    let id = parse_managed_id(session_id)?;
    let mgr = state.session_manager().await;
    let record = mgr.get(&id).await.map_err(managed_err)?;
    mgr.send_input(&id, text).await.map_err(managed_err)?;
    Ok(json!({
        "id": record.id.to_string(),
        "sent": true,
        "tmux_name": record.tmux_name,
    }))
}

/// Bulk-tear-down every ephemeral session (`session_decommission_ephemeral` tool, #1508).
///
/// Why: gives the driver a typed "clean up all my throwaway test sessions" path
/// over MCP, delegating to the SAME
/// [`SessionManager::decommission_all_ephemeral`](crate::session_manager::SessionManager::decommission_all_ephemeral)
/// engine the HTTP route and CLI use, so behaviour is identical across transports.
/// REAL sessions default `ephemeral=false` and are unreachable — durable work is safe.
/// What: decommissions every ephemeral session and returns `{ decommissioned: <count> }`.
/// Test: `dispatch_session_decommission_ephemeral_tool` (mock) in `crate::mcp`.
pub async fn session_decommission_ephemeral(state: &Arc<DaemonState>) -> Result<Value, String> {
    let mgr = state.session_manager().await;
    let count = mgr
        .decommission_all_ephemeral()
        .await
        .map_err(managed_err)?;
    Ok(json!({ "decommissioned": count }))
}

/// By-state prune + tombstone compaction (`session_prune` tool, #1508).
///
/// Why: the fleet-wide teardown tool exposed over MCP so the legacy stale records
/// can be purged with the SAME
/// [`SessionManager::prune_managed`](crate::session_manager::SessionManager::prune_managed)
/// engine the HTTP route and CLI use.
/// What: parses `state` (an unknown spelling is a `400`-style error string), then
/// prunes with `dry_run`/`include_active` and returns the serialized
/// [`crate::session_manager::PruneOutcome`]. A RUNNING session is NEVER torn down
/// unless `include_active`; with `dry_run` nothing is mutated.
/// Test: `dispatch_session_prune_tool`, `dispatch_session_prune_rejects_bad_state`
/// (mock) in `crate::mcp`.
pub async fn session_prune(
    state: &Arc<DaemonState>,
    filter: &str,
    dry_run: bool,
    include_active: bool,
) -> Result<Value, String> {
    let filter = PruneFilter::parse(filter)?;
    let mgr = state.session_manager().await;
    let outcome = mgr
        .prune_managed(filter, dry_run, include_active)
        .await
        .map_err(managed_err)?;
    serde_json::to_value(&outcome).map_err(|e| format!("failed to serialize prune outcome: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> Arc<DaemonState> {
        DaemonState::shared()
    }

    /// Why: a non-UUID id must be rejected before any store lookup.
    /// Test: this test.
    #[test]
    fn parse_managed_id_rejects_garbage() {
        let err = parse_managed_id("not-a-uuid").unwrap_err();
        assert!(err.contains("not a valid managed session id"), "{err}");
    }

    /// Why: an unknown (well-formed) id must produce a "not found" error, not a
    /// panic, for each single-id lifecycle tool.
    /// What: drives stop/resume/decommission/activity/send against a fresh state
    /// with a random UUID and asserts each returns Err containing "not found".
    /// Test: this test.
    #[tokio::test]
    async fn unknown_id_errors_for_all_single_id_tools() {
        let s = state();
        let id = uuid::Uuid::new_v4().to_string();

        assert!(
            session_stop(&s, &id)
                .await
                .unwrap_err()
                .contains("not found")
        );
        assert!(
            session_resume(&s, &id)
                .await
                .unwrap_err()
                .contains("not found")
        );
        assert!(
            session_decommission(&s, &id)
                .await
                .unwrap_err()
                .contains("not found")
        );
        assert!(
            session_delete(&s, &id, false)
                .await
                .unwrap_err()
                .contains("not found")
        );
        assert!(
            session_activity(&s, &id, 60)
                .await
                .unwrap_err()
                .contains("not found")
        );
        assert!(
            session_send(&s, &id, "hi")
                .await
                .unwrap_err()
                .contains("not found")
        );
    }

    /// Why: garbage ids must be rejected with the parse message (before lookup)
    /// for every single-id tool.
    /// Test: this test.
    #[tokio::test]
    async fn garbage_id_rejected_for_all_single_id_tools() {
        let s = state();
        for r in [
            session_stop(&s, "xx").await,
            session_resume(&s, "xx").await,
            session_decommission(&s, "xx").await,
            session_delete(&s, "xx", false).await,
            session_activity(&s, "xx", 60).await,
            session_send(&s, "xx", "t").await,
        ] {
            assert!(r.unwrap_err().contains("valid managed session id"));
        }
    }

    /// `session_delete` fail-closed guard + `force` bypass, driven end-to-end
    /// against a real (isolated) [`SessionManager`] record (#2012).
    ///
    /// Why: `unknown_id_errors_for_all_single_id_tools` only proves the
    /// not-found path; the actual running-guard/force behaviour needs a seeded
    /// record, which requires an isolated store (never the shared production
    /// one `state()` above points at).
    /// What: seeds an Active (running) record via `create_with_id`, asserts a
    /// non-forced `session_delete` call is refused (error mentions the
    /// session), then asserts `force=true` succeeds, returns `deleted: true`,
    /// and the record is gone from the store.
    /// Test: this function IS the test.
    #[tokio::test]
    async fn session_delete_refuses_running_then_force_bypasses() {
        use crate::runtime::RuntimeKind;

        let root = tempfile::TempDir::new().expect("tempdir");
        let s = Arc::new(DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await);
        let mgr = s.session_manager().await;

        let id = ManagedSessionId::new();
        let ws = root.path().join(format!("{id}-mcp-delete"));
        mgr.create_with_id(
            id,
            "mcp session_delete test".to_string(),
            Some(ws.clone()),
            None,
            Some(ws),
            Some("https://example.com/r.git".to_string()),
            Some("main".to_string()),
            RuntimeKind::default(),
            false,
            false,
        )
        .await
        .expect("seed session");

        // Non-forced delete of a running (Provisioning) session must be refused.
        let err = session_delete(&s, &id.to_string(), false)
            .await
            .expect_err("must refuse to delete a running session without force");
        assert!(err.contains(&id.to_string()), "{err}");

        // The record must still be present after the refusal.
        assert!(mgr.get(&id).await.is_ok(), "record must survive refusal");

        // `force: true` bypasses the guard and hard-deletes the record.
        let value = session_delete(&s, &id.to_string(), true)
            .await
            .expect("force must bypass the running guard");
        assert_eq!(value["deleted"], serde_json::json!(true));
        assert!(
            matches!(mgr.get(&id).await, Err(ManagedError::SessionNotFound(_))),
            "record must be gone from the store after forced delete"
        );
    }

    /// Why: an unknown `runtime` selector must be rejected up front (before any
    /// provisioning/tmux work), with a message naming the supported values.
    /// Test: this test.
    #[tokio::test]
    async fn session_new_invalid_runtime_errors() {
        let s = state();
        let err = session_new(
            &s,
            "https://example.com/r.git",
            "main",
            "t",
            None,
            Some("bogus"),
            None,
        )
        .await
        .unwrap_err();
        assert!(err.contains("unknown runtime"), "{err}");
    }
}
