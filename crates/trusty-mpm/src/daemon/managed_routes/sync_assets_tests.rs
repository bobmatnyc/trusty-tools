//! Route-level tests for `daemon::managed_routes::sync_assets` (issue #2444).
//!
//! Why: split out of `sync_assets.rs` (test-file budget: 1500 SLOC) mirroring
//! the `managed.rs`/`managed_tests.rs` split — the HOME-redirection guard
//! boilerplate every filesystem-touching test needs would otherwise push the
//! production file toward its 500-SLOC cap.
//! What: calls the route handlers directly with axum's `State`/`Path`
//! extractors (the established pattern — see `provision_status.rs`'s own
//! `#[cfg(test)] mod tests`), against a hermetic
//! `DaemonState::with_root_isolated_managed`. Records that must actually be
//! retrievable via `mgr.get` are seeded by direct `mgr.store` upsert (the
//! crate-visible field `session_manager::tests::seed_record` also uses)
//! rather than the full `create_with_id` provisioning path, since these tests
//! need explicit control over `state`/`workspace_path`/`cwd` combinations
//! `create_with_id` does not expose.
//! Test: this file IS the test module.

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path as AxumPath, State};
use axum::response::IntoResponse;
use chrono::Utc;
use serde_json::Value;
use tempfile::TempDir;

use super::*;
use crate::daemon::state::DaemonState;
use crate::session_manager::ManagedSessionId;

/// RAII guard restoring `$HOME` on drop (including panic) — mirrors the
/// identical pattern in `core::session_assets::tests::HomeGuard`.
///
/// Why: [`sync_one`] resolves its bundled-source half via
/// `FrameworkPaths::for_managed_workspace`, which always anchors the
/// framework SOURCE tree at `FrameworkPaths::default().root` (the real
/// `$HOME/.trusty-mpm` in production — the daemon supports no `--root`
/// override). Any test that exercises a successful redeploy must therefore
/// point `$HOME` at a throwaway tempdir so it reads/writes a fake framework
/// tree, never the developer's real one.
struct HomeGuard(Option<String>);
impl Drop for HomeGuard {
    fn drop(&mut self) {
        // SAFETY: paired with `#[serial_test::serial]` — no other thread
        // reads/writes the environment concurrently.
        match self.0 {
            Some(ref p) => unsafe { std::env::set_var("HOME", p) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }
}

/// Point `$HOME` at a fresh tempdir for the duration of the guard.
fn fake_home() -> (TempDir, HomeGuard) {
    let home = TempDir::new().unwrap();
    let prior = std::env::var("HOME").ok();
    // SAFETY: serialized via `#[serial_test::serial]` on every caller.
    unsafe { std::env::set_var("HOME", home.path()) };
    (home, HomeGuard(prior))
}

/// Upsert a `SessionRecord` directly into `state`'s store, bypassing the full
/// `create_with_id` provisioning path.
///
/// Why: these tests need explicit control over `state`/`workspace_path`/`cwd`
/// combinations (in particular a `Decommissioned` record with a NON-empty
/// `cwd` — exactly the shape `decommission` leaves on disk, see
/// `SessionRecord::workspace_path`'s doc) that the ordinary create path does
/// not expose.
async fn seed(state: &Arc<DaemonState>, record: SessionRecord) {
    let mgr = state.session_manager().await;
    mgr.store
        .write()
        .await
        .upsert(record)
        .await
        .expect("seed upsert");
}

/// Build a record with every optional field at its default/absent value.
fn base_record(id: ManagedSessionId, state: ManagedSessionState, cwd: PathBuf) -> SessionRecord {
    SessionRecord {
        id,
        tmux_name: format!("tmpm-test-{id}"),
        cwd,
        task: "test".to_string(),
        state,
        created_at: Utc::now(),
        last_activity_at: None,
        workspace_path: None,
        repo_url: None,
        branch: None,
        pending_decision: None,
        proposed_default: None,
        correlation: Default::default(),
        runtime: Default::default(),
        ephemeral: false,
        workspace_owned: false,
        source_id: None,
        claude_session_id: None,
        scrollback_path: None,
        last_cwd: None,
        deliverable_id: None,
        pane_id: None,
        injection_status: Default::default(),
    }
}

async fn test_state() -> (Arc<DaemonState>, TempDir) {
    let tmp = TempDir::new().expect("tempdir");
    let state = Arc::new(DaemonState::with_root_isolated_managed(tmp.path().to_owned()).await);
    (state, tmp)
}

/// #2444: a syncable (`Active`) session with a genuinely stale bundled agent
/// must have it redeployed, and the response must report exactly what changed.
#[tokio::test]
#[serial_test::serial]
async fn sync_route_redeploys_stale_agent() {
    let (home, _guard) = fake_home();
    let fw = FrameworkPaths::default();
    let bundled = fw.agent_source_dir();
    std::fs::create_dir_all(&bundled).unwrap();
    std::fs::write(
        bundled.join("rust-engineer.md"),
        "---\nname: rust-engineer\n---\nv1",
    )
    .unwrap();

    let (state, _tmp) = test_state().await;
    let id = ManagedSessionId::new();
    let workspace = home.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let mut record = base_record(id, ManagedSessionState::Active, workspace.clone());
    record.workspace_path = Some(workspace.clone());
    seed(&state, record).await;

    let resp = sync_session_assets_route(State(state.clone()), AxumPath(id.to_string()))
        .await
        .into_response();
    assert_eq!(resp.status(), axum::http::StatusCode::OK, "{:?}", resp);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        json["agents_deployed"],
        serde_json::json!(["rust-engineer.md"]),
        "the freshly-deployed agent must be reported: {json}"
    );
    assert!(
        workspace
            .join(".claude")
            .join("agents")
            .join("rust-engineer.md")
            .exists(),
        "the agent must actually land on disk in the session workspace"
    );
}

/// #2457-style convention: a nonexistent id must be a hard 404, not a silent
/// success.
#[tokio::test]
async fn sync_route_missing_session_404() {
    let (state, _tmp) = test_state().await;
    let id = ManagedSessionId::new();
    let resp = sync_session_assets_route(State(state), AxumPath(id.to_string()))
        .await
        .into_response();
    assert_eq!(resp.status(), axum::http::StatusCode::NOT_FOUND);
}

/// THE pinning test for the code-critic HIGH finding: a `Decommissioned`
/// session's `workspace_path` is cleared by `decommission`, but `cwd` is NOT
/// — it still names the now-deleted workspace directory. Without the
/// `syncable` gate, `sync_session_assets_route` would fall back to `cwd` via
/// `session_workdir` and the deployers' `create_dir_all` would silently
/// RECREATE that deleted directory. This asserts the route refuses with 409
/// AND that no filesystem write happens at all.
#[tokio::test]
async fn sync_route_decommissioned_session_blocked_no_fs_write() {
    let (state, tmp) = test_state().await;
    let id = ManagedSessionId::new();
    // `cwd` names a path that does NOT exist on disk — exactly the shape a
    // real decommission leaves behind (the workspace directory was removed).
    let vanished_workspace = tmp.path().join("vanished-workspace");
    assert!(
        !vanished_workspace.exists(),
        "precondition: the path must not exist before the route call"
    );
    let record = base_record(
        id,
        ManagedSessionState::Decommissioned,
        vanished_workspace.clone(),
    );
    seed(&state, record).await;

    let resp = sync_session_assets_route(State(state), AxumPath(id.to_string()))
        .await
        .into_response();
    assert_eq!(
        resp.status(),
        axum::http::StatusCode::CONFLICT,
        "a decommissioned session must be refused, not silently synced"
    );
    assert!(
        !vanished_workspace.exists(),
        "the gate must block BEFORE any deployer runs — the deleted workspace \
         directory must never be recreated on disk"
    );
}

/// A `Provisioning` session (no deploy has happened yet) must also be
/// refused, mirroring the decommissioned case — this is the OTHER half of
/// the `syncable` gate.
#[tokio::test]
async fn sync_route_provisioning_session_returns_conflict() {
    let (state, tmp) = test_state().await;
    let id = ManagedSessionId::new();
    let ws = tmp.path().join("provisioning-ws");
    let record = base_record(id, ManagedSessionState::Provisioning, ws);
    seed(&state, record).await;

    let resp = sync_session_assets_route(State(state), AxumPath(id.to_string()))
        .await
        .into_response();
    assert_eq!(resp.status(), axum::http::StatusCode::CONFLICT);
}

/// #2444: the fleet-wide `--all` route must skip `Provisioning` and
/// `Decommissioned` sessions (reported in `skipped`) and only actually
/// redeploy the `Active` one (reported in `synced`).
#[tokio::test]
#[serial_test::serial]
async fn sync_all_route_skips_provisioning_and_decommissioned() {
    let (home, _guard) = fake_home();
    let fw = FrameworkPaths::default();
    let bundled = fw.agent_source_dir();
    std::fs::create_dir_all(&bundled).unwrap();
    std::fs::write(
        bundled.join("rust-engineer.md"),
        "---\nname: rust-engineer\n---\nv1",
    )
    .unwrap();

    let (state, tmp) = test_state().await;

    let active_id = ManagedSessionId::new();
    let active_ws = home.path().join("active-ws");
    std::fs::create_dir_all(&active_ws).unwrap();
    let mut active = base_record(active_id, ManagedSessionState::Active, active_ws.clone());
    active.workspace_path = Some(active_ws);
    seed(&state, active).await;

    let provisioning_id = ManagedSessionId::new();
    let provisioning = base_record(
        provisioning_id,
        ManagedSessionState::Provisioning,
        tmp.path().join("provisioning-ws"),
    );
    seed(&state, provisioning).await;

    let decommissioned_id = ManagedSessionId::new();
    let decommissioned = base_record(
        decommissioned_id,
        ManagedSessionState::Decommissioned,
        tmp.path().join("gone-ws"),
    );
    seed(&state, decommissioned).await;

    let resp = sync_all_session_assets_route(State(state))
        .await
        .into_response();
    assert_eq!(resp.status(), axum::http::StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();

    let synced_ids: Vec<&str> = json["synced"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        synced_ids,
        vec![active_id.to_string()],
        "only the Active session must be synced: {json}"
    );

    let skipped: Vec<&str> = json["skipped"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap())
        .collect();
    assert!(skipped.contains(&provisioning_id.to_string().as_str()));
    assert!(skipped.contains(&decommissioned_id.to_string().as_str()));
    assert_eq!(
        skipped.len(),
        2,
        "exactly the two non-syncable sessions: {json}"
    );
}

/// Pure unit coverage for the [`syncable`] predicate itself, independent of
/// any HTTP/filesystem plumbing.
#[test]
fn syncable_gates_exactly_active_stopped_errored() {
    assert!(syncable(&ManagedSessionState::Active));
    assert!(syncable(&ManagedSessionState::Stopped));
    assert!(syncable(&ManagedSessionState::Errored));
    assert!(!syncable(&ManagedSessionState::Provisioning));
    assert!(!syncable(&ManagedSessionState::Decommissioned));
}
