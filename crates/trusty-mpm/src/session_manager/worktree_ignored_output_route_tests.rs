//! Route tests for the #8534 gitignored-output gate (critic round 2).
//!
//! Why: the first round gated only the agent reap; decommission, the merged-PR
//! reclaim and the orphan prune reached `git worktree remove --force` through
//! the shared remover with no such gate, so a zero-commit tree's gitignored
//! `results/` was deleted. Each route gets a keep test and a remove test, so a
//! gate that refused everything fails as surely as one that refused nothing.
//! What: real [`GitWorktreeFixture`] trees, seeded after the tree is clean,
//! driven through each route's own entry point.
//! Test: this file IS the test module.

use std::path::{Path, PathBuf};

use crate::session_manager::decommission_force::{ProvisioningDirt, remove_in_project_worktree};
use crate::session_manager::record::ManagedSessionId;
use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;
use crate::session_manager::worktree_ownership::{AgentDelegationState, AgentWorktreeOwner};
use crate::session_manager::worktree_reclaim::{
    KeepList, LiveClaims, PrIndex, ReclaimMode, ReclaimOutcome,
};
use crate::session_manager::worktree_reclaim_sweep::{FreshProbes, reclaim_with_probes};
use crate::session_manager::{DirtyWorktreePolicy, SessionManager};

/// What a finished run left in a tree, all of it gitignored.
#[derive(Debug, Clone, Copy)]
enum Seed {
    /// `results/out.json` — the run output #8534 lost.
    RunOutput,
    /// `target/debug/app` — cargo output.
    Target,
    /// `web/node_modules/pkg/index.js` — nested package output.
    NestedNodeModules,
    /// `.claude/settings.json` plus the in-tree ownership marker.
    Harness,
}

/// Every seed that must NOT keep a tree.
const REGENERABLE: [Seed; 3] = [Seed::Target, Seed::NestedNodeModules, Seed::Harness];

/// Ignore what the seeds write, for every worktree of `fx`'s repository — the
/// shared `info/exclude`, as `core::harness_exclude` writes it.
fn ignore_seeds(fx: &GitWorktreeFixture) {
    std::fs::write(
        fx.repo.join(".git/info/exclude"),
        "results/\ntarget/\nnode_modules/\n/.claude/settings.json\n/.trusty-mpm-worktree\n",
    )
    .expect("write info/exclude");
}

/// Write `seed` into `wt`.
fn seed(wt: &Path, seed: Seed) {
    let (dir, file) = match seed {
        Seed::RunOutput => ("results", "results/out.json"),
        Seed::Target => ("target/debug", "target/debug/app"),
        Seed::NestedNodeModules => ("web/node_modules/pkg", "web/node_modules/pkg/index.js"),
        Seed::Harness => (".claude", ".claude/settings.json"),
    };
    std::fs::create_dir_all(wt.join(dir)).expect("mkdir seed dir");
    std::fs::write(wt.join(file), "{}").expect("write seed file");
    if matches!(seed, Seed::Harness) {
        GitWorktreeFixture::stamp_reclaimable_sentinel(wt);
    }
}

/// A clean, owned tree named `name`, then seeded.
fn seeded_tree(fx: &GitWorktreeFixture, name: &str, s: Seed) -> PathBuf {
    let wt = fx.add_worktree(name);
    GitWorktreeFixture::stamp_reclaimable_sentinel(&wt);
    seed(&wt, s);
    wt
}

/// Run `body` with every tracing event rendered into the returned lines.
async fn captured<F: std::future::Future>(body: F) -> (F::Output, Vec<String>) {
    use tracing_subscriber::layer::SubscriberExt;
    crate::test_support::enable_event_capture();
    let buffer = trusty_common::log_buffer::LogBuffer::new(256);
    let subscriber = tracing_subscriber::registry().with(
        trusty_common::log_buffer::LogBufferLayer::new(buffer.clone()),
    );
    let guard = tracing::subscriber::set_default(subscriber);
    let out = body.await;
    drop(guard);
    (out, buffer.tail(256))
}

/// Decommission's removal step keeps a tree holding gitignored run output and
/// says why. Fails at 199c2e447, where the shared remover had no such gate.
#[tokio::test]
async fn decommission_keeps_gitignored_run_output() {
    let fx = GitWorktreeFixture::new();
    ignore_seeds(&fx);
    let wt = seeded_tree(&fx, "decom-8534", Seed::RunOutput);

    let verdict = remove_in_project_worktree(
        &ManagedSessionId::new(),
        None,
        &wt,
        ProvisioningDirt::Refuse,
    )
    .await;

    assert!(!verdict.removed, "{verdict:?}");
    assert!(
        wt.join("results/out.json").exists(),
        "the run output survives"
    );
    let reason = verdict.kept_reason.expect("a kept tree names why");
    assert!(
        reason.contains("#8534") && reason.contains("results/"),
        "{reason}"
    );
}

/// `--force` excuses provisioning files, never run output.
#[tokio::test]
async fn force_decommission_keeps_gitignored_run_output() {
    let fx = GitWorktreeFixture::new();
    ignore_seeds(&fx);
    let wt = seeded_tree(&fx, "decom-force-8534", Seed::RunOutput);

    let verdict = remove_in_project_worktree(
        &ManagedSessionId::new(),
        None,
        &wt,
        ProvisioningDirt::Discard,
    )
    .await;

    assert!(!verdict.removed && wt.join("results/out.json").exists());
}

/// Decommission still removes a tree whose only gitignored content is build
/// or harness output.
#[tokio::test]
async fn decommission_removes_build_and_harness_output() {
    for (i, s) in REGENERABLE.into_iter().enumerate() {
        let fx = GitWorktreeFixture::new();
        ignore_seeds(&fx);
        let wt = seeded_tree(&fx, &format!("decom-regen-8534-{i}"), s);

        let verdict = remove_in_project_worktree(
            &ManagedSessionId::new(),
            None,
            &wt,
            ProvisioningDirt::Refuse,
        )
        .await;

        assert!(verdict.removed, "{s:?}: {:?}", verdict.kept_reason);
        assert!(!wt.exists(), "{s:?}: the tree is gone");
    }
}

/// The strictest agent probe: no agent is known to be done.
fn no_agents(_: &AgentWorktreeOwner) -> AgentDelegationState {
    AgentDelegationState::Unknown
}

/// Land `wt` as a merged PR leaves it, seed it, and run the merged-PR reclaim.
fn reclaim(fx: &GitWorktreeFixture, name: &str, s: Seed) -> (PathBuf, ReclaimOutcome) {
    let wt = fx.add_worktree(name);
    std::fs::write(wt.join("landed.rs"), "// landed\n").expect("write landed file");
    GitWorktreeFixture::commit_all_and_push(&wt, "landed");
    seed(&wt, s);
    let branch = format!("session/{name}");
    let index = move |_: &Path| {
        PrIndex::from_json(
            &format!(r#"[{{"number": 37, "headRefName": "{branch}", "state": "MERGED"}}]"#),
            400,
        )
    };
    let out = reclaim_with_probes(
        &fx.repos_root,
        &FreshProbes {
            launched_from: &[],
            keep_list: &KeepList::default,
            agent_state: &no_agents,
            // #7652: the fixture sentinel's owner has ended.
            in_use_now: &|| {
                Some(LiveClaims {
                    owners: GitWorktreeFixture::reclaimable_owner_gone(),
                    ..LiveClaims::default()
                })
            },
            index_for: &index,
        },
        ReclaimMode::Remove,
        &[],
    );
    (wt, out)
}

/// The merged-PR reclaim keeps a tree holding gitignored run output and
/// reports why. Fails at 199c2e447.
#[test]
fn reclaim_keeps_gitignored_run_output() {
    let fx = GitWorktreeFixture::new();
    ignore_seeds(&fx);
    let (wt, out) = reclaim(&fx, "reclaim-8534", Seed::RunOutput);

    assert!(out.removed.is_empty(), "{out:?}");
    assert!(
        wt.join("results/out.json").exists(),
        "the run output survives"
    );
    assert!(
        out.removal_failed
            .iter()
            .any(|r| r.contains("#8534") && r.contains("results/")),
        "{out:?}"
    );
}

/// The merged-PR reclaim still removes build and harness output.
#[test]
fn reclaim_removes_build_and_harness_output() {
    for (i, s) in REGENERABLE.into_iter().enumerate() {
        let fx = GitWorktreeFixture::new();
        ignore_seeds(&fx);
        let (wt, out) = reclaim(&fx, &format!("reclaim-regen-8534-{i}"), s);
        assert_eq!(out.removed, vec![wt.clone()], "{s:?}: {out:?}");
        assert!(!wt.exists(), "{s:?}: the tree is gone");
    }
}

/// A manager whose store is empty, so every stamped tree is an orphan.
async fn manager() -> (SessionManager, tempfile::TempDir) {
    let store = tempfile::tempdir().expect("tempdir");
    let mgr = SessionManager::new(
        store.path(),
        crate::session_manager::tests::FakeTmuxDriver::new(),
    )
    .await
    .expect("manager");
    (mgr, store)
}

/// The orphan prune keeps a tree holding gitignored run output and logs why.
/// Fails at 199c2e447.
#[tokio::test]
#[serial_test::serial]
async fn prune_keeps_gitignored_run_output() {
    let (mgr, _store) = manager().await;
    let fx = GitWorktreeFixture::new();
    ignore_seeds(&fx);
    let wt = seeded_tree(&fx, "prune-8534", Seed::RunOutput);

    let (outcome, lines) = captured(mgr.prune_orphaned_worktrees(
        &fx.repos_root,
        &[],
        false,
        DirtyWorktreePolicy::Skip,
        &[],
    ))
    .await;

    let outcome = outcome.expect("prune must not error");
    assert!(!outcome.removed.contains(&wt), "{:?}", outcome.removed);
    assert!(
        wt.join("results/out.json").exists(),
        "the run output survives"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("worktree kept") && l.contains("#8534")),
        "{lines:#?}"
    );
}

/// `--discard-dirty` is the one opt-in that discards gitignored output too.
#[tokio::test]
async fn force_discard_prune_removes_gitignored_output() {
    let (mgr, _store) = manager().await;
    let fx = GitWorktreeFixture::new();
    ignore_seeds(&fx);
    let wt = seeded_tree(&fx, "prune-discard-8534", Seed::RunOutput);

    let outcome = mgr
        .prune_orphaned_worktrees(
            &fx.repos_root,
            &[],
            false,
            DirtyWorktreePolicy::ForceDiscard,
            &[],
        )
        .await
        .expect("prune must not error");

    assert!(outcome.removed.contains(&wt), "{:?}", outcome.removed);
    assert!(!wt.exists());
}

/// The orphan prune still removes build and harness output.
#[tokio::test]
async fn prune_removes_build_and_harness_output() {
    for (i, s) in REGENERABLE.into_iter().enumerate() {
        let (mgr, _store) = manager().await;
        let fx = GitWorktreeFixture::new();
        ignore_seeds(&fx);
        let wt = seeded_tree(&fx, &format!("prune-regen-8534-{i}"), s);

        let outcome = mgr
            .prune_orphaned_worktrees(&fx.repos_root, &[], false, DirtyWorktreePolicy::Skip, &[])
            .await
            .expect("prune must not error");

        assert!(
            outcome.removed.contains(&wt),
            "{s:?}: {:?}",
            outcome.removed
        );
        assert!(!wt.exists(), "{s:?}: the tree is gone");
    }
}
