//! End-to-end coverage for an agent tree whose dispatching Claude session a
//! restart or `/clear` has replaced (#7771).
//!
//! Why: on 2026-09-28 a `--merged-prs` preview spared 193 of 199 worktrees at
//! the agent-ownership gate. 185 of the 191 agent trees named a Claude session
//! that no record carried any more — each record had moved on to a newer id —
//! and the only place the old id survived was the #7617 session-link sidecar.
//! Gate 4 read "no record names it" as undeterminable, and a delegation record
//! left `Running` by the dead agent refused the tree on its own.
//! What: a real session store behind `workspace_claims`, rooted where
//! production roots it (`<root>/session-manager`), a real link sidecar, and a
//! real merged agent-store worktree, run through the survey and the delete
//! loop. These tests use only interfaces that predate the fix, so the file runs
//! unchanged against the pre-fix commit.
//! Test: this file IS the test module.

use std::path::{Path, PathBuf};

use chrono::Utc;
use tempfile::TempDir;

use crate::core::session_links::record_link;
use crate::session_manager::manager::SessionManager;
use crate::session_manager::record::{ManagedSessionId, ManagedSessionState, SessionRecord};
use crate::session_manager::tests::FakeTmuxDriver;
use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;
use crate::session_manager::worktree_ownership::{AgentDelegationState, AgentWorktreeOwner};
use crate::session_manager::worktree_reclaim::{
    KeepList, LiveClaims, PrIndex, ReclaimMode, ReclaimVerdict,
};
use crate::session_manager::worktree_reclaim_sweep::{
    FreshProbes, SurveyBudget, reclaim_scoped, survey_with_index,
};

const PR: u64 = 7771;

/// A merged, clean agent-store tree and the Claude session its owner file names.
pub(super) struct Scene {
    pub(super) fx: GitWorktreeFixture,
    pub(super) wt: PathBuf,
    pub(super) branch: String,
    /// The Claude session id the owner file names as the dispatcher.
    pub(super) dispatcher: String,
}

pub(super) fn scene(name: &str) -> Scene {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree_at(&fx.repo.join(".claude").join("worktrees"), name);
    std::fs::write(wt.join("landed.rs"), "// landed\n").expect("write landed file");
    GitWorktreeFixture::commit_all_and_push(&wt, "landed");
    let owner = GitWorktreeFixture::stamp_agent_sentinel(&wt, &format!("a{name}"));
    Scene {
        branch: format!("wt/{name}"),
        dispatcher: owner.parent_session_id.0.to_string(),
        fx,
        wt,
    }
}

/// The PM session's record: live in tmux, carrying `current` as its Claude id.
pub(super) fn pm_record(tmux_name: &str, current: &str) -> SessionRecord {
    SessionRecord {
        id: ManagedSessionId::for_adopted_tmux_name(tmux_name),
        tmux_name: tmux_name.to_owned(),
        cwd: PathBuf::new(),
        task: "the PM that dispatched the agent".into(),
        state: ManagedSessionState::Deleted,
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
        claude_session_id: Some(current.to_owned()),
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

/// How the PM's Claude sessions are recorded.
enum Links {
    /// The record carries a newer id; the sidecar lists the dispatcher too.
    Superseded,
    /// The record still carries the dispatcher as its current id.
    Current,
    /// As `Superseded`, plus a sidecar entry that cannot be read.
    SupersededUnreadable,
    /// The sidecar links the dispatcher to a session no record names.
    LinkedToAnUnstoredSession,
}

/// The claim set the daemon's own producer builds, with the PM live in tmux.
async fn claims(scene: &Scene, links: Links) -> LiveClaims {
    let root = TempDir::new().expect("framework root");
    let data_dir = root.path().join("session-manager");
    std::fs::create_dir_all(&data_dir).expect("session-manager dir");
    let tmux_name = "tm-pm-7771";
    let fake = FakeTmuxDriver::new();
    fake.seeded_names
        .lock()
        .expect("lock")
        .push(tmux_name.to_string());
    let mgr = SessionManager::new(&data_dir, fake).await.expect("manager");
    let managed = ManagedSessionId::for_adopted_tmux_name(tmux_name).to_string();
    let newer = "0b7e1a55-7771-4000-8000-000000000001";
    let current = match links {
        Links::Current => scene.dispatcher.as_str(),
        _ => newer,
    };
    mgr.store
        .write()
        .await
        .upsert(pm_record(tmux_name, current))
        .await
        .expect("upsert");
    let linked_to = match links {
        Links::LinkedToAnUnstoredSession => "tm-compacted-7771",
        _ => managed.as_str(),
    };
    record_link(root.path(), linked_to, &scene.dispatcher);
    record_link(root.path(), &managed, current);
    if matches!(links, Links::SupersededUnreadable) {
        let dir = root.path().join("usage").join("session-links");
        std::fs::create_dir(dir.join("tm-unreadable-7771")).expect("an unreadable entry");
    }
    mgr.workspace_claims(Some("tm-caller-7771".to_string()))
        .await
}

pub(super) fn agent_live(_: &AgentWorktreeOwner) -> AgentDelegationState {
    AgentDelegationState::Live
}

fn no_agents(_: &AgentWorktreeOwner) -> AgentDelegationState {
    AgentDelegationState::Unknown
}

pub(super) fn merged_index(branch: &str) -> PrIndex {
    PrIndex::from_json(
        &format!(r#"[{{"number": {PR}, "headRefName": "{branch}", "state": "MERGED"}}]"#),
        400,
    )
}

/// The survey's verdict for the scene's worktree.
pub(super) fn verdict(
    scene: &Scene,
    claims: &LiveClaims,
    agent_state: &dyn Fn(&AgentWorktreeOwner) -> AgentDelegationState,
) -> ReclaimVerdict {
    let branch = scene.branch.clone();
    survey_with_index(
        &scene.fx.repos_root,
        claims,
        &|_: &Path| merged_index(&branch),
        agent_state,
        SurveyBudget::default(),
        false,
        &KeepList::default(),
        &[],
    )
    .candidates
    .into_iter()
    .find(|c| c.path == scene.wt)
    .unwrap_or_else(|| panic!("the survey must list {}", scene.wt.display()))
    .verdict
}

/// The refusal text, panicking on any grant.
pub(super) fn refused(v: &ReclaimVerdict) -> String {
    match v {
        ReclaimVerdict::Blocked { reason, .. } | ReclaimVerdict::BlockedByAgent { reason, .. } => {
            reason.clone()
        }
        _ => panic!("the tree must be kept: {v:?}"),
    }
}

/// 🔴 #7771: the reported case. The dispatching Claude session was replaced on
/// a live PM record, the dead agent's delegation record still reads `Running`,
/// and the PR merged. Survey and delete loop both reclaim the tree.
///
/// Fails before the fix: the survey refused it with "owned by dispatched agent
/// … a delegation naming it has not ended".
#[tokio::test]
async fn worktree_7771_a_superseded_sessions_open_delegation_tree_is_reclaimed() {
    let s = scene("agent-superseded-7771");
    let c = claims(&s, Links::Superseded).await;
    assert_eq!(
        verdict(&s, &c, &agent_live),
        ReclaimVerdict::Reclaimable { pr: PR }
    );

    let branch = s.branch.clone();
    let out = reclaim_scoped(
        &s.fx.repos_root,
        &FreshProbes {
            prove: &crate::session_manager::worktree_reclaim_landed::reclaim_landed_proof,
            launched_from: &[],
            keep_list: &KeepList::default,
            agent_state: &agent_live,
            in_use_now: &|| Some(c.clone()),
            index_for: &|_: &Path| merged_index(&branch),
        },
        ReclaimMode::Remove,
        &[],
        &crate::session_manager::worktree_scope::WorktreeScope::all(),
    );
    assert_eq!(out.removed.len(), 1, "the delete loop reclaims it: {out:?}");
    assert!(!s.wt.exists(), "and the directory is gone");
}

/// #5661 kept: the same tree whose dispatcher is the record's CURRENT Claude
/// session is a live agent's, so the open delegation keeps it.
#[tokio::test]
async fn worktree_7771_the_current_sessions_open_delegation_keeps_the_tree() {
    let s = scene("agent-current-7771");
    let c = claims(&s, Links::Current).await;
    let v = verdict(&s, &c, &agent_live);
    assert!(
        matches!(v, ReclaimVerdict::BlockedByAgent { .. }),
        "gate 4 keeps it: {v:?}"
    );
    assert!(refused(&v).contains("has not ended"), "{v:?}");
}

/// 🔴 #7771 fail-closed: a link sidecar that cannot be read proves nothing, so
/// a superseded dispatcher stays undeterminable and the tree is kept.
#[tokio::test]
async fn worktree_7771_an_unreadable_link_history_keeps_the_tree() {
    let s = scene("agent-unreadable-7771");
    let c = claims(&s, Links::SupersededUnreadable).await;
    let reason = refused(&verdict(&s, &c, &agent_live));
    assert!(reason.contains("has not ended"), "{reason}");
    let reason = refused(&verdict(&s, &c, &no_agents));
    assert!(reason.contains(&s.dispatcher), "{reason}");
    assert!(reason.contains("could not be read"), "{reason}");
}

/// 🔴 #7771 fail-closed: the sidecar links the dispatcher to a session no
/// stored record names — compacted or never persisted — which is not evidence
/// that it ended.
#[tokio::test]
async fn worktree_7771_a_history_naming_an_unstored_session_keeps_the_tree() {
    let s = scene("agent-unstored-7771");
    let c = claims(&s, Links::LinkedToAnUnstoredSession).await;
    let reason = refused(&verdict(&s, &c, &agent_live));
    assert!(reason.contains("has not ended"), "{reason}");
    let reason = refused(&verdict(&s, &c, &no_agents));
    assert!(reason.contains(&s.dispatcher), "{reason}");
    assert!(reason.contains("tm-compacted-7771"), "{reason}");
}
