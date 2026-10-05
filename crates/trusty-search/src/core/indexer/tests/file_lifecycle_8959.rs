//! #8959 / #9179: a single-file write replaces the file's chunks, and file
//! writes and removals stop rebuilding the whole symbol graph per call.
//!
//! Why: `index_file` upserted by chunk id, and an id embeds the chunk's line
//! span or symbol name, so an edit that renamed a symbol left the old chunk
//! searchable beside the new one. Every `index_file` and `remove_file` also
//! ran a whole-corpus `rebuild_symbol_graph` (60 s and ~1.2 GB on a
//! 315K-chunk index).
//! What: the replace across corpus, BM25, HNSW, entities and the graph; the
//! deferred rebuild, counted; and the debounce predicate.
//! Test: `cargo test -p trusty-search -- file_lifecycle_8959`

use super::make_indexer;
use crate::core::indexer::graph_refresh::rebuild_due;
use crate::core::indexer::SearchQuery;
use crate::core::symbol_graph::SymbolMatch;

const PATH: &str = "src/zoo.rs";
const OLD: &str = "fn zebra_quokka_old() {\n    let marker = 1;\n}\n";
const NEW: &str = "fn walrus_pelican_new() {\n    let marker = 2;\n}\n";

/// Fails against origin/main (0e8496aee8) through behaviour: the old chunk
/// stays in the corpus, BM25, HNSW and the rebuilt graph, and a search for
/// its symbol returns it. Uses only APIs that exist on main.
#[tokio::test]
async fn index_file_replaces_a_files_prior_chunks() {
    let idx = make_indexer();
    idx.index_file(PATH, OLD).await.expect("first write");
    let old_ids = idx.chunk_ids_for_file(PATH).await;
    assert!(!old_ids.is_empty(), "the fixture must land chunks");

    idx.index_file(PATH, NEW).await.expect("second write");
    let new_ids = idx.chunk_ids_for_file(PATH).await;
    assert!(
        new_ids.iter().any(|id| id.contains("walrus_pelican_new")),
        "the new content must land: {new_ids:?}"
    );
    assert!(
        old_ids.iter().all(|id| !new_ids.contains(id)),
        "the rewrite left the old chunk ids on the path ({old_ids:?} vs {new_ids:?})"
    );

    // Corpus: only the new content is held for the path.
    {
        let chunks = idx.chunks.read().await;
        for id in &old_ids {
            assert!(
                !chunks.contains_key(id),
                "old chunk {id} left in the corpus"
            );
        }
        assert!(
            chunks
                .values()
                .filter(|c| c.file == PATH)
                .all(|c| !c.content.contains("zebra_quokka_old")),
            "old content left in the corpus"
        );
    }
    // BM25: no document answers the old symbol.
    let bm25_hits = idx
        .bm25
        .read()
        .await
        .score_query_all("zebra_quokka_old", 10);
    assert!(
        bm25_hits.iter().all(|(id, _)| !old_ids.contains(id)),
        "old chunk still in BM25: {bm25_hits:?}"
    );
    // HNSW: the old vectors are gone.
    let store = idx.store.as_ref().expect("make_indexer wires a store");
    for id in &old_ids {
        assert!(!store.contains(id).await, "old vector {id} left in HNSW");
    }
    // Entities: the file's list names only the new symbol.
    let ents = format!("{:?}", idx.entities.read().await.get(PATH));
    assert!(
        !ents.contains("zebra_quokka_old"),
        "old entity left: {ents}"
    );

    // Search: no hit carries the old content.
    let q = SearchQuery {
        text: "zebra_quokka_old".to_string(),
        top_k: 10,
        expand_graph: false,
        compact: false,
        ..Default::default()
    };
    let results = idx.search(&q).await.expect("search");
    assert!(
        results
            .iter()
            .all(|c| !c.content.contains("zebra_quokka_old")),
        "a search returned the old content: {results:#?}"
    );

    // Symbol graph: a full rebuild over the corpus holds only the new symbol.
    idx.rebuild_symbol_graph_now().await;
    let graph = idx.symbol_graph().await;
    assert_eq!(
        graph.resolve_symbol("zebra_quokka_old"),
        SymbolMatch::NotFound
    );
    assert_ne!(
        graph.resolve_symbol("walrus_pelican_new"),
        SymbolMatch::NotFound
    );
}

/// Fails against origin/main through behaviour: blank content left the old
/// chunk in place, because the commit only ran when chunks were produced.
/// A JSON path, because blank JSON parses into zero chunks; a blank `.rs`
/// file still gets the AST chunker's whole-file fallback chunk.
#[tokio::test]
async fn index_file_with_blank_content_drops_the_files_old_chunks() {
    const JSON: &str = "config/zoo.json";
    let idx = make_indexer();
    idx.index_file(JSON, "{\"zebra_quokka_old\": 1}\n")
        .await
        .expect("first write");
    assert!(!idx.chunk_ids_for_file(JSON).await.is_empty());

    idx.index_file(JSON, "\n").await.expect("blank write");
    assert!(
        idx.chunk_ids_for_file(JSON).await.is_empty(),
        "a file rewritten blank keeps no chunks"
    );
}

/// #8959 / #9179: one `index_file` and one `remove_file` on a three-file
/// index run no full rebuild; one flush then runs exactly one and leaves the
/// graph matching the corpus. Fails with `rebuild_symbol_graph` restored in
/// either write path: the counter moves before the flush.
#[tokio::test]
async fn index_file_and_remove_file_defer_the_symbol_graph_rebuild() {
    let idx = make_indexer();
    let files = vec![
        (
            "src/a.rs".to_string(),
            "fn alpha() { beta(); }\n".to_string(),
        ),
        ("src/b.rs".to_string(), "fn beta() {}\n".to_string()),
        ("src/c.rs".to_string(), "fn gamma() {}\n".to_string()),
    ];
    idx.index_files_batch(&files).await.expect("seed batch");
    let seeded = idx.symbol_graph_full_rebuilds();
    assert!(
        !idx.symbol_graph_is_stale(),
        "a batch commit rebuilds in place"
    );

    // Removing a path the index never held leaves nothing to rebuild.
    assert_eq!(idx.remove_file("src/never.rs").await.expect("no-op"), 0);
    assert!(
        !idx.symbol_graph_is_stale(),
        "a no-op removal marks nothing"
    );

    idx.index_file("src/a.rs", "fn omega() { gamma(); }\n")
        .await
        .expect("index_file");
    let removed = idx.remove_file("src/b.rs").await.expect("remove_file");
    assert!(removed > 0, "precondition: src/b.rs had chunks");
    assert_eq!(
        idx.symbol_graph_full_rebuilds(),
        seeded,
        "a single-file write or removal must not run a whole-corpus rebuild"
    );
    assert!(idx.symbol_graph_is_stale());

    assert!(
        idx.flush_symbol_graph().await,
        "the pending writes are flushed"
    );
    assert_eq!(
        idx.symbol_graph_full_rebuilds(),
        seeded + 1,
        "one rebuild per burst"
    );
    assert!(!idx.flush_symbol_graph().await, "nothing left to flush");
    assert_eq!(idx.symbol_graph_full_rebuilds(), seeded + 1);

    // Names share no substring: `resolve_symbol` falls back to substring match.
    let graph = idx.snapshot_symbol_graph().await;
    assert_eq!(graph.resolve_symbol("alpha"), SymbolMatch::NotFound);
    assert_eq!(graph.resolve_symbol("beta"), SymbolMatch::NotFound);
    assert_ne!(graph.resolve_symbol("omega"), SymbolMatch::NotFound);
    assert_ne!(graph.resolve_symbol("gamma"), SymbolMatch::NotFound);
}

/// The debounce rule: fresh never fires; stale fires after the quiet window
/// or, under a continuous write stream, after the max wait.
#[test]
fn rebuild_due_waits_for_quiet_or_max_wait() {
    // (stale_since, last_write, now, quiet, max_wait, due)
    let cases = [
        (0, 0, 10_000, 0, 0, false),                   // fresh
        (1_000, 1_000, 1_500, 2_000, 60_000, false),   // inside the quiet window
        (1_000, 1_000, 3_000, 2_000, 60_000, true),    // quiet window elapsed
        (1_000, 60_500, 61_000, 2_000, 60_000, true),  // writes never stop: max wait
        (1_000, 60_500, 60_900, 2_000, 60_000, false), // neither bound reached
    ];
    for (stale, last, now, quiet, max, due) in cases {
        assert_eq!(
            rebuild_due(stale, last, now, quiet, max),
            due,
            "stale={stale} last={last} now={now} quiet={quiet} max={max}"
        );
    }
}

/// #9179: the real debounce constants on paused tokio time. A write every
/// second never leaves a 2 s quiet window, so only the 60 s cap fires the
/// rebuild; after it, a quiet burst rebuilds 2 s after its last write.
#[tokio::test(start_paused = true)]
async fn continuous_writes_still_rebuild_at_the_max_wait_cap() {
    use std::time::Duration;

    use crate::core::indexer::graph_refresh::{GRAPH_REFRESH_MAX_WAIT, GRAPH_REFRESH_QUIET};

    let idx = make_indexer();
    let base = idx.symbol_graph_full_rebuilds();
    let tick = Duration::from_secs(1);

    // Write stream: one deferred write, then one ticker pass, each second.
    let mut fired_at = None;
    for second in 1..=GRAPH_REFRESH_MAX_WAIT.as_secs() + 5 {
        idx.mark_symbol_graph_stale();
        tokio::time::advance(tick).await;
        if idx
            .refresh_symbol_graph_if_due(GRAPH_REFRESH_QUIET, GRAPH_REFRESH_MAX_WAIT)
            .await
        {
            fired_at = Some(second);
            break;
        }
    }
    assert_eq!(
        fired_at,
        Some(GRAPH_REFRESH_MAX_WAIT.as_secs()),
        "a continuous write stream must rebuild exactly at the max-wait cap"
    );
    assert_eq!(idx.symbol_graph_full_rebuilds(), base + 1);
    assert!(!idx.symbol_graph_is_stale());

    // Quiet burst: due only once 2 s pass with no write.
    idx.mark_symbol_graph_stale();
    tokio::time::advance(tick).await;
    assert!(
        !idx.refresh_symbol_graph_if_due(GRAPH_REFRESH_QUIET, GRAPH_REFRESH_MAX_WAIT)
            .await,
        "inside the quiet window"
    );
    tokio::time::advance(tick).await;
    assert!(
        idx.refresh_symbol_graph_if_due(GRAPH_REFRESH_QUIET, GRAPH_REFRESH_MAX_WAIT)
            .await,
        "the quiet window elapsed"
    );
    assert_eq!(idx.symbol_graph_full_rebuilds(), base + 2);
}

// ---------------------------------------------------------------------------
// Code-critic round on #8959 / #9179: durability, fail-closed delete, and the
// same-path write race.
// ---------------------------------------------------------------------------

/// A corpus-backed indexer under a test-unique id, so the id-keyed fault seam
/// never reaches a concurrently running test.
fn corpus_indexer(id: &str, redb: &std::path::Path) -> crate::core::indexer::CodeIndexer {
    let mut idx = crate::core::indexer::CodeIndexer::new(id, "/tmp/lifecycle-8959");
    let store = crate::core::corpus::CorpusStore::open(redb).expect("open corpus store");
    idx.set_corpus_store(std::sync::Arc::new(store));
    idx
}

/// Arms the failed-redb-delete seam for one index id; disarms on drop.
struct FailChunkDelete(String);

impl FailChunkDelete {
    fn arm(id: &str) -> Self {
        crate::core::indexer::TEST_FAIL_CHUNK_DELETE
            .lock()
            .expect("seam lock")
            .push(id.to_string());
        Self(id.to_string())
    }
}

impl Drop for FailChunkDelete {
    fn drop(&mut self) {
        if let Ok(mut f) = crate::core::indexer::TEST_FAIL_CHUNK_DELETE.lock() {
            f.retain(|id| *id != self.0);
        }
    }
}

/// #8959 finding 1: a write's deferred rebuild survives a restart inside the
/// debounce window. Fails at 35ccee8252: the stale mark lived only in memory,
/// so the reopened index booted the old persisted graph as current and never
/// rebuilt it.
#[tokio::test]
async fn a_deferred_rebuild_survives_a_reopen() {
    const ID: &str = "lifecycle-8959-reopen";
    let dir = tempfile::tempdir().expect("tempdir");
    let redb = dir.path().join("index.redb");
    {
        let idx = corpus_indexer(ID, &redb);
        idx.index_files_batch(&[
            ("src/zoo.rs".to_string(), OLD.to_string()),
            (
                "src/keep.rs".to_string(),
                "fn heron_keep() {}\n".to_string(),
            ),
        ])
        .await
        .expect("seed batch persists the graph");
        idx.index_file("src/zoo.rs", NEW).await.expect("rewrite");
        assert!(idx.symbol_graph_is_stale(), "the rebuild is deferred");
        // Dropped with the rebuild still pending: no flush, no ticker.
    }

    let idx = corpus_indexer(ID, &redb);
    idx.load_chunks_from_redb().await.expect("warm boot");
    assert!(
        idx.symbol_graph_is_stale(),
        "the reopened index must schedule the rebuild the old process owed"
    );
    assert!(
        idx.refresh_symbol_graph_if_due(std::time::Duration::ZERO, std::time::Duration::MAX)
            .await,
        "the scheduled rebuild runs on the first due tick"
    );
    let graph = idx.snapshot_symbol_graph().await;
    assert_eq!(
        graph.resolve_symbol("zebra_quokka_old"),
        SymbolMatch::NotFound,
        "the rewritten-away symbol is gone after the scheduled rebuild"
    );
    assert_ne!(
        graph.resolve_symbol("walrus_pelican_new"),
        SymbolMatch::NotFound
    );
    drop(idx);

    // The rebuild persisted its graph and cleared the durable mark.
    let idx = corpus_indexer(ID, &redb);
    idx.load_chunks_from_redb().await.expect("second warm boot");
    assert!(!idx.symbol_graph_is_stale(), "a rebuilt graph boots fresh");
}

/// #8959 finding 2, error arm: a failed redb delete of the superseded ids
/// fails the write before the commit and leaves the old chunks in place, and
/// a retry then succeeds. Fails at 35ccee8252: the failure was only logged,
/// so the write answered `Ok`.
#[tokio::test]
async fn a_failed_superseded_delete_fails_the_write_and_stays_retryable() {
    const ID: &str = "lifecycle-8959-delete-err";
    let dir = tempfile::tempdir().expect("tempdir");
    let idx = corpus_indexer(ID, &dir.path().join("index.redb"));
    idx.index_file(PATH, OLD).await.expect("first write");
    let mut old_ids = idx.chunk_ids_for_file(PATH).await;
    old_ids.sort();
    assert!(!old_ids.is_empty());

    let fault = FailChunkDelete::arm(ID);
    let err = idx.index_file(PATH, NEW).await;
    assert!(
        err.is_err(),
        "a failed redb delete must fail the write, not answer Ok"
    );
    let mut held = idx.chunk_ids_for_file(PATH).await;
    held.sort();
    assert_eq!(
        held, old_ids,
        "a failed write commits nothing and keeps the old ids for a retry"
    );

    drop(fault);
    idx.index_file(PATH, NEW).await.expect("retry");
    let ids = idx.chunk_ids_for_file(PATH).await;
    assert!(ids.iter().all(|id| !old_ids.contains(id)), "{ids:?}");
    assert!(
        ids.iter().any(|id| id.contains("walrus_pelican_new")),
        "{ids:?}"
    );
}

/// #8959 finding 2, durable arm: a rewrite retried after a failed delete
/// leaves no old id in redb, so a reopen loads only the new chunks. Fails at
/// 35ccee8252: the failed write dropped the old ids from memory only, the
/// retry found nothing to remove, and the reopen loaded them back.
#[tokio::test]
async fn a_rewrite_after_a_failed_delete_leaves_no_old_ids_after_reopen() {
    const ID: &str = "lifecycle-8959-delete-reopen";
    let dir = tempfile::tempdir().expect("tempdir");
    let redb = dir.path().join("index.redb");
    let old_ids = {
        let idx = corpus_indexer(ID, &redb);
        idx.index_file(PATH, OLD).await.expect("first write");
        let old_ids = idx.chunk_ids_for_file(PATH).await;
        {
            let _fault = FailChunkDelete::arm(ID);
            let _ = idx.index_file(PATH, NEW).await;
        }
        idx.index_file(PATH, NEW).await.expect("retry");
        old_ids
    };

    let idx = corpus_indexer(ID, &redb);
    idx.load_chunks_from_redb().await.expect("warm boot");
    let ids = idx.chunk_ids_for_file(PATH).await;
    assert!(
        ids.iter().all(|id| !old_ids.contains(id)),
        "old ids came back from redb after a reopen: {ids:?}"
    );
    assert!(
        ids.iter().any(|id| id.contains("walrus_pelican_new")),
        "{ids:?}"
    );
}

/// An embedder that blocks every batch on a gate the test opens one permit at
/// a time, and counts the batches that reached it.
struct GatedEmbedder {
    gate: tokio::sync::Semaphore,
    started: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl crate::core::embed::Embedder for GatedEmbedder {
    async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
        unimplemented!("index_file embeds through embed_batch")
    }
    async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        self.started
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.gate.acquire().await?.forget();
        Ok(texts.iter().map(|_| vec![0.5f32; 8]).collect())
    }
    fn dimension(&self) -> usize {
        8
    }
}

/// Wait, bounded, until `n` embed batches have reached the gate.
async fn wait_started(embedder: &GatedEmbedder, n: usize) {
    for _ in 0..500 {
        if embedder.started.load(std::sync::atomic::Ordering::SeqCst) >= n {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("embed batch {n} never reached the gate");
}

/// #8959 finding 4: two writes to one path that overlap at the embed await
/// end with only the later version's ids. Fails at 35ccee8252: both writes
/// snapshotted the first version's ids, so the earlier writer's chunks were
/// never superseded and both versions stayed indexed.
#[tokio::test]
async fn concurrent_writes_to_one_path_keep_only_the_last_version() {
    use std::sync::Arc;

    const V_A: &str = "fn osprey_first() {\n    let marker = 3;\n}\n";
    const V_B: &str = "fn ibis_second() {\n    let marker = 4;\n}\n";
    let embedder = Arc::new(GatedEmbedder {
        gate: tokio::sync::Semaphore::new(0),
        started: std::sync::atomic::AtomicUsize::new(0),
    });
    let dyn_embedder: Arc<dyn crate::core::embed::Embedder> = embedder.clone();
    let store: Arc<dyn crate::core::store::VectorStore> =
        Arc::new(crate::core::store::UsearchStore::new(8).expect("usearch new"));
    let idx = Arc::new(
        crate::core::indexer::CodeIndexer::new("lifecycle-8959-race", "/tmp/lifecycle-8959")
            .with_components(dyn_embedder, store),
    );

    embedder.gate.add_permits(1);
    idx.index_file(PATH, OLD).await.expect("seed write");

    let a = tokio::spawn({
        let idx = Arc::clone(&idx);
        async move { idx.index_file(PATH, V_A).await }
    });
    wait_started(&embedder, 2).await; // A holds its snapshot, parked at the embed.
    let b = tokio::spawn({
        let idx = Arc::clone(&idx);
        async move { idx.index_file(PATH, V_B).await }
    });
    // B runs until it blocks: on the path lock, or (racy code) at the embed
    // with a snapshot taken before A committed.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    embedder.gate.add_permits(1);
    a.await.expect("join A").expect("write A");
    embedder.gate.add_permits(1);
    b.await.expect("join B").expect("write B");

    let ids = idx.chunk_ids_for_file(PATH).await;
    assert!(
        ids.iter().any(|id| id.contains("ibis_second")),
        "the last write landed: {ids:?}"
    );
    assert!(
        ids.iter()
            .all(|id| !id.contains("osprey_first") && !id.contains("zebra_quokka_old")),
        "an earlier version's chunks stayed on the path: {ids:?}"
    );
}
