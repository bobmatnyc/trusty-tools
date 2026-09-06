//! Tests for M005 (issue #6581).
//!
//! Why: M005 clears and rebuilds an index's primary keys, so the assertions
//! here are about what an operator gets back — every id in the new shape, the
//! same chunk count a fresh index of the same tree produces, the existing
//! vectors reused rather than re-embedded, and the cap honoured.
//! What: builds a real tempdir tree with a redb corpus and a usearch store,
//! seeds it the way a pre-#6581 binary would have, and runs `apply`.
//! Test: this file.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crate::core::embed::Embedder;
use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::RwLock;

use super::*;
use crate::core::chunk_id::ChunkIdShapes;
use crate::core::chunker::RawChunk;
use crate::core::corpus::CorpusStore;
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId};
use crate::core::store::{UsearchStore, VectorStore};

const DIM: usize = 8;

/// A minified-bundle stand-in: three `function e` declarations, two sharing an
/// identical span and one ending on a different line.
///
/// Why: this is the exact input #6581 is about. Under the pre-#6581 id shape all
/// three collapse onto `bundle.js::Function::e::1` and two are dropped.
const BUNDLE_JS: &str =
    "function e(a){ return a }; function e(b){ return b }; function e(c){ return c\n}\n";

const LIB_RS: &str = "pub fn alpha() -> u32 {\n    1\n}\n\npub fn beta() -> u32 {\n    2\n}\n";

/// An embedder that refuses to embed and counts every attempt.
///
/// Why: the owner ruling's re-embed budget is zero. "Zero" is only provable by
/// an embedder that would be called if the migration embedded anything — a run
/// with no embedder wired proves nothing.
/// What: `embed_batch` bumps `calls` and returns an error, so an implementation
/// that embeds fails loudly rather than quietly costing money.
/// Test: `m005_reuses_the_existing_vectors`.
struct RefusingEmbedder {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Embedder for RefusingEmbedder {
    async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        anyhow::bail!("M005 must not embed: the re-embed budget is zero (#6581)")
    }
    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        self.calls.fetch_add(texts.len().max(1), Ordering::SeqCst);
        anyhow::bail!("M005 must not embed: the re-embed budget is zero (#6581)")
    }
    fn dimension(&self) -> usize {
        DIM
    }
}

/// A deterministic vector for `text`, so a reused vector is identifiable.
fn seed_vector(text: &str) -> Vec<f32> {
    let h = text_hash(text);
    let mut v = vec![0.0f32; DIM];
    for (i, slot) in v.iter_mut().enumerate() {
        *slot = f32::from(h[i]) / 255.0 + 0.01;
    }
    v
}

/// The pre-#6581 chunk id, rebuilt from a current chunk.
///
/// Why: the fixture has to look like a corpus an OLD binary wrote, and the only
/// honest way to produce that is to derive it from what the chunker emits today.
fn legacy_id(chunk: &RawChunk) -> String {
    match crate::core::chunk_id::parse(&chunk.id) {
        Some(p) => match p.shape {
            crate::core::chunk_id::ChunkIdShape::Named {
                chunk_type,
                name,
                start_line,
                ..
            } => format!("{}::{chunk_type}::{name}::{start_line}", p.file),
            _ => chunk.id.clone(),
        },
        None => chunk.id.clone(),
    }
}

struct Fixture {
    _tmp: tempfile::TempDir,
    handle: IndexHandle,
    store: Arc<UsearchStore>,
    embed_calls: Arc<AtomicUsize>,
    root: std::path::PathBuf,
}

/// Build a tree, chunk it the way a pre-#6581 binary would have, and seed the
/// corpus + vector store from that.
async fn fixture() -> Fixture {
    fixture_with_cap(None).await
}

/// As [`fixture`], but registered under `id` instead of `m005-test`.
///
/// Why: the per-index semaphore and the #3049 cancel flag are keyed by
/// `IndexId` in process-global maps, so a test that SETS either one under the
/// shared `m005-test` id perturbs every sibling running beside it. A test that
/// mutates that shared state takes its own id (#6581).
/// Test: `m005_stops_before_the_clear_when_a_delete_signals_cancel`.
async fn fixture_named(id: &str) -> Fixture {
    fixture_inner(id, None).await
}

/// As [`fixture`], but with an explicit per-index chunk cap.
///
/// Why: the cap is a per-index value since #6369, so a test sets it with
/// `with_chunk_cap` rather than by writing `TRUSTY_MAX_CHUNKS` — an env write
/// leaks into every other test sharing the process.
async fn fixture_with_cap(cap: Option<usize>) -> Fixture {
    fixture_inner("m005-test", cap).await
}

async fn fixture_inner(index_id: &str, cap: Option<usize>) -> Fixture {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join(".trusty-search")).unwrap();
    std::fs::write(root.join("src/lib.rs"), LIB_RS).unwrap();
    std::fs::write(root.join("bundle.js"), BUNDLE_JS).unwrap();

    // Chunk as today, then rewrite to the pre-#6581 shape and collapse the
    // collisions exactly as #6571's dedupe did — that IS the legacy corpus.
    let mut legacy_chunks: Vec<RawChunk> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for file in ["src/lib.rs", "bundle.js"] {
        let content = std::fs::read_to_string(root.join(file)).unwrap();
        let (chunks, _) = chunk_ast(file, &content);
        for mut c in chunks {
            // The dup tail is a post-#6581 invention; the legacy corpus never
            // held one, so strip it before rewriting to the legacy shape.
            let (base, _, _) = crate::core::chunk_id::split_tails(&c.id);
            c.id = base.to_string();
            c.id = legacy_id(&c);
            if seen.insert(c.id.clone()) {
                legacy_chunks.push(c);
            }
        }
    }
    assert!(
        legacy_chunks.iter().any(|c| c.file == "bundle.js"),
        "fixture must carry the minified bundle"
    );

    let corpus = CorpusStore::open(&root.join(".trusty-search").join("index.redb"))
        .expect("open corpus store");
    corpus.upsert_chunks(&legacy_chunks).expect("seed corpus");
    corpus.write_schema_version_sync(4).expect("stamp v4");

    let store = Arc::new(UsearchStore::new(DIM).expect("usearch"));
    for c in &legacy_chunks {
        store.upsert(&c.id, seed_vector(&c.content)).await.unwrap();
    }
    store
        .save_to(&root.join(".trusty-search").join("hnsw.usearch"))
        .await
        .unwrap();

    let embed_calls = Arc::new(AtomicUsize::new(0));
    let embedder: Arc<dyn Embedder> = Arc::new(RefusingEmbedder {
        calls: Arc::clone(&embed_calls),
    });
    let mut indexer = CodeIndexer::new(index_id, root.to_string_lossy().as_ref())
        .with_components(embedder, store.clone() as Arc<dyn VectorStore>);
    if let Some(cap) = cap {
        indexer = indexer.with_chunk_cap(cap);
    }
    indexer.set_corpus_store(Arc::new(corpus));
    let handle = IndexHandle::bare(
        IndexId::new(index_id),
        Arc::new(RwLock::new(indexer)),
        root.clone(),
    );
    Fixture {
        _tmp: tmp,
        handle,
        store,
        embed_calls,
        root,
    }
}

/// Chunk the same tree from scratch — the count a fresh index would hold.
fn fresh_chunk_count(root: &std::path::Path) -> usize {
    let mut n = 0;
    for file in ["src/lib.rs", "bundle.js"] {
        let content = std::fs::read_to_string(root.join(file)).unwrap();
        n += chunk_ast(file, &content).0.len();
    }
    n
}

#[test]
fn m005_advances_exactly_one_version() {
    let m = M005ChunkIdEndLine;
    assert_eq!(m.source_version(), 4);
    assert_eq!(m.target_version(), 5);
    assert_eq!(m.target_version(), super::super::M005_TARGET_VERSION);
    assert!(m.description().contains("M005"));
}

#[test]
fn identical_text_hashes_equal() {
    assert_eq!(text_hash("fn a() {}"), text_hash("fn a() {}"));
    assert_ne!(text_hash("fn a() {}"), text_hash("fn b() {}"));
}

/// The whole point of the migration: every chunk carries the new id shape
/// afterwards, and the corpus holds exactly what a fresh index of the same tree
/// would — which is MORE than it held before, because the collisions #6571
/// dropped are recovered.
#[tokio::test]
async fn m005_rechunks_the_whole_corpus() {
    let f = fixture().await;
    let before = {
        let idx = f.handle.indexer.read().await;
        idx.corpus_store().unwrap().load_all_chunks().unwrap().len()
    };

    M005ChunkIdEndLine.apply(&f.handle).await.expect("apply");

    let after = {
        let idx = f.handle.indexer.read().await;
        idx.corpus_store().unwrap().load_all_chunks().unwrap()
    };
    let expected = fresh_chunk_count(&f.root);
    assert_eq!(
        after.len(),
        expected,
        "a migrated corpus must hold what a fresh index of the same tree holds"
    );
    assert!(
        after.len() > before,
        "the collisions #6571 dropped must be recovered ({before} -> {})",
        after.len()
    );
    for c in &after {
        let parsed = crate::core::chunk_id::parse(&c.id)
            .unwrap_or_else(|| panic!("unparseable id after migration: {}", c.id));
        assert!(
            !parsed.is_legacy_named(),
            "id still in the pre-#6581 shape: {}",
            c.id
        );
    }
    let ids: std::collections::HashSet<&String> = after.iter().map(|c| &c.id).collect();
    assert_eq!(ids.len(), after.len(), "migrated ids must be unique");
}

/// The ruling's item 4: zero re-embed. The embedder here refuses and counts, so
/// an implementation that embeds unchanged text fails the run outright; the
/// vector store must come out the same size with its vectors re-pointed at the
/// new ids.
#[tokio::test]
async fn m005_reuses_the_existing_vectors() {
    let f = fixture().await;
    let vectors_before = f.store.len().await.unwrap();

    M005ChunkIdEndLine.apply(&f.handle).await.expect("apply");

    assert_eq!(
        f.embed_calls.load(Ordering::SeqCst),
        0,
        "M005 must enqueue no embed job for unchanged text (#6581 ruling item 4)"
    );
    assert_eq!(
        f.store.len().await.unwrap(),
        vectors_before,
        "no vector may be added or lost by the migration"
    );

    // Every stored vector must now answer under a CURRENT chunk id that the
    // corpus actually holds — the reuse, end to end.
    let corpus_ids: std::collections::HashSet<String> = {
        let idx = f.handle.indexer.read().await;
        idx.corpus_store()
            .unwrap()
            .load_all_chunks()
            .unwrap()
            .into_iter()
            .map(|c| c.id)
            .collect()
    };
    let probe = seed_vector(LIB_RS.split("\n\n").next().unwrap());
    let hits = f.store.search(&probe, vectors_before).await.unwrap();
    assert!(!hits.is_empty(), "the store must still answer");
    for hit in &hits {
        assert!(
            corpus_ids.contains(&hit.chunk_id),
            "vector left keyed by an id the corpus no longer holds: {}",
            hit.chunk_id
        );
    }
}

/// Running `apply` twice must leave the second run with nothing to do — the
/// crash-retry contract every migration owes.
#[tokio::test]
async fn m005_is_a_no_op_on_an_already_migrated_corpus() {
    let f = fixture().await;
    M005ChunkIdEndLine.apply(&f.handle).await.expect("first");
    let first: Vec<String> = {
        let idx = f.handle.indexer.read().await;
        let mut v: Vec<String> = idx
            .corpus_store()
            .unwrap()
            .load_all_chunks()
            .unwrap()
            .into_iter()
            .map(|c| c.id)
            .collect();
        v.sort();
        v
    };

    M005ChunkIdEndLine.apply(&f.handle).await.expect("second");
    let second: Vec<String> = {
        let idx = f.handle.indexer.read().await;
        let mut v: Vec<String> = idx
            .corpus_store()
            .unwrap()
            .load_all_chunks()
            .unwrap()
            .into_iter()
            .map(|c| c.id)
            .collect();
        v.sort();
        v
    };
    assert_eq!(first, second, "a second apply must change nothing");
    assert_eq!(f.embed_calls.load(Ordering::SeqCst), 0);
}

/// The ruling's item 2 tail: the chunk cap applies exactly as today. With the
/// cap below what the tree needs, the migration commits up to the cap and says
/// so rather than committing past it.
#[tokio::test]
async fn m005_honours_the_chunk_cap() {
    let f = fixture_with_cap(Some(2)).await;
    assert!(
        fresh_chunk_count(&f.root) > 2,
        "fixture must exceed the cap this test sets"
    );

    M005ChunkIdEndLine
        .apply(&f.handle)
        .await
        .expect("a capped migration still completes");

    let after = {
        let idx = f.handle.indexer.read().await;
        idx.corpus_store().unwrap().load_all_chunks().unwrap().len()
    };
    assert!(
        after <= 2,
        "the cap must bound the migrated corpus, got {after}"
    );
}

/// The ruling's item 3, per index: an index below the M005 marker matches both
/// named shapes, one at or above it matches only the new one.
#[tokio::test]
async fn the_suffix_policy_narrows_per_index() {
    let legacy = "src/auth.rs::Function::login::10";
    let current = "src/auth.rs::Function::login::10::40";
    let registry = super::super::MigrationRegistry::new();

    // Index A — stamped below the marker.
    let a = fixture().await;
    a.handle
        .indexer
        .read()
        .await
        .corpus_store()
        .unwrap()
        .write_schema_version_sync(4)
        .unwrap();
    let shapes_a = a.handle.indexer.read().await.chunk_id_shapes();
    assert_eq!(shapes_a, ChunkIdShapes::NewAndLegacy);
    assert!(crate::core::chunk_id::is_valid_suffix(
        &legacy["src/auth.rs".len()..],
        shapes_a
    ));

    // Index B — the runner walks it to the marker, which narrows the policy.
    let b = fixture().await;
    super::super::run_migrations(&b.handle, &registry)
        .await
        .expect("migrations");
    let shapes_b = b.handle.indexer.read().await.chunk_id_shapes();
    assert_eq!(
        shapes_b,
        ChunkIdShapes::NewOnly,
        "an index that has run M005 must stop accepting the legacy shape"
    );
    assert!(!crate::core::chunk_id::is_valid_suffix(
        &legacy["src/auth.rs".len()..],
        shapes_b
    ));
    assert!(crate::core::chunk_id::is_valid_suffix(
        &current["src/auth.rs".len()..],
        shapes_b
    ));

    // A is untouched by B's migration — the policy is per index, not global.
    assert_eq!(
        a.handle.indexer.read().await.chunk_id_shapes(),
        ChunkIdShapes::NewAndLegacy
    );
}

/// Incremental reindex after the migration must keep an unchanged declaration's
/// id stable — parent/child references and `Test:` pointers both rely on it.
#[tokio::test]
async fn ids_are_stable_across_a_reindex_of_unchanged_files() {
    let f = fixture().await;
    M005ChunkIdEndLine.apply(&f.handle).await.expect("apply");
    let migrated: Vec<String> = {
        let idx = f.handle.indexer.read().await;
        let mut v: Vec<String> = idx
            .corpus_store()
            .unwrap()
            .load_all_chunks()
            .unwrap()
            .into_iter()
            .map(|c| c.id)
            .collect();
        v.sort();
        v
    };

    // Re-chunk the same unchanged files the way an incremental reindex would.
    let mut rechunked: Vec<String> = Vec::new();
    for file in ["src/lib.rs", "bundle.js"] {
        let content = std::fs::read_to_string(f.root.join(file)).unwrap();
        rechunked.extend(chunk_ast(file, &content).0.into_iter().map(|c| c.id));
    }
    rechunked.sort();
    assert_eq!(
        migrated, rechunked,
        "an unchanged file must re-chunk to the same ids the migration produced"
    );
}

// ─── Crash recovery (#6581, code-critic BLOCK) ───────────────────────────────
//
// The guard used to answer "is a pass outstanding" by looking for a pre-#6581
// id in the corpus. Step 3 clears that corpus, so after any interruption the
// answer was `false`, `apply` returned `Ok`, and `run_migrations` stamped
// schema_version = 5 over a partial or empty index. These tests pin the
// durable-marker guard that replaced it.

/// Put the index in the exact on-disk state a crash between the corpus clear
/// and the first batch commit leaves behind: the plan durable, the corpus empty.
async fn simulate_crash_after_clear(fx: &Fixture) -> plan::M005Plan {
    let corpus = {
        let indexer = fx.handle.indexer.read().await;
        indexer.corpus_store().expect("fixture wires a corpus")
    };
    let old_chunks = corpus.load_all_chunks().expect("load");
    let mut vector_by_text: HashMap<[u8; 32], String> = HashMap::new();
    let mut files: BTreeSet<String> = BTreeSet::new();
    for chunk in &old_chunks {
        files.insert(chunk.file.clone());
        vector_by_text
            .entry(text_hash(&chunk.content))
            .or_insert_with(|| chunk.id.clone());
    }
    let plan = plan::M005Plan {
        files,
        vector_by_text: vector_by_text.into_iter().collect(),
        old_ids: old_chunks.iter().map(|c| c.id.clone()).collect(),
        old_count: old_chunks.len(),
    };
    // Order matters: the real pass persists the plan BEFORE it clears, which is
    // what makes every crashable state carry the marker.
    plan.store(&corpus).await.expect("store plan");
    {
        let indexer = fx.handle.indexer.read().await;
        indexer.clear_corpus_for_rechunk().await.expect("clear");
    }
    assert_eq!(
        corpus.load_all_chunks().expect("reload").len(),
        0,
        "the simulated crash must leave the corpus empty"
    );
    plan
}

/// The plan survives a round trip through the corpus `_meta` table.
///
/// Why: the marker is the whole guard. If it did not persist, or came back
/// different, the crash window this fix closes would silently reopen.
/// What: stores a plan, reads it back, compares, then clears it.
/// Test: this test.
#[tokio::test]
async fn m005_plan_roundtrips_through_the_corpus_meta_table() {
    let fx = fixture().await;
    let stored = simulate_crash_after_clear(&fx).await;
    let corpus = {
        let indexer = fx.handle.indexer.read().await;
        indexer.corpus_store().unwrap()
    };
    let loaded = plan::M005Plan::load(&corpus)
        .await
        .expect("load")
        .expect("a plan was stored");
    assert_eq!(loaded, stored, "the plan must survive the round trip");
    plan::M005Plan::clear(&corpus).await.expect("clear");
    assert!(
        plan::M005Plan::load(&corpus).await.expect("load").is_none(),
        "clearing must retire the marker"
    );
}

/// A crash after the clear and before the first batch is RECOVERED, not
/// mistaken for a finished migration.
///
/// Why: this is the CRITICAL fail-open the code critic blocked on. With the old
/// contents-based guard, `apply` over this state found no legacy id, returned
/// `Ok`, and let `run_migrations` mark the index migrated with zero chunks —
/// total, silent, permanent loss reported as success.
/// What: simulates the crash, re-runs `apply`, and asserts the corpus comes back
/// fully re-chunked in the new id shape with the marker retired.
/// Test: this test.
#[tokio::test]
async fn m005_resumes_after_a_crash_before_the_first_batch() {
    let fx = fixture().await;
    simulate_crash_after_clear(&fx).await;

    M005ChunkIdEndLine
        .apply(&fx.handle)
        .await
        .expect("the resumed pass must succeed");

    let corpus = {
        let indexer = fx.handle.indexer.read().await;
        indexer.corpus_store().unwrap()
    };
    let after = corpus.load_all_chunks().expect("reload");
    assert_eq!(
        after.len(),
        fresh_chunk_count(&fx.root),
        "recovery must restore every chunk a fresh index would hold, not leave the corpus empty"
    );
    assert!(
        after
            .iter()
            .all(|c| !crate::core::chunk_id::parse(&c.id).is_some_and(|p| p.is_legacy_named())),
        "every recovered id must carry the new shape"
    );
    assert!(
        plan::M005Plan::load(&corpus).await.expect("load").is_none(),
        "a completed pass must retire its marker"
    );
    assert_eq!(
        fx.embed_calls.load(Ordering::SeqCst),
        0,
        "recovery must not re-embed — the ruling's budget is zero on the resume path too"
    );
}

/// A crash between the sidecar remap (Step 4) and the flush (Step 5) recovers,
/// still without re-embedding.
///
/// Why: the remap rewrites the HNSW sidecar from old ids to new. A resume from
/// that state re-applies a mapping whose left-hand side is already gone, which
/// must be a no-op rather than a corruption or a re-embed.
/// What: runs a full pass, then re-arms the marker to put the index back in the
/// "pass did not finish" state with the corpus and sidecar already on new ids,
/// and re-runs `apply`.
/// Test: this test.
#[tokio::test]
async fn m005_resumes_after_a_crash_between_the_remap_and_the_flush() {
    let fx = fixture().await;
    M005ChunkIdEndLine
        .apply(&fx.handle)
        .await
        .expect("first pass");

    let corpus = {
        let indexer = fx.handle.indexer.read().await;
        indexer.corpus_store().unwrap()
    };
    let post_remap = corpus.load_all_chunks().expect("reload");
    let expected = post_remap.len();

    // Re-arm the marker: the on-disk shape of "Step 4 committed, Step 7 never ran".
    let plan = plan::M005Plan {
        files: post_remap.iter().map(|c| c.file.clone()).collect(),
        vector_by_text: post_remap
            .iter()
            .map(|c| (text_hash(&c.content), c.id.clone()))
            .collect(),
        old_ids: post_remap.iter().map(|c| c.id.clone()).collect(),
        old_count: post_remap.len(),
    };
    plan.store(&corpus).await.expect("re-arm");

    M005ChunkIdEndLine
        .apply(&fx.handle)
        .await
        .expect("the resumed pass must succeed");

    assert_eq!(
        corpus.load_all_chunks().expect("reload").len(),
        expected,
        "a resume over already-migrated state must converge on the same corpus"
    );
    assert!(
        plan::M005Plan::load(&corpus).await.expect("load").is_none(),
        "the resumed pass must retire its marker"
    );
    assert_eq!(
        fx.embed_calls.load(Ordering::SeqCst),
        0,
        "no text was re-embedded on either pass"
    );
}

/// `run_migrations` never stamps schema_version 5 over a corpus the interrupted
/// pass emptied.
///
/// Why: the version write is the permanent part. The old guard's `Ok` over an
/// empty corpus is what let the loss become unrecoverable, because a v5 index
/// never runs M005 again.
/// What: simulates the crash, runs the real migration runner, and asserts the
/// index only reaches 5 WITH its chunks restored.
/// Test: this test.
#[tokio::test]
async fn m005_never_advances_the_schema_over_missing_chunks() {
    let fx = fixture().await;
    simulate_crash_after_clear(&fx).await;

    let registry = crate::core::migration::MigrationRegistry::new();
    crate::core::migration::run_migrations(&fx.handle, &registry)
        .await
        .expect("the runner must recover rather than fail");

    let corpus = {
        let indexer = fx.handle.indexer.read().await;
        indexer.corpus_store().unwrap()
    };
    let chunks = corpus.load_all_chunks().expect("reload").len();
    let version = fx.handle.read_schema_version().await.expect("read version");
    assert_eq!(version, super::super::M005_TARGET_VERSION);
    assert_eq!(
        chunks,
        fresh_chunk_count(&fx.root),
        "reaching v{version} with {chunks} chunks would be the #6581 fail-open"
    );
}

/// A query landing inside the migration window is REFUSED, not answered with an
/// empty result set.
///
/// Why: the pass empties the corpus for the length of its re-chunk while every
/// read still succeeds, so a concurrent search returned `results: []` at HTTP
/// 200 — indistinguishable from a genuine miss.
/// What: raises the window by hand, searches, and asserts the typed refusal;
/// then asserts the guard's drop reopens the index.
/// Test: this test.
#[tokio::test]
async fn a_search_during_the_migration_window_is_refused_not_empty() {
    let fx = fixture().await;
    let flag = {
        let indexer = fx.handle.indexer.read().await;
        indexer.migration_flag()
    };
    let query = crate::core::indexer::SearchQuery {
        text: "alpha".to_string(),
        ..Default::default()
    };
    {
        let _window = crate::core::indexer::MigrationWindow::open(flag);
        let indexer = fx.handle.indexer.read().await;
        assert!(indexer.is_migrating(), "the window must be open");
        let err = indexer
            .search(&query)
            .await
            .expect_err("a search inside the window must refuse");
        assert!(
            err.downcast_ref::<crate::core::indexer::IndexMigrationInProgress>()
                .is_some(),
            "the refusal must be typed so the HTTP layer can render it as 503: {err:#}"
        );
    }
    // The guard drops with the window, so the index serves again.
    let indexer = fx.handle.indexer.read().await;
    assert!(
        !indexer.is_migrating(),
        "a failed or finished migration must not leave the index refusing forever"
    );
}

// ─── Reindex exclusion and marker corruption (#6581, code-critic round 2) ────

/// M005 waits for the per-index permit before it clears anything, so a reindex
/// already holding it can never snapshot a cleared corpus.
///
/// Why: this is the CRITICAL the round-2 critic blocked on. A reindex holds this
/// permit for its whole run — including `copy_all_from`, which snapshots the
/// LIVE corpus to carry hash-skipped unchanged files across the staged-corpus
/// swap (#839), and `commit_staged_corpus_swap`, which renames that staging file
/// over the live one. With nothing serializing the two, a snapshot taken while
/// M005 held the corpus emptied copied nothing, and the swap then discarded
/// every unchanged file's chunks with no error. Because the permit is a single
/// 1-permit semaphore, proving M005 blocks on it also proves the reverse
/// direction: a reindex cannot start inside M005's window either.
/// What: takes the permit (standing in for a running reindex), starts the
/// migration, and asserts it has NOT touched the corpus while the permit is
/// held; then releases and asserts the pass completes fully.
/// Test: this test.
#[tokio::test]
async fn m005_waits_for_the_index_permit_before_clearing_the_corpus() {
    let fx = fixture().await;
    let corpus = {
        let indexer = fx.handle.indexer.read().await;
        indexer.corpus_store().unwrap()
    };
    let before = corpus.load_all_chunks().expect("load").len();
    assert!(before > 0, "the fixture seeds a populated legacy corpus");

    // The reindex holds this for its whole run (`reindex::runner`).
    let reindex_permit = crate::service::reindex::index_semaphore(&fx.handle.id)
        .acquire_owned()
        .await
        .expect("permit");

    let registry = crate::core::migration::MigrationRegistry::new();
    // Blocked on the permit, so it cannot finish. The cancelled future never
    // acquired anything, so nothing partial is left behind.
    let blocked = tokio::time::timeout(
        std::time::Duration::from_millis(250),
        crate::core::migration::run_migrations_exclusive(&fx.handle, &registry),
    )
    .await;
    assert!(
        blocked.is_err(),
        "M005 must block on the permit a reindex holds, not run beside it"
    );
    assert_eq!(
        corpus.load_all_chunks().expect("reload").len(),
        before,
        "M005 must not have cleared the corpus a reindex could still snapshot"
    );
    assert_eq!(
        fx.handle.read_schema_version().await.expect("version"),
        4,
        "nothing may advance while the migration is blocked"
    );

    drop(reindex_permit);
    crate::core::migration::run_migrations_exclusive(&fx.handle, &registry)
        .await
        .expect("the migration then runs");

    assert_eq!(
        corpus.load_all_chunks().expect("reload").len(),
        fresh_chunk_count(&fx.root),
        "once the reindex releases, the pass completes and loses nothing"
    );
    assert_eq!(
        fx.handle.read_schema_version().await.expect("version"),
        super::super::M005_TARGET_VERSION
    );
}

/// An unreadable plan blob over an already-cleared corpus fails CLOSED.
///
/// Why: the marker is only ever written before the clear, so an unreadable one
/// means a pass started and its recovery inputs are gone. Degrading that to "no
/// plan" fell through to the corpus-contents check, which reads `false` over the
/// very corpus that pass emptied — reopening the round-1 fail-open through
/// corruption of the fix's own marker.
/// What: writes garbage under the plan key, clears the corpus, and asserts the
/// migration REFUSES rather than recording success, leaving the version at 4.
/// Test: this test.
#[tokio::test]
async fn an_unreadable_plan_over_a_cleared_corpus_fails_closed() {
    let fx = fixture().await;
    let corpus = {
        let indexer = fx.handle.indexer.read().await;
        indexer.corpus_store().unwrap()
    };
    corpus
        .write_m005_plan_sync(b"{ not json at all")
        .expect("store a corrupt marker");
    {
        let indexer = fx.handle.indexer.read().await;
        indexer.clear_corpus_for_rechunk().await.expect("clear");
    }

    let err = M005ChunkIdEndLine
        .apply(&fx.handle)
        .await
        .expect_err("an unreadable marker over a cleared corpus must refuse");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("unreadable"),
        "the refusal must name the cause: {msg}"
    );

    let registry = crate::core::migration::MigrationRegistry::new();
    let _ = crate::core::migration::run_migrations_exclusive(&fx.handle, &registry).await;
    assert_eq!(
        fx.handle.read_schema_version().await.expect("version"),
        4,
        "a refused pass must never advance the schema version"
    );
}

// ─── Delete cancellation (#6581, code-critic round 3) ────────────────────────

/// Clear `id`'s cancel flag and evict it, so nothing leaks into a sibling test.
///
/// Why: the flag lives in a process-global `DashMap`, and a test that leaves one
/// set aborts the next pass over that id. Each cancel test already takes its own
/// index id; this is the belt to that suspenders.
fn release_cancel(id: &crate::core::registry::IndexId) {
    crate::service::reindex::clear_index_cancel(id);
}

/// A cancel already signalled when `apply` reaches Step 3 stops the pass with a
/// distinct error, having destroyed nothing.
///
/// Why: `unregister_index` signals the cancel and then waits
/// `DELETE_QUIESCE_TIMEOUT` on the teardown lock's EXCLUSIVE side, which
/// `run_migrations_exclusive` holds the shared side of for the whole pass. With
/// no checkpoint, M005 re-chunks the entire corpus first, the wait expires, and
/// a `delete_data=true` DELETE takes the #3049 abandon branch — telling the
/// operator to re-issue a delete that will abandon again for as long as the
/// migration runs.
/// What: signals the cancel, applies, and asserts the typed abort, the intact
/// corpus, the surviving plan marker and the unchanged schema version — then
/// clears the cancel and asserts the pass resumes to completion.
/// Test: this test.
#[tokio::test]
async fn m005_stops_before_the_clear_when_a_delete_signals_cancel() {
    let fx = fixture_named("m005-cancel-before-clear").await;
    let corpus = {
        let indexer = fx.handle.indexer.read().await;
        indexer.corpus_store().unwrap()
    };
    let before = corpus.load_all_chunks().expect("load").len();
    assert!(before > 0, "the fixture seeds a populated legacy corpus");

    crate::service::reindex::signal_index_cancel(&fx.handle.id);

    let err = M005ChunkIdEndLine
        .apply(&fx.handle)
        .await
        .expect_err("a signalled cancel must stop the pass");
    assert!(
        err.downcast_ref::<M005Cancelled>().is_some(),
        "the abort must be typed so a caller can tell it from a failure: {err:#}"
    );
    assert_eq!(
        corpus.load_all_chunks().expect("reload").len(),
        before,
        "stopping before the clear must leave the corpus whole"
    );
    assert!(
        plan::M005Plan::load(&corpus)
            .await
            .expect("load plan")
            .is_some(),
        "Step 7 never ran, so the marker that resumes the pass must survive"
    );

    // The runner applies `?` to `apply` before `write_schema_version`, so the
    // abort pins the version at 4 — a v5 stamp here would record an unmigrated
    // corpus as migrated, which is the #6581 fail-open.
    let registry = crate::core::migration::MigrationRegistry::new();
    crate::core::migration::run_migrations(&fx.handle, &registry)
        .await
        .expect_err("the runner must surface the abort");
    assert_eq!(
        fx.handle.read_schema_version().await.expect("version"),
        4,
        "an aborted pass must never advance the schema version"
    );

    // The delete was abandoned or refused and the index survived: the pass has
    // to converge, not stay stuck behind its own marker.
    release_cancel(&fx.handle.id);
    crate::core::migration::run_migrations(&fx.handle, &registry)
        .await
        .expect("the pass resumes once the cancel clears");
    assert_eq!(
        corpus.load_all_chunks().expect("reload").len(),
        fresh_chunk_count(&fx.root),
        "the resumed pass loses nothing"
    );
    assert_eq!(
        fx.handle.read_schema_version().await.expect("version"),
        super::super::M005_TARGET_VERSION
    );
}

/// `rechunk_all` polls the cancel flag at the batch boundary, not only once
/// before it starts.
///
/// Why: the boundary check is the one that matters for a LARGE index — a delete
/// signalled halfway through a multi-minute re-chunk is exactly the case where
/// waiting for the pass to end blows the 30s quiesce budget. `apply`'s check
/// runs before the clear and so can never observe that arrival.
/// What: calls `rechunk_all` directly with the flag already set, which puts the
/// clear behind the checkpoint: the corpus is emptied and the FIRST batch
/// boundary then aborts. Asserts the typed abort and that nothing was committed.
/// Test: this test.
#[tokio::test]
async fn rechunk_all_stops_at_a_batch_boundary_when_a_delete_signals_cancel() {
    let fx = fixture_named("m005-cancel-at-batch").await;
    let corpus = {
        let indexer = fx.handle.indexer.read().await;
        indexer.corpus_store().unwrap()
    };
    let files: std::collections::BTreeSet<String> = corpus
        .load_all_chunks()
        .expect("load")
        .into_iter()
        .map(|c| c.file)
        .collect();
    assert!(!files.is_empty(), "the fixture seeds files to re-chunk");

    crate::service::reindex::signal_index_cancel(&fx.handle.id);
    let indexer_arc = Arc::clone(&fx.handle.indexer);
    let err = super::rechunk_all(
        &fx.handle,
        &indexer_arc,
        &fx.root,
        &files,
        &std::collections::HashMap::new(),
        0,
    )
    .await
    .expect_err("the first batch boundary must abort");
    release_cancel(&fx.handle.id);

    assert!(
        err.downcast_ref::<M005Cancelled>().is_some(),
        "the abort must be typed: {err:#}"
    );
    assert!(
        corpus.load_all_chunks().expect("reload").is_empty(),
        "the clear ran, so the abort came from the batch boundary and not from \
         `apply`'s earlier check"
    );
}
