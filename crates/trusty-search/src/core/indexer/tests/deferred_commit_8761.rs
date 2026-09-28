//! #8761 regression tests: a deferred-embed commit must not give a removed or
//! edited chunk its snapshot vector.
//!
//! Why: `plan_deferred_embed` snapshots the owed chunks and the embed phase
//! holds no lock (#8600), so `remove_file` can run between snapshot and commit.
//! The commit upserted every snapshot vector, re-inserting orphans for deleted
//! files that no removal path reclaims.
//! What: drives the three phases by hand and interleaves `remove_file`,
//! `remove_chunk` or an edit at each gap — the embed phase, the upsert, and
//! each removal's own gap between its map drop and its vector drop — plus the
//! error arms and the background rehydrate wait.
//! Test: the functions below.

use super::corpus_fault::break_corpus_reads;
use super::*;
use crate::core::embed::Embedder;
use crate::core::indexer::ingest::deferred::DeferredEmbedPlan;
use crate::core::indexer::ingest::embed::EmbedRun;
use crate::core::indexer::ingest::EmbedCatchUp;
use crate::core::store::VectorHit;
use std::sync::atomic::AtomicUsize;
use std::time::{Duration, Instant};
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
/// fail its next `fail_removes` removals.
struct GatedStore {
    inner: UsearchStore,
    before_upsert: Gate,
    after_remove: Gate,
    fail_removes: AtomicUsize,
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
        let fail = self
            .fail_removes
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok();
        if fail {
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
        fail_removes: AtomicUsize::new(0),
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
    let outcome = idx.commit_deferred_embed(plan, run).await.expect("commit");
    assert_eq!(outcome.embedded, 1, "only the untouched chunk is reported");

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
    let outcome = committed.expect("commit");
    assert_eq!(outcome.embedded, 1, "an evicted vector is not reported");

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

/// The file watcher's `remove_chunk` has the same gap as `remove_file`.
#[tokio::test]
async fn a_chunk_removal_racing_a_deferred_commit_leaves_no_orphan_vector() {
    let (idx, gated) = gated_indexer(None);
    stage(&idx, &[A, C]).await;
    let plan = idx.plan_deferred_embed().await;
    let run = plan.embed(None, None).await.expect("embed");

    let (reached, release) = gated.after_remove.arm();
    let (removed, committed) = tokio::join!(idx.remove_chunk("a"), async {
        reached.await.expect("remove reached");
        let committed = idx.commit_deferred_embed(plan, run).await;
        release.send(()).expect("release remove");
        committed
    });
    removed.expect("remove chunk a");
    committed.expect("commit");

    assert!(
        !has_vector(&idx, "a").await,
        "an orphan vector survived the chunk removal"
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
        gated.fail_removes.store(1, Ordering::SeqCst);
        release.send(()).expect("release upsert");
    });

    let err = committed.expect_err("an orphan the commit could not evict is an error");
    assert!(format!("{err:#}").contains("evict the vector of removed chunk a"));
}

/// The first failed eviction must not leave the other orphans in place.
#[tokio::test]
async fn one_failed_eviction_does_not_stop_the_others() {
    let (idx, gated) = gated_indexer(None);
    stage(&idx, &[A, ("b", "src/b.rs", "fn also_removed() {}"), C]).await;
    let plan = idx.plan_deferred_embed().await;
    let run = plan.embed(None, None).await.expect("embed");

    let (reached, release) = gated.before_upsert.arm();
    let (committed, ()) = tokio::join!(idx.commit_deferred_embed(plan, run), async {
        reached.await.expect("upsert reached");
        idx.remove_file("src/a.rs").await.expect("remove a");
        idx.remove_file("src/b.rs").await.expect("remove b");
        gated.fail_removes.store(1, Ordering::SeqCst);
        release.send(()).expect("release upsert");
    });

    let err = committed.expect_err("a failed eviction is still an error");
    assert!(
        format!("{err:#}").contains("1 of 2 removed chunks kept their vectors"),
        "{err:#}"
    );
    let orphans = has_vector(&idx, "a").await as usize + has_vector(&idx, "b").await as usize;
    assert_eq!(orphans, 1, "the eviction after the failed one still ran");
    assert!(has_vector(&idx, "c").await);
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
    // #8761: a regression that records no fault would otherwise wait 300 s.
    let committed = tokio::time::timeout(
        Duration::from_secs(20),
        idx.commit_deferred_embed(plan, run),
    )
    .await
    .expect("the unreadable map fails the commit before the ceiling");

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
    // #8761: a regression that records no fault would otherwise wait 300 s.
    let commit = tokio::time::timeout(
        Duration::from_secs(20),
        idx.commit_deferred_embed(plan, run),
    );
    let (committed, _unreadable) = tokio::join!(commit, async {
        reached.await.expect("upsert reached");
        let unreadable = make_corpus_unreadable(&idx).await;
        release.send(()).expect("release upsert");
        unreadable
    });
    let committed = committed.expect("the unreadable map fails the commit before the ceiling");

    let err = committed.expect_err("an unreadable chunk map must not evict committed vectors");
    assert!(format!("{err:#}").contains("find chunks removed during the vector upsert"));
    assert!(has_vector(&idx, "a").await && has_vector(&idx, "c").await);
}

/// Shrinks the interactive rehydrate budget to 3 x 25 ms and makes every
/// rehydrate scan take `scan_ms`; restores both when dropped.
struct SlowRehydrate;

impl SlowRehydrate {
    fn new(scan_ms: u64) -> Self {
        // SAFETY: serialized by `#[serial_test::serial]` on every caller.
        unsafe { std::env::set_var("TRUSTY_REHYDRATE_WAIT_MS", "25") };
        crate::core::indexer::TEST_REHYDRATE_DELAY_MS.store(scan_ms, Ordering::SeqCst);
        SlowRehydrate
    }
}

impl Drop for SlowRehydrate {
    fn drop(&mut self) {
        crate::core::indexer::TEST_REHYDRATE_DELAY_MS.store(0, Ordering::SeqCst);
        // SAFETY: serialized by `#[serial_test::serial]` on every caller.
        unsafe { std::env::remove_var("TRUSTY_REHYDRATE_WAIT_MS") };
    }
}

/// An indexer with a durable corpus, `A` and `C` staged, and one pass embedded.
async fn embedded_pass(dir: &std::path::Path) -> (CodeIndexer, DeferredEmbedPlan, EmbedRun) {
    let (idx, _gated) = gated_indexer(Some(&dir.join("index.redb")));
    stage(&idx, &[A, C]).await;
    let plan = idx.plan_deferred_embed().await;
    let run = plan.embed(None, None).await.expect("embed");
    (idx, plan, run)
}

/// The embed phase counts as activity, so the idle ticker does not evict the
/// chunk map the commit is about to read.
#[tokio::test]
async fn an_embed_pass_keeps_the_chunk_map_from_idle_eviction() {
    let dir = tempfile::tempdir().unwrap();
    let (idx, _gated) = gated_indexer(Some(&dir.path().join("index.redb")));
    stage(&idx, &[A, C]).await;
    let plan = idx.plan_deferred_embed().await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let _run = plan.embed(None, None).await.expect("embed");

    assert_eq!(
        idx.evict_chunks_if_idle(Duration::from_millis(250)).await,
        0,
        "an index mid-embed read as idle and lost its chunk map"
    );
}

/// A rehydrate slower than the ~27 s query budget (scaled here to 75 ms) is
/// waited out, not treated as a failure that throws the pass away.
#[tokio::test]
#[serial_test::serial]
async fn a_rehydrate_slower_than_the_query_budget_is_waited_out() {
    let dir = tempfile::tempdir().unwrap();
    let (idx, plan, run) = embedded_pass(dir.path()).await;
    let _slow = SlowRehydrate::new(400);
    assert!(
        idx.reclaim_memory_now().await > 0,
        "the chunk map was evicted"
    );

    let outcome = idx
        .commit_deferred_embed(plan, run)
        .await
        .expect("a slow rehydrate is waited out");

    assert_eq!(outcome.embedded, 2);
    assert!(has_vector(&idx, "a").await && has_vector(&idx, "c").await);
}

/// A read fault ends the wait at once and names the fault, rather than
/// running to the ceiling or reporting a generic eviction.
#[tokio::test]
#[serial_test::serial]
async fn a_read_fault_during_the_commit_wait_fails_fast() {
    let dir = tempfile::tempdir().unwrap();
    let (idx, plan, run) = embedded_pass(dir.path()).await;
    break_corpus_reads(&idx);
    let _slow = SlowRehydrate::new(400);
    assert!(
        idx.reclaim_memory_now().await > 0,
        "the chunk map was evicted"
    );

    let committed = tokio::time::timeout(
        Duration::from_secs(20),
        idx.commit_deferred_embed(plan, run),
    )
    .await
    .expect("a recorded read fault ends the wait before the ceiling");

    let err = committed.expect_err("an unreadable corpus fails the commit");
    assert!(
        err.downcast_ref::<crate::core::indexer::CorpusReadUnavailable>()
            .is_some(),
        "the error names the read fault: {err:#}"
    );
    assert!(!has_vector(&idx, "a").await && !has_vector(&idx, "c").await);
}

/// The wait is bounded: a rehydrate that outlasts the ceiling is an error.
#[tokio::test]
#[serial_test::serial]
async fn the_commit_wait_gives_up_at_its_ceiling() {
    let dir = tempfile::tempdir().unwrap();
    let (idx, _plan, _run) = embedded_pass(dir.path()).await;
    let _slow = SlowRehydrate::new(1_000);
    assert!(
        idx.reclaim_memory_now().await > 0,
        "the chunk map was evicted"
    );

    let started = Instant::now();
    let waited = idx.wait_for_corpus_view(Duration::from_millis(150)).await;

    let err = waited.expect_err("a rehydrate past the ceiling is an error");
    assert!(
        started.elapsed() < Duration::from_millis(800),
        "the wait ran past its ceiling"
    );
    assert!(
        format!("{err:#}").contains("did not rehydrate within"),
        "{err:#}"
    );
}

/// Holds `rehydrate_generation`'s lock on a plain thread until released. A
/// reclaim clears the chunk map, then needs this lock to mark it evicted, so
/// while it is held the reclaim parks between the two.
struct GenerationHeld {
    release: std::sync::mpsc::Sender<()>,
    holder: std::thread::JoinHandle<()>,
}

impl GenerationHeld {
    async fn lock(idx: &CodeIndexer) -> Self {
        let generation = Arc::clone(&idx.rehydrate_generation);
        let (locked_tx, locked_rx) = oneshot::channel();
        let (release, release_rx) = std::sync::mpsc::channel::<()>();
        let holder = std::thread::spawn(move || {
            let _held = generation.lock().unwrap_or_else(|e| e.into_inner());
            let _ = locked_tx.send(());
            let _ = release_rx.recv();
        });
        locked_rx.await.expect("generation lock taken");
        Self { release, holder }
    }

    /// Give the commit time to read the map, then let the reclaim finish.
    async fn release_after_the_commit_reads(self) {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let _ = self.release.send(());
        self.holder.join().expect("generation holder");
    }
}

/// Start a memory-pressure reclaim; return once it has cleared the chunk map
/// and parked on the held generation lock.
async fn park_a_reclaim(idx: &Arc<CodeIndexer>) -> tokio::task::JoinHandle<usize> {
    let reclaiming = Arc::clone(idx);
    let reclaim = tokio::spawn(async move { reclaiming.reclaim_memory_now().await });
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            // A held write lock, or a released empty map, means it cleared.
            let cleared = match idx.chunks.try_read() {
                Err(_) => true,
                Ok(map) => map.is_empty(),
            };
            if cleared {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the reclaim cleared the chunk map");
    reclaim
}

/// The commit waits the reclaim out and keeps both vectors.
async fn assert_both_vectors_kept(idx: &CodeIndexer, committed: anyhow::Result<EmbedCatchUp>) {
    let outcome = committed.expect("a reclaimed chunk map is waited out");
    assert_eq!(
        outcome.embedded, 2,
        "the commit read a reclaimed chunk map as an empty corpus"
    );
    assert!(!outcome.paused);
    assert!(
        has_vector(idx, "a").await && has_vector(idx, "c").await,
        "a vector of a live chunk is missing"
    );
}

/// A reclaim between the pre-upsert wait and its read. Before the fix the
/// commit read the cleared map as an empty corpus, dropped every chunk as
/// removed, and returned `Ok` with nothing committed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn a_reclaim_between_the_wait_and_the_pre_upsert_read_is_waited_out() {
    let dir = tempfile::tempdir().unwrap();
    let (idx, plan, run) = embedded_pass(dir.path()).await;
    let idx = Arc::new(idx);
    let held = GenerationHeld::lock(&idx).await;
    let reclaim = park_a_reclaim(&idx).await;

    let commit = tokio::time::timeout(
        Duration::from_secs(20),
        idx.commit_deferred_embed(plan, run),
    );
    let (committed, ()) = tokio::join!(commit, held.release_after_the_commit_reads());

    assert_both_vectors_kept(&idx, committed.expect("the commit finishes")).await;
    assert!(reclaim.await.expect("reclaim") > 0);
}

/// A reclaim between the post-upsert wait and its read. Before the fix the
/// commit read every chunk it had just upserted as removed and evicted its
/// vector.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
async fn a_reclaim_between_the_wait_and_the_post_upsert_read_is_waited_out() {
    let dir = tempfile::tempdir().unwrap();
    let (idx, gated) = gated_indexer(Some(&dir.path().join("index.redb")));
    let idx = Arc::new(idx);
    stage(&idx, &[A, C]).await;
    let plan = idx.plan_deferred_embed().await;
    let run = plan.embed(None, None).await.expect("embed");

    let (reached, release) = gated.before_upsert.arm();
    let commit = tokio::time::timeout(
        Duration::from_secs(20),
        idx.commit_deferred_embed(plan, run),
    );
    let (committed, reclaimed) = tokio::join!(commit, async {
        reached.await.expect("upsert reached");
        let held = GenerationHeld::lock(&idx).await;
        let reclaim = park_a_reclaim(&idx).await;
        release.send(()).expect("release upsert");
        held.release_after_the_commit_reads().await;
        reclaim.await.expect("reclaim")
    });

    assert_both_vectors_kept(&idx, committed.expect("the commit finishes")).await;
    assert!(reclaimed > 0);
}
