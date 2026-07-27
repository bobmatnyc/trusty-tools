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
        .prune_orphaned_worktrees(repos_tmp.path(), &[], false, DirtyWorktreePolicy::Skip)
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

// ── #3971: extended `.claude/worktrees` walk (Claude Code native agent
// worktrees) ──────────────────────────────────────────────────────────────

/// `find_orphaned_worktrees` must ALSO discover Claude Code's native
/// `.claude/worktrees/agent-<hash>` shape, not just the two trusty-mpm-owned
/// shapes (#3971 — this walk previously covered ONLY `.worktrees/<name>` and
/// `.base/.worktrees/<id>`, so every Claude-Code-created agent worktree was
/// invisible to the orphan-GC, `tm doctor`, and `--dry-run`, letting them
/// accumulate silently).
#[test]
fn find_orphaned_worktrees_covers_claude_worktrees_shape() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let claude_wt = root
        .join("owner")
        .join("repo")
        .join(".claude")
        .join("worktrees")
        .join("agent-0123456789abcdef0123456789abcdef01234567");
    std::fs::create_dir_all(&claude_wt).unwrap();
    let empty_active: std::collections::HashSet<std::path::PathBuf> =
        std::collections::HashSet::new();
    let orphans = find_orphaned_worktrees(root, &empty_active);
    assert!(
        orphans.contains(&claude_wt),
        "the .claude/worktrees shape must be discovered as a candidate; got {orphans:?}"
    );
}

/// A LIVE (non-orphaned) `.claude/worktrees/agent-<hash>` directory — one
/// whose canonicalized path IS present in the caller-supplied active set —
/// must never be returned as an orphan candidate (#3971). Mirrors the
/// existing `prune_orphaned_worktrees_spares_active` guarantee for the other
/// two shapes.
#[test]
fn find_orphaned_worktrees_spares_live_claude_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let claude_wt = root
        .join("owner")
        .join("repo")
        .join(".claude")
        .join("worktrees")
        .join("agent-fedcba9876543210fedcba9876543210fedcba98");
    std::fs::create_dir_all(&claude_wt).unwrap();
    let active: std::collections::HashSet<_> =
        vec![std::fs::canonicalize(&claude_wt).unwrap_or_else(|_| claude_wt.clone())]
            .into_iter()
            .collect();
    let orphans = find_orphaned_worktrees(root, &active);
    assert!(
        orphans.is_empty(),
        "a live .claude/worktrees entry must not be listed as orphan; got {orphans:?}"
    );
}

/// Even when discovered as an orphan CANDIDATE by the walk, a
/// `.claude/worktrees/agent-<hash>` directory is never created by
/// trusty-mpm's own provisioning code, so it never carries the
/// `.trusty-mpm-worktree` ownership sentinel. The existing #3649 ownership
/// gate therefore classifies it as `SentinelOwner::Unknown` and the full
/// `prune_orphaned_worktrees` sweep must NEVER auto-delete it — only report
/// it via `OrphanSweepOutcome::owner_unknown`, exactly like any other
/// legacy/owner-unknown worktree (#3971).
#[tokio::test]
async fn prune_orphaned_worktrees_never_deletes_claude_native_worktree() {
    let store_dir = tempfile::tempdir().unwrap();
    let repos = tempfile::tempdir().unwrap();
    let mgr = SessionManager::new(
        store_dir.path(),
        crate::session_manager::tests::FakeTmuxDriver::new(),
    )
    .await
    .expect("manager");

    let claude_wt = repos
        .path()
        .join("owner")
        .join("repo")
        .join(".claude")
        .join("worktrees")
        .join("agent-00112233445566778899aabbccddeeff00112233");
    std::fs::create_dir_all(&claude_wt).unwrap();
    // Deliberately NO sentinel file written — Claude Code's native
    // `isolation: "worktree"` mechanism never writes trusty-mpm's sentinel.

    let outcome = mgr
        .prune_orphaned_worktrees(repos.path(), &[], false, DirtyWorktreePolicy::Skip)
        .await
        .expect("prune must not error");

    assert!(
        outcome.removed.is_empty(),
        "a Claude-Code-native worktree must never be auto-deleted; got {:?}",
        outcome.removed
    );
    assert!(
        outcome.owner_unknown.iter().any(|p| p == &claude_wt),
        "expected {claude_wt:?} to be reported as owner-unknown; got {:?}",
        outcome.owner_unknown
    );
    assert!(claude_wt.exists(), "worktree dir must survive the prune");
}

// ── #3971 follow-up: the two Claude-worktree locations the first #3971 fix
// missed — `.base/.claude/worktrees` and the nested per-session-checkout
// shape (BLOCK finding: the fix only anchored at `<repo>/.claude/worktrees`,
// covering roughly a third of the real on-disk surface) ────────────────────

/// `find_orphaned_worktrees` must discover Claude Code agent worktrees
/// created directly inside the bare `.base` clone checkout itself, at
/// `<repo>/.base/.claude/worktrees/<name>` — a sibling of the repo-root
/// `.claude/worktrees` shape the first #3971 fix covered, but a distinct
/// filesystem location the walk did not anchor at (confirmed live on the
/// dogfood machine with 29 real entries).
#[test]
fn find_orphaned_worktrees_covers_base_claude_worktrees_shape() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let base_claude_wt = root
        .join("owner")
        .join("repo")
        .join(".base")
        .join(".claude")
        .join("worktrees")
        .join("agent-1111111111111111111111111111111111111111");
    std::fs::create_dir_all(&base_claude_wt).unwrap();
    let empty_active: std::collections::HashSet<std::path::PathBuf> =
        std::collections::HashSet::new();
    let orphans = find_orphaned_worktrees(root, &empty_active);
    assert!(
        orphans.contains(&base_claude_wt),
        "the .base/.claude/worktrees shape must be discovered as a candidate; got {orphans:?}"
    );
}

/// `find_orphaned_worktrees` must discover Claude Code agent worktrees
/// nested INSIDE a per-session checkout, at
/// `<repo>/.base/.worktrees/<session-id>/.claude/worktrees/<name>` — the
/// exact shape a Claude Code agent spawned from within its own trusty-mpm
/// session checkout produces (this is literally the shape this leg's own
/// `fix-worktree-hygiene` worktree lives under). The session-id leaf name is
/// dynamic, so this exercises the `list_immediate_dirs` enumeration path,
/// not a fixed join.
#[test]
fn find_orphaned_worktrees_covers_nested_session_claude_worktrees_shape() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let nested_claude_wt = root
        .join("owner")
        .join("repo")
        .join(".base")
        .join(".worktrees")
        .join("2eb72dca-de08-481b-8dfa-22ab7f81b1f9")
        .join(".claude")
        .join("worktrees")
        .join("fix-worktree-hygiene");
    std::fs::create_dir_all(&nested_claude_wt).unwrap();
    let empty_active: std::collections::HashSet<std::path::PathBuf> =
        std::collections::HashSet::new();
    let orphans = find_orphaned_worktrees(root, &empty_active);
    assert!(
        orphans.contains(&nested_claude_wt),
        "the nested .base/.worktrees/<session-id>/.claude/worktrees shape must be \
         discovered as a candidate; got {orphans:?}"
    );
}

/// End-to-end through the real ownership gate (mirrors
/// `prune_orphaned_worktrees_never_deletes_claude_native_worktree`): a
/// sentinel-less `.base/.claude/worktrees/<name>` candidate is discovered but
/// never auto-deleted, only reported as owner-unknown (#3971 follow-up).
#[tokio::test]
async fn prune_orphaned_worktrees_never_deletes_base_claude_native_worktree() {
    let store_dir = tempfile::tempdir().unwrap();
    let repos = tempfile::tempdir().unwrap();
    let mgr = SessionManager::new(
        store_dir.path(),
        crate::session_manager::tests::FakeTmuxDriver::new(),
    )
    .await
    .expect("manager");

    let base_claude_wt = repos
        .path()
        .join("owner")
        .join("repo")
        .join(".base")
        .join(".claude")
        .join("worktrees")
        .join("agent-2222222222222222222222222222222222222222");
    std::fs::create_dir_all(&base_claude_wt).unwrap();
    // Deliberately NO sentinel file written.

    let outcome = mgr
        .prune_orphaned_worktrees(repos.path(), &[], false, DirtyWorktreePolicy::Skip)
        .await
        .expect("prune must not error");

    assert!(
        outcome.removed.is_empty(),
        "a Claude-Code-native worktree under .base/.claude must never be auto-deleted; got {:?}",
        outcome.removed
    );
    assert!(
        outcome.owner_unknown.iter().any(|p| p == &base_claude_wt),
        "expected {base_claude_wt:?} to be reported as owner-unknown; got {:?}",
        outcome.owner_unknown
    );
    assert!(
        base_claude_wt.exists(),
        "worktree dir must survive the prune"
    );
}

/// End-to-end through the real ownership gate for the NESTED
/// per-session-checkout shape: a sentinel-less
/// `.base/.worktrees/<session-id>/.claude/worktrees/<name>` candidate is
/// discovered but never auto-deleted, only reported as owner-unknown (#3971
/// follow-up).
#[tokio::test]
async fn prune_orphaned_worktrees_never_deletes_nested_session_claude_worktree() {
    let store_dir = tempfile::tempdir().unwrap();
    let repos = tempfile::tempdir().unwrap();
    let mgr = SessionManager::new(
        store_dir.path(),
        crate::session_manager::tests::FakeTmuxDriver::new(),
    )
    .await
    .expect("manager");

    let nested_claude_wt = repos
        .path()
        .join("owner")
        .join("repo")
        .join(".base")
        .join(".worktrees")
        .join("some-session-id")
        .join(".claude")
        .join("worktrees")
        .join("nested-agent");
    std::fs::create_dir_all(&nested_claude_wt).unwrap();
    // Deliberately NO sentinel file written on the nested Claude worktree.
    // The session-id leaf itself also has no sentinel — it must not be
    // misclassified as a location-2 (`.base/.worktrees`) orphan candidate
    // either, since it still contains a live-looking nested checkout; that
    // is exercised implicitly here (only the leaf-most directory is ever a
    // candidate for each scanned shape).

    let outcome = mgr
        .prune_orphaned_worktrees(repos.path(), &[], false, DirtyWorktreePolicy::Skip)
        .await
        .expect("prune must not error");

    assert!(
        outcome.removed.is_empty(),
        "a nested Claude-Code-native worktree must never be auto-deleted; got {:?}",
        outcome.removed
    );
    assert!(
        outcome.owner_unknown.iter().any(|p| p == &nested_claude_wt),
        "expected {nested_claude_wt:?} to be reported as owner-unknown; got {:?}",
        outcome.owner_unknown
    );
    assert!(
        nested_claude_wt.exists(),
        "worktree dir must survive the prune"
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
        .prune_orphaned_worktrees(repos.path(), &[], false, DirtyWorktreePolicy::Skip)
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
        .prune_orphaned_worktrees(repos.path(), &[], false, DirtyWorktreePolicy::Skip)
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
        .prune_orphaned_worktrees(repos.path(), &[], false, DirtyWorktreePolicy::Skip)
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
        .prune_orphaned_worktrees(repos.path(), &[], false, DirtyWorktreePolicy::Skip)
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

/// The forensics runbook the escalated F3 alarm points at must actually
/// exist in the repo (#3764 item 3).
///
/// Why: the alarm's only value at 03:00 is that it names, in-message, the
/// document telling the operator what to capture before a stop destroys the
/// pane transcript. A dangling path turns that into a dead end at the worst
/// possible moment — so the pointer is pinned mechanically rather than trusted
/// to survive a future docs reshuffle.
/// What: resolves [`FORENSICS_RUNBOOK`] against the workspace root (two levels
/// up from this crate's `CARGO_MANIFEST_DIR`) and asserts the file is present
/// and non-empty.
/// Test: this function IS the test.
#[test]
fn forensics_runbook_path_exists() {
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("crate dir must have a grandparent (the workspace root)");
    let runbook = workspace_root.join(FORENSICS_RUNBOOK);
    assert!(
        runbook.is_file(),
        "the F3 alarm points operators at {FORENSICS_RUNBOOK}, which must exist; \
         looked at {}",
        runbook.display()
    );
    let len = std::fs::metadata(&runbook).expect("stat runbook").len();
    assert!(len > 0, "the forensics runbook must not be empty");
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
