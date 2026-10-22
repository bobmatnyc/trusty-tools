//! #10110: batch removal by path prefix or file list.
//!
//! Why: a per-file purge rehydrated the whole evicted corpus and stamped one
//! graph-dirty mark per file. The batch must remove every store's copy of the
//! selected files, touch nothing else, never rehydrate, stamp one mark, and
//! leave every failure branch in a state a retry finishes.
//! What: one test per acceptance criterion (A1-A3, A4 as F5) and per
//! failure branch of the plan table that lives in the indexer (F3, F4, F9,
//! fix-bar 1). The handler-level branches live in
//! `service::server::remove_files_tests`.
//! Test: the functions below.

use std::collections::BTreeSet;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use super::*;
use crate::core::path_selection::PathSelection;
use crate::core::store::VectorHit;
use crate::core::symbol_graph::SymbolMatch;

const DIM: usize = 8;
const ROOT: &str = "/tmp/batch-remove-10110";

/// Two files under `vendor/acme`, a sibling directory that shares the
/// prefix's text, and one unrelated file.
const FILES: [(&str, &str); 4] = [
    (
        "vendor/acme/a.rs",
        "fn acme_alpha_q10110() { acme_beta_q10110(); }\n",
    ),
    ("vendor/acme/b.rs", "fn acme_beta_q10110() {}\n"),
    ("vendor/acmebar/c.rs", "fn sibling_gamma_q10110() {}\n"),
    (
        "src/keep.rs",
        "fn keeper_delta_q10110() { sibling_gamma_q10110(); }\n",
    ),
];

/// A `UsearchStore` whose removals fail while `fail_removes` is set.
struct FlakyStore {
    inner: UsearchStore,
    fail_removes: AtomicBool,
}

#[async_trait::async_trait]
impl VectorStore for FlakyStore {
    async fn upsert(&self, id: &str, embedding: Vec<f32>) -> anyhow::Result<()> {
        self.inner.upsert(id, embedding).await
    }
    async fn search(&self, query: &[f32], top_k: usize) -> anyhow::Result<Vec<VectorHit>> {
        self.inner.search(query, top_k).await
    }
    async fn remove(&self, id: &str) -> anyhow::Result<()> {
        if self.fail_removes.load(Ordering::SeqCst) {
            anyhow::bail!("injected vector remove failure");
        }
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
    async fn ids_where(
        &self,
        keep: &(dyn for<'a> Fn(&'a str) -> bool + Send + Sync),
    ) -> anyhow::Result<Vec<String>> {
        self.inner.ids_where(keep).await
    }
}

/// A corpus-backed indexer with vectors, under a test-unique id.
fn indexer(id: &str, redb: &std::path::Path) -> (CodeIndexer, Arc<FlakyStore>) {
    let flaky = Arc::new(FlakyStore {
        inner: UsearchStore::new(DIM).expect("usearch new"),
        fail_removes: AtomicBool::new(false),
    });
    let store: Arc<dyn VectorStore> = flaky.clone();
    let mut idx =
        CodeIndexer::new(id, ROOT).with_components(Arc::new(MockEmbedder::new(DIM)), store);
    idx.set_corpus_store(Arc::new(CorpusStore::open(redb).expect("open corpus")));
    (idx, flaky)
}

async fn seed(idx: &CodeIndexer) {
    let files: Vec<(String, String)> = FILES
        .iter()
        .map(|(p, c)| ((*p).to_string(), (*c).to_string()))
        .collect();
    idx.index_files_batch(&files).await.expect("seed batch");
}

fn prefix(p: &str) -> PathSelection {
    PathSelection::Prefix(BTreeSet::from([p.to_string()]))
}

/// Chunk ids redb holds for files under `dir/`, sorted.
fn row_ids_under(idx: &CodeIndexer, dir: &str) -> Vec<String> {
    let corpus = idx.corpus_store().expect("corpus");
    let mut ids: Vec<String> = corpus
        .load_all_chunks()
        .expect("rows")
        .into_iter()
        .filter(|c| c.file.starts_with(&format!("{dir}/")))
        .map(|c| c.id)
        .collect();
    ids.sort();
    ids
}

/// Files holding an entity row in redb.
fn entity_files(idx: &CodeIndexer) -> BTreeSet<String> {
    let corpus = idx.corpus_store().expect("corpus");
    corpus
        .load_all_entities()
        .expect("entities")
        .into_iter()
        .map(|(file, _)| file)
        .collect()
}

/// Chunk ids the in-memory map holds for files under `dir/`.
async fn map_ids_under(idx: &CodeIndexer, dir: &str) -> Vec<String> {
    let chunks = idx.chunks.read().await;
    chunks
        .values()
        .filter(|c| c.file.starts_with(&format!("{dir}/")))
        .map(|c| c.id.clone())
        .collect()
}

async fn hit_ids(idx: &CodeIndexer, text: &str) -> Vec<String> {
    let query = SearchQuery {
        text: text.to_string(),
        top_k: 10,
        expand_graph: false,
        compact: false,
        ..Default::default()
    };
    idx.search(&query)
        .await
        .expect("search")
        .into_iter()
        .map(|c| c.id)
        .collect()
}

/// Arms a fault seam for one index id; disarms on drop.
struct Armed(&'static std::sync::Mutex<Vec<String>>, String);

impl Armed {
    fn arm(seam: &'static std::sync::Mutex<Vec<String>>, id: &str) -> Self {
        seam.lock().expect("seam").push(id.to_string());
        Self(seam, id.to_string())
    }
}

impl Drop for Armed {
    fn drop(&mut self) {
        if let Ok(mut f) = self.0.lock() {
            f.retain(|id| *id != self.1);
        }
    }
}

/// A1 + A3: a prefix removal empties redb rows, the chunk map, the vectors and
/// the entity rows of every file under the prefix, spares the sibling
/// `vendor/acmebar/`, leaves an unrelated query's answer unchanged, and
/// leaves no trace after a reopen.
#[tokio::test]
async fn prefix_removal_clears_every_store_and_spares_a_sibling() {
    let dir = tempfile::tempdir().expect("tempdir");
    let redb = dir.path().join("index.redb");
    let (idx, flaky) = indexer("batch-10110-a1", &redb);
    seed(&idx).await;
    let acme = row_ids_under(&idx, "vendor/acme");
    let sibling = row_ids_under(&idx, "vendor/acmebar");
    assert!(!acme.is_empty() && !sibling.is_empty(), "setup");
    assert!(
        flaky.contains_many(&acme).await.iter().all(|h| *h),
        "setup: the removed files have vectors"
    );
    let unrelated_before = hit_ids(&idx, "keeper_delta_q10110").await;
    assert!(!hit_ids(&idx, "acme_beta_q10110").await.is_empty(), "setup");

    let counts = idx
        .remove_files_matching(&prefix("vendor/acme"))
        .await
        .expect("batch removal");

    assert_eq!(counts.matched_files, 2);
    assert_eq!(counts.removed_files, 2);
    assert_eq!(counts.removed_chunks, acme.len());
    assert!(
        row_ids_under(&idx, "vendor/acme").is_empty(),
        "redb rows remain"
    );
    assert!(
        map_ids_under(&idx, "vendor/acme").await.is_empty(),
        "map ids remain"
    );
    assert!(
        flaky.contains_many(&acme).await.iter().all(|h| !*h),
        "vectors remain"
    );
    let entities = entity_files(&idx);
    assert!(
        !entities.iter().any(|f| f.starts_with("vendor/acme/")),
        "entity rows remain: {entities:?}"
    );
    assert_eq!(
        row_ids_under(&idx, "vendor/acmebar"),
        sibling,
        "sibling touched"
    );
    assert!(
        flaky.contains_many(&sibling).await.iter().all(|h| *h),
        "sibling vectors touched"
    );
    assert!(hit_ids(&idx, "acme_beta_q10110")
        .await
        .iter()
        .all(|id| !id.starts_with("vendor/acme/")));
    // A3: the hits outside the prefix keep their order.
    let outside: Vec<String> = unrelated_before
        .into_iter()
        .filter(|id| !id.starts_with("vendor/acme/"))
        .collect();
    assert_eq!(hit_ids(&idx, "keeper_delta_q10110").await, outside);
    drop(idx);

    let (reopened, _) = indexer("batch-10110-a1", &redb);
    reopened.load_chunks_from_redb().await.expect("warm boot");
    assert!(map_ids_under(&reopened, "vendor/acme").await.is_empty());
    assert_eq!(row_ids_under(&reopened, "vendor/acmebar"), sibling);
}

/// A1, list form: exactly the named files go, a file whose name extends a
/// named one stays.
#[tokio::test]
async fn list_removal_removes_exactly_the_named_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (idx, _) = indexer("batch-10110-list", &dir.path().join("index.redb"));
    seed(&idx).await;
    let keep_before = row_ids_under(&idx, "src");
    let sel = PathSelection::Files(BTreeSet::from([
        "vendor/acme/a.rs".to_string(),
        "vendor/acmebar/c.rs".to_string(),
        "vendor/acme/a".to_string(),
    ]));

    let counts = idx.remove_files_matching(&sel).await.expect("removal");

    assert_eq!(counts.removed_files, 2);
    let files: BTreeSet<String> = idx
        .corpus_store()
        .expect("corpus")
        .list_indexed_files()
        .expect("files")
        .into_iter()
        .collect();
    assert_eq!(
        files,
        BTreeSet::from(["vendor/acme/b.rs".to_string(), "src/keep.rs".to_string()])
    );
    assert_eq!(row_ids_under(&idx, "src"), keep_before);
}

/// A2: on an idle-evicted index the removal runs from redb ranges, so no
/// rehydrate scan starts and the map stays evicted; the call stamps exactly
/// one durable dirty mark, and the graph rebuilds once.
#[tokio::test]
async fn removal_on_an_evicted_index_does_not_rehydrate_and_stamps_one_mark() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (idx, _) = indexer("batch-10110-evicted", &dir.path().join("index.redb"));
    seed(&idx).await;
    assert!(idx.reclaim_memory_now().await > 0, "setup: eviction");
    assert!(idx.corpus_evicted());
    let scans = super::super::idle_evict::RehydrateScanCounter::arm(&idx);
    drop(idx.begin_graph_write().await.expect("probe stamp"));
    let corpus = idx.corpus_store().expect("corpus");
    let probe = corpus
        .kg_graph_dirty()
        .expect("read mark")
        .expect("probe mark");
    let rebuilds = idx.symbol_graph_full_rebuilds();

    let counts = idx
        .remove_files_matching(&prefix("vendor"))
        .await
        .expect("removal");

    assert_eq!(counts.removed_files, 3, "three files sit under vendor/");
    assert!(idx.corpus_evicted(), "the removal rehydrated the corpus");
    assert_eq!(scans.count(), 0, "a rehydrate scan ran");
    assert_eq!(
        corpus.kg_graph_dirty().expect("read mark"),
        Some(probe + 1),
        "one durable mark per call, not one per file"
    );
    assert!(
        idx.refresh_symbol_graph_if_due(Duration::ZERO, Duration::MAX)
            .await
    );
    assert_eq!(idx.symbol_graph_full_rebuilds(), rebuilds + 1);
}

/// F3: entity rows leave in the chunks' transaction, so a reopened index
/// rebuilds a graph without the removed symbols.
#[tokio::test]
async fn batch_remove_leaves_no_entity_rows_after_reopen() {
    let dir = tempfile::tempdir().expect("tempdir");
    let redb = dir.path().join("index.redb");
    {
        let (idx, _) = indexer("batch-10110-entities", &redb);
        seed(&idx).await;
        assert!(entity_files(&idx).contains("vendor/acme/b.rs"), "setup");
        idx.remove_files_matching(&PathSelection::Files(BTreeSet::from([
            "vendor/acme/b.rs".to_string()
        ])))
        .await
        .expect("removal");
        // Dropped before the deferred rebuild: a crash inside the window.
    }
    let (idx, _) = indexer("batch-10110-entities", &redb);
    assert!(!entity_files(&idx).contains("vendor/acme/b.rs"));
    idx.load_chunks_from_redb().await.expect("warm boot");
    assert!(idx.symbol_graph_is_stale(), "the durable mark survives");
    assert!(
        idx.refresh_symbol_graph_if_due(Duration::ZERO, Duration::MAX)
            .await
    );
    let graph = idx.snapshot_symbol_graph().await;
    assert_eq!(
        graph.resolve_symbol("acme_beta_q10110"),
        SymbolMatch::NotFound
    );
    assert_ne!(
        graph.resolve_symbol("keeper_delta_q10110"),
        SymbolMatch::NotFound
    );
}

/// Fix-bar 1: a vector delete that fails after the redb rows went is an
/// error, not a warning; the orphan vectors answer no search; and a retry,
/// which finds no row, still removes them.
#[tokio::test]
async fn a_failed_vector_removal_is_an_error_and_a_retry_removes_the_orphans() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (idx, flaky) = indexer("batch-10110-vectors", &dir.path().join("index.redb"));
    seed(&idx).await;
    let acme = row_ids_under(&idx, "vendor/acme");

    flaky.fail_removes.store(true, Ordering::SeqCst);
    let err = idx
        .remove_files_matching(&prefix("vendor/acme"))
        .await
        .expect_err("a failed vector delete must not answer Ok");
    flaky.fail_removes.store(false, Ordering::SeqCst);

    let BatchRemoveError::Failed { counts, .. } = err else {
        panic!("expected Failed, got {err}");
    };
    assert_eq!(counts.removed_chunks, acme.len(), "the rows committed");
    assert!(row_ids_under(&idx, "vendor/acme").is_empty());
    assert!(
        flaky.contains_many(&acme).await.iter().all(|h| *h),
        "setup of the retry: the failed call left the vectors"
    );
    assert!(hit_ids(&idx, "acme_alpha_q10110 acme_beta_q10110")
        .await
        .iter()
        .all(|id| !acme.contains(id)));

    let retry = idx
        .remove_files_matching(&prefix("vendor/acme"))
        .await
        .expect("the retry finishes");
    assert_eq!(retry.removed_chunks, 0, "no row was left to remove");
    assert!(
        flaky.contains_many(&acme).await.iter().all(|h| !*h),
        "the retry must remove the orphan vectors"
    );
}

/// F4: a dirty mark that cannot be stamped fails the call before anything
/// leaves any store; a retry then completes.
#[tokio::test]
async fn batch_remove_refuses_when_the_dirty_mark_cannot_be_stamped() {
    const ID: &str = "batch-10110-mark";
    let dir = tempfile::tempdir().expect("tempdir");
    let (idx, flaky) = indexer(ID, &dir.path().join("index.redb"));
    seed(&idx).await;
    let acme = row_ids_under(&idx, "vendor/acme");
    {
        let _fault = Armed::arm(&super::super::graph_refresh::TEST_FAIL_DIRTY_MARK, ID);
        idx.remove_files_matching(&prefix("vendor/acme"))
            .await
            .expect_err("a failed stamp fails the call");
    }
    assert_eq!(row_ids_under(&idx, "vendor/acme"), acme, "rows changed");
    assert_eq!(map_ids_under(&idx, "vendor/acme").await.len(), acme.len());
    assert!(flaky.contains_many(&acme).await.iter().all(|h| *h));

    let counts = idx
        .remove_files_matching(&prefix("vendor/acme"))
        .await
        .expect("retry");
    assert_eq!(counts.removed_chunks, acme.len());
    assert!(row_ids_under(&idx, "vendor/acme").is_empty());
}

/// A4 / F5: the dry run changes nothing — rows, eviction, durable mark,
/// reindex stamp — and its counts equal what the real run then removes.
#[tokio::test]
async fn dry_run_mutates_nothing_and_matches_the_real_run() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (idx, _) = indexer("batch-10110-dry", &dir.path().join("index.redb"));
    seed(&idx).await;
    assert!(idx.reclaim_memory_now().await > 0, "setup: eviction");
    let corpus = idx.corpus_store().expect("corpus");
    let rows = corpus.chunk_count().expect("count");
    let mark = corpus.kg_graph_dirty().expect("mark");
    let stamp = corpus.read_reindexed_unix_sync().expect("stamp");

    let dry = idx
        .count_files_matching(&prefix("vendor/acme"))
        .await
        .expect("dry run");

    assert_eq!((dry.matched_files, dry.removed_files), (2, 0));
    assert_eq!(corpus.chunk_count().expect("count"), rows);
    assert!(idx.corpus_evicted(), "the dry run rehydrated");
    assert_eq!(corpus.kg_graph_dirty().expect("mark"), mark);
    assert_eq!(corpus.read_reindexed_unix_sync().expect("stamp"), stamp);

    let real = idx
        .remove_files_matching(&prefix("vendor/acme"))
        .await
        .expect("real run");
    assert_eq!(real.matched_chunks, dry.matched_chunks);
    assert_eq!(real.removed_chunks, dry.matched_chunks);
    assert_eq!(real.removed_files, dry.matched_files);
}

/// F9: a corpus closed for shutdown refuses the removal, so it never reports
/// success for rows redb kept, and memory keeps every chunk.
#[tokio::test]
async fn batch_remove_is_refused_on_a_corpus_closed_for_shutdown() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut idx, flaky) = indexer("batch-10110-shutdown", &dir.path().join("index.redb"));
    seed(&idx).await;
    let acme = map_ids_under(&idx, "vendor/acme").await;
    assert!(idx.take_corpus_for_shutdown().is_some(), "setup");

    let err = idx
        .remove_files_matching(&prefix("vendor/acme"))
        .await
        .expect_err("refused");
    assert!(err.to_string().contains("closed for shutdown"), "{err}");
    let dry = idx
        .count_files_matching(&prefix("vendor/acme"))
        .await
        .expect_err("the dry run is refused too");
    assert!(
        format!("{dry:#}").contains("closed for shutdown"),
        "{dry:#}"
    );
    assert_eq!(map_ids_under(&idx, "vendor/acme").await.len(), acme.len());
    assert!(flaky.contains_many(&acme).await.iter().all(|h| *h));
}

/// F1, mid-batch: with one file per slice and the second slice refused, the
/// error reports the one committed file; the refused file keeps its rows and
/// its map ids; a retry removes it.
#[tokio::test]
async fn a_refused_later_slice_reports_what_committed_and_keeps_the_rest() {
    const ID: &str = "batch-10110-slices";
    let dir = tempfile::tempdir().expect("tempdir");
    let (idx, _) = indexer(ID, &dir.path().join("index.redb"));
    seed(&idx).await;
    let sel = PathSelection::Files(BTreeSet::from([
        "vendor/acme/a.rs".to_string(),
        "vendor/acme/b.rs".to_string(),
    ]));
    let seam = &super::super::TEST_FAIL_SLICE_FROM;
    seam.lock().expect("seam").push((ID.to_string(), 1));
    let refused = idx.remove_files_matching_sliced(&sel, 1).await;
    seam.lock().expect("seam").retain(|(id, _)| id != ID);

    let Err(BatchRemoveError::Failed { counts, .. }) = refused else {
        panic!("the refused slice must fail the call");
    };
    assert_eq!((counts.matched_files, counts.removed_files), (2, 1));
    let left: BTreeSet<String> = idx
        .corpus_store()
        .expect("corpus")
        .list_indexed_files()
        .expect("files")
        .into_iter()
        .filter(|f| f.starts_with("vendor/acme/"))
        .collect();
    assert_eq!(left, BTreeSet::from(["vendor/acme/b.rs".to_string()]));
    assert!(!idx.chunk_ids_for_file("vendor/acme/b.rs").await.is_empty());

    let retry = idx
        .remove_files_matching_sliced(&sel, 1)
        .await
        .expect("retry");
    assert_eq!(retry.removed_files, 1);
    assert!(row_ids_under(&idx, "vendor/acme").is_empty());
}
