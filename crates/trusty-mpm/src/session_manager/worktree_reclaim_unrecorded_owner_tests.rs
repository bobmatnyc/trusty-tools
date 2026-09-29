//! End-to-end coverage for an agent tree whose dispatching Claude session no
//! record proves ended — unrecorded, or recorded live after a relaunch (#7771).
//!
//! Why: on 2026-09-28 four clean, landed agent trees were kept because their
//! owner session appeared nowhere in the session store, and `tm pr cleanup`
//! refused two more because it "cannot observe tmux". Claude Code's own
//! per-process registry is the evidence that settles both.
//! What: a real store behind `workspace_claims`, a temp Claude config root
//! installed as the registry, and a real merged agent-store worktree, run
//! through the survey (and the delete loop for the reported case). A running
//! entry is this test process itself, with its real start time; a gone entry
//! is a reaped child's pid.
//! Test: this file IS the test module.

use std::path::{Path, PathBuf};

use tempfile::TempDir;

use crate::core::session_links::record_link;
use crate::session_manager::manager::SessionManager;
use crate::session_manager::record::ManagedSessionId;
use crate::session_manager::tests::FakeTmuxDriver;
use crate::session_manager::worktree_owner_gate::{format_lock_start, process_start_secs};
use crate::session_manager::worktree_ownership::{AgentDelegationState, AgentWorktreeOwner};
use crate::session_manager::worktree_reclaim::{KeepList, LiveClaims, ReclaimMode, ReclaimVerdict};
use crate::session_manager::worktree_reclaim_superseded_owner_tests::{
    Scene, agent_live, merged_index, pm_record, refused, scene, verdict,
};
use crate::session_manager::worktree_reclaim_sweep::{FreshProbes, reclaim_scoped};

const TMUX: &str = "tm-pm-7771";
const NEWER: &str = "0b7e1a55-7771-4000-8000-00000000000a";

fn a_dead_pid() -> u32 {
    let mut child = std::process::Command::new("true")
        .spawn()
        .expect("spawn `true`");
    let pid = child.id();
    child.wait().expect("reap `true`");
    pid
}

fn no_agents(_: &AgentWorktreeOwner) -> AgentDelegationState {
    AgentDelegationState::Unknown
}

/// This test process: a pid that runs, and its real start stamp.
fn me() -> (u32, String) {
    let pid = std::process::id();
    let start = process_start_secs(pid).expect("own start time");
    (pid, format_lock_start(start))
}

/// A Claude config root holding the dispatcher's transcript.
fn config_root(dispatcher: &str) -> TempDir {
    let root = TempDir::new().expect("claude config root");
    let project = root.path().join("projects").join("-work-repo");
    std::fs::create_dir_all(&project).expect("projects");
    std::fs::write(project.join(format!("{dispatcher}.jsonl")), "{}\n").expect("transcript");
    std::fs::create_dir_all(root.path().join("sessions")).expect("sessions");
    root
}

/// `sessions/<pid>.json`; `stamp` `None` omits `procStart`.
fn register(root: &Path, pid: u32, session: &str, stamp: Option<&str>) {
    let start = stamp.map_or(String::new(), |s| format!(r#","procStart":"{s}""#));
    let body = format!(r#"{{"pid":{pid},"sessionId":"{session}","tmux":"{TMUX}:@1.%1"{start}}}"#);
    std::fs::write(root.join("sessions").join(format!("{pid}.json")), body).expect("entry");
}

/// How the store records the dispatcher.
enum Store {
    /// No record names it; a live PM record carries an unrelated id.
    Unrecorded,
    /// The live PM record still carries it as its current Claude id.
    StaleCurrent,
    /// As `Unrecorded`, plus a link sidecar that cannot be read.
    UnreadableSidecar,
}

/// The daemon's claim set, with `claude` installed as the registry root.
async fn claims(s: &Scene, store: Store, claude: &Path) -> LiveClaims {
    let root = TempDir::new().expect("framework root");
    let data_dir = root.path().join("session-manager");
    std::fs::create_dir_all(&data_dir).expect("session-manager dir");
    let fake = FakeTmuxDriver::new();
    fake.seeded_names.lock().expect("lock").push(TMUX.into());
    let mgr = SessionManager::new(&data_dir, fake).await.expect("manager");
    assert!(mgr.install_claude_registry_roots(vec![claude.to_path_buf()]));
    let current = match store {
        Store::StaleCurrent => s.dispatcher.as_str(),
        _ => NEWER,
    };
    let record = pm_record(TMUX, current);
    mgr.store
        .write()
        .await
        .upsert(record)
        .await
        .expect("upsert");
    if matches!(store, Store::UnreadableSidecar) {
        let managed = ManagedSessionId::for_adopted_tmux_name(TMUX).to_string();
        record_link(root.path(), &managed, NEWER);
        let dir = root.path().join("usage").join("session-links");
        std::fs::create_dir(dir.join("tm-unreadable-7771")).expect("an unreadable entry");
    }
    mgr.workspace_claims(Some("tm-caller-7771".into())).await
}

/// 🔴 #7771 (a): the reported case. No record names the dispatcher, the dead
/// agent's delegation still reads `Running`, and the only registry entry
/// naming the dispatcher is a gone pid's. Survey and delete loop reclaim it.
///
/// Fails before the fix: "nothing proves that session ended: no stored
/// session record names it".
#[tokio::test]
async fn worktree_7771_an_unrecorded_owner_whose_process_is_gone_is_reclaimed() {
    let s = scene("agent-unrecorded-7771");
    let claude = config_root(&s.dispatcher);
    register(
        claude.path(),
        a_dead_pid(),
        &s.dispatcher,
        Some("Mon Sep 28 23:25:10 2026"),
    );
    let c = claims(&s, Store::Unrecorded, claude.path()).await;
    assert_eq!(
        verdict(&s, &c, &agent_live),
        ReclaimVerdict::Reclaimable { pr: 7771 }
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

/// 🔴 #7771 (b): the PM was relaunched in the same tmux window. Its record
/// still names the dispatcher and reads live, but the registry shows that
/// window running a newer session and no process running the dispatcher.
///
/// Fails before the fix: the live record kept the tree ("has not ended").
#[tokio::test]
async fn worktree_7771_a_session_replaced_in_its_tmux_window_is_reclaimed() {
    let s = scene("agent-replaced-7771");
    let claude = config_root(&s.dispatcher);
    let (pid, stamp) = me();
    register(claude.path(), pid, NEWER, Some(&stamp));
    let c = claims(&s, Store::StaleCurrent, claude.path()).await;
    assert_eq!(
        verdict(&s, &c, &agent_live),
        ReclaimVerdict::Reclaimable { pr: 7771 }
    );
}

/// #7771: the same live record with NO process registered in its tmux window
/// proves no replacement, so the live record keeps the tree.
#[tokio::test]
async fn worktree_7771_a_live_record_with_no_registered_process_is_kept() {
    let s = scene("agent-unreplaced-7771");
    let claude = config_root(&s.dispatcher);
    let c = claims(&s, Store::StaleCurrent, claude.path()).await;
    let reason = refused(&verdict(&s, &c, &agent_live));
    assert!(reason.contains("has not ended"), "{reason}");
}

/// 🔴 #7771 (c) fail-closed: a registry the process table cannot settle keeps
/// the tree — a running pid registered to the dispatcher with no start time,
/// or a live entry that does not parse.
#[tokio::test]
async fn worktree_7771_a_registry_probe_error_keeps_the_tree() {
    let s = scene("agent-probe-error-7771");
    let (pid, _) = me();
    let unstamped = config_root(&s.dispatcher);
    register(unstamped.path(), pid, &s.dispatcher, None);
    let c = claims(&s, Store::Unrecorded, unstamped.path()).await;
    let reason = refused(&verdict(&s, &c, &agent_live));
    assert!(reason.contains("has not ended"), "{reason}");

    let corrupt = config_root(&s.dispatcher);
    let entry = corrupt.path().join("sessions").join(format!("{pid}.json"));
    std::fs::write(entry, "{truncated").expect("corrupt entry");
    let c = claims(&s, Store::Unrecorded, corrupt.path()).await;
    let reason = refused(&verdict(&s, &c, &no_agents));
    assert!(reason.contains("may still run"), "{reason}");
}

/// #7771: a process still registered to the dispatcher keeps the tree.
#[tokio::test]
async fn worktree_7771_a_running_owner_process_keeps_the_tree() {
    let s = scene("agent-running-7771");
    let claude = config_root(&s.dispatcher);
    let (pid, stamp) = me();
    register(claude.path(), pid, &s.dispatcher, Some(&stamp));
    let c = claims(&s, Store::Unrecorded, claude.path()).await;
    let reason = refused(&verdict(&s, &c, &agent_live));
    assert!(reason.contains("has not ended"), "{reason}");
}

/// 🔴 #7771 fail-closed: an unreadable link sidecar outranks the registry's
/// proof, so the tree is kept.
#[tokio::test]
async fn worktree_7771_an_unreadable_sidecar_outranks_the_registry() {
    let s = scene("agent-sidecar-7771");
    let claude = config_root(&s.dispatcher);
    let c = claims(&s, Store::UnreadableSidecar, claude.path()).await;
    let reason = refused(&verdict(&s, &c, &agent_live));
    assert!(reason.contains("has not ended"), "{reason}");
}

/// #7771: the dirty-tree rule is unchanged — an ended owner's tree with an
/// untracked file is kept.
#[tokio::test]
async fn worktree_7771_a_dirty_tree_is_kept_though_its_owner_ended() {
    let s = scene("agent-dirty-7771");
    std::fs::write(s.wt.join("unsaved.rs"), "// work in progress\n").expect("dirty");
    let claude = config_root(&s.dispatcher);
    let c = claims(&s, Store::Unrecorded, claude.path()).await;
    let v = verdict(&s, &c, &agent_live);
    assert!(
        !matches!(v, ReclaimVerdict::Reclaimable { .. }),
        "kept: {v:?}"
    );
}

/// 🔴 #7771: `tm pr cleanup` — tmux unobservable — reclaims a foreign
/// agent tree once the registry proves its owner gone, and keeps it when the
/// registry cannot be settled.
#[tokio::test]
async fn cli_7771_tree_gate_reclaims_a_tree_whose_owner_process_is_gone() {
    use crate::core::pr_cleanup::{CallerOwnership, ClaimOwnership};
    use crate::session_manager::worktree_claude_registry::ClaudeRegistry;
    let s = scene("agent-cli-7771");
    let claude = config_root(&s.dispatcher);
    let roots: Vec<PathBuf> = vec![claude.path().to_path_buf()];
    let cli = |registry| CallerOwnership::new(vec!["my-session".into()]).with_claude(registry);
    assert_eq!(
        cli(ClaudeRegistry::read(&roots)).tree_gate(&s.wt).await,
        Ok(())
    );

    let (pid, _) = me();
    register(claude.path(), pid, &s.dispatcher, None);
    let why = cli(ClaudeRegistry::read(&roots))
        .tree_gate(&s.wt)
        .await
        .expect_err("an unsettled registry keeps the tree");
    assert!(why.contains("no readable start"), "{why}");
}
