//! Coverage for the store → [`LiveClaims`] producer (#7232).
//!
//! Why: this is where a tombstoned record stops being an owner. The three cases
//! that matter are the one that unblocks (a session tmux no longer knows), the
//! one that must NOT change (a session tmux still lists), and the one that must
//! fail closed (tmux could not be observed at all).
//! What: drives [`SessionManager::workspace_claims`] against the sibling
//! `tests` module's `FakeTmuxDriver`, so no real tmux is reached (#1790).
//! Test: this file IS the test module.

use std::path::{Path, PathBuf};

use chrono::Utc;
use tempfile::TempDir;

use crate::session_manager::manager::SessionManager;
use crate::session_manager::record::{ManagedSessionId, ManagedSessionState, SessionRecord};
use crate::session_manager::tests::FakeTmuxDriver;
use crate::session_manager::worktree_reclaim_claim::ClaimLiveness;

/// A record holding `workspace_path`, under `tmux_name`, in a terminal state.
///
/// Why: the incident's record was `deleted` with a live-looking path, so the
/// fixture is that shape — and the assertions must not be able to pass by
/// reading the state field, which #2919 forbids.
fn tombstoned(tmux_name: &str, workspace: &Path) -> SessionRecord {
    SessionRecord {
        id: ManagedSessionId::for_adopted_tmux_name(tmux_name),
        tmux_name: tmux_name.to_owned(),
        cwd: workspace.to_path_buf(),
        task: "adopted pane".into(),
        state: ManagedSessionState::Deleted,
        created_at: Utc::now(),
        last_activity_at: None,
        workspace_path: Some(workspace.to_path_buf()),
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
        worktree_owner: None,
        terminal_at: None,
        stop_cause: None,
    }
}

/// Seed a manager with two records: one tmux still lists, one it does not.
async fn seeded(
    dir: &TempDir,
    fake: std::sync::Arc<FakeTmuxDriver>,
    workspace: &Path,
) -> SessionManager {
    let mgr = SessionManager::new(dir.path(), fake).await.unwrap();
    let mut store = mgr.store.write().await;
    store
        .upsert(tombstoned("tm-gone", workspace))
        .await
        .unwrap();
    store
        .upsert(tombstoned("tm-still-here", workspace))
        .await
        .unwrap();
    drop(store);
    mgr
}

/// 🔴 #7232: a claim whose tmux session no longer exists must be marked gone,
/// while one tmux still lists stays live. Fails on `ad64460e8`, where
/// `WorkspaceClaim` has no liveness at all and every stored path is an owner
/// forever.
#[tokio::test]
async fn dead_sessions_claims_are_discarded_and_live_ones_are_not() {
    let dir = TempDir::new().unwrap();
    let workspace = PathBuf::from("/Users/masa/trusty-mpm-projects/bobmatnyc");
    let fake = FakeTmuxDriver::new();
    fake.seeded_names
        .lock()
        .unwrap()
        .push("tm-still-here".into());
    let mgr = seeded(&dir, fake, &workspace).await;

    let claims = mgr.workspace_claims(None).await;
    let gone = claims
        .claims
        .iter()
        .find(|c| c.session == ManagedSessionId::for_adopted_tmux_name("tm-gone").to_string())
        .expect("the dead session's claim is still IN the set, just not live");
    assert_eq!(gone.liveness, ClaimLiveness::SessionGone);

    let live = claims
        .claims
        .iter()
        .find(|c| c.session == ManagedSessionId::for_adopted_tmux_name("tm-still-here").to_string())
        .expect("the live session's claim");
    assert_eq!(
        live.liveness,
        ClaimLiveness::Live,
        "a session tmux still lists is live whatever its record's state says (#2919)"
    );
}

/// 🔴 #5856, restated for this producer: an unobservable tmux is not an empty
/// tmux. Every claim stays live, so the gate refuses exactly as it did before.
#[tokio::test]
async fn an_unobservable_tmux_leaves_every_claim_live() {
    let dir = TempDir::new().unwrap();
    let workspace = PathBuf::from("/Users/masa/trusty-mpm-projects/bobmatnyc");
    let fake = FakeTmuxDriver::new();
    fake.seeded_names
        .lock()
        .unwrap()
        .push("tm-still-here".into());
    let mgr = seeded(&dir, fake.clone(), &workspace).await;

    *fake.list_sessions_should_fail.lock().unwrap() = true;
    let claims = mgr.workspace_claims(None).await;
    assert!(
        !claims.claims.is_empty(),
        "the claim set must not empty out when tmux cannot be read"
    );
    for claim in &claims.claims {
        assert_eq!(
            claim.liveness,
            ClaimLiveness::Live,
            "a probe that could not answer must leave {} live",
            claim.session
        );
    }
}

/// A tmux name `observed_live_managed_names` filters out could never appear in
/// its answer, so its absence proves nothing — that record's claim stays live.
#[tokio::test]
async fn an_unmanaged_tmux_name_is_never_read_as_dead() {
    let dir = TempDir::new().unwrap();
    let workspace = PathBuf::from("/Users/masa/trusty-mpm-projects/bobmatnyc");
    let fake = FakeTmuxDriver::new();
    let mgr = SessionManager::new(dir.path(), fake).await.unwrap();
    {
        let mut store = mgr.store.write().await;
        store
            .upsert(tombstoned("someone-elses-pane", &workspace))
            .await
            .unwrap();
    }

    let claims = mgr.workspace_claims(None).await;
    assert_eq!(claims.claims.len(), 1);
    assert_eq!(claims.claims[0].liveness, ClaimLiveness::Live);
}
