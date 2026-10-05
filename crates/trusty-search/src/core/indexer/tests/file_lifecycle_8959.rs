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
    assert!(!new_ids.is_empty(), "the new content must land");
    assert!(
        old_ids.iter().all(|id| !new_ids.contains(id)),
        "precondition: the rename changes every chunk id ({old_ids:?} vs {new_ids:?})"
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
