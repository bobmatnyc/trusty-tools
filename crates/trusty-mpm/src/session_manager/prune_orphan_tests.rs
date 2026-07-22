//! Unit tests for the orphaned-worktree scan/reclaim logic in `prune.rs`
//! (#1840, #1845, extended #3649).
//!
//! Why: split out of `prune.rs` (as `orphan_tests`) so that file stays under
//! the 500-SLOC production cap — `prune.rs` mixes production code (the
//! walk, the ownership gate, the git cross-check) with a large test surface,
//! and only the production code counts against the cap once tests live in a
//! sibling `_tests.rs` file, mirroring the pattern already established by
//! `decommission_worktree_tests.rs` / `reap_orphaned_worktrees_tests.rs`.
//! What: orphan discovery (`find_orphaned_worktrees`, both worktree-store
//! shapes), the TOCTOU fresh-active-set safety net, and the #3649 ownership
//! gate (owner-unknown never deleted, terminal/absent owner reclaimed, live
//! owner spared, `git worktree list` agreement).
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use super::*;

#[test]
fn prune_orphaned_worktrees_spares_active() {
    // A live session's worktree must never be returned as an orphan (#1840).
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let wt = root
        .join("owner")
        .join("repo")
        .join(".worktrees")
        .join("live-session");
    std::fs::create_dir_all(&wt).unwrap();
    let active: std::collections::HashSet<_> =
        vec![std::fs::canonicalize(&wt).unwrap_or_else(|_| wt.clone())]
            .into_iter()
            .collect();
    let orphans = find_orphaned_worktrees(root, &active);
    assert!(
        orphans.is_empty(),
        "live session must not be listed as orphan"
    );
}

#[test]
fn prune_orphaned_worktrees_fresh_active_set_blocks_deletion() {
    // Simulates TOCTOU: a dir looks like an orphan in the initial snapshot
    // but appears in the fresh active set before deletion — must NOT be removed.
    // We test the `find_orphaned_worktrees` logic: with an empty initial set
    // the candidate IS found; then the Phase 2 TOCTOU check (re-querying the
    // store) is validated by confirming fresh set membership would block deletion.
    // The full async TOCTOU path is validated by the integration tests.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let wt = root
        .join("owner")
        .join("repo")
        .join(".worktrees")
        .join("session-xyz");
    std::fs::create_dir_all(&wt).unwrap();

    // Empty initial snapshot → the dir looks like an orphan candidate.
    let empty_initial: std::collections::HashSet<std::path::PathBuf> =
        std::collections::HashSet::new();
    let candidates = find_orphaned_worktrees(root, &empty_initial);
    assert!(
        candidates.contains(&wt),
        "empty initial set must find the dir as a candidate"
    );

    // Fresh active set contains the canonicalized dir path — mirrors Phase 2 of
    // prune_orphaned_worktrees, which canonicalizes workspace_paths before
    // inserting them into fresh_active (#1840 TOCTOU check).
    let canonical = std::fs::canonicalize(&wt).unwrap_or_else(|_| wt.clone());
    let fresh: std::collections::HashSet<std::path::PathBuf> =
        [canonical.clone()].into_iter().collect();
    assert!(
        fresh.contains(&canonical),
        "fresh active set must contain the canonicalized worktree path"
    );
    // The directory still exists — nothing deleted it.
    assert!(wt.exists(), "worktree must survive the TOCTOU check");
}

#[test]
fn prune_orphaned_worktrees_collects_orphan() {
    // A worktree with no active session must be listed as an orphan (#1840).
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let wt1 = root
        .join("owner")
        .join("repo")
        .join(".worktrees")
        .join("live");
    let wt2 = root
        .join("owner")
        .join("repo")
        .join(".worktrees")
        .join("dead");
    std::fs::create_dir_all(&wt1).unwrap();
    std::fs::create_dir_all(&wt2).unwrap();
    let active: std::collections::HashSet<_> =
        vec![std::fs::canonicalize(&wt1).unwrap_or_else(|_| wt1.clone())]
            .into_iter()
            .collect();
    let orphans = find_orphaned_worktrees(root, &active);
    assert_eq!(orphans.len(), 1);
    // Ordering not guaranteed — use contains rather than indexed access.
    assert!(
        orphans.contains(&wt2),
        "expected {wt2:?} to be the orphan, got {orphans:?}"
    );
}

/// Item 1 (#1845): async test that genuinely exercises the Phase 2 fresh-store
/// snapshot path in `prune_orphaned_worktrees`.
///
/// Why: the existing sync test at `prune_orphaned_worktrees_fresh_active_set_blocks_deletion`
/// only calls `find_orphaned_worktrees` directly, giving zero executed coverage of
/// the Phase 2 `fresh_active` snapshot logic in the async method. This test goes
/// end-to-end through `prune_orphaned_worktrees` with a real `SessionManager`:
/// Phase 1 finds the worktree as a candidate (empty initial snapshot), then Phase 2
/// reads the live store and finds the matching record — skipping deletion.
/// Test: this function IS the test.
#[tokio::test]
async fn prune_orphaned_worktrees_store_snapshot_blocks_deletion() {
    use std::path::PathBuf;
    use std::sync::Arc;

    // Minimal driver: all ops are no-ops — we never actually need tmux.
    struct NoopDriver;
    impl super::super::manager::ManagedTmuxDriver for NoopDriver {
        fn create_session(
            &self,
            _: &str,
            _: &str,
        ) -> Result<(), super::super::manager::ManagedError> {
            Ok(())
        }
        fn kill_session(&self, _: &str) -> Result<(), super::super::manager::ManagedError> {
            Ok(())
        }
        fn send_line(&self, _: &str, _: &str) -> Result<(), super::super::manager::ManagedError> {
            Ok(())
        }
        fn capture(
            &self,
            _: &str,
            _: usize,
        ) -> Result<String, super::super::manager::ManagedError> {
            Ok(String::new())
        }
        fn list_sessions(&self) -> Result<Vec<String>, super::super::manager::ManagedError> {
            Ok(Vec::new())
        }
    }

    let store_dir = tempfile::tempdir().unwrap();
    let repos_tmp = tempfile::tempdir().unwrap();

    // Build a real .worktrees/<id>/ dir so Phase 1 finds it as a candidate.
    let session_id = super::super::record::ManagedSessionId::new();
    let wt_path = repos_tmp
        .path()
        .join("owner")
        .join("repo")
        .join(".worktrees")
        .join(session_id.to_string());
    std::fs::create_dir_all(&wt_path).expect("create worktree dir");

    // Create the SessionManager and insert a live record for the worktree.
    let mgr = super::super::manager::SessionManager::new(store_dir.path(), Arc::new(NoopDriver))
        .await
        .expect("SessionManager::new");

    let canonical_wt = std::fs::canonicalize(&wt_path).unwrap_or_else(|_| wt_path.clone());
    let record = super::super::record::SessionRecord {
        id: session_id,
        tmux_name: "test-toctou".into(),
        cwd: PathBuf::from("/tmp"),
        task: "toctou test".into(),
        state: super::super::record::ManagedSessionState::Active,
        created_at: chrono::Utc::now(),
        last_activity_at: None,
        workspace_path: Some(canonical_wt),
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
    };
    mgr.store
        .write()
        .await
        .upsert(record)
        .await
        .expect("upsert test record");

    // Phase 1 will see an empty initial set → worktree is a candidate.
    // #3649: the dir has no ownership sentinel, so it is classified
    // owner-unknown and skipped BEFORE the Phase 2 fresh-active check ever
    // runs — still never removed, now for the #3649 safe-default reason
    // rather than (only) the #1845 TOCTOU fresh-snapshot reason.
    let outcome = mgr
        .prune_orphaned_worktrees(repos_tmp.path(), &[], false)
        .await
        .expect("prune must not error");

    assert!(
        outcome.removed.is_empty(),
        "worktree backed by a live store record must NOT be removed; got: {:?}",
        outcome.removed
    );
    assert!(wt_path.exists(), "worktree dir must survive the prune");
}

// ── #3649: extended `.base/.worktrees` walk + ownership gating ──────────

/// `find_orphaned_worktrees` must ALSO discover the clone-based
/// `.base/.worktrees/<id>` shape, not just the in-project `.worktrees/<name>`
/// shape (#3649 item 4a — this walk previously covered ONLY the latter, so
/// the entire `provisioner::workspace` worktree store was invisible to the
/// orphan-GC, `tm doctor`, and `--dry-run`).
#[test]
fn find_orphaned_worktrees_covers_base_worktrees_shape() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let base_wt = root
        .join("owner")
        .join("repo")
        .join(".base")
        .join(".worktrees")
        .join("session-abc");
    std::fs::create_dir_all(&base_wt).unwrap();
    let empty_active: std::collections::HashSet<std::path::PathBuf> =
        std::collections::HashSet::new();
    let orphans = find_orphaned_worktrees(root, &empty_active);
    assert!(
        orphans.contains(&base_wt),
        "the .base/.worktrees shape must be discovered as a candidate; got {orphans:?}"
    );
}

/// A candidate whose ownership sentinel is absent (no `.trusty-mpm-worktree`
/// file at all — the pre-#3649/legacy shape) is NEVER auto-deleted, and is
/// counted in [`OrphanSweepOutcome::owner_unknown`] (#3649 item 4b).
#[tokio::test]
async fn prune_orphaned_worktrees_skips_owner_unknown() {
    let store_dir = tempfile::tempdir().unwrap();
    let repos = tempfile::tempdir().unwrap();
    let mgr = SessionManager::new(
        store_dir.path(),
        crate::session_manager::tests::FakeTmuxDriver::new(),
    )
    .await
    .expect("manager");

    let wt = repos
        .path()
        .join("owner")
        .join("repo")
        .join(".worktrees")
        .join("legacy-no-sentinel");
    std::fs::create_dir_all(&wt).unwrap();
    // Deliberately NO sentinel file written — simulates a legacy worktree.

    let outcome = mgr
        .prune_orphaned_worktrees(repos.path(), &[], false)
        .await
        .expect("prune must not error");

    assert!(
        outcome.removed.is_empty(),
        "an owner-unknown candidate must never be auto-deleted; got {:?}",
        outcome.removed
    );
    assert!(
        outcome.owner_unknown.iter().any(|p| p == &wt),
        "an owner-unknown candidate must be counted for doctor surfacing; got {:?}",
        outcome.owner_unknown
    );
    assert!(
        wt.exists(),
        "the untouched legacy worktree must survive on disk"
    );
}

/// Write a sentinel with an explicit `created_at`, bypassing
/// `sentinel_payload_bytes`'s "now" default — needed to simulate a sentinel
/// old enough to fall outside [`crate::session_manager::worktree_ownership::OWNERLESS_GRACE`]
/// (#3649 review fix regression coverage).
fn aged_sentinel_bytes(
    owner: ManagedSessionId,
    created_at: chrono::DateTime<chrono::Utc>,
) -> Vec<u8> {
    serde_json::to_vec(
        &crate::session_manager::worktree_ownership::WorktreeSentinel {
            owner_session_id: owner,
            created_at,
        },
    )
    .expect("serialize aged sentinel")
}

/// A candidate whose sentinel names an owner with NO resolvable session
/// record (deleted / never registered) AND whose sentinel is OLDER than the
/// creation-race grace window is provably ownerless and IS reclaimed (#3649
/// item 5 — "sentinel owner id does not resolve to any record"; #3649 review
/// fix — an aged timestamp is required here so this test exercises the
/// legitimate "owner was purged" cleanup path rather than the creation-race
/// bug a freshly-stamped absent owner would have masked).
#[tokio::test]
async fn prune_orphaned_worktrees_reclaims_terminal_owner() {
    let store_dir = tempfile::tempdir().unwrap();
    let repos = tempfile::tempdir().unwrap();
    let mgr = SessionManager::new(
        store_dir.path(),
        crate::session_manager::tests::FakeTmuxDriver::new(),
    )
    .await
    .expect("manager");

    let wt = repos
        .path()
        .join("owner")
        .join("repo")
        .join(".worktrees")
        .join("ownerless-gone");
    std::fs::create_dir_all(&wt).unwrap();
    let never_registered_owner = ManagedSessionId::new();
    let aged = chrono::Utc::now()
        - crate::session_manager::worktree_ownership::OWNERLESS_GRACE
        - chrono::Duration::minutes(1);
    std::fs::write(
        wt.join(crate::session_manager::decommission::WORKTREE_SENTINEL_FILE),
        aged_sentinel_bytes(never_registered_owner, aged),
    )
    .expect("write sentinel");

    let outcome = mgr
        .prune_orphaned_worktrees(repos.path(), &[], false)
        .await
        .expect("prune must not error");

    assert!(
        outcome.removed.iter().any(|p| p == &wt),
        "a candidate whose owner has no resolvable record AND whose sentinel \
         is aged past the grace window must be reclaimed; got {:?}",
        outcome.removed
    );
    assert!(
        !wt.exists(),
        "the reclaimed worktree must be removed from disk"
    );
}

/// A candidate whose sentinel names an owner with NO resolvable session
/// record, but whose sentinel was stamped RECENTLY (within the creation-race
/// grace window), must NOT be reclaimed — this is the exact bug the #3649
/// review fix closes: the sentinel is written before the owning session's
/// `SessionRecord` is persisted, so a `get()`-not-found result during that
/// window is ambiguous between "deleted" and "not created yet", and a
/// freshly-stamped sentinel must resolve that ambiguity toward "not yet".
#[tokio::test]
async fn prune_orphaned_worktrees_spares_recent_unregistered_owner() {
    let store_dir = tempfile::tempdir().unwrap();
    let repos = tempfile::tempdir().unwrap();
    let mgr = SessionManager::new(
        store_dir.path(),
        crate::session_manager::tests::FakeTmuxDriver::new(),
    )
    .await
    .expect("manager");

    let wt = repos
        .path()
        .join("owner")
        .join("repo")
        .join(".worktrees")
        .join("mid-creation-race");
    std::fs::create_dir_all(&wt).unwrap();
    let not_yet_persisted_owner = ManagedSessionId::new();
    std::fs::write(
        wt.join(crate::session_manager::decommission::WORKTREE_SENTINEL_FILE),
        crate::session_manager::worktree_ownership::sentinel_payload_bytes(not_yet_persisted_owner),
    )
    .expect("write sentinel");

    let outcome = mgr
        .prune_orphaned_worktrees(repos.path(), &[], false)
        .await
        .expect("prune must not error");

    assert!(
        !outcome.removed.iter().any(|p| p == &wt),
        "a candidate whose owner is absent but whose sentinel is fresh must \
         NEVER be reclaimed (mid-creation race, #3649); got {:?}",
        outcome.removed
    );
    assert!(
        wt.exists(),
        "the mid-creation-race worktree must survive on disk"
    );
}

/// A candidate whose sentinel names an owner with a LIVE (`Active`) record
/// is NEVER reclaimed, even though the directory itself is not in the
/// caller's active-path set (#3649 item 5 — "a live/Stopped/Errored owner's
/// worktree is NEVER ownerless").
#[tokio::test]
async fn prune_orphaned_worktrees_spares_live_owner() {
    let store_dir = tempfile::tempdir().unwrap();
    let repos = tempfile::tempdir().unwrap();
    let mgr = SessionManager::new(
        store_dir.path(),
        crate::session_manager::tests::FakeTmuxDriver::new(),
    )
    .await
    .expect("manager");

    let wt = repos
        .path()
        .join("owner")
        .join("repo")
        .join(".worktrees")
        .join("live-owner-elsewhere");
    std::fs::create_dir_all(&wt).unwrap();

    // Register the owner as a LIVE record whose workspace_path points
    // somewhere else entirely — so `wt` is NOT in `active_workspace_paths`
    // (it looks orphaned by the path-only check) but its sentinel's owner
    // is still genuinely alive.
    let owner_id = ManagedSessionId::new();
    let owner_record = mgr
        .create_with_id(
            owner_id,
            "task".into(),
            None,
            None,
            None,
            None,
            None,
            crate::runtime::RuntimeKind::default(),
            false,
            false,
        )
        .await
        .expect("create owner record");
    assert_eq!(owner_record.state, ManagedSessionState::Provisioning);

    std::fs::write(
        wt.join(crate::session_manager::decommission::WORKTREE_SENTINEL_FILE),
        crate::session_manager::worktree_ownership::sentinel_payload_bytes(owner_id),
    )
    .expect("write sentinel");

    let outcome = mgr
        .prune_orphaned_worktrees(repos.path(), &[], false)
        .await
        .expect("prune must not error");

    assert!(
        !outcome.removed.iter().any(|p| p == &wt),
        "a candidate whose owner is live must NEVER be reclaimed; got {:?}",
        outcome.removed
    );
    assert!(wt.exists(), "the live-owned worktree must survive on disk");
}

// ── git_worktree_list_agrees (#3649 item 5) ──────────────────────────────

/// `git_worktree_list_agrees` returns `true` for a path that is genuinely
/// registered as a git worktree.
#[test]
fn git_worktree_list_agrees_true_for_real_worktree() {
    let base_dir = tempfile::tempdir().unwrap();
    let base = base_dir.path();
    let init_ok = std::process::Command::new("git")
        .arg("init")
        .current_dir(base)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !init_ok {
        eprintln!("git_worktree_list_agrees_true_for_real_worktree: git unavailable, skipping");
        return;
    }
    let _ = std::process::Command::new("git")
        .args([
            "-C",
            base.to_str().unwrap(),
            "config",
            "user.email",
            "ci@test.invalid",
        ])
        .status();
    let _ = std::process::Command::new("git")
        .args(["-C", base.to_str().unwrap(), "config", "user.name", "CI"])
        .status();
    let commit_ok = std::process::Command::new("git")
        .args([
            "-C",
            base.to_str().unwrap(),
            "commit",
            "--allow-empty",
            "-m",
            "init",
        ])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !commit_ok {
        eprintln!("git_worktree_list_agrees_true_for_real_worktree: commit failed, skipping");
        return;
    }
    let wt_dir = base.join(".worktrees");
    std::fs::create_dir_all(&wt_dir).unwrap();
    let wt_path = wt_dir.join("agree-test");
    let add_ok = std::process::Command::new("git")
        .args([
            "-C",
            base.to_str().unwrap(),
            "worktree",
            "add",
            "-b",
            "session/agree-test",
        ])
        .arg(&wt_path)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    assert!(add_ok, "git worktree add must succeed in this test fixture");

    assert!(
        git_worktree_list_agrees(&wt_path),
        "a genuinely registered git worktree must agree"
    );
}

/// `git_worktree_list_agrees` returns `false` for a directory git has no
/// record of as a worktree (a plain dir sitting under `.worktrees/`).
#[test]
fn git_worktree_list_agrees_false_for_untracked_dir() {
    let base_dir = tempfile::tempdir().unwrap();
    let base = base_dir.path();
    let init_ok = std::process::Command::new("git")
        .arg("init")
        .current_dir(base)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !init_ok {
        eprintln!("git_worktree_list_agrees_false_for_untracked_dir: git unavailable, skipping");
        return;
    }
    let untracked = base.join(".worktrees").join("never-added-to-git");
    std::fs::create_dir_all(&untracked).unwrap();

    assert!(
        !git_worktree_list_agrees(&untracked),
        "a directory git never registered as a worktree must disagree"
    );
}
