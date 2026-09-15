//! End-to-end coverage for gate 4b — a session-owned worktree is reclaimed only
//! when its owner is provably gone (#7652).
//!
//! Why: the hole was a chain, not a function. Gate 2 permits a live foreign
//! session's project-root claim over a nested worktree, gate 4 read a `Known`
//! sentinel as "no objection", and gates 5 and 6 then saw a merged, clean tree.
//! So every test here runs the real chain: a real session store behind
//! `workspace_claims`, a real git worktree carrying the owner's sentinel, and
//! the real survey or delete loop.
//! What: each test names one state of the owner's record and of tmux. These
//! tests use only interfaces that predate the fix, so the same file runs
//! against `76ad591ac`, where every refusal case below reclaims.
//! Test: this file IS the test module.

use std::cell::Cell;
use std::path::{Path, PathBuf};

use chrono::Utc;
use tempfile::TempDir;

use crate::session_manager::decommission::WORKTREE_SENTINEL_FILE;
use crate::session_manager::manager::SessionManager;
use crate::session_manager::record::{ManagedSessionId, ManagedSessionState, SessionRecord};
use crate::session_manager::tests::FakeTmuxDriver;
use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;
use crate::session_manager::worktree_ownership::{
    AgentDelegationState, AgentWorktreeOwner, sentinel_payload_bytes,
};
use crate::session_manager::worktree_reclaim::{
    KeepList, LiveClaims, PrIndex, ReclaimMode, ReclaimVerdict,
};
use crate::session_manager::worktree_reclaim_sweep::{
    FreshProbes, SurveyBudget, reclaim_with_probes, survey_with_index,
};

/// The owning session's record in the store.
#[derive(Clone, Copy)]
enum OwnerRecord {
    /// A record whose `workspace_path` is the project root (#7652's shape).
    ProjectRoot,
    /// A record with no `workspace_path` at all.
    NoWorkspace,
    /// No record names the owner.
    Absent,
}

/// What the tmux probe says about the owner's session.
#[derive(Clone, Copy)]
enum Tmux {
    /// tmux answers and lists the owner's session.
    ListsOwner,
    /// tmux answers and does not list it.
    OwnerGone,
    /// `tmux list-sessions` errors.
    Errors,
}

/// One scene: a landed worktree nested in the project, owned by a session.
struct Scene {
    fx: GitWorktreeFixture,
    wt: PathBuf,
    branch: String,
    owner: ManagedSessionId,
}

fn record(tmux_name: &str, workspace: Option<&Path>) -> SessionRecord {
    SessionRecord {
        id: ManagedSessionId::for_adopted_tmux_name(tmux_name),
        tmux_name: tmux_name.to_owned(),
        cwd: workspace.map(Path::to_path_buf).unwrap_or_default(),
        task: "owns a nested worktree".into(),
        // #2919: the state field is never the liveness test, so it is terminal
        // here in every scene, live ones included.
        state: ManagedSessionState::Deleted,
        created_at: Utc::now(),
        last_activity_at: None,
        workspace_path: workspace.map(Path::to_path_buf),
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

/// A merged, clean worktree at `<repo>/.worktrees/<name>` owned by `tm-<name>`.
fn scene(name: &str) -> Scene {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree(name);
    std::fs::write(wt.join("landed.rs"), "// landed\n").expect("write landed file");
    GitWorktreeFixture::commit_all_and_push(&wt, "landed");
    let owner = ManagedSessionId::for_adopted_tmux_name(&format!("tm-{name}"));
    std::fs::write(
        wt.join(WORKTREE_SENTINEL_FILE),
        sentinel_payload_bytes(owner),
    )
    .expect("write the owner's sentinel");
    Scene {
        branch: format!("session/{name}"),
        fx,
        wt,
        owner,
    }
}

/// The claim set the daemon's own producer builds for this store and tmux.
async fn claims(scene: &Scene, name: &str, rec: OwnerRecord, tmux: Tmux) -> LiveClaims {
    let tmux_name = format!("tm-{name}");
    let dir = TempDir::new().expect("tempdir");
    let fake = FakeTmuxDriver::new();
    if matches!(tmux, Tmux::ListsOwner) {
        fake.seeded_names
            .lock()
            .expect("lock")
            .push(tmux_name.clone());
    }
    let mgr = SessionManager::new(dir.path(), fake.clone())
        .await
        .expect("manager");
    let workspace = match rec {
        OwnerRecord::ProjectRoot => Some(scene.fx.repo.as_path()),
        OwnerRecord::NoWorkspace | OwnerRecord::Absent => None,
    };
    if !matches!(rec, OwnerRecord::Absent) {
        let mut store = mgr.store.write().await;
        store
            .upsert(record(&tmux_name, workspace))
            .await
            .expect("upsert");
    }
    if matches!(tmux, Tmux::Errors) {
        *fake.list_sessions_should_fail.lock().expect("lock") = true;
    }
    mgr.workspace_claims(Some("tm-caller-7652".to_string()))
        .await
}

fn no_agents(_: &AgentWorktreeOwner) -> AgentDelegationState {
    AgentDelegationState::Unknown
}

fn merged_index(branch: &str) -> PrIndex {
    PrIndex::from_json(
        &format!(r#"[{{"number": 7652, "headRefName": "{branch}", "state": "MERGED"}}]"#),
        400,
    )
}

/// The survey's verdict for the scene's worktree.
fn verdict(scene: &Scene, claims: &LiveClaims) -> ReclaimVerdict {
    let branch = scene.branch.clone();
    let survey = survey_with_index(
        &scene.fx.repos_root,
        claims,
        &|_: &Path| merged_index(&branch),
        &no_agents,
        SurveyBudget::default(),
        false,
        &KeepList::default(),
        &[],
    );
    survey
        .candidates
        .into_iter()
        .find(|c| c.path == scene.wt)
        .unwrap_or_else(|| panic!("the survey must list {}", scene.wt.display()))
        .verdict
}

/// Assert a refusal that names the owner, and return its text.
fn refused(v: &ReclaimVerdict, owner: &ManagedSessionId) -> String {
    let reason = match v {
        ReclaimVerdict::Blocked { reason, .. } | ReclaimVerdict::BlockedByAgent { reason, .. } => {
            reason.clone()
        }
        ReclaimVerdict::Reclaimable { .. } => {
            panic!("a session-owned tree whose owner is not proven gone was reclaimable: {v:?}")
        }
    };
    assert!(
        reason.contains(&owner.to_string()),
        "names the owner: {reason}"
    );
    assert!(reason.contains("#7652"), "{reason}");
    reason
}

/// 🔴 #7652 criterion 1: a LIVE owner's own nested worktree — merged PR, clean
/// tree — is refused. Fails on `76ad591ac`, where gate 4 read a `Known`
/// sentinel as no objection and this tree was `Reclaimable`.
#[tokio::test]
async fn worktree_7652_a_live_owners_nested_worktree_is_refused() {
    let s = scene("owner-live-7652");
    let c = claims(
        &s,
        "owner-live-7652",
        OwnerRecord::ProjectRoot,
        Tmux::ListsOwner,
    )
    .await;
    assert!(
        c.claim_state(&s.wt).refusal(false).is_none(),
        "premise: gate 2 permits a project-root claim over a nested worktree"
    );
    let reason = refused(&verdict(&s, &c), &s.owner);
    assert!(reason.contains("tmux still lists"), "{reason}");
}

/// 🔴 #7652 criterion 2: the same tree with an owner tmux no longer lists is
/// reclaimed. A gate that refuses every session-owned tree fails this.
#[tokio::test]
async fn worktree_7652_a_dead_owners_nested_worktree_is_reclaimed() {
    let s = scene("owner-dead-7652");
    let c = claims(
        &s,
        "owner-dead-7652",
        OwnerRecord::ProjectRoot,
        Tmux::OwnerGone,
    )
    .await;
    assert_eq!(verdict(&s, &c), ReclaimVerdict::Reclaimable { pr: 7652 });
}

/// 🔴 #7652 criterion 3: no stored record names the owner. tmux answered, but
/// nothing ties its silence to this session, so unrecorded is not dead.
/// Fails against a version where an unrecorded owner permits.
#[tokio::test]
async fn worktree_7652_an_owner_with_no_record_is_refused() {
    let s = scene("owner-unrecorded-7652");
    let c = claims(
        &s,
        "owner-unrecorded-7652",
        OwnerRecord::Absent,
        Tmux::OwnerGone,
    )
    .await;
    let reason = refused(&verdict(&s, &c), &s.owner);
    assert!(reason.contains("no stored session record"), "{reason}");
}

/// 🔴 #7652 criterion 3: a record with no `workspace_path` is still the owner's
/// record. Live in tmux, it refuses — the claim-list-derived map the previous
/// attempt used read it as unrecorded and permitted. Gone from an answering
/// tmux, it reclaims: that is the one evidence of death gate 4b accepts.
#[tokio::test]
async fn worktree_7652_owners_include_a_record_with_no_workspace_path() {
    let live = scene("owner-noworkspace-live-7652");
    let c = claims(
        &live,
        "owner-noworkspace-live-7652",
        OwnerRecord::NoWorkspace,
        Tmux::ListsOwner,
    )
    .await;
    refused(&verdict(&live, &c), &live.owner);

    let dead = scene("owner-noworkspace-dead-7652");
    let c = claims(
        &dead,
        "owner-noworkspace-dead-7652",
        OwnerRecord::NoWorkspace,
        Tmux::OwnerGone,
    )
    .await;
    assert_eq!(verdict(&dead, &c), ReclaimVerdict::Reclaimable { pr: 7652 });
}

/// 🔴 #7652 criterion 3: an errored tmux probe is not an empty tmux (#5856).
/// Fails against a version where the probe's error defaults to "not alive".
#[tokio::test]
async fn worktree_7652_an_errored_tmux_probe_is_refused() {
    let s = scene("owner-tmux-error-7652");
    let c = claims(
        &s,
        "owner-tmux-error-7652",
        OwnerRecord::ProjectRoot,
        Tmux::Errors,
    )
    .await;
    refused(&verdict(&s, &c), &s.owner);
}

/// 🔴 #7652 criterion 3: an unreadable session store. The delete loop's claim
/// probe returns `None`, so the survey falls back to an empty claim set — and
/// that fallback must refuse the session-owned tree at classification, not
/// merely at the pre-delete re-check. Fails on `76ad591ac`, whose survey
/// reported the tree reclaimable.
#[test]
fn worktree_7652_an_unreadable_session_store_reclaims_nothing() {
    let s = scene("owner-store-unreadable-7652");
    let branch = s.branch.clone();
    let out = reclaim_with_probes(
        &s.fx.repos_root,
        &FreshProbes {
            launched_from: &[],
            keep_list: &KeepList::default,
            agent_state: &no_agents,
            in_use_now: &|| None,
            index_for: &|_: &Path| merged_index(&branch),
        },
        ReclaimMode::Remove,
        &[],
    );
    assert!(out.removed.is_empty(), "{out:?}");
    assert!(s.wt.exists(), "the tree must still be on disk");
    let candidate = out
        .survey
        .candidates
        .iter()
        .find(|c| c.path == s.wt)
        .expect("listed");
    refused(&candidate.verdict, &s.owner);
}

/// 🔴 #7652: the pre-delete re-check reads the FRESH owner map. The survey saw
/// the owner gone; by the delete the owner's tmux session is back. Fails if
/// gate 4b lives only in `classify`, and on `76ad591ac`, which deletes.
#[tokio::test]
async fn worktree_7652_the_recheck_refuses_an_owner_that_came_back() {
    let s = scene("owner-came-back-7652");
    let name = "owner-came-back-7652";
    let gone = claims(&s, name, OwnerRecord::ProjectRoot, Tmux::OwnerGone).await;
    let back = claims(&s, name, OwnerRecord::ProjectRoot, Tmux::ListsOwner).await;
    let reads = Cell::new(0usize);
    let branch = s.branch.clone();
    let out = reclaim_with_probes(
        &s.fx.repos_root,
        &FreshProbes {
            launched_from: &[],
            keep_list: &KeepList::default,
            agent_state: &no_agents,
            // First read feeds the survey; every later one is the re-check's.
            in_use_now: &|| {
                reads.set(reads.get() + 1);
                Some(if reads.get() == 1 {
                    gone.clone()
                } else {
                    back.clone()
                })
            },
            index_for: &|_: &Path| merged_index(&branch),
        },
        ReclaimMode::Remove,
        &[],
    );
    assert!(out.removed.is_empty(), "{out:?}");
    assert!(s.wt.exists(), "the returning owner's tree must survive");
    assert!(
        out.refused_at_recheck
            .iter()
            .any(|r| r.contains("#7652") && r.contains(&s.owner.to_string())),
        "{:?}",
        out.refused_at_recheck
    );
}
