//! Per-stage cost profile of a full and an incremental reindex (issue #5024).
//!
//! Why: #5024 proposes a global content-addressed embedding cache, and a
//! separate proposal wants to seed a new worktree's index by copying an already
//! indexed clone's corpus and delta-reindexing. Both are bets on where reindex
//! time actually goes. This harness measures it instead of estimating, using the
//! `StageTimings` breakdown the same PR adds to the `complete` event.
//!
//! What: builds a throwaway corpus in a temp dir with colocated
//! `.trusty-search/` storage (so every redb/HNSW artifact — live AND staging —
//! stays inside the temp dir), wires a real `FastEmbedder` + `UsearchStore` +
//! `CorpusStore`, runs a cold force reindex followed by a warm incremental one
//! with a handful of files touched, and prints both breakdowns as ms and
//! percent-of-total.
//!
//! Safety: this NEVER touches the operator's running daemon, `~/.trusty-search`,
//! or any existing `.trusty-search/` belonging to a real checkout. Everything
//! lives under a `tempfile::TempDir` that is removed on drop. See issue #402 for
//! why that boundary is not negotiable.
//!
//! Run: cargo test -p trusty-search --test reindex_stage_profile \
//!        -- --include-ignored --nocapture
//!
//! Test: `#[ignore]`d — it needs the ONNX model and takes minutes, so it is a
//! measurement tool, not a CI gate.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use trusty_search::core::corpus::CorpusStore;
use trusty_search::core::indexer::CodeIndexer;
use trusty_search::core::registry::{IndexHandle, IndexId};
use trusty_search::core::store::{UsearchStore, VectorStore};
use trusty_search::core::{Embedder, FastEmbedder};
use trusty_search::service::reindex::{spawn_reindex, ReindexProgress, ReindexStatus};

/// Stage rows printed in the breakdown, in pipeline order.
///
/// Why: one list keeps the table order and the percent maths in sync.
/// What: `(json key, display label)` pairs read out of the `complete` event's
/// `timings` object. `pipeline_ms` is the batch loop as a whole; the four
/// subsystem figures nested inside it are printed separately below the table.
const STAGE_ROWS: &[(&str, &str)] = &[
    ("walk_ms", "walk"),
    ("hash_cache_ms", "hash-cache load"),
    ("carryover_ms", "corpus carryover copy"),
    ("pipeline_ms", "batch pipeline"),
    ("prune_ms", "prune deleted"),
    ("hnsw_commit_ms", "HNSW commit"),
    ("corpus_commit_ms", "corpus commit"),
    ("kg_ms", "KG / symbol graph"),
    ("poller_stop_ms", "RSS poller teardown"),
    ("other_ms", "other (unattributed)"),
];

/// Subsystem costs measured INSIDE `pipeline_ms`.
const PIPELINE_SUBROWS: &[(&str, &str)] = &[
    ("parse_ms", "parse/chunk"),
    ("embed_ms", "embed"),
    ("bm25_ms", "bm25 index"),
    ("vector_upsert_ms", "HNSW upsert"),
];

/// Copy up to `limit` `.rs` files from this crate's own `src/` into `dest`.
///
/// Why: a realistic Rust corpus with realistic chunk sizes and symbol density
/// beats synthetic filler, and this crate's `src/` is right here.
/// What: flat copy preserving relative paths; returns the copied paths in walk
/// order so the warm pass can touch a deterministic subset.
fn stage_corpus(dest: &Path, limit: usize) -> Vec<PathBuf> {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut copied = Vec::new();
    let mut entries: Vec<_> = walkdir::WalkDir::new(&src)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("rs"))
        .map(|e| e.path().to_path_buf())
        .collect();
    entries.sort();
    for path in entries.into_iter().take(limit) {
        let Ok(rel) = path.strip_prefix(&src) else {
            continue;
        };
        let target = dest.join(rel);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).expect("create corpus subdir");
        }
        if std::fs::copy(&path, &target).is_ok() {
            copied.push(target);
        }
    }
    copied
}

/// Build a fully-wired handle over `root` with colocated storage.
///
/// Why: staging, the hash cache, and the HNSW snapshot all route to
/// `.trusty-search/` when it exists — which is exactly what keeps this harness
/// inside the temp dir instead of the operator's data dir.
async fn build_handle(root: &Path, id: &str) -> Arc<IndexHandle> {
    let colocated = root.join(".trusty-search");
    std::fs::create_dir_all(&colocated).expect("create colocated storage dir");

    let embedder: Arc<dyn Embedder> = Arc::new(
        FastEmbedder::new()
            .await
            .expect("init FastEmbedder (downloads the ONNX model on first run)"),
    );
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(384).expect("init UsearchStore"));
    let mut indexer = CodeIndexer::new(id, root.to_path_buf()).with_components(embedder, store);
    let corpus = CorpusStore::open(&colocated.join("index.redb")).expect("open corpus store");
    indexer.set_corpus_store(Arc::new(corpus));

    let mut handle = IndexHandle::bare(
        IndexId::new(id),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        root.to_path_buf(),
    );
    // `bare` defaults `defer_embed: true`, which pushes embedding into a
    // background pass and reports `embed_ms: 0` — useless for sizing the embed
    // share. Force the inline path so `embed_ms` is real.
    handle.defer_embed = false;
    Arc::new(handle)
}

/// Run one reindex to a terminal status and return its `complete` event.
///
/// Why: the timings live on the terminal SSE event, and `spawn_reindex` is
/// fire-and-forget, so the harness has to poll for the terminal status.
async fn run_reindex(handle: Arc<IndexHandle>, force: bool) -> Value {
    let progress = Arc::new(ReindexProgress::new());
    spawn_reindex(Arc::clone(&handle), Arc::clone(&progress), force);

    // Generous ceiling: a cold pass over ~400 real source files with a live
    // embedder is minutes, not seconds.
    for _ in 0..3_600 {
        if progress.status.load() != ReindexStatus::Running {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert_eq!(
        progress.status.load(),
        ReindexStatus::Complete,
        "reindex did not complete cleanly"
    );

    let events = progress.events.lock().await;
    let complete = events
        .iter()
        .rev()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|v| v["event"] == "complete")
        .expect("a complete event must have been emitted");
    complete
}

/// Print one run's stage breakdown as absolute ms and percent of elapsed.
fn print_breakdown(label: &str, event: &Value) {
    let t = &event["timings"];
    let elapsed = event["elapsed_ms"].as_u64().unwrap_or(0).max(1);
    let get = |k: &str| t[k].as_u64().unwrap_or(0);

    println!("\n=== {label} ===");
    println!(
        "files={} indexed_new={} skipped={} chunks={} vectors={} elapsed_ms={}",
        event["indexed"],
        event["indexed_new"],
        event["skipped"],
        event["total_chunks"],
        t["vector_count"],
        elapsed,
    );
    println!("{:<26} {:>10} {:>8}", "stage", "ms", "% total");
    println!("{}", "-".repeat(46));
    let mut named = 0u64;
    for (key, label) in STAGE_ROWS {
        let ms = get(key);
        named += ms;
        println!(
            "{:<26} {:>10} {:>7.1}%",
            label,
            ms,
            ms as f64 * 100.0 / elapsed as f64
        );
    }
    println!("{}", "-".repeat(46));
    println!(
        "{:<26} {:>10} {:>7.1}%",
        "SUM (partition check)",
        named,
        named as f64 * 100.0 / elapsed as f64
    );

    println!("\n  within batch pipeline (overlapping — producer/consumer):");
    for (key, label) in PIPELINE_SUBROWS {
        let ms = get(key);
        println!(
            "  {:<24} {:>10} {:>7.1}%",
            label,
            ms,
            ms as f64 * 100.0 / elapsed as f64
        );
    }
}

/// Cold full index, then a warm incremental with a few files touched.
///
/// Why: answers the two sizing questions in #5024 — what fraction of a cold
/// index is embedding (the ceiling on a global embed cache), and what a warm
/// delta pass already costs (the floor a corpus-copy scheme cannot go below).
/// What: stages `file_count` real source files, force-reindexes, mutates 5
/// files, incremental-reindexes, and prints both breakdowns.
/// Test: this test; assertions are limited to the partition invariant so the
/// harness never fails on machine-dependent timings.
async fn profile_corpus(file_count: usize) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    let files = stage_corpus(&root, file_count);
    assert!(!files.is_empty(), "corpus fixture must not be empty");
    println!("\n########## corpus: {} files ##########", files.len());

    let handle = build_handle(&root, &format!("stage-profile-{file_count}")).await;

    let cold = run_reindex(Arc::clone(&handle), true).await;
    print_breakdown(&format!("COLD full reindex ({} files)", files.len()), &cold);

    // Touch a handful of files so the warm pass has real delta work rather than
    // hash-skipping everything.
    for path in files.iter().take(5) {
        let mut content = std::fs::read_to_string(path).unwrap_or_default();
        content.push_str("\n// stage-profile touch\nfn __stage_profile_touch() {}\n");
        std::fs::write(path, content).expect("touch corpus file");
    }

    let warm = run_reindex(Arc::clone(&handle), false).await;
    print_breakdown(
        &format!("WARM incremental reindex (5 of {} changed)", files.len()),
        &warm,
    );

    // The only hard invariant: the named stages must account for the elapsed
    // wall clock. `other_ms` absorbs the remainder by construction, so a drift
    // here means a stage clock was dropped in a refactor.
    for (label, event) in [("cold", &cold), ("warm", &warm)] {
        let elapsed = event["elapsed_ms"].as_u64().unwrap_or(0);
        let named: u64 = STAGE_ROWS
            .iter()
            .map(|(k, _)| event["timings"][k].as_u64().unwrap_or(0))
            .sum();
        assert!(
            named >= elapsed.saturating_sub(50) && named <= elapsed + 50,
            "{label}: named stages ({named}ms) must partition elapsed ({elapsed}ms)"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "measurement harness: needs the ONNX model and runs for minutes"]
async fn profile_reindex_stages_small() {
    profile_corpus(120).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "measurement harness: needs the ONNX model and runs for minutes"]
async fn profile_reindex_stages_large() {
    profile_corpus(400).await;
}
