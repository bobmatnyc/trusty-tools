//! #10294: a deferred-embed pass commits and checkpoints each wave's vectors
//! before the next wave starts, and a failed checkpoint fails the pass.
//!
//! Why: the pass held every vector until one commit at the end. A process
//! killed mid-pass lost every finished wave, a search during the pass saw no
//! vector at all, and a snapshot save that failed was only logged while the
//! pass settled `Ready`.
//! What: drives `run_embed_catch_up` over a real redb corpus in a child process
//! with its own data dir and `TRUSTY_EMBED_CHECKPOINT_CHUNKS=1`, so every wave
//! is checkpointed. The embedder parks the first call of wave 2 so the test can
//! read the store and the on-disk snapshot at that boundary.
//! Test: this file.

use super::run_embed_catch_up;
use crate::core::chunker::{ChunkType, RawChunk};
use crate::core::corpus::CorpusStore;
use crate::core::embed::Embedder;
use crate::core::indexer::{CodeIndexer, ParsedBatch};
use crate::core::registry::{IndexHandle, IndexId, StageStatus};
use crate::core::store::{UsearchStore, VectorHit, VectorStore};
use crate::service::reindex::ReindexProgress;
use crate::service::storage_layout::{StorageLayout, HNSW_FILE};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::oneshot;

const DIM: usize = 8;

fn chunk(n: usize) -> RawChunk {
    RawChunk {
        id: format!("w{n}.rs:1:1"),
        file: format!("w{n}.rs"),
        start_line: 1,
        end_line: 1,
        content: format!("fn wave_function_{n}() {{}}"),
        function_name: None,
        language: Some("rust".into()),
        chunk_type: ChunkType::Code,
        calls: vec![],
        inherits_from: vec![],
        chunk_depth: 0,
        parent_chunk_id: None,
        child_chunk_ids: vec![],
        nlp_keywords: vec![],
        nlp_code_refs: vec![],
        virtual_terms: vec![],
    }
}

/// The ends a test holds on a parked embed call: `reached` fires when the call
/// arrives, `release` lets it answer.
type Park = (oneshot::Sender<()>, oneshot::Receiver<()>);

/// An embedder that answers every call, parking call number `park_at` until
/// the test releases it. It records the texts the calls before `park_at`
/// answered.
struct ParkingEmbedder {
    park_at: usize,
    park: Mutex<Option<Park>>,
    calls: AtomicUsize,
    answered: Arc<Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl Embedder for ParkingEmbedder {
    async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
        std::future::pending().await
    }
    async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == self.park_at {
            let park = self.park.lock().expect("park lock").take();
            if let Some((reached, release)) = park {
                let _ = reached.send(());
                let _ = release.await;
            }
        }
        // Only the waves before the parked call: a sibling sub-batch of the
        // parked wave answers at once but is not committed until the wave ends.
        if call < self.park_at {
            let mut answered = self.answered.lock().expect("answered lock");
            answered.extend(texts.iter().map(|t| t.to_string()));
        }
        Ok(texts
            .iter()
            .enumerate()
            .map(|(i, _)| (0..DIM).map(|d| (i + d + 1) as f32).collect())
            .collect())
    }
    fn dimension(&self) -> usize {
        DIM
    }
}

/// A `UsearchStore` whose snapshot save always fails, counting the attempts.
struct FailingSaveStore {
    inner: UsearchStore,
    saves: Arc<AtomicUsize>,
}

/// The error a [`FailingSaveStore`] save returns.
const SAVE_FAILURE_TEXT: &str = "injected checkpoint save failure";

#[async_trait::async_trait]
impl VectorStore for FailingSaveStore {
    async fn upsert(&self, id: &str, embedding: Vec<f32>) -> anyhow::Result<()> {
        self.inner.upsert(id, embedding).await
    }
    async fn search(&self, query: &[f32], top_k: usize) -> anyhow::Result<Vec<VectorHit>> {
        self.inner.search(query, top_k).await
    }
    async fn remove(&self, id: &str) -> anyhow::Result<()> {
        self.inner.remove(id).await
    }
    async fn len(&self) -> anyhow::Result<usize> {
        self.inner.len().await
    }
    async fn upsert_batch(&self, items: &[(String, Vec<f32>)]) -> anyhow::Result<()> {
        self.inner.upsert_batch(items).await
    }
    async fn contains(&self, id: &str) -> bool {
        self.inner.contains(id).await
    }
    async fn contains_many(&self, ids: &[String]) -> Vec<bool> {
        self.inner.contains_many(ids).await
    }
    async fn save_to(&self, _path: &Path) -> anyhow::Result<()> {
        self.saves.fetch_add(1, Ordering::SeqCst);
        anyhow::bail!(SAVE_FAILURE_TEXT)
    }
}

/// An index of `n` chunks in a real redb corpus under `root`, with its
/// pending marker set, embedding through `embedder` into `store`.
async fn handle_with(
    id: &str,
    root: &Path,
    embedder: Arc<dyn Embedder>,
    store: Arc<dyn VectorStore>,
    n: usize,
) -> Arc<IndexHandle> {
    let mut entry = crate::service::persistence::PersistedIndex::new(id, root);
    entry.deferred_embed_pending = true;
    crate::service::persistence::upsert_index_registry_entry(entry).expect("persist entry");
    let mut indexer = CodeIndexer::new(id, root).with_components(embedder, store);
    let redb = CorpusStore::open(&root.join("index.redb")).expect("open corpus");
    indexer.set_corpus_store(Arc::new(redb));
    let parsed = ParsedBatch {
        chunks: (0..n).map(chunk).collect(),
        embeddings: vec![None; n],
        entities_by_file: vec![],
        parse_ms: 0,
        embed_ms: 0,
        vector_count: 0,
    };
    indexer
        .commit_parsed_batch(parsed, false)
        .await
        .expect("commit");
    Arc::new(IndexHandle::bare(
        IndexId::new(id),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        root.to_path_buf(),
    ))
}

/// Run the calling test in a child with its own data dir and a checkpoint
/// after every wave. Returns `true` in the child.
fn isolate(test_name: &str) -> bool {
    if crate::service::test_isolation::is_isolated_child() {
        return true;
    }
    let data_dir = tempfile::tempdir().expect("child data dir");
    crate::service::test_isolation::run_isolated(
        test_name,
        &[
            ("TRUSTY_DATA_DIR", data_dir.path().as_os_str()),
            ("TRUSTY_EMBED_CHECKPOINT_CHUNKS", "1".as_ref()),
        ],
    )
}

/// The ids of the chunks whose content `answered` holds.
fn ids_of(answered: &[String], n: usize) -> Vec<String> {
    (0..n)
        .map(chunk)
        .filter(|c| answered.contains(&c.content))
        .map(|c| c.id)
        .collect()
}

/// When wave 2 starts, wave 1's vectors are in the live store, a vector search
/// finds them, and the on-disk snapshot holds them, so a restart would not
/// embed them again.
///
/// Pre-fix the pass committed once at the end, so the store and the snapshot
/// were both empty at the wave-2 boundary.
/// Test: this IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::parallel]
async fn a_finished_wave_is_searchable_and_on_disk_before_the_next_wave_starts() {
    if !isolate(
        "service::reindex::defer_embed::tests_10294::\
         a_finished_wave_is_searchable_and_on_disk_before_the_next_wave_starts",
    ) {
        return;
    }
    let id = "wave-commit-10294";
    let tmp = tempfile::tempdir().expect("tempdir");
    // Wave 1 is `inflight` sub-batches; park the first call of wave 2.
    let inflight = crate::core::indexer::resolve_embed_inflight();
    let n = inflight * 512 + 1;
    let (reached_tx, reached_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let answered = Arc::new(Mutex::new(Vec::new()));
    let embedder = Arc::new(ParkingEmbedder {
        park_at: inflight,
        park: Mutex::new(Some((reached_tx, release_rx))),
        calls: AtomicUsize::new(0),
        answered: Arc::clone(&answered),
    });
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(DIM).expect("usearch"));
    let handle = handle_with(id, tmp.path(), embedder, Arc::clone(&store), n).await;

    let pass = tokio::spawn(run_embed_catch_up(
        Arc::clone(&handle),
        Arc::new(ReindexProgress::new()),
    ));
    tokio::time::timeout(Duration::from_secs(20), reached_rx)
        .await
        .expect("wave 2 must start")
        .expect("the parked call signals");

    let wave_one = ids_of(&answered.lock().expect("answered lock"), n);
    assert!(
        !wave_one.is_empty() && wave_one.len() < n,
        "wave 1 was answered, wave 2 was not"
    );
    assert!(
        store.contains_many(&wave_one).await.iter().all(|&has| has),
        "wave 1's vectors must be in the live store before wave 2 starts"
    );
    let query: Vec<f32> = (0..DIM).map(|d| (d + 1) as f32).collect();
    let hits = store.search(&query, 5).await.expect("vector search");
    assert!(
        !hits.is_empty(),
        "a search during the pass must see the committed waves"
    );
    let path = StorageLayout::default()
        .file(id, tmp.path(), HNSW_FILE)
        .expect("hnsw path");
    let on_disk = UsearchStore::load_from(&path)
        .await
        .expect("load snapshot")
        .expect("wave 1 must be checkpointed to disk before wave 2 starts");
    assert!(
        on_disk
            .contains_many(&wave_one)
            .await
            .iter()
            .all(|&has| has),
        "a restart must find wave 1's vectors on disk and not embed them again"
    );

    let _ = release_tx.send(());
    tokio::time::timeout(Duration::from_secs(20), pass)
        .await
        .expect("the pass must finish once released")
        .expect("the pass task must not panic");
    assert_eq!(
        handle.stages.read().await.semantic.status,
        StageStatus::Ready
    );
}

/// A checkpoint whose save keeps failing fails the pass with the save error,
/// stops it from embedding further waves, and keeps the pending marker.
///
/// Pre-fix the save ran on a detached task that only logged its failure, and
/// the pass embedded every wave and settled `Ready`.
/// Test: this IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::parallel]
async fn a_failed_checkpoint_fails_the_pass_instead_of_settling_ready() {
    if !isolate(
        "service::reindex::defer_embed::tests_10294::\
         a_failed_checkpoint_fails_the_pass_instead_of_settling_ready",
    ) {
        return;
    }
    let id = "ckpt-fail-10294";
    let tmp = tempfile::tempdir().expect("tempdir");
    let inflight = crate::core::indexer::resolve_embed_inflight();
    let n = inflight * 512 + 1;
    let answered = Arc::new(Mutex::new(Vec::new()));
    let embedder = Arc::new(ParkingEmbedder {
        park_at: usize::MAX,
        park: Mutex::new(None),
        calls: AtomicUsize::new(0),
        answered: Arc::clone(&answered),
    });
    let saves = Arc::new(AtomicUsize::new(0));
    let store: Arc<dyn VectorStore> = Arc::new(FailingSaveStore {
        inner: UsearchStore::new(DIM).expect("usearch"),
        saves: Arc::clone(&saves),
    });
    let handle = handle_with(id, tmp.path(), embedder, store, n).await;

    tokio::time::timeout(
        Duration::from_secs(30),
        run_embed_catch_up(Arc::clone(&handle), Arc::new(ReindexProgress::new())),
    )
    .await
    .expect("a pass whose checkpoint fails must settle, not hang");

    {
        let stages = handle.stages.read().await;
        assert_eq!(
            stages.semantic.status,
            StageStatus::Failed,
            "a pass whose vectors could not be made durable must not settle Ready"
        );
        let reason = format!("{:?}", stages.semantic.failure);
        assert!(
            reason.contains(SAVE_FAILURE_TEXT),
            "failure names the cause: {reason}"
        );
    }
    let embedded = answered.lock().expect("answered lock").len();
    assert!(
        embedded < n,
        "the pass must stop at the failed checkpoint, not embed all {n} chunks"
    );
    assert!(
        saves.load(Ordering::SeqCst) >= 2,
        "the checkpoint is retried before the pass fails"
    );
    let path = crate::service::persistence::indexes_toml_path().expect("indexes.toml path");
    let kept = crate::service::persistence::load_index_registry_at(&path)
        .expect("registry must load")
        .into_iter()
        .find(|e| e.id == id)
        .expect("entry must exist");
    assert!(
        kept.deferred_embed_pending,
        "the pending marker must survive so the next boot embeds the remainder"
    );
}

/// An embedder that reports the CoreML provider, grows the process RSS on its
/// first call, and records every call's batch size.
struct CoreMlSpikeEmbedder {
    sizes: Arc<Mutex<Vec<usize>>>,
    spike: Mutex<Vec<Vec<u8>>>,
}

#[async_trait::async_trait]
impl Embedder for CoreMlSpikeEmbedder {
    async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
        std::future::pending().await
    }
    async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        let first = {
            let mut sizes = self.sizes.lock().expect("sizes lock");
            sizes.push(texts.len());
            sizes.len() == 1
        };
        if first {
            // Touched pages, so the RSS read after the wave sees them.
            self.spike
                .lock()
                .expect("spike lock")
                .push(vec![1u8; 64 << 20]);
        }
        Ok(texts
            .iter()
            .enumerate()
            .map(|(i, _)| (0..DIM).map(|d| (i + d + 1) as f32).collect())
            .collect())
    }
    fn dimension(&self) -> usize {
        DIM
    }
    fn provider(&self) -> trusty_common::embedder::ExecutionProvider {
        trusty_common::embedder::ExecutionProvider::CoreML
    }
}

/// Once the CoreML RSS tripwire halves the batch size, every later embed call
/// of the same pass uses the halved size, including calls in later waves.
///
/// Pre-fix each wave started over at the full CoreML batch size, so the
/// tripwire's jetsam guard lasted one wave.
/// Test: this IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::parallel]
async fn a_tripped_coreml_batch_size_holds_for_the_rest_of_the_pass() {
    // One sub-batch per inner wave, 8 chunks per CoreML call, 32 per pass wave.
    let test_name = "service::reindex::defer_embed::tests_10294::\
                     a_tripped_coreml_batch_size_holds_for_the_rest_of_the_pass";
    if !crate::service::test_isolation::is_isolated_child() {
        let data_dir = tempfile::tempdir().expect("child data dir");
        crate::service::test_isolation::run_isolated(
            test_name,
            &[
                ("TRUSTY_DATA_DIR", data_dir.path().as_os_str()),
                ("TRUSTY_EMBED_INFLIGHT", "1".as_ref()),
                ("TRUSTY_MAX_BATCH_SIZE", "32".as_ref()),
                ("TRUSTY_COREML_BATCH_SIZE", "8".as_ref()),
                ("TRUSTY_COREML_TRIPWIRE_MB", "1".as_ref()),
            ],
        );
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let n = 48;
    let sizes = Arc::new(Mutex::new(Vec::new()));
    let embedder = Arc::new(CoreMlSpikeEmbedder {
        sizes: Arc::clone(&sizes),
        spike: Mutex::new(Vec::new()),
    });
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(DIM).expect("usearch"));
    let handle = handle_with("tripwire-10294", tmp.path(), embedder, store, n).await;

    tokio::time::timeout(
        Duration::from_secs(30),
        run_embed_catch_up(Arc::clone(&handle), Arc::new(ReindexProgress::new())),
    )
    .await
    .expect("the pass must finish");

    let sizes = sizes.lock().expect("sizes lock").clone();
    assert_eq!(sizes.first(), Some(&8), "the pass starts at full size");
    assert_eq!(sizes.iter().sum::<usize>(), n, "every chunk embedded once");
    assert!(
        sizes[1..].iter().all(|&s| s <= 4),
        "after the tripwire fires, no call of the pass may return to the full \
         batch size, including in the next wave: {sizes:?}"
    );
}

/// A finished pass compacts a graph whose churn is due, as the forced persist
/// it replaced did (#9450).
///
/// Pre-fix the pass wrote its own checkpoint, which never compacted, so the
/// churn stayed until some unrelated persist ran.
/// Test: this IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::parallel]
async fn a_finished_pass_compacts_a_churned_graph() {
    if !isolate(
        "service::reindex::defer_embed::tests_10294::a_finished_pass_compacts_a_churned_graph",
    ) {
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(UsearchStore::new(DIM).expect("usearch"));
    let items: Vec<(String, Vec<f32>)> = (0..200)
        .map(|i| {
            (
                format!("old{i}"),
                (0..DIM).map(|d| (i + d + 1) as f32).collect(),
            )
        })
        .collect();
    store.upsert_batch(&items).await.expect("upsert");
    for i in 0..60 {
        store.remove(&format!("old{i}")).await.expect("remove");
    }
    let embedder = Arc::new(ParkingEmbedder {
        park_at: usize::MAX,
        park: Mutex::new(None),
        calls: AtomicUsize::new(0),
        answered: Arc::new(Mutex::new(Vec::new())),
    });
    let dyn_store: Arc<dyn VectorStore> = store.clone();
    let handle = handle_with("compact-10294", tmp.path(), embedder, dyn_store, 4).await;
    assert!(
        store.churn_since_compact() >= 60,
        "precondition: churn past the threshold"
    );

    tokio::time::timeout(
        Duration::from_secs(30),
        run_embed_catch_up(Arc::clone(&handle), Arc::new(ReindexProgress::new())),
    )
    .await
    .expect("the pass must finish");
    assert_eq!(
        handle.stages.read().await.semantic.status,
        StageStatus::Ready
    );

    let deadline =
        tokio::time::Instant::now() + store.compaction_quiet_window() + Duration::from_secs(10);
    while store.churn_since_compact() != 0 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        store.churn_since_compact(),
        0,
        "#9450: a finished pass compacts the churned graph once writes stop"
    );
    assert_eq!(store.len().await.expect("len"), 144, "no vector lost");
}
