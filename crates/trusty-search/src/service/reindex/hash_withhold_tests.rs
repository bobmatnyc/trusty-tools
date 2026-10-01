//! #8976: which content hashes a batch commit records.
//!
//! Why: a recorded hash makes the next reindex skip the file. A hash recorded
//! over chunks that never landed, or an old hash left in place when the new
//! one was withheld, kept a file out of search until a forced reindex.
//! What: end-to-end reindex runs under a chunk cap, a commit that fails at the
//! vector upsert, and the withhold pass's logging and keep rules.
//! Test: `cargo test -p trusty-search -- hash_withhold_tests`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use super::batch::{commit_parsed_and_finalize, BatchCtx, ParsedReadyBatch};
use super::hash::{hash_content, hashes_for};
use super::hash_withhold::{withhold_chunkless_hashes, WITHHELD_HASH};
use super::progress::ReindexProgress;
use super::spawn_reindex_awaitable;
use crate::core::chunker::chunk_ast;
use crate::core::embed::{Embedder, MockEmbedder};
use crate::core::indexer::{CodeIndexer, ParsedBatch};
use crate::core::registry::{IndexHandle, IndexId};
use crate::core::store::{VectorHit, VectorStore};

/// One chunk: the content a file is reverted to.
const ALPHA: &str = "pub fn alpha_8976() -> u8 {\n    1\n}\n";
/// Two chunks: the edit that only half fits under the cap.
const BRAVO: &str =
    "pub fn bravo_one_8976() -> u8 {\n    2\n}\n\npub fn bravo_two_8976() -> u8 {\n    3\n}\n";

fn handle_over(id: &str, root: &Path, indexer: CodeIndexer) -> Arc<IndexHandle> {
    Arc::new(IndexHandle::bare(
        IndexId::new(id),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        root.to_path_buf(),
    ))
}

async fn reindex(handle: &Arc<IndexHandle>) {
    spawn_reindex_awaitable(handle.clone(), Arc::new(ReindexProgress::new()), false)
        .await
        .expect("reindex task must not panic");
}

/// Every landed chunk's content for `file`, joined.
async fn landed_text(handle: &Arc<IndexHandle>, file: &str) -> String {
    let chunks = handle
        .indexer
        .read()
        .await
        .raw_chunks_snapshot()
        .await
        .expect("corpus snapshot");
    chunks
        .into_iter()
        .filter(|c| c.file == file)
        .map(|c| c.content)
        .collect::<Vec<_>>()
        .join("\n")
}

fn ctx_for(handle: &Arc<IndexHandle>) -> BatchCtx {
    BatchCtx {
        handle: handle.clone(),
        progress: Arc::new(ReindexProgress::new()),
        root: handle.root_path.clone(),
        index_id: handle.id.clone(),
        hashes: hashes_for(&handle.id),
        mem_limit: None,
        mem_abort: Arc::new(AtomicBool::new(false)),
        peak_rss_atomic: Arc::new(AtomicU64::new(0)),
        started: Instant::now(),
        total: 1,
        lexical_only: false,
        skip_vector: false,
        defer_embed: false,
        embedder_pid_slot: None,
    }
}

/// The HIGH from the #8976 review. Fails against fe7e377e35: there the
/// withheld hash was dropped, `a.rs` kept ALPHA's hash, and the revert to
/// ALPHA was skipped, leaving BRAVO's half-landed chunk as the file's text.
#[tokio::test]
async fn withheld_hash_overwrites_the_old_one_so_a_revert_reindexes() {
    assert_eq!(
        chunk_ast("a.rs", BRAVO).0.len(),
        2,
        "BRAVO must be 2 chunks"
    );
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    fs::write(root.join("a.rs"), ALPHA).unwrap();
    fs::write(
        root.join("b.rs"),
        "pub fn filler_8976() -> u8 {\n    0\n}\n",
    )
    .unwrap();
    let indexer = CodeIndexer::new("revert-after-withhold-8976", root.clone()).with_chunk_cap(2);
    let handle = handle_over("revert-after-withhold-8976", &root, indexer);
    let hashes = hashes_for(&handle.id);
    let key = PathBuf::from("a.rs");

    reindex(&handle).await;
    assert_eq!(
        hashes.get(&key).map(|h| h.clone()),
        Some(hash_content(ALPHA))
    );

    // The edit: the cap is full, so only one of BRAVO's two chunks lands.
    fs::write(root.join("a.rs"), BRAVO).unwrap();
    reindex(&handle).await;
    assert_eq!(
        hashes.get(&key).map(|h| h.clone()).as_deref(),
        Some(WITHHELD_HASH),
        "a withheld hash must overwrite ALPHA's"
    );

    // The revert must be re-read, not skipped as unchanged.
    fs::write(root.join("a.rs"), ALPHA).unwrap();
    reindex(&handle).await;
    let text = landed_text(&handle, "a.rs").await;
    assert!(
        text.contains("alpha_8976"),
        "a.rs must hold ALPHA: {text:?}"
    );
    assert!(
        !text.contains("bravo"),
        "no BRAVO chunk may survive: {text:?}"
    );
    assert_eq!(
        hashes.get(&key).map(|h| h.clone()),
        Some(hash_content(ALPHA))
    );
}

/// A two-chunk file with one chunk landed records no real hash. Fails
/// against 889f555fc3, which recorded BRAVO's hash over half its chunks.
#[tokio::test]
async fn partial_chunk_landing_withholds_the_hash() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    fs::write(root.join("a.rs"), BRAVO).unwrap();
    let indexer = CodeIndexer::new("partial-landing-8976", root.clone()).with_chunk_cap(1);
    let handle = handle_over("partial-landing-8976", &root, indexer);

    reindex(&handle).await;
    let landed = handle.indexer.read().await.chunk_ids_for_file("a.rs").await;
    assert_eq!(landed.len(), 1, "exactly one chunk fits the cap");
    let hashes = hashes_for(&handle.id);
    assert_eq!(
        hashes
            .get(&PathBuf::from("a.rs"))
            .map(|h| h.clone())
            .as_deref(),
        Some(WITHHELD_HASH)
    );
}

/// Blank content and JSON above the window ceiling have final zero chunks,
/// so they keep their hash and are not re-read on every reindex. Fails when
/// the `final_chunkless` keep rule is removed from `withhold_chunkless_hashes`.
#[tokio::test]
async fn blank_and_too_large_files_keep_their_hash() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    let blank = "\n  \n\n";
    let huge = format!("{{\n{}  \"end\": 0\n}}\n", "  \"k\": 1,\n".repeat(10_001));
    fs::write(root.join("blank.json"), blank).unwrap();
    fs::write(root.join("huge.json"), &huge).unwrap();
    let indexer = CodeIndexer::new("final-chunkless-8976", root.clone());
    let handle = handle_over("final-chunkless-8976", &root, indexer);

    reindex(&handle).await;
    let hashes = hashes_for(&handle.id);
    for (file, content) in [("blank.json", blank), ("huge.json", huge.as_str())] {
        let ids = handle.indexer.read().await.chunk_ids_for_file(file).await;
        assert!(ids.is_empty(), "{file} has no chunks: {ids:?}");
        assert_eq!(
            hashes.get(&PathBuf::from(file)).map(|h| h.clone()),
            Some(hash_content(content)),
            "{file} keeps its hash"
        );
    }
}

/// A store whose upsert always fails, so `commit_parsed_batch` errors after
/// the pre-commit remove has already dropped the file's old chunks.
struct RefusingStore;

#[async_trait::async_trait]
impl VectorStore for RefusingStore {
    async fn upsert(&self, _id: &str, _embedding: Vec<f32>) -> anyhow::Result<()> {
        anyhow::bail!("upsert refused")
    }
    async fn search(&self, _query: &[f32], _top_k: usize) -> anyhow::Result<Vec<VectorHit>> {
        Ok(Vec::new())
    }
    async fn remove(&self, _id: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn len(&self) -> anyhow::Result<usize> {
        Ok(0)
    }
}

/// The commit-error arm. Fails against fe7e377e35: the error returned early
/// and ALPHA's hash survived over a file whose chunks were already removed.
#[tokio::test]
async fn commit_error_clears_the_old_hash() {
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(8));
    let store: Arc<dyn VectorStore> = Arc::new(RefusingStore);
    let indexer = CodeIndexer::new("commit-error-8976", "/tmp/commit-error-8976")
        .with_components(embedder, store);
    let (old, _) = chunk_ast("a.rs", ALPHA);
    indexer
        .commit_parsed_batch(
            ParsedBatch {
                embeddings: vec![None; old.len()],
                chunks: old,
                entities_by_file: vec![],
                parse_ms: 0,
                embed_ms: 0,
                vector_count: 0,
            },
            true,
        )
        .await
        .expect("seed commit without vectors");
    let handle = handle_over(
        "commit-error-8976",
        Path::new("/tmp/commit-error-8976"),
        indexer,
    );
    let ctx = ctx_for(&handle);
    let key = PathBuf::from("a.rs");
    ctx.hashes.insert(key.clone(), hash_content(ALPHA));

    let (new, _) = chunk_ast("a.rs", BRAVO);
    let ready = ParsedReadyBatch {
        parsed: ParsedBatch {
            embeddings: vec![Some(vec![0.5; 8]); new.len()],
            chunks: new,
            entities_by_file: vec![],
            parse_ms: 0,
            embed_ms: 0,
            vector_count: 0,
        },
        new_hashes: vec![(key.clone(), hash_content(BRAVO))],
        batch_files: 1,
        changed_corpus_paths: vec!["a.rs".to_string()],
        final_chunkless_paths: Vec::new(),
    };
    commit_parsed_and_finalize(&ctx, ready).await;

    assert!(
        handle
            .indexer
            .read()
            .await
            .chunk_ids_for_file("a.rs")
            .await
            .is_empty(),
        "the failed commit left a.rs without chunks"
    );
    assert_eq!(
        ctx.progress.errors.load(Ordering::Acquire),
        1,
        "the error is reported"
    );
    assert_eq!(
        ctx.hashes.get(&key).map(|h| h.clone()).as_deref(),
        Some(WITHHELD_HASH),
        "ALPHA's hash must not survive the failed commit"
    );
}

/// Counts WARN events on the current thread.
struct WarnCounter(Arc<AtomicUsize>);

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for WarnCounter {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        if *event.metadata().level() == tracing::Level::WARN {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
}

/// One summary WARN per batch, not one per file. Fails when the WARN moves
/// back inside the per-file loop, as it was at fe7e377e35.
#[tokio::test]
async fn withheld_hashes_log_one_summary_warn_per_batch() {
    use tracing_subscriber::layer::SubscriberExt as _;

    let indexer = CodeIndexer::new("one-warn-8976", "/tmp/one-warn-8976");
    let handle = handle_over("one-warn-8976", Path::new("/tmp/one-warn-8976"), indexer);
    let ctx = ctx_for(&handle);
    let new_hashes: Vec<(PathBuf, String)> = (0..7)
        .map(|i| (PathBuf::from(format!("f{i}.rs")), format!("h{i}")))
        .collect();

    let warns = Arc::new(AtomicUsize::new(0));
    let subscriber = tracing_subscriber::registry().with(WarnCounter(warns.clone()));
    let kept = tracing::subscriber::with_default(subscriber, || {
        withhold_chunkless_hashes(&ctx, new_hashes, &Default::default(), &[])
    });

    assert_eq!(warns.load(Ordering::SeqCst), 1, "one WARN for seven files");
    assert_eq!(kept.len(), 7, "every hash is recorded, none dropped");
    assert!(kept.iter().all(|(_, h)| h == WITHHELD_HASH));
}
