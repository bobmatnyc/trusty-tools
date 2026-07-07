//! Issue #2178 (P0 data-risk) regression tests.
//!
//! Why: isolated from `tests.rs` to keep that file under the 1500-SLOC
//! test-file cap while giving the incident regression its own focused,
//! self-contained coverage. A live daemon ran `reindex -i cto`; the #402/
//! #1073 "colocated root moved" heuristic decided `cto`'s root had moved
//! from its real, persisted location to an unrelated git worktree that
//! merely happened to also have colocated `.trusty-search/` storage. The
//! daemon walked the unrelated worktree (2,506 files) and the post-loop
//! prune pass then deleted every one of the real corpus's 369,568 chunks
//! that weren't seen in that walk. See `validate::root_move_is_trusted` for
//! the full root-cause writeup.
//!
//! What: drives `spawn_reindex` end-to-end against a real, durable
//! `CorpusStore` pre-seeded with chunks, reproducing the exact divergence
//! that triggered the incident (in-memory `IndexHandle::root_path` disagrees
//! with both the corpus's own persisted `indexed_root` and the durably
//! persisted `indexes.toml` entry), and proves the corpus survives. A second
//! test proves the legitimate #402/#1073 relocation case — where the
//! candidate root DOES match the persisted `indexes.toml` entry (mirroring
//! `POST /indexes/:id/relocate`, which persists before swapping the handle)
//! — still completes normally.
//!
//! Test: `reindex_refuses_untrusted_root_move_and_preserves_corpus` (the core
//! #2178 regression) and `reindex_accepts_root_move_that_matches_persisted_config`
//! (the surviving legitimate-move case).

use super::*;
use crate::core::chunker::{ChunkType, RawChunk};
use crate::core::corpus::CorpusStore;
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId};
use crate::service::persistence::PersistedIndex;
use serial_test::serial;
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// Minimal `RawChunk` fixture — mirrors the helper duplicated across
/// `tests.rs` / `prune_tests.rs`.
fn chunk(file: &str, id: &str) -> RawChunk {
    RawChunk {
        id: id.to_string(),
        file: file.to_string(),
        start_line: 1,
        end_line: 1,
        content: format!("fn {id}() {{}}"),
        function_name: None,
        language: Some("rust".to_string()),
        chunk_type: ChunkType::Code,
        calls: Vec::new(),
        inherits_from: Vec::new(),
        chunk_depth: 0,
        parent_chunk_id: None,
        child_chunk_ids: Vec::new(),
        nlp_keywords: Vec::new(),
        nlp_code_refs: Vec::new(),
        virtual_terms: Vec::new(),
    }
}

/// Wait up to 5s for `progress` to reach a terminal status.
async fn wait_for_terminal(progress: &ReindexProgress) {
    for _ in 0..100 {
        let status = progress.status.load();
        if status == ReindexStatus::Failed || status == ReindexStatus::Complete {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// THE core #2178 regression: an in-memory handle whose `root_path` diverges
/// from both the corpus's own persisted `indexed_root` AND the durably
/// persisted `indexes.toml` `root_path` must NEVER be trusted enough to walk
/// (or, on the next incremental reindex, prune) the real corpus — even
/// though the candidate root has its own colocated `.trusty-search/`
/// storage, exactly as in the live incident.
///
/// Why: this is the direct incident reproduction. Before the fix, the old
/// heuristic accepted the candidate purely because
/// `has_colocated_storage(candidate)` was `true` — a signal that is
/// trivially satisfied by ANY colocated project, not just the correct one.
/// What: seeds a real `CorpusStore` with 50 chunks (standing in for the
/// incident's 369,568), stamps the corpus's `indexed_root` at the REAL root
/// (A), persists `indexes.toml`'s `root_path` at A too, then constructs an
/// `IndexHandle` whose in-memory `root_path` is an UNRELATED root (B) — the
/// exact shape of the `POST /indexes/:id/reindex` `root_path` override
/// (issue #63) landing without ever updating `indexes.toml`. Asserts the
/// reindex is refused (status `Failed`), the walk never ran
/// (`total_files == 0`), and — the data-safety proof — all 50 original
/// chunks are still present in the corpus afterward.
/// Test: this test.
#[tokio::test]
#[serial]
async fn reindex_refuses_untrusted_root_move_and_preserves_corpus() {
    // Isolate `indexes.toml` so this test's persisted config can't collide
    // with other tests or the developer's real data dir. `#[serial]` is
    // required because `TRUSTY_DATA_DIR` is shared process (env) state.
    let data_dir = tempfile::tempdir().expect("data dir");
    unsafe { std::env::set_var("TRUSTY_DATA_DIR", data_dir.path()) };

    // root_a: the index's REAL, persisted root (simulates
    // `/Users/masa/Duetto/cto`).
    let root_a = tempfile::tempdir().expect("root a");
    // root_b: an unrelated directory the in-memory handle gets hijacked to
    // (simulates the unrelated trusty-tools git worktree from the
    // incident). It even carries its own colocated `.trusty-search/` dir,
    // exactly like the incident (this very workspace self-indexes) —
    // proving the old #402 "has colocated storage" signal alone is not
    // sufficient legitimacy proof.
    let root_b = tempfile::tempdir().expect("root b");
    std::fs::create_dir_all(root_b.path().join(".trusty-search")).unwrap();

    let id = IndexId::new("cto-2178-hijack-sim");

    // Seed a real durable corpus with chunks — stands in for the incident's
    // 369,568-chunk `cto` corpus.
    let redb_path = data_dir.path().join("cto_corpus.redb");
    let corpus = Arc::new(CorpusStore::open(&redb_path).expect("open corpus"));
    let seed_chunks: Vec<RawChunk> = (0..50)
        .map(|i| chunk("real/file.rs", &format!("real:{i}")))
        .collect();
    corpus.upsert_chunks(&seed_chunks).expect("seed chunks");
    assert_eq!(
        corpus.load_all_chunks().unwrap().len(),
        50,
        "test setup: corpus must start with 50 chunks"
    );

    let mut indexer = CodeIndexer::new(id.0.clone(), root_b.path().to_path_buf());
    indexer.set_corpus_store(Arc::clone(&corpus));
    let handle = Arc::new(IndexHandle::bare(
        id.clone(),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        root_b.path().to_path_buf(), // HIJACKED in-memory root.
    ));

    // Stamp the corpus's own `indexed_root` = root_a — what a prior,
    // legitimate reindex against the real root would have written.
    handle
        .write_indexed_root(root_a.path())
        .await
        .expect("stamp prior indexed_root");

    // Persist `indexes.toml`'s `root_path` = root_a — the durable source of
    // truth. The in-memory handle above disagrees (root_b): exactly the
    // #2178 divergence (an unpersisted `root_path` override, or any other
    // path by which the in-memory handle drifted from disk).
    crate::service::persistence::upsert_index_registry_entry(PersistedIndex {
        id: id.0.clone(),
        root_path: root_a.path().to_path_buf(),
        ..Default::default()
    })
    .expect("persist indexes.toml entry");

    let progress = Arc::new(ReindexProgress::new());
    spawn_reindex(handle.clone(), progress.clone(), false);
    wait_for_terminal(&progress).await;

    assert_eq!(
        progress.status.load(),
        ReindexStatus::Failed,
        "an untrusted root move (in-memory root disagrees with the \
         persisted indexes.toml root_path) must abort the reindex, not \
         silently walk/prune against the hijacked root"
    );

    // The gate must fire BEFORE Phase 1 — no walk was performed against the
    // untrusted candidate root.
    assert_eq!(
        progress.total_files.load(Ordering::Acquire),
        0,
        "the #2178 gate must fire before the walk starts"
    );

    // THE core #2178 assertion: the corpus's original chunks must survive
    // completely untouched — no prune pass ever ran.
    let surviving = corpus.load_all_chunks().expect("read corpus after abort");
    assert_eq!(
        surviving.len(),
        50,
        "#2178 regression: the real corpus's chunks must survive a refused \
         reindex against an untrusted root; found {} (a lower count means \
         the corpus was wrongly pruned)",
        surviving.len()
    );

    unsafe { std::env::remove_var("TRUSTY_DATA_DIR") };
}

/// The surviving legitimate case: a root move whose candidate root DOES
/// match the durably persisted `indexes.toml` entry (mirroring `POST
/// /indexes/:id/relocate`, which persists the new `root_path` BEFORE the
/// handle is swapped) must still be trusted — the #402/#1073 colocated
/// relocation behaviour keeps working.
///
/// Why: proves the #2178 fix narrows the auto-detected-move convenience
/// without breaking the one path that is actually safe: an operator-driven,
/// durably-persisted relocation.
/// What: same shape as the hijack test, except `indexes.toml`'s
/// `root_path` is set to the NEW root (matching the in-memory handle), and
/// the new root has one real file to index. Asserts the reindex completes
/// normally and the walk ran.
/// Test: this test.
#[tokio::test]
#[serial]
async fn reindex_accepts_root_move_that_matches_persisted_config() {
    let data_dir = tempfile::tempdir().expect("data dir");
    unsafe { std::env::set_var("TRUSTY_DATA_DIR", data_dir.path()) };

    let old_root = tempfile::tempdir().expect("old root");
    let new_root = tempfile::tempdir().expect("new root");
    std::fs::write(new_root.path().join("keep.rs"), "fn keep() {}\n").unwrap();
    // Colocated storage marker at the new root (mirrors a genuine `mv` of a
    // colocated project).
    std::fs::create_dir_all(new_root.path().join(".trusty-search")).unwrap();

    let id = IndexId::new("legit-move-2178-sim");

    let redb_path = data_dir.path().join("legit_corpus.redb");
    let corpus = Arc::new(CorpusStore::open(&redb_path).expect("open corpus"));

    let mut indexer = CodeIndexer::new(id.0.clone(), new_root.path().to_path_buf());
    indexer.set_corpus_store(Arc::clone(&corpus));
    let handle = Arc::new(IndexHandle::bare(
        id.clone(),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        new_root.path().to_path_buf(),
    ));

    // The corpus remembers the OLD root — a genuine prior reindex happened
    // there before the project was relocated.
    handle
        .write_indexed_root(old_root.path())
        .await
        .expect("stamp prior indexed_root");

    // The relocate flow persists `indexes.toml`'s `root_path` to the NEW
    // root BEFORE the handle is swapped (mirrors `indexes_relocate.rs`).
    crate::service::persistence::upsert_index_registry_entry(PersistedIndex {
        id: id.0.clone(),
        root_path: new_root.path().to_path_buf(),
        ..Default::default()
    })
    .expect("persist indexes.toml entry");

    let progress = Arc::new(ReindexProgress::new());
    spawn_reindex(handle.clone(), progress.clone(), false);
    wait_for_terminal(&progress).await;

    assert_eq!(
        progress.status.load(),
        ReindexStatus::Complete,
        "a root move that matches the persisted indexes.toml root_path must \
         still be trusted — the #402/#1073 legitimate relocation case must \
         keep working"
    );
    assert_eq!(
        progress.total_files.load(Ordering::Acquire),
        1,
        "the trusted move must proceed to walk the new root normally"
    );

    unsafe { std::env::remove_var("TRUSTY_DATA_DIR") };
}
