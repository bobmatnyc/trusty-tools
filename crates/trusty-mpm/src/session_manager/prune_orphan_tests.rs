//! Unit tests for the orphaned-worktree scan/reclaim logic in `prune.rs`
//! (#1840, #1845, extended #3649).
//!
//! Why: split out of `prune.rs` (as `orphan_tests`) so that file stays under
//! the 500-SLOC production cap — `prune.rs` mixes production code (discovery,
//! the ownership gate, the git cross-check) with a large test surface, and
//! only the production code counts against the cap once tests live in a
//! sibling `_tests.rs` file, mirroring the pattern already established by
//! `decommission_worktree_tests.rs` / `reap_orphaned_worktrees_tests.rs`.
//! What: orphan discovery (`find_orphaned_worktrees`, now git-derived per
//! #4207 — every worktree in these tests is a REAL `git worktree add`, because
//! a `mkdir` is no longer a candidate and would make an assertion vacuous), the
//! TOCTOU fresh-active-set safety net, and the #3649 ownership gate
//! (owner-unknown never deleted, terminal/absent owner reclaimed, live owner
//! spared, `git worktree list` agreement).
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use super::*;

/// A live session's worktree must never be returned as an orphan (#1840).
///
/// #4207: the worktree is now a REAL `git worktree add`, not a `mkdir`. Under
/// the git-native enumeration a bare directory is not a candidate at all, so
/// the `mkdir` version of this test passed vacuously — it asserted an empty
/// list against an empty list.
#[test]
fn prune_orphaned_worktrees_spares_active() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("live-session");
    let active: std::collections::HashSet<_> = [wt.clone()].into_iter().collect();
    let orphans = find_orphaned_worktrees(&fx.repos_root, &active);
    assert!(
        orphans.is_empty(),
        "live session must not be listed as orphan; got {orphans:?}"
    );
}

/// Simulates TOCTOU: a worktree looks like an orphan in the initial snapshot
/// but appears in the fresh active set before deletion — must NOT be removed.
#[test]
fn prune_orphaned_worktrees_fresh_active_set_blocks_deletion() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("session-xyz");

    // Empty initial snapshot → the worktree looks like an orphan candidate.
    let empty_initial: std::collections::HashSet<std::path::PathBuf> =
        std::collections::HashSet::new();
    let candidates = find_orphaned_worktrees(&fx.repos_root, &empty_initial);
    assert!(
        candidates.contains(&wt),
        "empty initial set must find the worktree as a candidate; got {candidates:?}"
    );

    // A fresh active set containing it blocks the deletion (Phase 2, #1840).
    let fresh: std::collections::HashSet<std::path::PathBuf> = [wt.clone()].into_iter().collect();
    assert!(
        find_orphaned_worktrees(&fx.repos_root, &fresh).is_empty(),
        "a candidate in the fresh active set must not be proposed for deletion"
    );
    assert!(wt.exists(), "worktree must survive the TOCTOU check");
}

/// A worktree with no active session must be listed as an orphan (#1840).
#[test]
fn prune_orphaned_worktrees_collects_orphan() {
    let fx = GitWorktreeFixture::new();
    let live = fx.add_worktree("live");
    let dead = fx.add_worktree("dead");
    let active: std::collections::HashSet<_> = [live.clone()].into_iter().collect();
    let orphans = find_orphaned_worktrees(&fx.repos_root, &active);
    assert_eq!(
        orphans,
        vec![dead],
        "only the unclaimed worktree is an orphan"
    );
}

/// Item 1 (#1845): async test that genuinely exercises the Phase 2 fresh-store
/// snapshot path in `prune_orphaned_worktrees`.
///
/// Why: the existing sync test at `prune_orphaned_worktrees_fresh_active_set_blocks_deletion`
/// only calls `find_orphaned_worktrees` directly, giving zero executed coverage of
/// the Phase 2 `fresh_in_use` snapshot logic in the async method. This test goes
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
    // #4207: a REAL `git worktree add`, so Phase 1 genuinely finds it as a
    // candidate — a `mkdir` is no longer enumerated and made this vacuous.
    let fx = GitWorktreeFixture::new();
    let session_id = super::super::record::ManagedSessionId::new();
    let wt_path = fx.add_worktree(&session_id.to_string());

    // Create the SessionManager and insert a live record for the worktree.
    let mgr = super::super::manager::SessionManager::new(store_dir.path(), Arc::new(NoopDriver))
        .await
        .expect("SessionManager::new");

    let canonical_wt = wt_path.clone();
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
        .prune_orphaned_worktrees(&fx.repos_root, &[], false, DirtyWorktreePolicy::Skip)
        .await
        .expect("prune must not error");

    assert!(
        outcome.removed.is_empty(),
        "worktree backed by a live store record must NOT be removed; got: {:?}",
        outcome.removed
    );
    assert!(wt_path.exists(), "worktree dir must survive the prune");
}

// ── #4207 slice 1: discovery is derived from git, not from location ─────
//
// The five location shapes this walk used to probe (`.worktrees/`,
// `.base/.worktrees/`, `.claude/worktrees/`, `.base/.claude/worktrees/`, and
// `.base/.worktrees/<id>/.claude/worktrees/`) each arrived as a bug report
// about a location the previous list had missed. There is nothing left to
// enumerate per-shape: a worktree is discovered because git registered it.
// The two tests below replace all seven shape-coverage tests.

/// THE #4207 regression test at the reclaim layer: a registered worktree at a
/// location NONE of the five hard-coded shapes covered is discovered.
///
/// Why: `<repo>/agents/scratch/wt-1` matches no shape the removed walk probed,
/// so reverting `find_orphaned_worktrees` to that walk makes this fail with an
/// empty candidate list. Location is no longer a variable the code reasons
/// about, which is the whole point of the slice.
#[test]
fn find_orphaned_worktrees_discovers_worktree_at_unwalked_location() {
    let fx = GitWorktreeFixture::new();
    let parked = fx.add_worktree_at(&fx.repo.join("agents").join("scratch"), "wt-1");
    let empty: std::collections::HashSet<std::path::PathBuf> = std::collections::HashSet::new();
    let orphans = find_orphaned_worktrees(&fx.repos_root, &empty);
    assert!(
        orphans.contains(&parked),
        "a registered worktree must be found wherever it lives; got {orphans:?}"
    );
}

/// A plain directory parked in a worktree-shaped location is NOT a worktree and
/// must never be proposed for deletion.
///
/// Why: the old walk collected any leaf directory under a `.worktrees/` parent,
/// so a stray `mkdir` was indistinguishable from a checkout git created. This
/// is the deliberate narrowing derive-not-walk buys, and it is the safer
/// direction: a directory git does not own is not trusty-mpm's to reclaim.
#[test]
fn find_orphaned_worktrees_ignores_plain_directory() {
    let fx = GitWorktreeFixture::new();
    let fake = fx.repo.join(".worktrees").join("just-a-mkdir");
    std::fs::create_dir_all(&fake).expect("mkdir");
    let empty: std::collections::HashSet<std::path::PathBuf> = std::collections::HashSet::new();
    let orphans = find_orphaned_worktrees(&fx.repos_root, &empty);
    assert!(
        !orphans.contains(&fake),
        "a plain directory is not a registered worktree; got {orphans:?}"
    );
}

/// A Claude-Code-native agent worktree is discovered but NEVER auto-deleted:
/// it never carries trusty-mpm's ownership sentinel, so the #3649 gate reports
/// it as owner-unknown (#3971, kept location-agnostic by #4207).
#[tokio::test]
async fn prune_orphaned_worktrees_never_deletes_claude_native_worktree() {
    let store_dir = tempfile::tempdir().unwrap();
    let mgr = SessionManager::new(
        store_dir.path(),
        crate::session_manager::tests::FakeTmuxDriver::new(),
    )
    .await
    .expect("manager");

    let fx = GitWorktreeFixture::new();
    // Deliberately NO sentinel written — Claude Code's native
    // `isolation: "worktree"` mechanism never writes trusty-mpm's.
    let claude_wt = fx.add_worktree_at(
        &fx.repo.join(".claude").join("worktrees"),
        "agent-00112233445566778899aabbccddeeff00112233",
    );

    let outcome = mgr
        .prune_orphaned_worktrees(&fx.repos_root, &[], false, DirtyWorktreePolicy::Skip)
        .await
        .expect("prune must not error");

    assert!(
        outcome.removed.is_empty(),
        "a Claude-Code-native worktree must never be auto-deleted; got {:?}",
        outcome.removed
    );
    assert!(
        outcome.owner_unknown.contains(&claude_wt),
        "expected {claude_wt:?} to be reported as owner-unknown; got {:?}",
        outcome.owner_unknown
    );
    assert!(claude_wt.exists(), "worktree dir must survive the prune");
}

/// A candidate whose ownership sentinel is absent (no `.trusty-mpm-worktree`
/// file at all — the pre-#3649/legacy shape) is NEVER auto-deleted, and is
/// counted in [`OrphanSweepOutcome::owner_unknown`] (#3649 item 4b).
#[tokio::test]
async fn prune_orphaned_worktrees_skips_owner_unknown() {
    let store_dir = tempfile::tempdir().unwrap();
    let mgr = SessionManager::new(
        store_dir.path(),
        crate::session_manager::tests::FakeTmuxDriver::new(),
    )
    .await
    .expect("manager");

    let fx = GitWorktreeFixture::new();
    // Deliberately NO sentinel file written — simulates a legacy worktree.
    let wt = fx.add_worktree("legacy-no-sentinel");

    let outcome = mgr
        .prune_orphaned_worktrees(&fx.repos_root, &[], false, DirtyWorktreePolicy::Skip)
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
    let mgr = SessionManager::new(
        store_dir.path(),
        crate::session_manager::tests::FakeTmuxDriver::new(),
    )
    .await
    .expect("manager");

    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("ownerless-gone");
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
        .prune_orphaned_worktrees(&fx.repos_root, &[], false, DirtyWorktreePolicy::Skip)
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

/// THE OTHER DIRECTION of the #4224 containment boundary: a genuine tm-owned
/// husk at a location NO hard-coded shape covers is still reclaimed, end to end.
///
/// Why: the #4224 review's HIGH is that git-native enumeration widened the
/// auto-deletion surface, and the fix narrows it back to "inside the managed
/// project". A narrowing is only correct if it costs no reclaim capability, and
/// the cheapest way to satisfy a containment test is to stop deleting
/// altogether — which would silently restore the very leak #4207 exists to fix
/// while every exclusion test still passed. So this asserts the positive:
/// `<repo>/agents/scratch/husk` matches none of the five removed shapes
/// (`.worktrees/`, `.base/.worktrees/`, `.claude/worktrees/`,
/// `.base/.claude/worktrees/`, `.base/.worktrees/<id>/.claude/worktrees/`), and
/// it is REMOVED FROM DISK.
///
/// This runs the whole gauntlet, not just enumeration: the #3649 ownership
/// gate (aged sentinel, owner that never resolves), the `git worktree list`
/// cross-check, and the #4091/#4118 dirty gate.
/// Test: this function IS the test.
#[tokio::test]
async fn prune_orphaned_worktrees_reclaims_owned_husk_at_an_unwalked_location() {
    let store_dir = tempfile::tempdir().unwrap();
    let mgr = SessionManager::new(
        store_dir.path(),
        crate::session_manager::tests::FakeTmuxDriver::new(),
    )
    .await
    .expect("manager");

    let fx = GitWorktreeFixture::new();
    let husk = fx.add_worktree_at(&fx.repo.join("agents").join("scratch"), "husk");
    assert!(
        !husk.starts_with(fx.repo.join(".worktrees"))
            && !husk.starts_with(fx.repo.join(".claude"))
            && !husk.starts_with(fx.repo.join(".base")),
        "test invariant: the husk must sit at a location none of the five \
         removed shapes covered, or this test cannot prove the #4207 fix \
         survived the #4224 narrowing; got {}",
        husk.display()
    );
    GitWorktreeFixture::stamp_reclaimable_sentinel(&husk);

    let outcome = mgr
        .prune_orphaned_worktrees(&fx.repos_root, &[], false, DirtyWorktreePolicy::Skip)
        .await
        .expect("prune must not error");

    assert!(
        outcome.removed.iter().any(|p| p == &husk),
        "a sentinel-bearing, ownerless worktree inside the project must be \
         reclaimed wherever in the project it lives; got removed={:?} \
         owner_unknown={:?} skipped_dirty={:?}",
        outcome.removed,
        outcome.owner_unknown,
        outcome.skipped_dirty
    );
    assert!(!husk.exists(), "the reclaimed husk must be gone from disk");
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
    let mgr = SessionManager::new(
        store_dir.path(),
        crate::session_manager::tests::FakeTmuxDriver::new(),
    )
    .await
    .expect("manager");

    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("mid-creation-race");
    let not_yet_persisted_owner = ManagedSessionId::new();
    std::fs::write(
        wt.join(crate::session_manager::decommission::WORKTREE_SENTINEL_FILE),
        crate::session_manager::worktree_ownership::sentinel_payload_bytes(not_yet_persisted_owner),
    )
    .expect("write sentinel");

    // The candidate IS discovered — otherwise this test would pass vacuously.
    let empty: std::collections::HashSet<std::path::PathBuf> = std::collections::HashSet::new();
    assert!(
        find_orphaned_worktrees(&fx.repos_root, &empty).contains(&wt),
        "test invariant: the worktree must reach the ownership gate"
    );

    let outcome = mgr
        .prune_orphaned_worktrees(&fx.repos_root, &[], false, DirtyWorktreePolicy::Skip)
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
    let mgr = SessionManager::new(
        store_dir.path(),
        crate::session_manager::tests::FakeTmuxDriver::new(),
    )
    .await
    .expect("manager");

    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("live-owner-elsewhere");

    // Register the owner as a LIVE record whose workspace_path points
    // somewhere else entirely — so `wt` is NOT in `in_use_workspace_paths`
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

    // The candidate IS discovered — otherwise this test would pass vacuously.
    let empty: std::collections::HashSet<std::path::PathBuf> = std::collections::HashSet::new();
    assert!(
        find_orphaned_worktrees(&fx.repos_root, &empty).contains(&wt),
        "test invariant: the worktree must reach the ownership gate"
    );

    let outcome = mgr
        .prune_orphaned_worktrees(&fx.repos_root, &[], false, DirtyWorktreePolicy::Skip)
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

/// THE #4207 regression test for the ownership half of the slice: a worktree
/// physically inside `<repo>/.base/.worktrees/` but REGISTERED to the parent
/// repo `<repo>` must be recognised as a real worktree.
///
/// Why: this is the exact state fourteen worktrees on the dogfood machine were
/// in on 2026-07-27, and it made every one of them STRUCTURALLY unreclaimable.
/// The old rule derived the owning checkout from the candidate's GRANDPARENT,
/// which here is `<repo>/.base` — a genuine but DIFFERENT repository that
/// correctly disowns the path. `git_worktree_list_agrees` therefore returned
/// `false` and the reclaim path skipped conservatively, forever, no matter how
/// clean or how ownerless the worktree was.
///
/// REVERT-PROOF: restore
/// `let repo_root = candidate.parent().and_then(|p| p.parent())` and aim the
/// `worktree list` at it, and this test fails — `.base` is a real repository
/// whose registry genuinely does not contain the candidate, so the old code
/// takes its success path and returns `false`. The two tests above cannot
/// catch it: in both, the grandparent happens to be the owning repository.
#[test]
fn git_worktree_list_agrees_true_for_worktree_registered_to_parent_repo() {
    let fx = GitWorktreeFixture::new();
    // Create the `.base` bare clone first, so the candidate's grandparent is a
    // real repository — one that does NOT own the candidate.
    fx.add_base_clone_worktree("owned-by-base");
    let parent_registered = fx.add_worktree_at(
        &fx.repo.join(".base").join(".worktrees"),
        "registered-to-parent",
    );

    let grandparent = parent_registered
        .parent()
        .and_then(|p| p.parent())
        .expect("grandparent");
    assert!(
        crate::session_manager::worktree_registry::registry_root_for(grandparent).is_some(),
        "test invariant: the grandparent must itself be a repository, or the old \
         rule would fall through its best-effort `true` and pass by accident"
    );

    assert!(
        git_worktree_list_agrees(&parent_registered),
        "a worktree registered to the PARENT repo but living under .base must be \
         recognised — asking the grandparent (.base) disowns it"
    );
}

/// The #1845 F3 canonicalize-fallback streak must escalate to `error!` at
/// exactly [`CANONICALIZE_FAILURE_STREAK_THRESHOLD`] consecutive failures,
/// not before (#3715 item 3).
///
/// Why: this is pure counter logic — no daemon, no timing, no filesystem —
/// exercised directly against [`CanonicalizeFailureStreaks`] so the test is
/// deterministic and independent of the real ~60s sweep cadence.
/// What: calls `record_failure` repeatedly for the same path and asserts the
/// returned streak length increments 1..=N, crossing the threshold on the
/// Nth call.
#[test]
fn canonicalize_streak_escalates_at_threshold() {
    let mut streaks = CanonicalizeFailureStreaks::default();
    let path = std::path::Path::new("/some/vanished/workspace");

    for expected in 1..CANONICALIZE_FAILURE_STREAK_THRESHOLD {
        let streak = streaks.record_failure(path);
        assert_eq!(streak, expected, "streak must increment by 1 per failure");
        assert!(
            streak < CANONICALIZE_FAILURE_STREAK_THRESHOLD,
            "must not have reached the escalation threshold yet"
        );
    }

    let final_streak = streaks.record_failure(path);
    assert_eq!(final_streak, CANONICALIZE_FAILURE_STREAK_THRESHOLD);
    assert!(
        final_streak >= CANONICALIZE_FAILURE_STREAK_THRESHOLD,
        "the Nth consecutive failure must cross the escalation threshold"
    );
}

/// A single successful canonicalize must fully reset the streak for that
/// path — a subsequent failure starts back at 1, not N+1 (#3715 item 3).
///
/// What: drives a path partway toward the threshold, calls `record_success`,
/// then asserts the next `record_failure` call returns 1 again. Also checks
/// that a DIFFERENT path's streak is unaffected by another path's reset.
#[test]
fn canonicalize_streak_resets_on_success() {
    let mut streaks = CanonicalizeFailureStreaks::default();
    let path_a = std::path::Path::new("/workspace/a");
    let path_b = std::path::Path::new("/workspace/b");

    streaks.record_failure(path_a);
    streaks.record_failure(path_a);
    streaks.record_failure(path_a);
    streaks.record_failure(path_b);
    streaks.record_failure(path_b);

    streaks.record_success(path_a);
    assert_eq!(
        streaks.record_failure(path_a),
        1,
        "streak must restart at 1 after a success resets it"
    );
    assert_eq!(
        streaks.record_failure(path_b),
        3,
        "an unrelated path's streak must be untouched by another path's reset"
    );
}

/// A path whose session leaves the active set (decommissioned, deleted, or
/// `workspace_path` changed) must have its streak evicted, not linger forever
/// — otherwise `CanonicalizeFailureStreaks` grows unbounded across the
/// daemon's lifetime (#3715 finding 2).
///
/// What: builds up failure streaks for two paths, then calls `retain_active`
/// with an active set containing only ONE of them; asserts the evicted
/// path's streak restarts at 1 on its next failure (proving the old count
/// was actually removed, not just no longer escalating) while the retained
/// path's streak is untouched.
#[test]
fn canonicalize_streak_evicts_paths_no_longer_active() {
    let mut streaks = CanonicalizeFailureStreaks::default();
    let still_active = std::path::PathBuf::from("/workspace/still-active");
    let left_active = std::path::PathBuf::from("/workspace/decommissioned");

    streaks.record_failure(&still_active);
    streaks.record_failure(&still_active);
    streaks.record_failure(&left_active);
    streaks.record_failure(&left_active);
    streaks.record_failure(&left_active);

    let active: std::collections::HashSet<std::path::PathBuf> =
        [still_active.clone()].into_iter().collect();
    streaks.retain_active(&active);

    assert_eq!(
        streaks.record_failure(&left_active),
        1,
        "a path evicted by retain_active must restart its streak at 1, proving \
         the old count was removed rather than merely capped"
    );
    assert_eq!(
        streaks.record_failure(&still_active),
        3,
        "a path still present in the active set must be untouched by eviction"
    );
}

// ── #4091: the dirty-tree gate, wired into the reclaim path ──────────────
//
// Every test below builds a REAL git checkout + worktree in a throwaway
// `tempfile::tempdir()` via `GitWorktreeFixture`, stamps it with an aged
// ownership sentinel so the #3649 owner gate PASSES (otherwise the candidate
// would be spared for the wrong reason and prove nothing), and then asserts
// what the #4091 gate does with it. No live worktree is ever touched.

use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;

/// Build a manager plus a fixture whose worktree `name` is fully reclaimable
/// as far as the #3649 ownership gate is concerned — so the ONLY thing that
/// can still spare it is the #4091 dirty gate.
async fn reclaimable_fixture(
    name: &str,
) -> (
    SessionManager,
    tempfile::TempDir,
    GitWorktreeFixture,
    std::path::PathBuf,
) {
    let store_dir = tempfile::tempdir().unwrap();
    let mgr = SessionManager::new(
        store_dir.path(),
        crate::session_manager::tests::FakeTmuxDriver::new(),
    )
    .await
    .expect("manager");
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree(name);
    GitWorktreeFixture::stamp_reclaimable_sentinel(&wt);
    (mgr, store_dir, fx, wt)
}

/// The control case: a genuinely clean, fully-pushed, owner-known stale
/// worktree IS still reclaimed. Without this the guard could "pass" every
/// other test simply by never deleting anything, which would be a silent leak
/// rather than a fix.
#[tokio::test]
async fn prune_orphaned_worktrees_reclaims_clean_pushed_worktree() {
    let (mgr, _store, fx, wt) = reclaimable_fixture("clean-pushed").await;

    let outcome = mgr
        .prune_orphaned_worktrees(&fx.repos_root, &[], false, DirtyWorktreePolicy::Skip)
        .await
        .expect("prune must not error");

    assert!(
        outcome.skipped_dirty.is_empty(),
        "a clean worktree must not be reported dirty; got {:?}",
        outcome.skipped_dirty
    );
    assert!(
        outcome.removed.iter().any(|p| p == &wt),
        "a clean, fully-pushed, owner-known stale worktree must still be reclaimed; got {:?}",
        outcome.removed
    );
    assert!(
        !wt.exists(),
        "the reclaimed worktree must be gone from disk"
    );
}

/// #4118 TOCTOU: a candidate that goes dirty AFTER the Phase 1.5 scan but
/// BEFORE its own removal must not be removed.
///
/// Why: the scan classifies every candidate up front and the removal loop runs
/// afterwards, so across a ~95-candidate / ~730 GB sweep the gap between
/// "certified clean" and "deleted" is the whole sweep duration — minutes, not
/// the sub-millisecond window `prune_orphaned_worktrees` documents for the
/// active-session check. Re-checking adjacent to the deletion is what closes it.
///
/// Both candidates scan CLEAN — asserted below, so the test cannot pass because
/// `zzz-victim` was dirty from the start. `find_orphaned_worktrees` sorts its
/// candidates, so `aaa-remover` is always processed first. A watcher waits for
/// `aaa-remover`'s directory to disappear and only then writes an untracked
/// file into `zzz-victim`: a transition that is impossible during Phase 1.5 and
/// guaranteed to land before `zzz-victim`'s own removal.
///
/// The window is not a gamble. `remove_session_worktree` runs
/// `git worktree prune` and `git branch -D` AFTER the directory is gone — two
/// process spawns, tens of milliseconds — while the watcher polls at 200 µs and
/// the loop needs only a `canonicalize` before re-checking. The watcher also
/// reports whether it fired, so a lost race FAILS the test rather than passing
/// it vacuously.
///
/// Without the pre-removal re-check, `zzz-victim` is still on the Phase 1.5
/// `reclaimable` list and is handed to `remove_session_worktree` regardless.
#[tokio::test]
async fn prune_orphaned_worktrees_rechecks_dirt_immediately_before_removal() {
    let (mgr, _store, fx, remover) = reclaimable_fixture("aaa-remover").await;
    let victim = fx.add_worktree("zzz-victim");
    GitWorktreeFixture::stamp_reclaimable_sentinel(&victim);

    // Both are clean and reclaimable RIGHT NOW — assert it, so the test cannot
    // pass because the victim was dirty from the start.
    let preview = mgr
        .prune_orphaned_worktrees(&fx.repos_root, &[], true, DirtyWorktreePolicy::Skip)
        .await
        .expect("dry run must not error");
    assert!(
        preview.skipped_dirty.is_empty() && preview.removed.len() == 2,
        "premise broken: both candidates must scan CLEAN; got removed={:?} skipped={:?}",
        preview.removed,
        preview.skipped_dirty
    );

    let fired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let watcher = {
        let (remover, victim, fired) = (remover.clone(), victim.clone(), fired.clone());
        std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while remover.exists() {
                if std::time::Instant::now() > deadline {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_micros(200));
            }
            std::fs::write(victim.join("notes.md"), "written mid-sweep\n").unwrap();
            fired.store(true, std::sync::atomic::Ordering::SeqCst);
        })
    };

    let outcome = mgr
        .prune_orphaned_worktrees(&fx.repos_root, &[], false, DirtyWorktreePolicy::Skip)
        .await
        .expect("prune must not error");
    watcher.join().expect("watcher thread");

    assert!(
        fired.load(std::sync::atomic::Ordering::SeqCst),
        "the watcher never observed the first removal, so nothing was tested"
    );
    assert!(
        outcome.removed.iter().any(|p| p == &remover),
        "the first candidate was clean throughout and must still be reclaimed; got {:?}",
        outcome.removed
    );
    assert!(
        !outcome.removed.iter().any(|p| p == &victim),
        "a candidate dirtied AFTER the scan must NOT be removed; got {:?}",
        outcome.removed
    );
    assert!(
        victim.join("notes.md").exists(),
        "the work written mid-sweep must still be on disk"
    );
    assert!(
        outcome.skipped_dirty.iter().any(|d| d.path == victim),
        "the mid-sweep skip must be REPORTED, not merely logged; got {:?}",
        outcome.skipped_dirty
    );
}

/// #4118: the age-based auto-reaper reaches the SAME destructive sequence
/// (`worktree remove --force` → `remove_dir_all` → `branch -D`) without going
/// through the orphan sweep, and it fires automatically on every daemon GC
/// tick. Guarding one sweep while its sibling deletes freely is a half-measure.
///
/// There is deliberately no discard opt-in on this path: an automatic reaper
/// must never be able to destroy work, whatever an operator asks for elsewhere.
#[tokio::test]
async fn reap_aged_ephemeral_spares_a_worktree_holding_unsaved_work() {
    let store_dir = tempfile::tempdir().unwrap();
    let mgr = SessionManager::new(
        store_dir.path(),
        crate::session_manager::tests::FakeTmuxDriver::new(),
    )
    .await
    .expect("manager");
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("aged-ephemeral");
    std::fs::write(wt.join("rescued.rs"), "// never committed\n").unwrap();

    let id = crate::session_manager::record::ManagedSessionId::new();
    let record = crate::session_manager::record::SessionRecord {
        id,
        tmux_name: format!("tmpm-aged-{id}"),
        cwd: fx.repo.clone(),
        task: "aged".into(),
        state: crate::session_manager::record::ManagedSessionState::Active,
        created_at: chrono::Utc::now() - chrono::Duration::hours(2),
        last_activity_at: None,
        workspace_path: Some(wt.clone()),
        repo_url: None,
        branch: None,
        pending_decision: None,
        proposed_default: None,
        correlation: Default::default(),
        runtime: Default::default(),
        ephemeral: true,
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
    mgr.store.write().await.upsert(record).await.expect("seed");

    let reaped = mgr
        .reap_aged_ephemeral(chrono::Duration::hours(1))
        .await
        .expect("reap must not error");

    assert_eq!(
        reaped, 0,
        "an aged ephemeral holding uncommitted work must not be auto-reaped"
    );
    assert!(
        wt.join("rescued.rs").exists(),
        "the uncommitted work must still be on disk"
    );
    assert_eq!(
        mgr.get(&id).await.unwrap().state,
        crate::session_manager::record::ManagedSessionState::Active,
        "the record must be left for an operator, not silently decommissioned"
    );
}

/// A worktree with MODIFIED TRACKED files must not be removed, and the skip
/// must be visible in the return value — not merely logged.
#[tokio::test]
async fn prune_orphaned_worktrees_skips_modified_tracked_file() {
    let (mgr, _store, fx, wt) = reclaimable_fixture("modified").await;
    std::fs::write(wt.join("README.md"), "uncommitted edit\n").unwrap();

    let outcome = mgr
        .prune_orphaned_worktrees(&fx.repos_root, &[], false, DirtyWorktreePolicy::Skip)
        .await
        .expect("prune must not error");

    assert!(
        outcome.removed.is_empty(),
        "a worktree with modified tracked files must NOT be removed; got {:?}",
        outcome.removed
    );
    let dirt = outcome
        .skipped_dirty
        .iter()
        .find(|d| d.path == wt)
        .expect("the skip must be reported in skipped_dirty, not just logged");
    assert_eq!(dirt.dirty_files, 1, "reason: {}", dirt.reason);
    assert!(wt.exists(), "the dirty worktree must survive on disk");
}

/// A worktree holding ONLY untracked (non-ignored) files must not be removed.
/// Untracked work exists nowhere else at all.
#[tokio::test]
async fn prune_orphaned_worktrees_skips_untracked_file() {
    let (mgr, _store, fx, wt) = reclaimable_fixture("untracked").await;
    std::fs::write(wt.join("scratch-notes.md"), "never added to git\n").unwrap();

    let outcome = mgr
        .prune_orphaned_worktrees(&fx.repos_root, &[], false, DirtyWorktreePolicy::Skip)
        .await
        .expect("prune must not error");

    assert!(
        outcome.removed.is_empty(),
        "a worktree with untracked files must NOT be removed; got {:?}",
        outcome.removed
    );
    assert!(
        outcome.skipped_dirty.iter().any(|d| d.path == wt),
        "the untracked-file skip must be reported; got {:?}",
        outcome.skipped_dirty
    );
    assert!(wt.exists(), "the dirty worktree must survive on disk");
}

/// A worktree whose work is COMMITTED but never pushed must not be removed.
/// This is the case that looks safe and is not: `remove_session_worktree`
/// deletes the `session/<leaf>` branch, taking the commit's last reachable
/// ref with it.
#[tokio::test]
async fn prune_orphaned_worktrees_skips_unpushed_commit() {
    let (mgr, _store, fx, wt) = reclaimable_fixture("unpushed").await;
    GitWorktreeFixture::commit_unpushed(&wt);

    let outcome = mgr
        .prune_orphaned_worktrees(&fx.repos_root, &[], false, DirtyWorktreePolicy::Skip)
        .await
        .expect("prune must not error");

    assert!(
        outcome.removed.is_empty(),
        "a worktree with unpushed commits must NOT be removed; got {:?}",
        outcome.removed
    );
    let dirt = outcome
        .skipped_dirty
        .iter()
        .find(|d| d.path == wt)
        .expect("the unpushed-commit skip must be reported");
    assert_eq!(dirt.unpushed_commits, 1, "reason: {}", dirt.reason);
    assert!(wt.exists(), "the worktree must survive on disk");
}

/// FAIL-SAFE, end to end: when the dirty check itself cannot complete, the
/// candidate is SKIPPED, never removed. An error on the check must never be a
/// green light to delete — that would invert the entire guard.
///
/// The failure is injected by replacing the worktree's git index with a
/// directory, which makes `git status` exit 128 while leaving the worktree
/// registration intact (so the #3649 `git worktree list` cross-check still
/// agrees and the candidate genuinely reaches the #4091 gate).
#[tokio::test]
async fn prune_orphaned_worktrees_skips_when_dirty_check_errors() {
    let (mgr, _store, fx, wt) = reclaimable_fixture("broken-index").await;
    let index = fx
        .repo
        .join(".git")
        .join("worktrees")
        .join("broken-index")
        .join("index");
    std::fs::remove_file(&index).expect("remove worktree index");
    std::fs::create_dir(&index).expect("replace index with a directory");

    let outcome = mgr
        .prune_orphaned_worktrees(&fx.repos_root, &[], false, DirtyWorktreePolicy::Skip)
        .await
        .expect("prune must not error");

    assert!(
        outcome.removed.is_empty(),
        "a candidate whose dirty-check errored must NOT be removed; got {:?}",
        outcome.removed
    );
    let dirt = outcome
        .skipped_dirty
        .iter()
        .find(|d| d.path == wt)
        .expect("an errored dirty-check must be reported as a dirty skip");
    assert!(
        dirt.reason.contains("dirty-check failed"),
        "unexpected reason: {}",
        dirt.reason
    );
    assert!(
        wt.exists(),
        "the unexaminable worktree must survive on disk"
    );
}

/// The explicit override DOES remove a dirty worktree — the guard is a
/// default, not a wall. If this ever stops working the override is dead code
/// and operators will reach for `rm -rf` instead.
#[tokio::test]
async fn prune_orphaned_worktrees_force_discards_dirty() {
    let (mgr, _store, fx, wt) = reclaimable_fixture("force-discard").await;
    std::fs::write(wt.join("README.md"), "uncommitted edit\n").unwrap();

    let outcome = mgr
        .prune_orphaned_worktrees(
            &fx.repos_root,
            &[],
            false,
            DirtyWorktreePolicy::ForceDiscard,
        )
        .await
        .expect("prune must not error");

    assert!(
        outcome.removed.iter().any(|p| p == &wt),
        "the explicit force-discard opt-in must remove a dirty worktree; got {:?}",
        outcome.removed
    );
    assert!(
        outcome.skipped_dirty.is_empty(),
        "force-discard reports nothing as skipped; got {:?}",
        outcome.skipped_dirty
    );
    assert!(!wt.exists(), "the force-discarded worktree must be gone");
}

/// REGRESSION (#4091): the DEFAULT policy — the one the `/tm-session-pause`
/// path and the periodic daemon orphan-GC both use, neither of which has any
/// argument that could change it — cannot delete dirty work.
///
/// Asserting `DirtyWorktreePolicy::default()` here (rather than naming `Skip`)
/// is deliberate: it fails if anyone ever flips the default, which is the only
/// way the pause path could silently gain destructive behaviour.
#[tokio::test]
async fn prune_orphaned_worktrees_default_policy_cannot_delete_dirty_work() {
    let (mgr, _store, fx, wt) = reclaimable_fixture("default-policy").await;
    std::fs::write(wt.join("precious.txt"), "hours of work\n").unwrap();

    let outcome = mgr
        .prune_orphaned_worktrees(&fx.repos_root, &[], false, DirtyWorktreePolicy::default())
        .await
        .expect("prune must not error");

    assert!(
        outcome.removed.is_empty(),
        "the DEFAULT prune policy must never delete dirty work; got {:?}",
        outcome.removed
    );
    assert!(
        wt.exists(),
        "the dirty worktree must survive the default sweep"
    );
    assert_eq!(
        std::fs::read_to_string(wt.join("precious.txt")).unwrap(),
        "hours of work\n",
        "the untracked file's contents must be intact"
    );
}

/// A `dry_run` preview must report the same dirty skip a real run would, so an
/// operator previewing a 95-worktree sweep sees exactly what will be spared.
#[tokio::test]
async fn prune_orphaned_worktrees_dry_run_reports_dirty_skip() {
    let (mgr, _store, fx, wt) = reclaimable_fixture("dry-run-dirty").await;
    std::fs::write(wt.join("README.md"), "uncommitted edit\n").unwrap();

    let outcome = mgr
        .prune_orphaned_worktrees(&fx.repos_root, &[], true, DirtyWorktreePolicy::Skip)
        .await
        .expect("prune must not error");

    assert!(
        outcome.removed.is_empty(),
        "dry-run must not list a dirty worktree as would-remove; got {:?}",
        outcome.removed
    );
    assert!(
        outcome.skipped_dirty.iter().any(|d| d.path == wt),
        "dry-run must report the dirty skip; got {:?}",
        outcome.skipped_dirty
    );
    assert!(wt.exists(), "dry-run must not touch anything");
}

/// #4288 M3: the Phase 2 `fresh_in_use` snapshot spares a worktree whose record
/// the CALLER's (stale) snapshot missed — the read that nothing else covers.
///
/// Why: `prune_orphaned_worktrees` protects a live worktree twice — once via
/// the caller-supplied set, and again via the Phase 2 re-read of the store
/// taken immediately before deletion. The Phase 2 read is the LAST thing
/// between a reclaimable candidate and `remove_session_worktree`, and until
/// this test it had ZERO executed coverage: the one test that names it
/// (`prune_orphaned_worktrees_store_snapshot_blocks_deletion`) builds its
/// worktree with NO ownership sentinel, so the #3649 gate classifies it
/// owner-unknown and skips it BEFORE Phase 2 ever runs — as that test's own
/// comment states. Filtering Phase 2 by `state` therefore broke nothing in the
/// entire suite, which is exactly the invisible-loss shape this PR exists to
/// close.
///
/// What: an EMPTY caller set models the realistic TOCTOU case Phase 2 was
/// built for (#1845 item 9) — a snapshot taken before the record existed, or
/// stale by the time the sweep reaches the deletion loop. Phase 1.5 still
/// votes "reclaim" (the sentinel names a never-registered owner, unrelated to
/// the record below), so Phase 2 is the ONLY thing that can spare it.
///
/// Non-vacuity: the CONTROL is the dry-run of the SAME sweep with the SAME
/// record present. Dry-run returns before Phase 2, so it still lists the
/// worktree as reclaimable — the delta between that and the real run below IS
/// the Phase 2 protection, and it cannot be faked by a fixture that was simply
/// never reclaimable.
/// Test: this function IS the test.
#[tokio::test]
async fn phase2_fresh_snapshot_spares_a_record_the_caller_set_missed() {
    use crate::session_manager::record::{ManagedSessionState, SessionRecord};

    let (mgr, _store, fx, wt) = reclaimable_fixture("stale-caller-set").await;

    // Register the worktree to a record and drive it to `Stopped` — the shape
    // observed on a session that was in fact still running.
    let record = mgr
        .create_with_id(
            ManagedSessionId::new(),
            "pinned by #4288".into(),
            Some(wt.clone()),
            None,
            Some(wt.clone()),
            None,
            None,
            crate::runtime::RuntimeKind::default(),
            false,
            false,
        )
        .await
        .expect("seed session record");
    let stopped = SessionRecord {
        state: ManagedSessionState::Stopped,
        ..record
    };
    mgr.store
        .write()
        .await
        .upsert(stopped)
        .await
        .expect("persist the Stopped record");

    // CONTROL: dry-run returns BEFORE Phase 2, so the worktree is still a
    // reclaimable candidate even with the record in the store. This proves
    // every earlier gate votes "reclaim" and that only Phase 2 can save it.
    let control = mgr
        .prune_orphaned_worktrees(&fx.repos_root, &[], true, DirtyWorktreePolicy::Skip)
        .await
        .expect("control sweep must not error");
    assert!(
        control.removed.contains(&wt),
        "CONTROL: {} must reach Phase 2 as a reclaimable candidate, otherwise \
         this test proves nothing; removed={:?} owner_unknown={:?} skipped_dirty={:?}",
        wt.display(),
        control.removed,
        control.owner_unknown,
        control.skipped_dirty
    );

    // THE PIN: the real (deleting) sweep, caller set still empty. Only the
    // Phase 2 re-read of the store stands between this worktree and deletion.
    let outcome = mgr
        .prune_orphaned_worktrees(&fx.repos_root, &[], false, DirtyWorktreePolicy::Skip)
        .await
        .expect("prune must not error");

    assert!(
        !outcome.removed.contains(&wt),
        "the Phase 2 fresh snapshot must spare a worktree backed by a store \
         record the caller's set missed; {} appeared in the removed set {:?}",
        wt.display(),
        outcome.removed
    );
    assert!(
        wt.exists(),
        "{} must still exist on disk after the real sweep",
        wt.display()
    );
}
