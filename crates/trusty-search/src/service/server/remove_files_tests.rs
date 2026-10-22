//! #10110: `search.index.files.remove` through its transport-free core.
//!
//! Why: the handler owns the failure branches that cross stores — the
//! content hashes, the reindex stamp, the 400 safety guard, and the 503 a
//! racing rehydrate earns — so they are proven here, on a registered index.
//! What: one test per handler-level branch of the plan table (F1, F2 with
//! fix-bar 2, F7, F8) plus the Q5 refusals.
//! Test: the functions below.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;

use super::remove_files::{remove_files_report, RemoveFilesRequest};
use super::state::SearchAppState;
use crate::core::corpus::CorpusStore;
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use crate::service::reindex::hash::hashes_for;

const FILES: [(&str, &str); 3] = [
    ("vendor/acme/a.rs", "fn acme_alpha_h10110() {}\n"),
    ("vendor/acme/b.rs", "fn acme_beta_h10110() {}\n"),
    ("src/keep.rs", "fn keeper_h10110() {}\n"),
];

/// Hash rows: one per indexed file, plus `vendor/acme/gone.rs`, a hash with
/// no chunk left — the state a failed forget leaves behind.
const HASHED: [&str; 4] = [
    "vendor/acme/a.rs",
    "vendor/acme/b.rs",
    "vendor/acme/gone.rs",
    "src/keep.rs",
];

struct Fixture {
    _dir: tempfile::TempDir,
    id: &'static str,
    root: PathBuf,
    state: Arc<SearchAppState>,
    handle: Arc<IndexHandle>,
}

impl Fixture {
    async fn new(id: &'static str) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).expect("root");
        let mut idx = CodeIndexer::new(id, root.to_str().expect("utf8"));
        let corpus = Arc::new(CorpusStore::open(&dir.path().join("index.redb")).expect("open"));
        idx.set_corpus_store(Arc::clone(&corpus));
        let files: Vec<(String, String)> = FILES
            .iter()
            .map(|(p, c)| ((*p).to_string(), (*c).to_string()))
            .collect();
        idx.index_files_batch(&files).await.expect("seed");
        let rows: Vec<(&str, &str)> = HASHED.iter().map(|f| (*f, "h")).collect();
        corpus.upsert_file_hashes(&rows).expect("hash rows");
        let cache = hashes_for(&IndexId::new(id));
        for f in HASHED {
            cache.insert(PathBuf::from(f), "h".into());
        }
        // A known stamp, so a stamp written in the same second still shows.
        corpus.write_reindexed_unix_sync(1).expect("stamp");
        let registry = IndexRegistry::new();
        let handle = registry.register(IndexHandle::bare(
            IndexId::new(id),
            Arc::new(tokio::sync::RwLock::new(idx)),
            root.clone(),
        ));
        Self {
            _dir: dir,
            id,
            root,
            state: Arc::new(SearchAppState::new(registry)),
            handle,
        }
    }

    async fn call(
        &self,
        body: serde_json::Value,
    ) -> Result<serde_json::Value, (StatusCode, serde_json::Value)> {
        let req: RemoveFilesRequest = serde_json::from_value(body).expect("request");
        remove_files_report(&self.state, self.id, req).await
    }

    async fn corpus(&self) -> Arc<CorpusStore> {
        self.handle
            .indexer
            .read()
            .await
            .corpus_store()
            .expect("corpus")
    }

    async fn files(&self) -> BTreeSet<String> {
        let corpus = self.corpus().await;
        corpus
            .list_indexed_files()
            .expect("files")
            .into_iter()
            .collect()
    }

    async fn hash_rows(&self) -> BTreeSet<String> {
        let corpus = self.corpus().await;
        corpus
            .load_file_hashes()
            .expect("hashes")
            .into_iter()
            .map(|(f, _)| f)
            .collect()
    }

    fn cached(&self, key: &str) -> bool {
        hashes_for(&IndexId::new(self.id)).contains_key(Path::new(key))
    }

    async fn stamp(&self) -> Option<u64> {
        let corpus = self.corpus().await;
        corpus.read_reindexed_unix_sync().expect("stamp")
    }
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

/// Arms one `(index id, value)` seam entry; disarms on drop.
struct Armed<T: PartialEq + Clone + 'static>(&'static std::sync::Mutex<Vec<T>>, T);

impl<T: PartialEq + Clone + 'static> Armed<T> {
    fn arm(seam: &'static std::sync::Mutex<Vec<T>>, entry: T) -> Self {
        seam.lock().expect("seam").push(entry.clone());
        Self(seam, entry)
    }
}

impl<T: PartialEq + Clone + 'static> Drop for Armed<T> {
    fn drop(&mut self) {
        if let Ok(mut f) = self.0.lock() {
            f.retain(|e| *e != self.1);
        }
    }
}

/// F1: a refused slice answers 500 `remove_files_failed` with the committed
/// counts, keeps the rows and every hash, writes no stamp, and a retry
/// removes the files, forgets the selected hashes and stamps.
#[tokio::test]
async fn batch_remove_fails_closed_on_a_refused_slice_and_a_retry_finishes() {
    let fx = Fixture::new("remove-files-f1").await;
    let refused = {
        let _fault = Armed::arm(
            &crate::core::indexer::TEST_FAIL_SLICE_FROM,
            (fx.id.to_string(), 0),
        );
        fx.call(serde_json::json!({ "path_prefix": "vendor/acme" }))
            .await
    };
    let (status, body) = refused.expect_err("a refused slice must not answer 200");
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_eq!(body["error"], "remove_files_failed", "{body}");
    assert_eq!(body["partial"], false, "{body}");
    assert_eq!(body["removed_chunks"], 0, "{body}");
    assert_eq!(body["retryable"], true, "{body}");
    assert!(fx.files().await.contains("vendor/acme/a.rs"), "rows went");
    assert_eq!(fx.hash_rows().await, set(&HASHED), "hashes went");
    assert!(fx.cached("vendor/acme/a.rs"));
    assert_eq!(fx.stamp().await, Some(1), "a refused delete stamped");

    let done = fx
        .call(serde_json::json!({ "path_prefix": "vendor/acme" }))
        .await
        .expect("the retry answers 200");
    assert_eq!(done["removed_files"], 2, "{done}");
    assert_eq!(fx.files().await, set(&["src/keep.rs"]));
    assert_eq!(fx.hash_rows().await, set(&["src/keep.rs"]));
    assert!(!fx.cached("vendor/acme/a.rs") && fx.cached("src/keep.rs"));
    assert!(fx.stamp().await > Some(1), "a committed delete stamps");
}

/// F2 + fix-bar 2: a failed hash forget answers 500 `hash_forget_failed`
/// after the rows are gone; the retry matches no chunk and still clears every
/// selected hash, including one whose file had no chunk.
#[tokio::test]
async fn batch_remove_reports_a_failed_hash_forget_and_a_retry_clears_it() {
    let fx = Fixture::new("remove-files-f2").await;
    let refused = {
        let _fault = Armed::arm(
            &crate::service::reindex::hash::TEST_FAIL_FORGET_HASH,
            fx.id.to_string(),
        );
        fx.call(serde_json::json!({ "path_prefix": "vendor/acme" }))
            .await
    };
    let (status, body) = refused.expect_err("a failed forget must not answer 200");
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_eq!(body["error"], "hash_forget_failed", "{body}");
    assert_eq!(body["partial"], true, "{body}");
    assert_eq!(body["removed_files"], 2, "{body}");
    assert_eq!(
        fx.files().await,
        set(&["src/keep.rs"]),
        "the rows committed"
    );
    assert!(fx.hash_rows().await.contains("vendor/acme/a.rs"), "setup");

    let retry = fx
        .call(serde_json::json!({ "path_prefix": "vendor/acme" }))
        .await
        .expect("the retry answers 200");
    assert_eq!(retry["matched_chunks"], 0, "{retry}");
    assert_eq!(
        fx.hash_rows().await,
        set(&["src/keep.rs"]),
        "stale hashes survived the retry"
    );
    assert!(!fx.cached("vendor/acme/gone.rs"));
}

/// F7: a prefix matching nothing answers 200 with zeros and writes no dirty
/// mark and no stamp.
#[tokio::test]
async fn a_prefix_matching_nothing_answers_zeros_and_writes_no_mark_or_stamp() {
    let fx = Fixture::new("remove-files-f7").await;
    let corpus = fx.corpus().await;
    let mark = corpus.kg_graph_dirty().expect("mark");

    let reply = fx
        .call(serde_json::json!({ "path_prefix": "nowhere" }))
        .await
        .expect("200");

    for field in [
        "matched_files",
        "matched_chunks",
        "removed_files",
        "removed_chunks",
    ] {
        assert_eq!(reply[field], 0, "{field}: {reply}");
    }
    assert_eq!(
        corpus.kg_graph_dirty().expect("mark"),
        mark,
        "a mark was written"
    );
    assert_eq!(fx.stamp().await, Some(1), "a stamp was written");
    assert_eq!(fx.hash_rows().await, set(&HASHED));
}

/// Q5: every unsafe or malformed selection is a 400 and removes nothing.
#[tokio::test]
async fn remove_files_refuses_an_unsafe_selection() {
    let fx = Fixture::new("remove-files-q5").await;
    let root = fx.root.to_string_lossy().into_owned();
    let too_many: Vec<String> = (0..10_001).map(|n| format!("f{n}.rs")).collect();
    let outside = "remove_files_path_outside_root";
    let invalid = "remove_files_invalid_request";
    let cases = [
        (serde_json::json!({ "path_prefix": "" }), outside),
        (serde_json::json!({ "path_prefix": "." }), outside),
        (serde_json::json!({ "path_prefix": ".." }), outside),
        (
            serde_json::json!({ "path_prefix": "vendor/../src" }),
            outside,
        ),
        (serde_json::json!({ "path_prefix": root.clone() }), outside),
        (
            serde_json::json!({ "path_prefix": format!("{root}/") }),
            outside,
        ),
        (serde_json::json!({ "path_prefix": "/" }), outside),
        (serde_json::json!({ "path_prefix": "@root1" }), outside),
        (
            serde_json::json!({ "paths": ["src/keep.rs", "../escape.rs"] }),
            outside,
        ),
        (
            serde_json::json!({ "path_prefix": "vendor", "paths": ["src/keep.rs"] }),
            invalid,
        ),
        (serde_json::json!({}), invalid),
        (serde_json::json!({ "paths": [] }), invalid),
        (serde_json::json!({ "paths": too_many }), invalid),
    ];
    for (body, code) in cases {
        let shown = body.to_string();
        let (status, refusal) = fx.call(body).await.expect_err(&shown);
        assert_eq!(status, StatusCode::BAD_REQUEST, "{shown}: {refusal}");
        assert_eq!(refusal["error"], code, "{shown}: {refusal}");
    }
    assert_eq!(
        fx.files().await,
        set(&["src/keep.rs", "vendor/acme/a.rs", "vendor/acme/b.rs"]),
        "a refused selection removed something"
    );
}

/// Restores the global rehydrate delay even when the test panics.
struct RehydrateDelay;

impl RehydrateDelay {
    fn set(ms: u64) -> Self {
        crate::core::indexer::TEST_REHYDRATE_DELAY_MS.store(ms, Ordering::SeqCst);
        Self
    }
}

impl Drop for RehydrateDelay {
    fn drop(&mut self) {
        crate::core::indexer::TEST_REHYDRATE_DELAY_MS.store(0, Ordering::SeqCst);
    }
}

/// F8 + Q4: while a corpus rehydrate is scanning, the removal answers 503
/// `remove_files_busy` (retryable) and changes nothing; once the scan has
/// published, the retry removes the files and no removed id comes back.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn batch_remove_during_a_rehydrate_is_refused_and_leaves_no_orphan_ids() {
    let fx = Fixture::new("remove-files-f8").await;
    let indexer = Arc::clone(&fx.handle.indexer);
    let idx = indexer.read().await;
    assert!(idx.reclaim_memory_now().await > 0, "setup: eviction");
    let scans = crate::core::indexer::RehydrateScanCounter::arm(&idx);
    let delay = RehydrateDelay::set(1_500);
    let published = idx.begin_corpus_rehydrate().expect("a rehydrate starts");
    let started = tokio::time::timeout(Duration::from_secs(10), async {
        while scans.count() == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    assert!(started.is_ok(), "setup: the rehydrate scan never started");

    let refused = fx
        .call(serde_json::json!({ "path_prefix": "vendor/acme" }))
        .await;
    let (status, body) = refused.expect_err("a removal racing a scan must be refused");
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["error"], "remove_files_busy", "{body}");
    assert_eq!(body["retryable"], true, "{body}");
    assert!(fx.files().await.contains("vendor/acme/a.rs"), "rows went");

    // Poll, not `notified()`: a notification sent before the wait is lost.
    drop(published);
    let warm = tokio::time::timeout(Duration::from_secs(20), async {
        while idx.corpus_evicted() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(warm.is_ok(), "the rehydrate never published");
    drop(delay);
    let done = fx
        .call(serde_json::json!({ "path_prefix": "vendor/acme" }))
        .await
        .expect("the retry answers 200");
    assert_eq!(done["removed_files"], 2, "{done}");
    assert!(idx.chunk_ids_for_file("vendor/acme/a.rs").await.is_empty());
    assert!(idx.reclaim_memory_now().await > 0, "re-evict");
    assert!(
        idx.chunk_ids_for_file("vendor/acme/a.rs").await.is_empty(),
        "a removed id came back after a rehydrate"
    );
}
