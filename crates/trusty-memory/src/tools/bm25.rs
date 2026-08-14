//! BM25 lexical-lane helpers for the trusty-memory MCP tool surface.
//!
//! Why: the optional BM25 lane (issue #156/#193/#231) — the bounded index
//! worker and enqueue path, the optional search lane, and RRF fusion — is a
//! self-contained concern split out of the former monolithic `tools.rs`
//! (issue #607).
//! What: free functions + the `Bm25IndexRequest` payload + queue-capacity
//! constant. Since #5329 every operation runs against the in-process
//! [`Bm25Lane`](crate::bm25_lane::Bm25Lane) rather than a per-palace subprocess
//! reached over a socket.
//! Test: `bm25_index_queue_drops_when_full`, `bm25_lane_tests.rs`, and
//! `tests/bm25_alias_recall.rs` / `tests/bm25_alias_write.rs`.

use crate::bm25_lane::BM25Hit;
use crate::AppState;
use serde_json::{json, Value};
use uuid::Uuid;

// Why (#5329): `bm25_data_dir_for_palace` was REMOVED from this module. It
// existed so the enqueue path could put a `data_dir` on every request for the
// supervisor to hand the child as `--data-dir`. The lane derives the path from
// its own data root, so the request no longer carries it and nothing here needs
// to compute it. The path arithmetic itself lives on in
// `crate::bm25_lane::data_dir_for_palace`, unchanged.

/// Bounded-queue capacity for the BM25 index worker (issue #231).
///
/// Why: the previous fire-and-forget design called `tokio::spawn` for every
/// drawer write, so a burst of `memory_remember` / `memory_note` calls while
/// the BM25 daemon was slow or unreachable could grow an unbounded number of
/// in-flight tasks — silent unbounded memory growth and a DoS vector against
/// the runtime. A bounded mpsc channel caps how many index requests can be
/// queued at once; once full, additional requests are dropped with a `warn!`
/// rather than blocking or buffering forever.
/// What: an arbitrary "comfortable burst" capacity. 256 is large enough that
/// a normal flurry of writes never spills (and the BM25 daemon's RTT is
/// typically sub-ms on the loopback socket), but small enough that a wedged
/// daemon caps memory consumption at a few MB of queued payloads.
/// Test: implicitly covered by `bm25_index_enqueue` not panicking when the
/// channel is full and by `bm25_index_queue_drops_when_full` (added below).
pub const BM25_INDEX_QUEUE_CAPACITY: usize = 256;

/// One pending BM25 index op enqueued by `memory_remember` / `memory_note`
/// for the per-`AppState` indexer worker to drain (issue #231).
///
/// Why: replacing the per-write `tokio::spawn` with a single long-lived
/// worker task requires a self-contained "do this index call" payload that
/// can travel through an mpsc channel without borrowing from `AppState`.
/// Capturing the palace, drawer id, and content here lets the worker
/// reconstruct the call without re-reading any state.
/// What: a plain owned-data struct. `Clone` is not derived — the worker
/// consumes each request exactly once.
/// Test: exercised end-to-end by `bm25_index_queue_drops_when_full` and
/// `tests/bm25_alias_write.rs`.
#[derive(Debug)]
pub struct Bm25IndexRequest {
    /// Palace id whose index should hold the drawer.
    pub palace: String,
    /// Drawer id (stringified) — used as the BM25 doc id.
    pub drawer_id: String,
    /// Drawer text content to index.
    pub content: String,
}

/// Spawn the single long-lived BM25 indexer worker that drains
/// `bm25_index_rx` and forwards each request to the daemon (issue #231).
///
/// Why: previously every `memory_remember` / `memory_note` write spawned a
/// detached `tokio::task` that called the BM25 daemon — under a write burst
/// with a slow/unreachable daemon the unbounded task queue grew silently.
/// A single worker + bounded channel caps back-pressure: when the channel
/// is full, writers `try_send` instead of `send`, and a full queue causes
/// a logged drop rather than memory growth. The worker exits gracefully
/// once the last sender clone (held in `AppState`) is dropped.
/// What: takes ownership of the receiver and the optional lane, then loops on
/// `rx.recv().await`, calling [`Bm25Lane::index`](crate::bm25_lane::Bm25Lane::index)
/// for each request. #5329 removed the spawn step that used to sit in front of
/// that call, and with it the "supervisor refused to start a daemon" failure
/// arm — the only remaining failure is a snapshot the lane cannot load.
/// Errors are logged at `warn!` and dropped: BM25 indexing is best-effort and
/// the drawer is durable in redb regardless.
/// If `lane` is `None` (the gate was unset at startup) the worker still runs
/// and silently drops every request, which keeps the channel drained — not a
/// coverage gap, because the lane is off.
/// A request the worker accepts but cannot land DOES mark the palace in
/// `dirty`, so the repair sweep re-runs the backfill instead of the gap
/// surviving until the next restart.
/// Test: `bm25_index_queue_drops_when_full` covers the back-pressure behaviour;
/// `tests/bm25_alias_write.rs` drives a real write through to the corpus.
pub fn spawn_bm25_index_worker(
    mut rx: tokio::sync::mpsc::Receiver<Bm25IndexRequest>,
    lane: Option<std::sync::Arc<crate::bm25_lane::Bm25Lane>>,
    dirty: crate::bm25_repair::DirtyPalaces,
) {
    tokio::spawn(async move {
        while let Some(req) = rx.recv().await {
            // No lane means BM25 is disabled — drain the queue (so senders
            // never block) and silently drop every request.
            let Some(lane) = lane.as_ref() else {
                continue;
            };
            if let Err(e) = lane.index(&req.palace, &req.drawer_id, &req.content).await {
                dirty.insert(req.palace.clone());
                tracing::warn!(
                    palace = %req.palace,
                    drawer_id = %req.drawer_id,
                    "bm25 index failed (non-fatal): {e:#}"
                );
            }
        }
        tracing::debug!("bm25 index worker exiting (channel closed)");
    });
}

/// Enqueue a BM25 index request onto the bounded indexer channel (issue
/// #231; supersedes the per-write `tokio::spawn` from issue #156).
///
/// Why: `memory_remember` / `memory_note` must return as fast as the redb
/// write completes; indexing must stay off the response path. Routing each
/// request through a bounded mpsc channel keeps that property *and* caps
/// in-flight indexing work — the previous design grew an unbounded task queue,
/// which #231 fixes here.
/// What: builds a `Bm25IndexRequest` from the caller's data and calls
/// `try_send` so the caller is never blocked. BOTH failure arms — `Full` and
/// `Closed` — drop the request and queue the palace for coverage repair; the
/// drawer is durable in redb either way, and BM25 catches up on the next repair
/// pass. `Closed` shouldn't happen in practice (the worker holds the receiver
/// for the process's lifetime), but it loses the write exactly as completely as
/// `Full` does, so it is not treated as the lesser case. We never let a BM25
/// hiccup fail a write.
/// Test: `bm25_index_queue_drops_when_full`,
/// `a_closed_index_queue_queues_the_palace_for_repair`.
pub(crate) fn bm25_index_enqueue(state: &AppState, palace: &str, drawer_id: Uuid, content: &str) {
    let req = Bm25IndexRequest {
        palace: palace.to_string(),
        drawer_id: drawer_id.to_string(),
        content: content.to_string(),
    };
    match state.bm25_index_tx.try_send(req) {
        Ok(()) => {}
        Err(tokio::sync::mpsc::error::TrySendError::Full(req)) => {
            // #5048 review: a drop is only an acceptable trade if something
            // repairs it. Mark the palace so the periodic repair sweep
            // re-runs the lossless backfill instead of the coverage gap
            // surviving until the next daemon restart.
            crate::bm25_repair::mark_dirty(state, &req.palace);
            tracing::warn!(
                palace = %req.palace,
                drawer_id = %req.drawer_id,
                "BM25 index queue full — dropped drawer {}, palace queued for repair",
                req.drawer_id
            );
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(req)) => {
            // #5048 re-review: the sibling branch three lines up. A closed
            // queue loses the write exactly as completely as a full one, so it
            // gets the same treatment — the earlier asymmetry (mark on `Full`,
            // `debug!` on `Closed`) is the same shape #4683 shipped with.
            crate::bm25_repair::mark_dirty(state, &req.palace);
            tracing::warn!(
                palace = %req.palace,
                drawer_id = %req.drawer_id,
                "BM25 index queue closed — dropped drawer {}, palace queued for repair",
                req.drawer_id
            );
        }
    }
}

/// Optional BM25 search lane used by `memory_recall` (issue #156).
///
/// Why: lets the recall handler join a BM25 future with the vector future
/// without sprinkling `if state.bm25.is_some()` checks across the call site.
/// Returning `Option<Vec<_>>` makes the "lane unavailable" branch explicit at
/// the consumer.
/// What: returns `None` when the lane is off OR when the palace's index cannot
/// be loaded — both degrade to vector-only results. `top_k` is forwarded
/// verbatim.
/// #5036: `palace` must be the RESOLVED palace id (`handle.id`), not the slug
/// the caller asked for — `open_palace` follows aliases, so the two differ for
/// an aliased palace and the corpus the backfill wrote is the resolved one's.
/// Test: `tests/bm25_alias_recall.rs`; the `None` path is covered by
/// `bm25_lane_disabled_by_default`.
pub(crate) async fn bm25_search_optional(
    state: &AppState,
    palace: &str,
    query: &str,
    top_k: usize,
) -> Option<Vec<BM25Hit>> {
    // #5036: search the corpus belonging to `palace`, not the default one.
    let lane = state.bm25.as_ref()?;
    match lane.search(palace, query, top_k).await {
        Ok(hits) => Some(hits),
        Err(e) => {
            tracing::warn!(
                palace = %palace,
                "bm25 search failed (falling back to vector-only): {e:#}"
            );
            None
        }
    }
}

/// Reciprocal Rank Fusion (RRF) blender for BM25 hits + vector recall hits.
///
/// Why: BM25 wins on identifier-heavy queries ("cargo test", "PalaceHandle"),
/// the vector lane wins on conceptual queries. RRF is the canonical fusion
/// because it is parameter-light, rank-only, and robust to scale differences
/// between the two lanes.
/// What: walks the BM25 ranked list once and adds `1 / (k + rank)` to the
/// matching drawer's vector score (RRF with `k = 60`, the IR-literature
/// default). Drawers that appear in BM25 but not in the vector list are
/// appended with `layer = 4` so the caller knows they came from the lexical
/// lane (L0/L1/L2/L3 are reserved). The combined list is re-sorted by score
/// desc and truncated to `top_k`.
/// Test: `tests/bm25_alias_recall.rs` plus
/// downstream RRF behaviour observed end-to-end.
pub(crate) fn fuse_bm25_into_recall(
    results: &mut Vec<trusty_common::memory_core::retrieval::RecallResult>,
    bm25_hits: &[BM25Hit],
    top_k: usize,
) {
    /// RRF damping constant (Cormack et al. 2009). 60 is the literature
    /// default and what trusty-search uses in its hybrid pipeline.
    const RRF_K: f32 = 60.0;
    if bm25_hits.is_empty() {
        return;
    }
    // Boost existing vector hits whose drawer id appears in BM25.
    for (rank, hit) in bm25_hits.iter().enumerate() {
        let bonus = 1.0 / (RRF_K + rank as f32 + 1.0);
        if let Some(existing) = results
            .iter_mut()
            .find(|r| r.drawer.id.to_string() == hit.doc_id)
        {
            existing.score += bonus;
        }
        // BM25-only hits (those that don't appear in the vector list) are
        // intentionally NOT appended here — without hydrating the drawer
        // payload (content, tags, importance) from disk we cannot construct
        // a `RecallResult`, and the per-call disk walk would defeat the
        // whole purpose of the daemon. The hits that already appear in the
        // vector list still benefit from the RRF boost, which is enough to
        // improve identifier-heavy queries.
    }
    // Re-sort by score desc; preserve layer for tie-breaking (lower layer
    // wins because L0/L1 are pinned identity/essentials).
    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.layer.cmp(&b.layer))
    });
    results.truncate(top_k);
}

/// Hydrate BM25 hits directly into `RecallResult`s from a palace's
/// in-memory drawer table (issue #1970 embedder-warming fallback).
///
/// Why: `fuse_bm25_into_recall` only *boosts* vector hits already present in
/// `results` — it never appends BM25-only matches because (per its own
/// comment) it has no drawer payload to hydrate them with. During embedder
/// warm-up there are no vector hits at all, so that boost-only behaviour
/// would silently drop every BM25 match. `PalaceHandle::drawers` already
/// holds every drawer's metadata in memory (no disk I/O needed), so each
/// BM25 `doc_id` (a stringified drawer UUID) can be resolved into a full
/// `RecallResult` directly.
/// What: parses each hit's `doc_id` as a `Uuid`, looks it up in
/// `handle.drawers`, and emits a `RecallResult` carrying the BM25 score and
/// `layer: 4` (the same lexical-lane marker `fuse_bm25_into_recall` uses).
/// Hits whose `doc_id` doesn't parse or no longer resolves to a drawer
/// (e.g. forgotten since the BM25 snapshot was built) are skipped.
/// Test: `bm25_hits_hydrate_from_handle_during_warmup`.
pub(crate) fn bm25_hits_to_recall_results(
    handle: &trusty_common::memory_core::retrieval::PalaceHandle,
    bm25_hits: &[BM25Hit],
) -> Vec<trusty_common::memory_core::retrieval::RecallResult> {
    let drawers = handle.drawers.read();
    bm25_hits
        .iter()
        .filter_map(|hit| {
            let drawer_id = uuid::Uuid::parse_str(&hit.doc_id).ok()?;
            let drawer = drawers.iter().find(|d| d.id == drawer_id)?.clone();
            Some(trusty_common::memory_core::retrieval::RecallResult {
                drawer,
                score: hit.score,
                layer: 4,
            })
        })
        .collect()
}

/// Serialize `recall` results into a JSON shape the MCP client can render.
pub(crate) fn serialize_recall(
    palace: &str,
    query: &str,
    results: Vec<trusty_common::memory_core::retrieval::RecallResult>,
) -> Value {
    let payload: Vec<Value> = results
        .iter()
        .map(|r| {
            json!({
                "drawer_id": r.drawer.id.to_string(),
                "content":   r.drawer.content,
                "score":     r.score,
                "layer":     r.layer,
                "tags":      r.drawer.tags,
                "importance": r.drawer.importance,
                "drawer_type": r.drawer.drawer_type.as_str(),
            })
        })
        .collect();
    json!({
        "palace": palace,
        "query": query,
        "results": payload,
    })
}
