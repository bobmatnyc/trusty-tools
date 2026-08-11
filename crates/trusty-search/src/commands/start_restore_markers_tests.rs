//! End-to-end warm-boot tests for the boot-integrity markers (#4390, #4391).
//!
//! Why: #4391's own analysis names the gap — `reconcile_tests.rs` hand-builds
//! handles with a stored SHA, which bypasses the restore path that erased the
//! staleness under test, so "there is no end-to-end test of restore →
//! reconcile". These tests drive the real `restore_one_index` against a real
//! `indexes.toml`, which is the only place the defect was observable.
//! What: three cases — a restored handle carries the persisted SHA rather than
//! live HEAD; an index carrying the deferred-embed marker gets its catch-up
//! re-armed and the completed pass clears the marker; a `skip_vector` index
//! drops the marker instead of re-arming forever.
//!
//! `flavor = "multi_thread"` is required, not cosmetic: `restore_one_index`
//! opens redb through `spawn_blocking` and the re-armed pass parks on the
//! background semaphore. On a single-threaded runtime the test aborts inside
//! those before reaching an assertion and would report success by never running.
//!
//! Test: this file IS the suite.

use std::sync::Arc;

use super::{restore_one_index, RelocationScan};
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry, StageStatus};
use crate::service::boot_markers::persist_deferred_embed_pending;
use crate::service::persistence::{
    indexes_toml_path, load_index_registry_at, upsert_index_registry_entry, PersistedIndex,
};
use crate::service::SearchAppState;

/// Deterministic in-test embedder — `MockEmbedder` is `cfg(test)`-gated inside
/// the LIBRARY, so it is not linked into this binary test target.
struct TestEmbedder;

#[async_trait::async_trait]
impl crate::core::Embedder for TestEmbedder {
    async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
        Ok(vec![0.1; 8])
    }
    async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| vec![0.1; 8]).collect())
    }
    fn dimension(&self) -> usize {
        8
    }
}

/// Build a git repo with one commit; returns the tempdir and its HEAD SHA.
fn git_repo_with_commit(filename: &str, content: &str) -> (tempfile::TempDir, String) {
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

fn commit_again(root: &std::path::Path, filename: &str, content: &str) -> String {
    use std::process::Command;
    std::fs::write(root.join(filename), content).expect("write");
    for args in [vec!["add", "."], vec!["commit", "-m", "second"]] {
        let out = Command::new("git")
            .args(&args)
            .current_dir(root)
            .output()
            .expect("git");
        assert!(out.status.success(), "git {args:?} failed");
    }
    crate::core::git::head_sha(root).expect("HEAD after second commit")
}

fn read_entry(id: &str) -> PersistedIndex {
    let path = indexes_toml_path().expect("indexes.toml path");
    load_index_registry_at(&path)
        .expect("registry must load")
        .into_iter()
        .find(|e| e.id == id)
        .unwrap_or_else(|| panic!("entry {id} must exist in indexes.toml"))
}

fn entry_for(id: &str, root: &std::path::Path) -> PersistedIndex {
    PersistedIndex {
        id: id.to_owned(),
        root_path: root.to_path_buf(),
        colocated: true,
        ..Default::default()
    }
}

/// Register `entry` and drive the real warm-boot restore path for it.
async fn restore(entry: PersistedIndex) -> Arc<IndexHandle> {
    upsert_index_registry_entry(entry.clone()).expect("persist entry");
    let state = SearchAppState::new(IndexRegistry::new());
    let embedder: Arc<dyn crate::core::Embedder> = Arc::new(TestEmbedder);
    restore_one_index(
        &state,
        &embedder,
        entry.clone(),
        RelocationScan::Unavailable,
    )
    .await;
    state
        .registry
        .get(&IndexId::new(entry.id.clone()))
        .unwrap_or_else(|| panic!("restore must register {}", entry.id))
}

/// Serialised: `upsert_index_registry_entry` is load-all -> push -> save-all,
/// so two concurrent upserts against the process-shared `indexes.toml` can
/// drop each other's entry (#4871 tracks the general fix). `#[serial]` on the
/// shared key is what keeps these tests deterministic in the meantime.
///
/// #4391: a restored handle carries the persisted SHA, not live HEAD.
///
/// Why: with the SHA re-derived here, `reconcile_git_path` compared current HEAD
/// against current HEAD, so a commit made while the daemon was down was
/// structurally undetectable and `results_may_be_stale` stayed false.
/// What: stamp at commit 1, advance HEAD to commit 2 while "down", restore.
/// Against the pre-fix restore the assertion fails carrying the SECOND sha.
/// Test: this IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial(trusty_search_indexes_toml)]
async fn restored_handle_keeps_the_persisted_head_sha_across_downtime() {
    let (dir, first_sha) = git_repo_with_commit("a.rs", "fn a() {}");
    let id = "start-restore-4391-downtime";
    let mut entry = entry_for(id, dir.path());
    entry.indexed_head_sha = Some(first_sha.clone());

    let second_sha = commit_again(dir.path(), "a.rs", "fn a() { let b = 1; }");
    assert_ne!(first_sha, second_sha, "fixture must actually advance HEAD");

    let handle = restore(entry).await;

    assert_eq!(
        handle.indexed_head_sha.read().await.clone(),
        Some(first_sha),
        "the restored handle must present the SHA its corpus was built against, \
         so boot reconcile can see the down-time commit (#4391)"
    );
}

/// #4390: restore re-arms an interrupted catch-up and the pass runs to
/// completion.
///
/// Why: the issue's core finding is that no AUTOMATIC trigger exists — repair
/// waits for the next HEAD advance plus a query-wake, which is hours on an
/// active repo and never on a dormant one. This drives the whole path: restore
/// reads the marker, enqueues through the size-ordered queue, and the pass runs
/// under the single background permit.
///
/// The observation point is the handle's own semantic stage, not the on-disk
/// marker. Warm boot derives `Pending` for this fixture (empty corpus, no HNSW
/// snapshot), and only a pass that actually ran can move it to `Ready` — so
/// `Ready` is proof the re-arm fired, and it is unaffected by a sibling test
/// rewriting the process-shared `indexes.toml` during the poll window. The
/// durable clear is asserted on disk by `restore_drops_the_marker_for_a_skip_vector_index`
/// and by `service::boot_markers`'s marker tests.
/// What: an entry with `deferred_embed_pending = true` and an embedder wired.
/// Against the pre-fix restore nothing is ever enqueued, so the stage stays
/// `Pending` and this fails on the assertion after the poll window.
/// Test: this IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial(trusty_search_indexes_toml)]
async fn restore_rearms_an_interrupted_deferred_embed_pass() {
    let (dir, _sha) = git_repo_with_commit("c.rs", "fn c() {}");
    let id = "start-restore-4390-rearm";
    let mut entry = entry_for(id, dir.path());
    entry.deferred_embed_pending = true;

    let handle = restore(entry).await;
    assert!(
        handle.indexer.read().await.has_embedder(),
        "precondition: the re-arm is gated on an embedder being wired"
    );

    let mut ran = false;
    for _ in 0..200 {
        if handle.stages.read().await.semantic.status == StageStatus::Ready {
            ran = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(
        ran,
        "restore must re-arm the interrupted catch-up and the pass must publish \
         a terminal semantic state — without this the index waits for an \
         unrelated reindex, indefinitely on a dormant repo (#4390)"
    );
}

/// #4390: a `skip_vector` index drops the marker instead of re-firing forever.
///
/// Why: the vector component can be disabled (`PATCH /indexes/:id/config`) after
/// a pass was queued. The work is then owed to nobody, and a marker no pass will
/// ever clear would re-arm on every boot for the life of the index.
/// What: `skip_vector` plus the marker; assert restore clears it. Also asserts
/// the marker was genuinely set beforehand, so a clear that never happened
/// cannot be mistaken for a marker that was never written.
/// Test: this IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial(trusty_search_indexes_toml)]
async fn restore_drops_the_marker_for_a_skip_vector_index() {
    let (dir, _sha) = git_repo_with_commit("d.rs", "fn d() {}");
    let id = "start-restore-4390-skip-vector";
    let mut entry = entry_for(id, dir.path());
    entry.deferred_embed_pending = true;
    entry.skip_vector = true;
    upsert_index_registry_entry(entry.clone()).expect("persist entry");
    persist_deferred_embed_pending(id, true);
    assert!(
        read_entry(id).deferred_embed_pending,
        "precondition: the marker is really set on disk before restore"
    );

    let handle = restore(entry).await;
    assert!(handle.skip_vector, "precondition: vector lane is disabled");
    assert!(
        !read_entry(id).deferred_embed_pending,
        "a disabled vector lane owes no catch-up — the marker must be dropped, \
         not re-armed on every boot forever (#4390)"
    );
}
