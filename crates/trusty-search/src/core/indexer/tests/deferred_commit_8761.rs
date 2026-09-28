//! #8761 regression tests: a deferred-embed commit must not give a removed or
//! edited chunk its snapshot vector.
//!
//! Why: `plan_deferred_embed` snapshots the owed chunks and the embed phase
//! holds no lock (#8600), so `remove_file` can run between snapshot and commit.
//! The commit upserted every snapshot vector, re-inserting orphans for deleted
//! files that no removal path reclaims.
//! What: drives the three phases by hand and interleaves `remove_file` or an
//! edit at each gap — the embed phase, the upsert, and `remove_file`'s own gap
//! between its map drop and its vector drop — plus the error arms.
//! Test: the functions below.

use super::corpus_fault::break_corpus_reads;
use super::*;
use crate::core::embed::Embedder;
use crate::core::store::VectorHit;
use std::sync::atomic::AtomicBool;
use tokio::sync::oneshot;

/// A one-shot pause point: signals `reached`, then waits for `release`.
#[derive(Default)]
struct Gate(std::sync::Mutex<Option<(oneshot::Sender<()>, oneshot::Receiver<()>)>>);

impl Gate {
    /// Arm the gate; returns the test's `(reached, release)` ends.
    fn arm(&self) -> (oneshot::Receiver<()>, oneshot::Sender<()>) {
        let (reached_tx, reached_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        *self.0.lock().expect("gate lock") = Some((reached_tx, release_rx));
        (reached_rx, release_tx)
    }

    async fn pass(&self) {
        let armed = self.0.lock().expect("gate lock").take();
        if let Some((reached, release)) = armed {
            let _ = reached.send(());
            let _ = release.await;
        }
    }
}

/// A `UsearchStore` that can pause before an upsert or after a removal, and
/// fail removals on demand.
struct GatedStore {
    inner: UsearchStore,
    before_upsert: Gate,
    after_remove: Gate,
    fail_remove: AtomicBool,
}

#[async_trait::async_trait]
impl VectorStore for GatedStore {
    async fn upsert(&self, id: &str, embedding: Vec<f32>) -> anyhow::Result<()> {
        self.inner.upsert(id, embedding).await
    }
    async fn search(&self, query: &[f32], top_k: usize) -> anyhow::Result<Vec<VectorHit>> {
        self.inner.search(query, top_k).await
    }
    async fn remove(&self, id: &str) -> anyhow::Result<()> {
        if self.fail_remove.load(Ordering::SeqCst) {
            anyhow::bail!("injected remove failure");
        }
        self.inner.remove(id).await?;
        self.after_remove.pass().await;
        Ok(())
    }
    async fn len(&self) -> anyhow::Result<usize> {
        self.inner.len().await
    }
    async fn upsert_batch(&self, items: &[(String, Vec<f32>)]) -> anyhow::Result<()> {
        self.before_upsert.pass().await;
        self.inner.upsert_batch(items).await
    }
    async fn contains(&self, id: &str) -> bool {
        self.inner.contains(id).await
    }
    async fn contains_many(&self, ids: &[String]) -> Vec<bool> {
        self.inner.contains_many(ids).await
    }
}

const DIM: usize = 32;

/// An indexer over a [`GatedStore`], optionally with a durable corpus.
fn gated_indexer(redb: Option<&std::path::Path>) -> (CodeIndexer, Arc<GatedStore>) {
    let gated = Arc::new(GatedStore {
        inner: UsearchStore::new(DIM).expect("usearch new"),
        before_upsert: Gate::default(),
        after_remove: Gate::default(),
        fail_remove: AtomicBool::new(false),
    });
    let store: Arc<dyn VectorStore> = gated.clone();
    let mut idx = CodeIndexer::new("test", TEST_ROOT)
        .with_components(Arc::new(MockEmbedder::new(DIM)), store);
    if let Some(path) = redb {
        idx.set_corpus_store(Arc::new(CorpusStore::open(path).expect("open corpus")));
    }
    (idx, gated)
}

/// Commit `(id, file, content)` chunks with no embedding, as a fast pass does.
async fn stage(idx: &CodeIndexer, chunks: &[(&str, &str, &str)]) {
    let parsed = ParsedBatch {
        chunks: chunks.iter().map(|(i, f, c)| raw(i, f, c)).collect(),
        embeddings: vec![None; chunks.len()],
        entities_by_file: vec![],
        parse_ms: 0,
        embed_ms: 0,
        vector_count: 0,
    };
    idx.commit_parsed_batch(parsed, true)
        .await
        .expect("stage chunks");
}

async fn has_vector(idx: &CodeIndexer, id: &str) -> bool {
    idx.store.as_ref().expect("store").contains(id).await
}

const A: (&str, &str, &str) = ("a", "src/a.rs", "fn removed() {}");
const C: (&str, &str, &str) = ("c", "src/c.rs", "fn kept() {}");

/// The main #8761 failure: a file removed, and one edited, while the pass
/// embedded. Before the fix the removed file's vector was re-inserted.
#[tokio::test]
async fn a_file_removed_or_edited_during_the_embed_phase_gets_no_vector() {
    let idx = make_indexer();
    stage(&idx, &[A, ("b", "src/b.rs", "fn edited() {}"), C]).await;
    let plan = idx.plan_deferred_embed().await;
    let run = plan.embed(None, None).await.expect("embed");

    idx.remove_file("src/a.rs").await.expect("remove a");
    stage(&idx, &[("b", "src/b.rs", "fn edited_again() {}")]).await;
    idx.commit_deferred_embed(plan, run).await.expect("commit");

    assert!(
        !has_vector(&idx, "a").await,
        "a removed file's vector came back"
    );
    assert!(
        !has_vector(&idx, "b").await,
        "an edited chunk got its pre-edit vector"
    );
    assert!(idx.get_embedding("a").is_none() && idx.get_embedding("b").is_none());
    assert!(
        has_vector(&idx, "c").await,
        "an untouched chunk is committed"
    );

    // The skipped edit is owed, not lost: the next pass embeds its new content.
    let next = idx
        .embed_deferred_chunks_gated(None, None)
        .await
        .expect("pass 2");
    assert_eq!(next.embedded, 1);
    let current = MockEmbedder::new(DIM)
        .embed("fn edited_again() {}")
        .await
        .unwrap();
    assert_eq!(idx.get_embedding("b"), Some(current));
}

/// A removal that lands after the pre-commit check and before the upsert:
/// `remove_file` finds no vector yet, so the commit must evict its own.
#[tokio::test]
async fn a_file_removed_during_the_upsert_has_its_vector_evicted() {
    let (idx, gated) = gated_indexer(None);
    stage(&idx, &[A, C]).await;
    let plan = idx.plan_deferred_embed().await;
    let run = plan.embed(None, None).await.expect("embed");

    let (reached, release) = gated.before_upsert.arm();
    let (committed, ()) = tokio::join!(idx.commit_deferred_embed(plan, run), async {
        reached.await.expect("upsert reached");
        idx.remove_file("src/a.rs").await.expect("remove a");
        release.send(()).expect("release upsert");
    });
    committed.expect("commit");

    assert!(
        !has_vector(&idx, "a").await,
        "an orphan vector survived the commit"
    );
    assert!(idx.get_embedding("a").is_none());
    assert!(has_vector(&idx, "c").await);
}

/// `remove_file` paused between its two drops. Dropping the vector first left
/// the map entry for the commit to see, and it upserted an orphan.
#[tokio::test]
async fn a_removal_racing_a_deferred_commit_leaves_no_orphan_vector() {
    let (idx, gated) = gated_indexer(None);
    stage(&idx, &[A, C]).await;
    let plan = idx.plan_deferred_embed().await;
    let run = plan.embed(None, None).await.expect("embed");

    let (reached, release) = gated.after_remove.arm();
    let (removed, committed) = tokio::join!(idx.remove_file("src/a.rs"), async {
        reached.await.expect("remove reached");
        let committed = idx.commit_deferred_embed(plan, run).await;
        release.send(()).expect("release remove");
        committed
    });
    removed.expect("remove a");
    committed.expect("commit");

    assert!(
        !has_vector(&idx, "a").await,
        "an orphan vector survived the removal"
    );
    assert!(has_vector(&idx, "c").await);
}

/// A vector the post-upsert check cannot evict is an error, not a warning.
#[tokio::test]
async fn a_failed_eviction_after_the_upsert_is_an_error() {
    let (idx, gated) = gated_indexer(None);
    stage(&idx, &[A, C]).await;
    let plan = idx.plan_deferred_embed().await;
    let run = plan.embed(None, None).await.expect("embed");

    let (reached, release) = gated.before_upsert.arm();
    let (committed, ()) = tokio::join!(idx.commit_deferred_embed(plan, run), async {
        reached.await.expect("upsert reached");
        idx.remove_file("src/a.rs").await.expect("remove a");
        gated.fail_remove.store(true, Ordering::SeqCst);
        release.send(()).expect("release upsert");
    });

    let err = committed.expect_err("an orphan the commit could not evict is an error");
    assert!(format!("{err:#}").contains("evict the vector of removed chunk a"));
}

/// Restores the rehydrate wait budget when dropped.
struct UnreadableCorpus;

impl Drop for UnreadableCorpus {
    fn drop(&mut self) {
        // SAFETY: serialized by `#[serial_test::serial]` on every caller.
        unsafe { std::env::remove_var("TRUSTY_REHYDRATE_WAIT_MS") };
    }
}

/// Break redb reads and evict the chunk map, so no rehydrate can refill it.
async fn make_corpus_unreadable(idx: &CodeIndexer) -> UnreadableCorpus {
    break_corpus_reads(idx);
    // SAFETY: serialized by `#[serial_test::serial]` on every caller.
    unsafe { std::env::set_var("TRUSTY_REHYDRATE_WAIT_MS", "25") };
    assert!(
        idx.reclaim_memory_now().await > 0,
        "the chunk map was evicted"
    );
    UnreadableCorpus
}

/// An evicted chunk map that cannot be rehydrated cannot answer "is this
/// chunk still live", so the commit refuses instead of guessing.
#[tokio::test]
#[serial_test::serial]
async fn commit_refuses_when_the_chunk_map_cannot_be_rehydrated() {
    let dir = tempfile::tempdir().unwrap();
    let (idx, _gated) = gated_indexer(Some(&dir.path().join("index.redb")));
    stage(&idx, &[A, C]).await;
    let plan = idx.plan_deferred_embed().await;
    let run = plan.embed(None, None).await.expect("embed");

    let _unreadable = make_corpus_unreadable(&idx).await;
    let committed = idx.commit_deferred_embed(plan, run).await;

    let err = committed.expect_err("an unreadable chunk map must not read as an empty corpus");
    // The pre-upsert check refuses; skipping it would drop every chunk as
    // removed and leave only the post-upsert check to fail.
    assert!(format!("{err:#}").contains("confirm the embedded chunks are still in the corpus"));
    assert!(!has_vector(&idx, "a").await && !has_vector(&idx, "c").await);
}

/// The post-upsert check refuses the same way: read as empty, the map would
/// mark every chunk just committed as removed and evict its vector.
#[tokio::test]
#[serial_test::serial]
async fn post_upsert_check_refuses_when_the_chunk_map_cannot_be_rehydrated() {
    let dir = tempfile::tempdir().unwrap();
    let (idx, gated) = gated_indexer(Some(&dir.path().join("index.redb")));
    stage(&idx, &[A, C]).await;
    let plan = idx.plan_deferred_embed().await;
    let run = plan.embed(None, None).await.expect("embed");

    let (reached, release) = gated.before_upsert.arm();
    let (committed, _unreadable) = tokio::join!(idx.commit_deferred_embed(plan, run), async {
        reached.await.expect("upsert reached");
        let unreadable = make_corpus_unreadable(&idx).await;
        release.send(()).expect("release upsert");
        unreadable
    });

    let err = committed.expect_err("an unreadable chunk map must not evict committed vectors");
    assert!(format!("{err:#}").contains("find chunks removed during the vector upsert"));
    assert!(has_vector(&idx, "a").await && has_vector(&idx, "c").await);
}
