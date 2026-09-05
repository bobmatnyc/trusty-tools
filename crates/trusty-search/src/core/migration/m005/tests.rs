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

/// As [`fixture`], but with an explicit per-index chunk cap.
///
/// Why: the cap is a per-index value since #6369, so a test sets it with
/// `with_chunk_cap` rather than by writing `TRUSTY_MAX_CHUNKS` — an env write
/// leaks into every other test sharing the process.
async fn fixture_with_cap(cap: Option<usize>) -> Fixture {
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
    let mut indexer = CodeIndexer::new("m005-test", root.to_string_lossy().as_ref())
        .with_components(embedder, store.clone() as Arc<dyn VectorStore>);
    if let Some(cap) = cap {
        indexer = indexer.with_chunk_cap(cap);
    }
    indexer.set_corpus_store(Arc::new(corpus));
    let handle = IndexHandle::bare(
        IndexId::new("m005-test"),
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
