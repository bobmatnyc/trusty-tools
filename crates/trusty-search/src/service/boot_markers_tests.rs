//! Unit regression tests for the durable boot-integrity markers (#4390, #4391).
//!
//! Why: both defects were invisible to the existing suite for the same reason —
//! `reconcile_tests.rs` hand-builds handles with `indexed_head_sha:
//! Some("old_sha")`, which bypasses the restore path that erases exactly the
//! staleness under test, and nothing drove a deferred-embed pass across a
//! restart at all.
//! What: this file covers the marker read/write contract and the re-arm
//! decision. The end-to-end proof that the REAL warm-boot path honours both
//! markers lives bin-side, in `commands::start_restore`'s
//! `start_restore_markers_tests` — `restore_one_index` is compiled into the
//! binary, not the library, so a lib test cannot reach it.
//!
//! Multi-thread flavour on anything touching a handle: the registry write lock
//! and the corpus open both park on blocking calls, and a single-threaded
//! runtime aborts there before reaching an assertion — which would make a test
//! "pass" post-fix by never running its body.
//!
//! Test isolation: `data_dir()` resolves to a per-PROCESS directory in a test
//! binary (#4094 / #4255), so every test here shares one `indexes.toml`. Each
//! uses a unique index id, and `patch_index_registry_entry_at` serialises
//! concurrent writers under the registry write lock.

use super::*;
use crate::core::indexer::CodeIndexer;
use crate::core::registry::IndexId;
use crate::service::persistence::{load_index_registry_at, upsert_index_registry_entry};

/// Build a git repo with one commit; returns the tempdir and its HEAD SHA.
pub(crate) fn git_repo_with_commit(filename: &str, content: &str) -> (tempfile::TempDir, String) {
    use std::process::Command;
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();
    let run = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(&root)
            .output()
            .expect("git");
        assert!(out.status.success(), "git {args:?} failed");
    };
    run(&["init", "-b", "main"]);
    run(&["config", "user.email", "test@test.test"]);
    run(&["config", "user.name", "Test"]);
    std::fs::write(root.join(filename), content).expect("write");
    run(&["add", "."]);
    run(&["commit", "-m", "initial"]);
    let sha = crate::core::git::head_sha(&root).expect("HEAD after commit");
    (dir, sha)
}

/// Read one entry back out of the process's real `indexes.toml`.
fn read_entry(id: &str) -> PersistedIndex {
    let path = indexes_toml_path().expect("indexes.toml path");
    load_index_registry_at(&path)
        .expect("registry must load")
        .into_iter()
        .find(|e| e.id == id)
        .unwrap_or_else(|| panic!("entry {id} must exist in indexes.toml"))
}

fn entry_for(id: &str, root: &std::path::Path) -> PersistedIndex {
    {
        let mut e = PersistedIndex::new(id.to_owned(), root.to_path_buf());
        e.colocated = true;
        e
    }
}

/// #4391: a stored stamp wins over live git.
///
/// Why: this is the whole defect. Restore called `git::head_sha(root)`
/// unconditionally, so `stored == current` was true by construction and
/// `reconcile_git_path` reported `up_to_date` for every git-backed index on
/// every boot — commits landing while the daemon was down could never be seen.
/// What: an entry stamped at commit 1 while HEAD has moved to commit 2 (the
/// "daemon was down" window). Against the pre-fix resolution this returns the
/// SECOND sha and the assertion fails on the value, not on a panic.
/// Test: this IS the test.
#[test]
fn resolve_prefers_the_persisted_head_sha_over_live_git() {
    use std::process::Command;
    let (dir, first_sha) = git_repo_with_commit("a.rs", "fn a() {}");
    std::fs::write(dir.path().join("a.rs"), "fn a() { let b = 1; }").expect("write");
    for args in [vec!["add", "."], vec!["commit", "-m", "second"]] {
        assert!(Command::new("git")
            .args(&args)
            .current_dir(dir.path())
            .output()
            .expect("git")
            .status
            .success());
    }
    let second_sha = crate::core::git::head_sha(dir.path()).expect("HEAD");
    assert_ne!(first_sha, second_sha, "fixture must actually advance HEAD");

    let mut entry = entry_for("boot-markers-4391-prefers", dir.path());
    entry.indexed_head_sha = Some(first_sha.clone());

    assert_eq!(
        resolve_indexed_head_sha(&entry),
        Some(first_sha),
        "restore must carry the SHA the corpus was BUILT against, not live HEAD \
         — re-deriving it is what made boot reconcile blind to down-time \
         commits (#4391)"
    );
}

/// Serialised: `upsert_index_registry_entry` is load-all -> push -> save-all,
/// so two concurrent upserts against the process-shared `indexes.toml` can
/// drop each other's entry (#4871 tracks the general fix). These use the
/// DEFAULT `#[serial]` key deliberately — it is the key the existing
/// registry-mutating tests already use, and a named key would have excluded
/// only each other while `save_index_registry` republished the whole file
/// underneath them.
///
/// #4391: a legacy entry with no stored SHA adopts live HEAD and BACKFILLS it.
///
/// Why: `indexed_head_sha` was never persisted at any point in history, so every
/// existing `indexes.toml` loads as `None`. Reindexing each of those would
/// re-walk a whole fleet on one upgrade; adopting live HEAD once reproduces
/// exactly the pre-fix behaviour for that single boot and leaves every later
/// boot a genuine value to compare against.
/// What: no stamp on the entry; assert the live SHA comes back AND lands on
/// disk. Pre-fix there is no write-through, so the on-disk assertion fails.
/// Test: this IS the test.
#[test]
#[serial_test::serial]
fn resolve_backfills_a_missing_head_sha_from_live_git() {
    let (dir, sha) = git_repo_with_commit("b.rs", "fn b() {}");
    let id = "boot-markers-4391-backfills";
    let entry = entry_for(id, dir.path());
    upsert_index_registry_entry(entry.clone()).expect("persist entry");
    assert!(
        entry.indexed_head_sha.is_none(),
        "fixture is a legacy entry"
    );

    assert_eq!(
        resolve_indexed_head_sha(&entry),
        Some(sha.clone()),
        "a never-stamped entry adopts live HEAD, exactly as before the fix"
    );
    assert_eq!(
        read_entry(id).indexed_head_sha,
        Some(sha),
        "the adopted SHA must be written through, or the next boot backfills \
         again and can still never detect drift (#4391)"
    );
}

/// #4391: `stamp_handle` — the delta-reconcile writer — must persist.
///
/// Why: `reconcile.rs`'s own doc claimed reindex-completion stamping is what
/// makes the next boot see `stored == current`. It wrote memory only, so nothing
/// about it survived to the next boot in either direction.
/// What: stamp a bare handle whose id has a registry entry, then read the entry
/// back off disk. Against the pre-fix `stamp_handle` this fails with `None`.
/// Test: this IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn stamp_handle_persists_the_sha_it_writes() {
    let id = "boot-markers-4391-stamp";
    let dir = tempfile::tempdir().expect("tempdir");
    upsert_index_registry_entry(entry_for(id, dir.path())).expect("persist entry");
    let handle = Arc::new(IndexHandle::bare(
        IndexId::new(id),
        Arc::new(tokio::sync::RwLock::new(CodeIndexer::new(
            id,
            dir.path().to_path_buf(),
        ))),
        dir.path().to_path_buf(),
    ));

    crate::service::reconcile::stamp_handle(&handle, "deadbeefcafe").await;

    assert_eq!(
        read_entry(id).indexed_head_sha.as_deref(),
        Some("deadbeefcafe"),
        "a delta reconcile that only stamps memory is erased by the next handle \
         rebuild — restart, cold-park reload, selective warm boot (#4391)"
    );
}

/// #4390: an interrupted deferred-embed pass leaves a durable marker behind.
///
/// Why: C2 accumulates every embedding in memory and commits once at the end, so
/// a stop anywhere inside it persists zero vectors. Nothing on disk recorded that
/// the pass was owed, and warm boot read the pre-existing HNSW snapshot and
/// reported `semantic: ready` regardless.
/// What: set the marker (what `finish_reindex` now does before queueing), then
/// simulate the interruption by never running the pass. Pre-fix there is no
/// writer at all, so the read-back is `false`.
/// Test: this IS the test.
#[test]
#[serial_test::serial]
fn interrupted_embed_pass_leaves_the_pending_marker_set() {
    let id = "boot-markers-4390-interrupted";
    let dir = tempfile::tempdir().expect("tempdir");
    upsert_index_registry_entry(entry_for(id, dir.path())).expect("persist entry");

    persist_deferred_embed_pending(id, true);
    // The daemon stops here — the pass never commits.

    assert!(
        read_entry(id).deferred_embed_pending,
        "the marker must survive the stop; it is the only record that the \
         semantic lane is short the chunks that pass was embedding (#4390)"
    );
}

/// #4390: clearing the marker is durable too.
///
/// Why: a marker that could not be cleared would re-arm a completed pass on
/// every boot for the life of the index.
/// What: set then clear, and read back off disk.
/// Test: this IS the test.
#[test]
#[serial_test::serial]
fn a_completed_pass_clears_the_pending_marker_durably() {
    let id = "boot-markers-4390-cleared";
    let dir = tempfile::tempdir().expect("tempdir");
    upsert_index_registry_entry(entry_for(id, dir.path())).expect("persist entry");

    persist_deferred_embed_pending(id, true);
    persist_deferred_embed_pending(id, false);

    assert!(!read_entry(id).deferred_embed_pending);
}

/// #4390 / #4391: legacy `indexes.toml` files load with both markers absent.
///
/// Why: every `indexes.toml` in the field predates both fields. If either had a
/// deserialisation default that errored or flipped, an upgrade would drop the
/// whole registry rather than one field.
/// What: parse a minimal legacy entry and assert both markers land on their
/// safe defaults — no stamp, no pass owed.
/// Test: this IS the test.
#[test]
fn legacy_entries_load_with_both_markers_absent() {
    let tmp = tempfile::NamedTempFile::new().expect("tempfile");
    std::fs::write(
        tmp.path(),
        "[[index]]\nid = \"legacy\"\nroot_path = \"/tmp/legacy_markers\"\n",
    )
    .expect("write");
    let entries = load_index_registry_at(tmp.path()).expect("legacy file must load");
    assert_eq!(entries.len(), 1);
    assert!(entries[0].indexed_head_sha.is_none());
    assert!(!entries[0].deferred_embed_pending);
}

/// #4390: the re-arm decision itself, exhaustively.
///
/// Why: the three guards are what keep a boot re-arm from burning the single
/// background permit on work that provably cannot progress.
/// What: table over `(pending, skip_vector, lexical_only, has_embedder)`.
/// Test: this IS the test.
#[test]
fn rearm_decision_respects_skip_vector_and_embedder_presence() {
    assert!(should_rearm_deferred_embed(true, false, false, true));
    assert!(
        !should_rearm_deferred_embed(false, false, false, true),
        "no marker, no work"
    );
    assert!(
        !should_rearm_deferred_embed(true, true, false, true),
        "skip_vector must never run the embedder"
    );
    assert!(
        !should_rearm_deferred_embed(true, false, true, true),
        "lexical_only stops before the semantic stage entirely"
    );
    assert!(
        !should_rearm_deferred_embed(true, false, false, false),
        "no embedder wired — the pass cannot make progress"
    );
}
