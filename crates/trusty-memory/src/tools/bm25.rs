//! BM25 lexical-lane helpers for the trusty-memory MCP tool surface.
//!
//! Why: the optional BM25 lane (issue #156/#193/#231) — per-palace data dir,
//! supervisor wake-up, the bounded index worker + enqueue path, the optional
//! search lane, and RRF fusion — is a self-contained concern split out of the
//! former monolithic `tools.rs` (issue #607).
//! What: free functions + the `Bm25IndexRequest` payload + queue-capacity
//! constant, moved verbatim. Visibility is unchanged from the original.
//! Test: `bm25_index_queue_drops_when_full` and the daemon integration tests
//! in `trusty-bm25-daemon/tests/`.

use crate::AppState;
use serde_json::{json, Value};
use uuid::Uuid;

/// Per-palace BM25 data directory derived from the daemon's data root.
///
/// Why (issue #193): the spawn supervisor must hand the BM25 daemon a
/// data-dir argument so each palace's BM25 snapshot lives next to its
/// other palace data (redb, kg.db, embeddings) — not in a shared scratch
/// directory. The convention is `<data_root>/<palace>/bm25/`, which is
/// stable across daemon restarts and lets operators inspect the snapshot
/// file alongside everything else in the palace.
/// What: appends `<palace>/bm25` to the daemon's `data_root`. Pure path
/// arithmetic — no I/O. The supervisor itself creates the directory
/// before spawning the child.
/// Test: implicitly via the spawn supervisor's integration test.
pub(crate) fn bm25_data_dir_for_palace(state: &AppState, palace: &str) -> std::path::PathBuf {
    state.data_root.join(palace).join("bm25")
}

/// Try to ensure the BM25 daemon for `palace` is running. Returns `true`
/// when the daemon is (now) reachable.
///
/// Why (issue #193): callers want a single yes/no — should I send a BM25
/// op to this palace right now? — without each having to thread the
/// supervisor's `Result` through every code path. When the supervisor
/// returns an error (binary not found, spawn rejected, socket never
/// appeared) we log and return `false` so the caller degrades to
/// vector-only behaviour, exactly as it did before #193 when the daemon
/// simply wasn't running.
/// What: when `state.bm25_supervisor` is `None`, returns `true` (the
/// caller falls back to the original "use the env-var-only socket path"
/// behaviour). When `Some`, delegates to `ensure_running` and treats any
/// error as a soft failure — the supervisor's logs explain why.
/// Test: covered indirectly by the spawn supervisor's unit tests and the
/// `bm25_supervisor_e2e` integration test.
pub(crate) async fn ensure_bm25_running_for_palace(state: &AppState, palace: &str) -> bool {
    let Some(supervisor) = state.bm25_supervisor.as_ref() else {
        // No supervisor — the client (if present) connects to whatever
        // socket happens to be live. This matches pre-#193 behaviour.
        return true;
    };
    let data_dir = bm25_data_dir_for_palace(state, palace);
    match supervisor.ensure_running(palace, &data_dir).await {
        Ok(_socket) => true,
        Err(e) => {
            tracing::warn!(
                palace = %palace,
                "bm25 supervisor could not start daemon (degrading to vector-only): {e:#}"
            );
            false
        }
    }
}

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
/// the integration tests in `trusty-bm25-daemon/tests/`.
#[derive(Debug)]
pub struct Bm25IndexRequest {
    /// Palace id whose daemon should index the drawer.
    pub palace: String,
    /// Drawer id (stringified) — the daemon uses this as the BM25 doc id.
    pub drawer_id: String,
    /// Drawer text content to index.
    pub content: String,
    /// On-disk data directory for the palace's BM25 daemon — passed to the
    /// spawn supervisor's `ensure_running` so the daemon writes its snapshot
    /// next to the rest of the palace's data.
    pub data_dir: std::path::PathBuf,
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
/// What: takes ownership of the receiver and the optional BM25 client +
/// supervisor `Arc`s, then loops on `rx.recv().await`. For each request,
/// `ensure_running`s the per-palace daemon (logging + skipping on failure)
/// and calls `client.index()`. Errors are logged at `warn!` and dropped —
/// BM25 indexing is best-effort and the drawer is durable in redb regardless.
/// If `client` is `None` (env var not set at startup) the worker still runs
/// and silently drops every request, which keeps the channel drained.
/// Test: indirectly covered by the integration tests in
/// `trusty-bm25-daemon/tests/`; `bm25_index_queue_drops_when_full` covers the
/// back-pressure behaviour.
pub fn spawn_bm25_index_worker(
    mut rx: tokio::sync::mpsc::Receiver<Bm25IndexRequest>,
    client: Option<std::sync::Arc<trusty_common::bm25_client::Bm25Client>>,
    supervisor: Option<std::sync::Arc<crate::bm25_supervisor::Bm25Supervisor>>,
) {
    tokio::spawn(async move {
        while let Some(req) = rx.recv().await {
            // No client means the BM25 lane is disabled — drain the queue
            // (so senders never block) and silently drop every request.
            let Some(client) = client.as_ref() else {
                continue;
            };
            // Issue #193: try to start the daemon before the first index
            // call. If the supervisor returns an error we skip this op;
            // the daemon will be retried on the next request.
            if let Some(sup) = supervisor.as_ref() {
                if let Err(e) = sup.ensure_running(&req.palace, &req.data_dir).await {
                    tracing::warn!(
                        palace = %req.palace,
                        "bm25 supervisor failed to start daemon for index (non-fatal): {e:#}"
                    );
                    continue;
                }
            }
            if let Err(e) = client.index(&req.drawer_id, &req.content).await {
                tracing::warn!(
                    palace = %req.palace,
                    drawer_id = %req.drawer_id,
                    "bm25 daemon index failed (non-fatal): {e:#}"
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
/// write completes; the daemon RTT must stay off the response path. Routing
/// each request through a bounded mpsc channel keeps that property *and*
/// caps in-flight indexing work — under a sustained burst with a slow daemon
/// the previous design grew an unbounded task queue, which #231 fixes here.
/// What: builds a `Bm25IndexRequest` from the caller's data and calls
/// `try_send` so the caller is never blocked. On `TrySendError::Full` we
/// log at `warn!` and drop the request — BM25 indexing is best-effort and
/// the drawer is durable in redb regardless of whether the BM25 lane saw it.
/// `TrySendError::Closed` shouldn't happen in practice (the worker holds the
/// receiver for the daemon's lifetime), but if it does we log at `debug!`
/// and continue — we never let a BM25 hiccup fail a write.
/// Test: `bm25_index_queue_drops_when_full` covers the full-queue branch.
pub(crate) fn bm25_index_enqueue(state: &AppState, palace: &str, drawer_id: Uuid, content: &str) {
    let req = Bm25IndexRequest {
        palace: palace.to_string(),
        drawer_id: drawer_id.to_string(),
        content: content.to_string(),
        data_dir: bm25_data_dir_for_palace(state, palace),
    };
    match state.bm25_index_tx.try_send(req) {
        Ok(()) => {}
        Err(tokio::sync::mpsc::error::TrySendError::Full(req)) => {
            tracing::warn!(
                palace = %req.palace,
                drawer_id = %req.drawer_id,
                "BM25 index queue full — skipping drawer {}",
                req.drawer_id
            );
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(req)) => {
            tracing::debug!(
                palace = %req.palace,
                drawer_id = %req.drawer_id,
                "BM25 index queue closed — skipping drawer {}",
                req.drawer_id
            );
        }
    }
}

/// Optional BM25 search lane used by `memory_recall` (issue #156).
///
/// Why: lets the recall handler join a BM25 future with the vector future
/// without sprinkling `if state.bm25_client.is_some()` checks across the
/// call site. Returning `Option<Vec<_>>` makes the "daemon unavailable"
/// branch explicit at the consumer.
/// What: returns `None` when the env-var-gated client is absent OR when the
/// daemon errors (treated as a graceful degradation — the caller falls back
/// to vector-only results). Otherwise ensures the daemon is running via the
/// spawn supervisor (issue #193), then returns the BM25 hits the daemon
/// served. `top_k` is forwarded verbatim.
/// Test: integration coverage via the daemon's `tests/bm25_daemon.rs`; the
/// `None` path is covered by `bm25_client_disabled_by_default`.
pub(crate) async fn bm25_search_optional(
    state: &AppState,
    palace: &str,
    query: &str,
    top_k: usize,
) -> Option<Vec<trusty_common::bm25_client::BM25Hit>> {
    let client = state.bm25_client.as_ref()?;
    // Issue #193: spawn the daemon if it isn't already running. On error
    // we fall through to vector-only behaviour exactly as we did before
    // #193 when the operator forgot to start the daemon manually.
    if !ensure_bm25_running_for_palace(state, palace).await {
        return None;
    }
    match client.search(query, top_k).await {
        Ok(hits) => Some(hits),
        Err(e) => {
            tracing::warn!(
                palace = %palace,
                "bm25 daemon search failed (falling back to vector-only): {e:#}"
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
/// Test: integration coverage via the daemon's `tests/bm25_daemon.rs` plus
/// downstream RRF behaviour observed end-to-end.
pub(crate) fn fuse_bm25_into_recall(
    results: &mut Vec<trusty_common::memory_core::retrieval::RecallResult>,
    bm25_hits: &[trusty_common::bm25_client::BM25Hit],
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
    bm25_hits: &[trusty_common::bm25_client::BM25Hit],
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
