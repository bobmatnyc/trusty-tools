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
//! #5357 extends the module to the gate's two INPUTS: both were read
//! fail-open, so a redb fault or an unparseable `indexes.toml` skipped the very
//! check these tests pin. Those two tests inject a real read failure at each
//! input and assert the same corpus-preserving refusal.
//!
//! Test: `reindex_refuses_untrusted_root_move_and_preserves_corpus` (the core
//! #2178 regression), `reindex_accepts_root_move_that_matches_persisted_config`
//! (the surviving legitimate-move case),
//! `reindex_refuses_when_the_persisted_registry_cannot_be_read`,
//! `reindex_refuses_when_the_corpus_indexed_root_read_fails` and
//! `reindex_refuses_when_the_indexed_root_value_is_corrupt` (#5357).

use super::*;
use crate::core::chunker::{ChunkType, RawChunk};
use crate::core::corpus::CorpusStore;
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId};
use crate::service::persistence::PersistedIndex;
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// Run this test's body in a dedicated child process whose `TRUSTY_DATA_DIR`
/// is set at spawn time, and which runs this one test alone.
///
/// Why (issue #4213): both tests below need `indexes.toml` pointed at a
/// throwaway directory, because the #2178 gate they exercise reads the
/// durably persisted registry entry. The previous mechanism —
/// `unsafe { std::env::set_var("TRUSTY_DATA_DIR", …) }` guarded by
/// `#[serial]` — was green by luck: `#[serial]` only excludes other
/// `#[serial]` tests from the same group, so any of the binary's many
/// non-serial tests could still read (or, via its own `remove_var`, clobber)
/// the process-global variable inside the window. `load_index_registry` then
/// found no entry for this index, the gate saw an absent persisted root, and
/// the core assertion flipped `Failed` → `Complete`. That was observed once
/// under a full `cargo test --workspace` run. This guard is the *P0
/// data-loss* regression for #2178, so "usually green" is not good enough:
/// a test that is green by luck is indistinguishable from a test that is
/// green by correctness.
/// What: allocates the throwaway data dir and defers to
/// [`super::test_isolation::run_isolated`], which does the re-execution. The
/// tempdir is held alive for the whole child run and removed when this
/// function returns. (#4721 extracted the mechanism so the #3979 resume tests
/// could reuse it rather than clone it a third time.)
/// Test: both tests in this module route through it.
fn isolate_in_child_process(test_name: &str) -> bool {
    if super::test_isolation::is_isolated_child() {
        return true;
    }
    let data_dir = tempfile::tempdir().expect("isolated data dir");
    super::test_isolation::run_isolated(test_name, &[("TRUSTY_DATA_DIR", data_dir.path())])
}

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
async fn reindex_refuses_untrusted_root_move_and_preserves_corpus() {
    // #4213: run alone in a child process whose TRUSTY_DATA_DIR is set at
    // spawn — deterministic isolation, not `#[serial]` + a racy global.
    if !isolate_in_child_process(
        "service::reindex::root_hijack_tests::\
         reindex_refuses_untrusted_root_move_and_preserves_corpus",
    ) {
        return;
    }
    // `indexes.toml` resolves under the child's own TRUSTY_DATA_DIR; the
    // corpus and roots below get their own tempdirs within it.
    let data_dir = tempfile::tempdir().expect("data dir");

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
    // Issue #2730: await the reindex task's JoinHandle rather than polling
    // `progress.status` on a wall-clock budget. Under load the task queues on
    // the global 2-permit interactive semaphore and is CPU-starved, so a fixed
    // poll could time out with the status still `Running`. Awaiting is a
    // deterministic rendezvous — `run_reindex` has set a terminal status before
    // the handle resolves.
    spawn_reindex_awaitable(handle.clone(), progress.clone(), false)
        .await
        .expect("reindex task must not panic");

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

    // #5357: the refusal must also repair the divergence it found. The handler
    // applies the override's `set_root_path` at request time, so an index that
    // reaches this abort has its indexer resolving root-relative chunk paths
    // against the refused root while the corpus is relative to the old one —
    // and `file_is_within_root` is a lexical prefix test, so those dangling
    // paths pass it and `stale_index_root` reads `false`. Pointing the indexer
    // back at the corpus's own root makes the same state fail closed.
    assert_eq!(
        handle.indexer.read().await.root_path,
        root_a.path(),
        "#5357: a refused root move must leave the indexer resolving against \
         the root the corpus is actually relative to, not the refused one"
    );
}

/// #5357: an `indexes.toml` that cannot be READ must refuse the move, not read
/// back as "this index has no persisted entry".
///
/// Why: `load_index_registry().ok()` dropped the error, and
/// `root_move_is_trusted(None, _)` returns `true` — so the one input that could
/// contradict the hijacked in-memory root was silently discarded and the #2178
/// gate waved the same incident straight through. #4317/#4871 already found
/// unparseable registries in the wild, which is why that load is an `Err` at
/// all rather than an empty vec.
/// What: the hijack fixture, except `indexes.toml` holds garbage instead of the
/// real entry. Asserts the reindex is refused before the walk and the corpus
/// survives.
/// Test: this test.
#[tokio::test]
async fn reindex_refuses_when_the_persisted_registry_cannot_be_read() {
    if !isolate_in_child_process(
        "service::reindex::root_hijack_tests::\
         reindex_refuses_when_the_persisted_registry_cannot_be_read",
    ) {
        return;
    }
    let data_dir = tempfile::tempdir().expect("data dir");
    let root_a = tempfile::tempdir().expect("root a");
    let root_b = tempfile::tempdir().expect("root b");
    // A real file under the hijack target, so a gate that fails open walks
    // something and the `total_files == 0` assertion below actually discriminates.
    std::fs::write(root_b.path().join("bystander.rs"), "fn bystander() {}\n").unwrap();
    std::fs::create_dir_all(root_b.path().join(".trusty-search")).unwrap();

    let id = IndexId::new("cto-5357-registry-unreadable");
    let corpus =
        Arc::new(CorpusStore::open(&data_dir.path().join("cto_corpus.redb")).expect("open corpus"));
    let seed_chunks: Vec<RawChunk> = (0..50)
        .map(|i| chunk("real/file.rs", &format!("real:{i}")))
        .collect();
    corpus.upsert_chunks(&seed_chunks).expect("seed chunks");

    let mut indexer = CodeIndexer::new(id.0.clone(), root_b.path().to_path_buf());
    indexer.set_corpus_store(Arc::clone(&corpus));
    let handle = Arc::new(IndexHandle::bare(
        id.clone(),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        root_b.path().to_path_buf(),
    ));
    handle
        .write_indexed_root(root_a.path())
        .await
        .expect("stamp prior indexed_root");

    // The durable source of truth is present but unreadable — the state
    // `load_index_registry` reports as an error rather than an empty registry.
    std::fs::write(
        crate::service::persistence::indexes_toml_path().expect("registry path"),
        "this file is not valid toml = = =\n",
    )
    .expect("write an unparseable registry");

    let progress = Arc::new(ReindexProgress::new());
    spawn_reindex_awaitable(handle.clone(), progress.clone(), false)
        .await
        .expect("reindex task must not panic");

    assert_eq!(
        progress.status.load(),
        ReindexStatus::Failed,
        "#5357: an unreadable indexes.toml must refuse the root move — \
         discarding that read is how the #2178 gate was bypassed"
    );
    assert_eq!(
        progress.total_files.load(Ordering::Acquire),
        0,
        "#5357: the refusal must fire before the walk starts"
    );
    assert_eq!(
        corpus
            .load_all_chunks()
            .expect("read corpus after abort")
            .len(),
        50,
        "#5357: the real corpus must survive a reindex refused for an \
         unreadable registry"
    );
}

/// #5357: a corpus whose last-indexed root cannot be READ must refuse the
/// reindex, not proceed as though the index had never been indexed.
///
/// Why: `handle.read_indexed_root().await.unwrap_or(None)` collapsed two
/// different states into one. `Ok(None)` — no durable corpus, or never
/// stamped — legitimately means "nothing to relativize against" and skips the
/// gate. `Err` means the answer is unknown, and skipping the gate there hands
/// an unvalidated root to the walk and then to the prune pass, which is the
/// #402 root-hijack incident's exact ending.
/// What: seeds a corpus at root A, points the in-memory handle at an unrelated
/// root B, then breaks the corpus's `_meta` table so every read of it errors.
/// Asserts the reindex is refused before the walk and all 50 chunks survive —
/// pre-fix, the gate never ran, root B was walked, and the prune pass deleted
/// every chunk it did not see there.
/// Test: this test.
#[tokio::test]
async fn reindex_refuses_when_the_corpus_indexed_root_read_fails() {
    if !isolate_in_child_process(
        "service::reindex::root_hijack_tests::\
         reindex_refuses_when_the_corpus_indexed_root_read_fails",
    ) {
        return;
    }
    let fx = corpus_fault_fixture("cto-5357-corpus-unreadable").await;
    crate::core::corpus::test_support::break_meta_table(&fx.corpus)
        .expect("inject the _meta schema fault");
    assert_corpus_fault_refused(&fx, "an unreadable corpus indexed_root").await;
}

/// #5357 (code-critic CRITICAL): a corpus whose stored `indexed_root` VALUE is
/// corrupt must refuse too — the narrower sibling of the test above.
///
/// Why: `read_indexed_root_sync` answered `Ok(None)` when the stored bytes
/// failed UTF-8 decode, which is the same answer it gives for an index that was
/// never stamped. A corrupted root therefore never reached the `Err` arm the
/// rest of this fix routes through: the gate was skipped outright, the candidate
/// root walked, and the corpus pruned against it. The schema-fault injector in
/// the test above cannot reach this path — it fails at `open_table`, several
/// steps earlier.
/// What: same fixture, but the fault is three non-UTF-8 bytes written under
/// `META_KEY_INDEXED_ROOT` with the table's schema left intact.
/// Test: this test.
#[tokio::test]
async fn reindex_refuses_when_the_indexed_root_value_is_corrupt() {
    if !isolate_in_child_process(
        "service::reindex::root_hijack_tests::\
         reindex_refuses_when_the_indexed_root_value_is_corrupt",
    ) {
        return;
    }
    let fx = corpus_fault_fixture("cto-5357-corpus-corrupt-value").await;
    crate::core::corpus::test_support::corrupt_indexed_root_value(&fx.corpus)
        .expect("inject the corrupt indexed_root value");
    assert_corpus_fault_refused(&fx, "a corrupt corpus indexed_root value").await;
}

/// The shared shape of the two corpus-fault tests above: a 50-chunk corpus
/// stamped at root A, an in-memory handle hijacked to an unrelated root B that
/// holds a real file, and an `indexes.toml` that AGREES with the corpus.
///
/// Why: the registry agrees deliberately — it makes the corpus read the only
/// input under test. Pre-fix, its failure alone was enough to skip the gate.
/// The file under root B is what makes `total_files == 0` discriminate: a gate
/// that fails open walks it.
/// What: returns the tempdirs (held so they outlive the run), the corpus, and
/// the handle. Each caller then injects its own fault.
/// Test: both `reindex_refuses_when_the_*` tests above.
struct CorpusFaultFixture {
    _data_dir: tempfile::TempDir,
    _root_a: tempfile::TempDir,
    _root_b: tempfile::TempDir,
    corpus: Arc<CorpusStore>,
    handle: Arc<IndexHandle>,
}

async fn corpus_fault_fixture(id: &str) -> CorpusFaultFixture {
    let data_dir = tempfile::tempdir().expect("data dir");
    let root_a = tempfile::tempdir().expect("root a");
    let root_b = tempfile::tempdir().expect("root b");
    std::fs::write(root_b.path().join("bystander.rs"), "fn bystander() {}\n").unwrap();
    std::fs::create_dir_all(root_b.path().join(".trusty-search")).unwrap();

    let id = IndexId::new(id);
    let corpus =
        Arc::new(CorpusStore::open(&data_dir.path().join("cto_corpus.redb")).expect("open corpus"));
    let seed_chunks: Vec<RawChunk> = (0..50)
        .map(|i| chunk("real/file.rs", &format!("real:{i}")))
        .collect();
    corpus.upsert_chunks(&seed_chunks).expect("seed chunks");

    let mut indexer = CodeIndexer::new(id.0.clone(), root_b.path().to_path_buf());
    indexer.set_corpus_store(Arc::clone(&corpus));
    let handle = Arc::new(IndexHandle::bare(
        id.clone(),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        root_b.path().to_path_buf(),
    ));
    handle
        .write_indexed_root(root_a.path())
        .await
        .expect("stamp prior indexed_root");

    crate::service::persistence::upsert_index_registry_entry(PersistedIndex {
        id: id.0.clone(),
        root_path: root_a.path().to_path_buf(),
        ..Default::default()
    })
    .expect("persist indexes.toml entry");

    CorpusFaultFixture {
        _data_dir: data_dir,
        _root_a: root_a,
        _root_b: root_b,
        corpus,
        handle,
    }
}

/// Run the reindex and assert it refused before touching anything.
async fn assert_corpus_fault_refused(fx: &CorpusFaultFixture, fault: &str) {
    let progress = Arc::new(ReindexProgress::new());
    spawn_reindex_awaitable(fx.handle.clone(), progress.clone(), false)
        .await
        .expect("reindex task must not panic");

    assert_eq!(
        progress.status.load(),
        ReindexStatus::Failed,
        "#5357: {fault} must refuse the reindex — 'the read failed' is not \
         'this index has no prior root'"
    );
    assert_eq!(
        progress.total_files.load(Ordering::Acquire),
        0,
        "#5357: the refusal must fire before the walk starts ({fault})"
    );
    assert_eq!(
        fx.corpus
            .load_all_chunks()
            .expect("read corpus after abort")
            .len(),
        50,
        "#5357: the real corpus must survive ({fault}) — pre-fix this walked \
         the unrelated root and pruned every chunk it did not find there"
    );
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
async fn reindex_accepts_root_move_that_matches_persisted_config() {
    // #4213: same deterministic child-process isolation as the hijack test —
    // it shares the identical `TRUSTY_DATA_DIR` dependency and race.
    if !isolate_in_child_process(
        "service::reindex::root_hijack_tests::\
         reindex_accepts_root_move_that_matches_persisted_config",
    ) {
        return;
    }
    let data_dir = tempfile::tempdir().expect("data dir");

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
    // Issue #2730: deterministic rendezvous — await the task instead of polling
    // status on a wall-clock budget (see the hijack test above).
    spawn_reindex_awaitable(handle.clone(), progress.clone(), false)
        .await
        .expect("reindex task must not panic");

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
}
