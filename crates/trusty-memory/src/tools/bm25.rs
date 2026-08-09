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
use trusty_common::bm25_client::{BM25Hit, Bm25Client};
use trusty_common::memory_core::palace::Drawer;
use trusty_common::memory_core::retrieval::{PalaceHandle, RecallResult};
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

/// Resolve a BM25 client bound to **this palace's** socket, starting the
/// daemon if necessary. `None` means "do not use the lexical lane".
///
/// Why (#5036): the lane is per-palace everywhere except the client. Each
/// palace gets its own daemon, its own snapshot, and its own socket
/// (`socket_path_for_palace`), and `bm25_backfill` indexes each palace through
/// the socket the supervisor hands back. `AppState::bm25_client`, though, is
/// built exactly once — `Bm25Client::for_palace(default_palace)` — so every
/// search and every live index op went to the DEFAULT palace's socket no
/// matter which palace was being queried. The predecessor of this function
/// returned a bare `bool` and threw the supervisor's socket away, which is how
/// the two drifted apart. The consequence is worse than a miss: a search for
/// palace X reads a corpus the backfill never wrote to (silently empty), while
/// a write for palace X lands in the default palace's corpus (silently
/// cross-indexed). Handing back the socket the supervisor actually resolved is
/// what keeps read, write, and backfill pointed at one index.
/// What: `None` when the lane is off (`bm25_client` is `None` — the
/// `TRUSTY_BM25_DAEMON=1` gate) or when the supervisor could not start the
/// daemon, in which case the caller degrades to vector-only exactly as before.
/// With no supervisor (`TRUSTY_BM25_EXTERNAL=1`), falls back to the canonical
/// per-palace socket path, which is the convention an out-of-band operator
/// follows anyway.
/// Test: `recall_http_bm25_fusion.rs::http_recall_returns_a_lexical_match_the_vector_lane_misses`
/// exercises it against a non-default palace, which is the case the old code
/// got wrong.
pub(crate) async fn bm25_client_for_palace(state: &AppState, palace: &str) -> Option<Bm25Client> {
    // The lane gate. The client's own socket is deliberately unused — it is
    // pinned to the default palace and is only a proxy for "lane enabled".
    state.bm25_client.as_ref()?;
    let Some(supervisor) = state.bm25_supervisor.as_ref() else {
        return Some(Bm25Client::for_palace(palace));
    };
    let data_dir = bm25_data_dir_for_palace(state, palace);
    match supervisor.ensure_running(palace, &data_dir).await {
        Ok(socket) => Some(Bm25Client::new(socket)),
        Err(e) => {
            tracing::warn!(
                palace = %palace,
                "bm25 supervisor could not start daemon (degrading to vector-only): {e:#}"
            );
            None
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
/// and silently drops every request, which keeps the channel drained — that is
/// not a coverage gap, because the lane is off.
/// A request the worker accepts but cannot land (daemon spawn refused, index
/// call failed) DOES mark the palace in `dirty`, so the repair sweep re-runs
/// the backfill instead of the gap surviving until the next restart.
/// Test: indirectly covered by the integration tests in
/// `trusty-bm25-daemon/tests/`; `bm25_index_queue_drops_when_full` covers the
/// back-pressure behaviour.
pub fn spawn_bm25_index_worker(
    mut rx: tokio::sync::mpsc::Receiver<Bm25IndexRequest>,
    client: Option<std::sync::Arc<trusty_common::bm25_client::Bm25Client>>,
    supervisor: Option<std::sync::Arc<crate::bm25_supervisor::Bm25Supervisor>>,
    dirty: crate::bm25_repair::DirtyPalaces,
) {
    tokio::spawn(async move {
        while let Some(req) = rx.recv().await {
            // No client means the BM25 lane is disabled — drain the queue
            // (so senders never block) and silently drop every request.
            if client.is_none() {
                continue;
            };
            // Issue #193: try to start the daemon before the first index
            // call. If the supervisor returns an error we skip this op;
            // the daemon will be retried on the next request.
            // #5036: index through the socket the supervisor resolved for THIS
            // palace. The captured `client` is pinned to the default palace, so
            // using it here filed every palace's drawers into one corpus.
            let per_palace = if let Some(sup) = supervisor.as_ref() {
                match sup.ensure_running(&req.palace, &req.data_dir).await {
                    Ok(socket) => Bm25Client::new(socket),
                    Err(e) => {
                        // The write did not land. Same reasoning as the dropped
                        // enqueue: queue the palace so a repair pass re-runs the
                        // backfill rather than leaving the gap until restart.
                        dirty.insert(req.palace.clone());
                        tracing::warn!(
                            palace = %req.palace,
                            "bm25 supervisor failed to start daemon for index (non-fatal): {e:#}"
                        );
                        continue;
                    }
                }
            } else {
                Bm25Client::for_palace(&req.palace)
            };
            if let Err(e) = per_palace.index(&req.drawer_id, &req.content).await {
                dirty.insert(req.palace.clone());
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
/// `try_send` so the caller is never blocked. BOTH failure arms — `Full` and
/// `Closed` — drop the request and queue the palace for coverage repair; the
/// drawer is durable in redb either way, and BM25 catches up on the next repair
/// pass. `Closed` shouldn't happen in practice (the worker holds the receiver
/// for the daemon's lifetime), but it loses the write exactly as completely as
/// `Full` does, so it is not treated as the lesser case. We never let a BM25
/// hiccup fail a write.
/// Test: `bm25_index_queue_drops_when_full`,
/// `a_closed_index_queue_queues_the_palace_for_repair`.
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
) -> Option<Vec<BM25Hit>> {
    // Issue #193: spawn the daemon if it isn't already running. On error
    // we fall through to vector-only behaviour exactly as we did before
    // #193 when the operator forgot to start the daemon manually.
    // #5036: and search the socket belonging to `palace`, not the default one.
    let client = bm25_client_for_palace(state, palace).await?;
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

/// Fuse the BM25 lexical lane into a vector recall list: boost what both
/// lanes found, and promote what only BM25 found.
///
/// Why: BM25 wins on identifier-heavy queries ("cargo test", "PalaceHandle"),
/// the vector lane wins on conceptual queries. The predecessor of this
/// function (#156) only ever *boosted* drawers the vector lane had already
/// returned, so a drawer the vector lane missed entirely stayed missed — which
/// is precisely the case the lexical lane exists to cover, and why #5036 could
/// not be closed by wiring the old helper into one more caller.
///
/// Why the two lanes are scored differently, rather than by textbook RRF: the
/// `score` this returns is consumed as a cosine-similarity-scaled relevance
/// value, and `prompt_context` drops everything under
/// `DEFAULT_RELEVANCE_FLOOR` (0.35, #5037). A rank-only RRF score tops out at
/// `2/61 ≈ 0.033`, so re-scoring both lanes the textbook way would put every
/// result under that floor and empty the injection entirely. Vector scores are
/// therefore left on their own scale and a promoted lexical hit is normalised
/// onto the same 0..1 band.
///
/// What: (1) adds the RRF bonus `1 / (k + rank + 1)` (`k = 60`, the
/// IR-literature default) to any drawer both lanes returned; (2) hydrates
/// BM25-only hits from `handle.drawers` — already in memory, so no disk walk —
/// admits them through `admits`, and scores each at its share of the lexical
/// lane's best hit, marking them `layer = 4` (L0/L1/L2/L3 are reserved);
/// (3) re-sorts by score desc, tie-breaking on the lower layer, and truncates
/// to `top_k`. An empty `bm25_hits` returns without touching `results`, so a
/// deployment with the lane off is bit-for-bit unchanged.
///
/// Known limitation: normalising against the lane's own best hit means the
/// best lexical match scores 1.0 even when it is only the best of a weak
/// field. The relevance floor cannot catch that, because normalisation is what
/// puts it above the floor in the first place.
///
/// Test: `recall_http_bm25_fusion.rs::http_recall_returns_a_lexical_match_the_vector_lane_misses`
/// (promotion, end-to-end through the HTTP path against a real daemon and a
/// real embedder); `fuse_bm25_lane_is_a_no_op_without_hits`,
/// `fuse_bm25_lane_promotes_a_bm25_only_drawer`.
pub(crate) fn fuse_bm25_lane(
    results: &mut Vec<RecallResult>,
    handle: &PalaceHandle,
    bm25_hits: &[BM25Hit],
    top_k: usize,
    admits: impl Fn(&Drawer) -> bool,
) {
    /// RRF damping constant (Cormack et al. 2009). 60 is the literature
    /// default and what trusty-search uses in its hybrid pipeline.
    const RRF_K: f32 = 60.0;
    if bm25_hits.is_empty() {
        return;
    }
    // Normalisation denominator. Taken as an explicit max rather than assuming
    // the daemon returned its hits in descending order.
    let best = bm25_hits
        .iter()
        .map(|h| h.score)
        .fold(f32::MIN_POSITIVE, f32::max);

    let drawers = handle.drawers.read();
    for (rank, hit) in bm25_hits.iter().enumerate() {
        if let Some(existing) = results
            .iter_mut()
            .find(|r| r.drawer.id.to_string() == hit.doc_id)
        {
            existing.score += 1.0 / (RRF_K + rank as f32 + 1.0);
            continue;
        }
        // BM25-only. `doc_id` is a stringified drawer UUID; a hit that no
        // longer resolves (forgotten since the snapshot was built) is skipped.
        let Ok(drawer_id) = Uuid::parse_str(&hit.doc_id) else {
            continue;
        };
        let Some(drawer) = drawers.iter().find(|d| d.id == drawer_id) else {
            continue;
        };
        if !admits(drawer) {
            continue;
        }
        results.push(RecallResult {
            drawer: drawer.clone(),
            score: (hit.score / best).clamp(0.0, 1.0),
            layer: 4,
        });
    }
    drop(drawers);

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
/// Why: during embedder warm-up there are no vector results to fuse into, so
/// [`fuse_bm25_lane`] has nothing to attach the lexical lane to. This is the
/// standalone hydration the warming path needs. `PalaceHandle::drawers`
/// already holds every drawer's metadata in memory (no disk I/O needed), so
/// each BM25 `doc_id` (a stringified drawer UUID) can be resolved into a full
/// `RecallResult` directly. Unlike [`fuse_bm25_lane`] it emits the raw BM25
/// score, which is safe here only because nothing else is in the list to
/// compare against.
/// What: parses each hit's `doc_id` as a `Uuid`, looks it up in
/// `handle.drawers`, and emits a `RecallResult` carrying the BM25 score and
/// `layer: 4` (the same lexical-lane marker [`fuse_bm25_lane`] uses).
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
