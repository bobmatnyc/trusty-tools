//! Tests for the daemon's removal recorder (#8994 S2c criteria 3 and 4).
//!
//! Why: the merged-PR reclaim and the agent-worktree reap must record their
//! removals and only those; a tree a removal left behind stays live.
//! What: the reclaim's outcome lists fed to [`record_reclaimed`], a real
//! `SessionEnd` reap through [`super::super::agent_worktree_reap`], and the
//! relative-root arm of [`recorder_at`].
//! Test: this file.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::{record_reclaimed, recorder_at, recorder_for};
use crate::core::hook::HookEvent;
use crate::core::session::SessionId;
use crate::core::worktree_ledger::fold::fold;
use crate::core::worktree_ledger::removal_evidence::RemovalVerdict;
use crate::core::worktree_ledger::{
    EventKind, LEDGER_FILE, LedgerEvent, Origin, WorktreeLedger, ledger_key,
};
use crate::daemon::state::DaemonState;
use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;
use crate::session_manager::worktree_ownership::{AgentWorktreeOwner, write_agent_sentinel};
use crate::session_manager::worktree_reclaim::ReclaimOutcome;

fn hermetic() -> (Arc<DaemonState>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("temp dir");
    let paths = crate::core::paths::FrameworkPaths::under(dir.path());
    (Arc::new(DaemonState::with_paths(&paths)), dir)
}

fn ledger_of(state: &DaemonState) -> WorktreeLedger {
    WorktreeLedger::at(state.framework_root().join(LEDGER_FILE))
}

fn seed(ledger: &WorktreeLedger, tree: &Path, repo: &Path) {
    ledger
        .append(&LedgerEvent::now(
            ledger_key(tree),
            EventKind::Observed {
                repo: ledger_key(repo),
                branch: None,
                origin: Origin::AgentIsolation,
                session: None,
            },
        ))
        .expect("seed");
}

fn live(ledger: &WorktreeLedger, tree: &Path) -> bool {
    fold(&ledger.read().expect("read").events)
        .live
        .contains_key(&ledger_key(tree))
}

fn removed_lines(ledger: &WorktreeLedger) -> Vec<PathBuf> {
    ledger
        .read()
        .expect("read")
        .events
        .into_iter()
        .filter(|e| e.kind == EventKind::Removed)
        .map(|e| e.path)
        .collect()
}

/// #8994 S2c criterion 3: the merged-PR reclaim records the trees in its
/// `removed` list, and never one in `removal_failed` or `partially_removed`
/// — those left a directory behind and stay live.
#[test]
fn merged_pr_reclaim_records_only_its_removed_list() {
    let (state, _dir) = hermetic();
    let fx = GitWorktreeFixture::new();
    let ledger = ledger_of(&state);
    let gone = fx.add_worktree("s2c-reclaimed");
    let failed = fx.add_worktree("s2c-failed");
    let partial = fx.add_worktree("s2c-partial");
    for t in [&gone, &failed, &partial] {
        seed(&ledger, t, &fx.repo);
    }
    let gone_key = ledger_key(&gone);
    std::process::Command::new("git")
        .arg("-C")
        .arg(&fx.repo)
        .args(["worktree", "remove", "--force"])
        .arg(&gone)
        .status()
        .expect("git");
    std::fs::remove_file(partial.join("README.md")).expect("half-removed content");
    let outcome = ReclaimOutcome {
        removed: vec![gone_key.clone()],
        removal_failed: vec![format!("{}: locked", failed.display())],
        partially_removed: vec![format!("{}: git failed", partial.display())],
        ..ReclaimOutcome::default()
    };

    let evidence = record_reclaimed(&recorder_for(&state), &outcome);

    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].verdict, RemovalVerdict::Recorded);
    assert_eq!(removed_lines(&ledger), vec![gone_key]);
    assert!(live(&ledger, &failed) && live(&ledger, &partial));
}

/// A harness-shaped agent tree of `fx`, attributed to `agent` of `parent`.
fn agent_tree(fx: &GitWorktreeFixture, name: &str, agent: &str, parent: SessionId) -> PathBuf {
    let wt = fx.add_worktree_at(&fx.repo.join(".claude").join("worktrees"), name);
    write_agent_sentinel(
        &wt,
        AgentWorktreeOwner {
            agent_id: agent.to_string(),
            delegation_id: crate::core::agent::DelegationId(uuid::Uuid::new_v4()),
            parent_session_id: parent,
        },
    )
    .expect("write agent sentinel");
    wt
}

/// #8994 S2c criterion 3: a `SessionEnd` reap records `removed` for the tree
/// it removed and nothing for the tree it kept for unsaved work.
#[tokio::test]
async fn session_end_reap_records_only_the_tree_it_removed() {
    let (state, _dir) = hermetic();
    let session = SessionId(uuid::Uuid::new_v4());
    let fx = GitWorktreeFixture::new();
    let clean = agent_tree(&fx, "agent-s2c-clean", "a-s2c-1", session);
    std::fs::write(clean.join("work.txt"), "done\n").expect("write work");
    GitWorktreeFixture::commit_all_and_push(&clean, "agent work");
    let dirty = agent_tree(&fx, "agent-s2c-dirty", "a-s2c-2", session);
    std::fs::write(dirty.join("README.md"), "never committed\n").expect("dirty");
    let ledger = ledger_of(&state);
    let clean_key = ledger_key(&clean);
    seed(&ledger, &clean, &fx.repo);
    seed(&ledger, &dirty, &fx.repo);

    let summary = super::super::agent_worktree_reap::spawn_on_session_end(
        &state,
        session,
        HookEvent::SessionEnd,
        &serde_json::json!({ "cwd": fx.repo.to_string_lossy() }),
    )
    .expect("a sweep ran")
    .await
    .expect("sweep task");

    assert_eq!((summary.removed, summary.kept), (1, 1), "{summary:?}");
    assert_eq!(removed_lines(&ledger), vec![clean_key.clone()]);
    assert!(!live(&ledger, &clean_key));
    assert!(dirty.exists() && live(&ledger, &dirty));
}

/// #8994 S2c error arm: a relative framework root has no ledger, so the
/// recorder appends nothing and warns.
#[test]
#[serial_test::serial]
fn a_relative_framework_root_records_nothing_and_warns() {
    use tracing_subscriber::layer::SubscriberExt;

    crate::test_support::enable_event_capture();
    let buffer = trusty_common::log_buffer::LogBuffer::new(16);
    let subscriber = tracing_subscriber::registry().with(
        trusty_common::log_buffer::LogBufferLayer::new(buffer.clone()),
    );
    let recorder = recorder_at(Path::new("relative-8994"), Arc::new(Mutex::new(())));

    let evidence = tracing::subscriber::with_default(subscriber, || {
        recorder("test route", &[PathBuf::from("/x/.worktrees/t")])
    });

    assert!(evidence.is_empty());
    let lines = buffer.tail(16);
    assert!(
        lines
            .iter()
            .any(|l| l.contains("WARN") && l.contains("test route")),
        "Captured: {lines:#?}"
    );
}
