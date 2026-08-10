//! Reindex trigger and progress-stream handlers.
//!
//! Why: The `POST /indexes/:id/reindex` trigger and
//! `GET /indexes/:id/reindex/stream` SSE endpoint share reindex-progress
//! types and cooldown logic; grouping them here keeps reindex concerns isolated.
//! What: `ReindexRequest`, `reindex_handler`, `reindex_stream_handler`.
//! Test: `reindex_handler_rejects_within_cooldown` and
//! `reindex_status_aborted_memory_serializes_lowercase`.
use axum::{
    body::Body,
    extract::{Path, State},
    http::StatusCode,
    response::Response,
    Json,
};
use futures::stream::{self, StreamExt};
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use tokio_stream::wrappers::BroadcastStream;

use crate::core::registry::{IndexHandle, IndexId};
use crate::service::reindex::{
    spawn_reindex_with_cleanup, validate, ReindexProgress, ReindexStatus,
};

use super::helpers::{find_root_path_collision, validate_root_path};
use super::state::SearchAppState;

#[derive(Deserialize, Default)]
pub struct ReindexRequest {
    #[serde(default)]
    pub root_path: Option<std::path::PathBuf>,
    /// When `true`, the daemon clears the per-index content-hash cache before
    /// walking the tree, forcing every file to be re-embedded even if its
    /// content hasn't changed. Set by `trusty-search index --force`.
    #[serde(default)]
    pub force: Option<bool>,
    /// When `true`, routes this reindex through the background (low-priority)
    /// semaphore so it cannot starve user-initiated requests (issue #458).
    /// Set by the startup auto-discover path; never sent by interactive CLI or
    /// MCP callers. Defaults to `false` (interactive/priority path).
    #[serde(default)]
    pub background: Option<bool>,
}

pub(super) async fn reindex_handler(
    State(state): State<Arc<SearchAppState>>,
    Path(id): Path<String>,
    body: Option<Json<ReindexRequest>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let index_id = IndexId::new(id.clone());
    let mut handle = state.registry.get(&index_id).ok_or((
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({
            "error": format!("unknown index: {}", index_id.0),
        })),
    ))?;

    // Issue #120: cooldown guard. If the most recent reindex for this index
    // aborted at the memory limit, refuse to queue another one for
    // `TRUSTY_REINDEX_COOLDOWN_SECS` (default 300 s). Re-running immediately
    // would just hit the limit again because the un-processed files have no
    // content-hash entries yet, producing an infinite reindex loop. Operators
    // can lower batch size / raise the memory limit and try again after the
    // cooldown elapses.
    if let Some(aborted_at) = state.last_reindex_aborted_at.get(&index_id) {
        let elapsed = aborted_at.elapsed();
        let cooldown = std::time::Duration::from_secs(
            std::env::var("TRUSTY_REINDEX_COOLDOWN_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(300),
        );
        if elapsed < cooldown {
            let remaining_secs = (cooldown - elapsed).as_secs();
            tracing::warn!(
                "reindex_handler: refusing reindex for index {} — last run \
                 aborted at memory limit {}s ago, cooldown {}s remaining",
                index_id.0,
                elapsed.as_secs(),
                remaining_secs,
            );
            return Err((
                StatusCode::TOO_MANY_REQUESTS,
                Json(serde_json::json!({
                    "error": "reindex cooldown active after memory-limit abort",
                    "index_id": index_id.0,
                    "retry_after_secs": remaining_secs,
                    "cooldown_secs": cooldown.as_secs(),
                    "hint": "lower TRUSTY_MAX_BATCH_SIZE or raise TRUSTY_MEMORY_LIMIT_MB before retrying",
                })),
            ));
        }
        // Cooldown elapsed — drop the stale entry so the next abort (if any)
        // starts a fresh window. Done outside the `get()` guard to avoid
        // holding a DashMap shard lock across the removal.
        drop(aborted_at);
        state.last_reindex_aborted_at.remove(&index_id);
    }

    // If caller supplied a root_path and the stored handle doesn't have one
    // (or differs), re-register with the new path. We can't mutate the
    // existing Arc in place, but registering replaces the entry.
    let mut force = false;
    // Issue #458: `background=true` routes to the low-priority semaphore so
    // startup auto-discover reindexes never starve interactive requests.
    // Default false (interactive/priority path) when the field is absent.
    let mut is_interactive = true;
    if let Some(Json(req)) = body {
        force = req.force.unwrap_or(false);
        is_interactive = !req.background.unwrap_or(false);
        if let Some(new_root) = req.root_path {
            // Issue #63: a caller-supplied override must pass the same
            // absolute-existing-directory check as `POST /indexes`. Without
            // this, `POST /indexes/:id/reindex { root_path: "." }` would
            // silently re-point an existing index at the daemon's CWD.
            //
            // Issue (indexed-paths-mismatch): use the canonical form so a
            // re-register via a symlink alias normalises to the same identity
            // the original `POST /indexes` stored.
            // Issue #829: validate_root_path is now async. Reindex's root
            // override has no `allow_sensitive_path` opt-in, so this always
            // enforces the full denylist (`false`) — unchanged from before.
            let new_root = match validate_root_path(&new_root, false).await {
                Ok(canonical) => canonical,
                Err(resp) => {
                    let (parts, body) = resp.into_parts();
                    let status = parts.status;
                    let body_bytes = axum::body::to_bytes(body, 4096).await.unwrap_or_default();
                    let json: serde_json::Value = serde_json::from_slice(&body_bytes)
                        .unwrap_or_else(|_| serde_json::json!({}));
                    return Err((status, Json(json)));
                }
            };
            // Issue #3993 (Gap E): a root_path override previously had NO
            // collision check at all — `state.registry.register(new_handle)`
            // below would silently re-point this index onto a root_path a
            // DIFFERENT live index already owns, the same #2305/#2336 hazard
            // (two ids claiming one on-disk `<root>/.trusty-search/index.redb`
            // corpus) `create_index_handler` and `relocate_index_handler`
            // already guard against. Reuse the exact same primitive rather
            // than adding a fourth collision mechanism.
            //
            // Issue #3993 second round (adversarial BLOCK): also check
            // `state.cold_store` — a reindex override onto a cold entry's
            // parked root is the same root-theft as the other two write
            // paths, so it must go through the same widened primitive.
            let live_handles = state.registry.list_handles();
            let cold_entries = state.cold_store.snapshot();
            if let Some(existing_id) =
                find_root_path_collision(&live_handles, &cold_entries, &new_root, Some(&index_id))
            {
                tracing::warn!(
                    "reindex_handler: refusing root_path override for '{}' to {} — \
                     already owned by '{}' (issue #3993)",
                    index_id.0,
                    new_root.display(),
                    existing_id,
                );
                return Err((
                    StatusCode::CONFLICT,
                    Json(serde_json::json!({
                        "error": format!(
                            "root_path {:?} is already registered to index '{}'; two \
                             indexes cannot share one on-disk corpus (issues #2305, \
                             #2336, #3993)",
                            new_root.display(),
                            existing_id,
                        ),
                        "existing_id": existing_id.0,
                    })),
                ));
            }
            // #4951 review HIGH-1: refuse an override the reindex runner will
            // refuse anyway. Syncing the indexer root below is only sound if
            // the walk that re-relativizes the corpus against `new_root`
            // actually runs. When the corpus's `indexed_root` disagrees with
            // `new_root`, `runner.rs` re-checks `root_move_is_trusted` against
            // the PERSISTED `indexes.toml` root and aborts before walking
            // (#2178) — leaving chunks relative to the old root while the
            // indexer resolves them against the new one. `file_is_within_root`
            // is a lexical prefix test with no existence check, so every such
            // path would pass it and `stale_index_root` would read `false`:
            // a dangling (or wrong-file) result reported as healthy. Mirroring
            // the runner's own gate here keeps the two decisions identical, so
            // an accepted override always gets its walk.
            let prior_indexed_root = handle.read_indexed_root().await.unwrap_or(None);
            if validate::needs_path_relativization(prior_indexed_root.as_deref(), &new_root) {
                let persisted_root = match &state.registry_path_override {
                    Some(path) => crate::service::persistence::load_index_registry_at(path),
                    None => crate::service::persistence::load_index_registry(),
                }
                .ok()
                .and_then(|entries| entries.into_iter().find(|e| e.id == index_id.0))
                .map(|e| e.root_path);
                if !validate::root_move_is_trusted(persisted_root.as_deref(), &new_root) {
                    tracing::warn!(
                        "reindex_handler: refusing root_path override for '{}' to {} — the \
                         corpus was last relativized against {:?} and the persisted \
                         indexes.toml root_path is {:?}; the reindex would abort at the \
                         #2178 guard, leaving chunk paths relative to a root the index no \
                         longer claims (issue #4951)",
                        index_id.0,
                        new_root.display(),
                        prior_indexed_root,
                        persisted_root,
                    );
                    return Err((
                        StatusCode::CONFLICT,
                        Json(serde_json::json!({
                            "error": format!(
                                "root_path {:?} disagrees with this index's last-indexed root \
                                 {:?} and its persisted indexes.toml root_path {:?}; a reindex \
                                 override cannot durably move an index — use POST \
                                 /indexes/{}/relocate for an explicit, persisted move \
                                 (issues #2178, #4951)",
                                new_root.display(),
                                prior_indexed_root,
                                persisted_root,
                                index_id.0,
                            ),
                            "indexed_root": prior_indexed_root,
                            "persisted_root": persisted_root,
                        })),
                    ));
                }
            }
            if handle.root_path.as_os_str().is_empty() || handle.root_path != new_root {
                let indexer = Arc::clone(&handle.indexer);
                // #4951: the override rebuilds the handle around this SAME
                // indexer Arc, so the indexer's own `root_path` — the base
                // every root-relative chunk path is joined against to produce
                // the absolute `CodeChunk::file` — must move with it. Left
                // stale, materialization joined against the OLD root while
                // `search_handler`'s `file_is_within_root` post-filter compared
                // against the NEW one, so every candidate was discarded and
                // search returned `results: []` with `stale_index_root: true`
                // on an index reporting `status: ready` and 85k chunks.
                //
                // Review MEDIUM: the guard is held across `registry.register`
                // below, not dropped right after the write. A search that grabs
                // the indexer read lock in between would otherwise materialize
                // against the NEW indexer root while `search_handler`'s
                // post-filter still holds the OLD handle — the same divergence,
                // narrowed to a race window instead of made permanent.
                let indexer_for_guard = Arc::clone(&indexer);
                let mut indexer_guard = indexer_for_guard.write().await;
                indexer_guard.set_root_path(new_root.clone());
                // Preserve the filter set / domain vocabulary recorded on the
                // existing handle — only the root_path is being overridden.
                let new_handle = IndexHandle {
                    id: index_id.clone(),
                    indexer,
                    root_path: new_root,
                    include_paths: handle.include_paths.clone(),
                    exclude_globs: handle.exclude_globs.clone(),
                    extensions: handle.extensions.clone(),
                    domain_terms: handle.domain_terms.clone(),
                    include_docs: handle.include_docs,
                    respect_gitignore: handle.respect_gitignore,
                    // Preserve the symlink policy across the root_path override.
                    follow_links: handle.follow_links,
                    // Issue #1372: preserve the hygiene knobs across the
                    // root_path override — they are orthogonal to the path.
                    extra_skip_dirs: handle.extra_skip_dirs.clone(),
                    data_file_max_bytes: handle.data_file_max_bytes,
                    path_filter: handle.path_filter.clone(),
                    // Preserve the previously inferred context (if any). A
                    // fresh reindex will overwrite this with the metadata
                    // scraped from the new root.
                    context_embedding: Arc::clone(&handle.context_embedding),
                    context_summary: Arc::clone(&handle.context_summary),
                    // Preserve the indexed-HEAD SHA across the root_path
                    // override — a subsequent reindex will refresh it.
                    indexed_head_sha: Arc::clone(&handle.indexed_head_sha),
                    // Preserve the last-indexed timestamp across root-path
                    // override — a subsequent reindex will refresh it (issue #878).
                    last_indexed_at: Arc::clone(&handle.last_indexed_at),
                    // Issue #109, Phase 1: preserve the lexical-only flag
                    // and stages snapshot across the root override — a
                    // root_path override is not an opt-out change.
                    lexical_only: handle.lexical_only,
                    // Issue #313: preserve the skip_kg flag across the
                    // root_path override — the operator's KG choice is
                    // orthogonal to the path being indexed.
                    skip_kg: handle.skip_kg,
                    // Issue #2984 Phase 1: preserve the skip_vector flag
                    // across the root_path override — orthogonal to the path.
                    skip_vector: handle.skip_vector,
                    // Issue #923: preserve the defer_embed flag across the
                    // root_path override — the operator's embedding-mode
                    // choice is orthogonal to the path being indexed.
                    defer_embed: handle.defer_embed,
                    stages: Arc::clone(&handle.stages),
                    search_pressure: Arc::clone(&handle.search_pressure),
                    // Preserve walk diagnostics across root-path override — a
                    // subsequent reindex will refresh the snapshot.
                    walk_diagnostics: Arc::clone(&handle.walk_diagnostics),
                };
                // Issue #3995 round 5 (CRITICAL): snapshot the cold-store
                // entry (if any) parked under `index_id` BEFORE registering
                // the override handle below — the "expected" half of the
                // identity guard, mirrored from `create_index_handler` /
                // `relocate_index_handler`. See `mark_loaded_if` below.
                let cold_entry_before_register = state.cold_store.entry_token(&index_id);
                handle = state.registry.register(new_handle);
                // Both roots are now the new one; readers may proceed.
                drop(indexer_guard);
                // Issue #3993 review round 3 (HIGH, applied here for
                // consistency with create/relocate): reap any stale
                // `state.cold_store` record parked under this SAME id. Same
                // no-op-under-correct-operation reasoning as
                // `relocate_index_handler`: reaching this branch already
                // required `state.registry.get(&index_id)` to succeed at the
                // top of this handler (404 otherwise), so `id` was already
                // live before the override ran and cannot simultaneously be
                // cold. Defense-in-depth against residual corruption
                // predating the create-time fix; keyed by `IndexId`, so it
                // can only ever touch this id's own record.
                //
                // Issue #3995 round 5 (CRITICAL): identity-guarded against
                // `cold_entry_before_register` — see `create_index_handler`'s
                // identical comment and `ColdIndexStore::mark_loaded_if` for
                // the race this closes.
                state
                    .cold_store
                    .mark_loaded_if(&index_id, cold_entry_before_register);
            }
        }
    }

    // Replace any prior progress entry so SSE subscribers see fresh state.
    let progress = Arc::new(ReindexProgress::new());
    state
        .reindex_progress
        .insert(index_id.clone(), Arc::clone(&progress));

    spawn_reindex_with_cleanup(
        handle,
        progress,
        force,
        Some(Arc::clone(&state.reindex_progress)),
        Some(Arc::clone(&state.last_reindex_aborted_at)),
        // Issue #282: forward the live sidecar PID slot so the reindex
        // orchestrator can sample embedderd's RSS during the run and
        // emit `embedderd_peak_rss_mb` in the SSE `complete` event.
        Some(Arc::clone(&state.embedderd_pid_slot)),
        // Issue #458: route based on caller intent. `background=true` in the
        // request body maps to `priority=false` (background semaphore, never
        // blocks interactive requests). Default is interactive (priority=true).
        is_interactive,
        // Issue #764: quarantine tracking not wired through the HTTP handler yet.
        None,
    );

    Ok(Json(serde_json::json!({
        "index_id": index_id.0,
        "queued": true,
        "stream_url": format!("/indexes/{}/reindex/stream", index_id.0),
    })))
}

/// Heartbeat interval for the reindex SSE stream.
///
/// Why: under memory pressure the embedderd sidecar can stall between batches,
/// leaving the SSE body idle for minutes. Without any bytes flowing, the OS
/// (or any intermediate proxy/reverse-proxy) tears down the idle TCP connection
/// before the terminal event is ever written — the client sees a decode error
/// or "stream ended without completion event". Emitting a comment-only SSE
/// frame (`: heartbeat\n\n`) every `SSE_HEARTBEAT_INTERVAL` keeps the body
/// transport alive so the connection survives long stalls.
/// What: used by `reindex_stream_handler` to pace the `IntervalStream`.
/// Test: covered indirectly by the full reindex path; the interval fires even
/// when no real events are produced.
const SSE_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(20);

/// SSE keep-alive heartbeat frame (SSE comment — ignored by all spec-compliant
/// clients including `eventsource-stream`).
const SSE_HEARTBEAT_FRAME: &str = ": heartbeat\n\n";

/// SSE stream of reindex progress events.
///
/// Why: Mirrors the `/status/stream` SSE pattern (manual `Response::builder()`
/// with `text/event-stream` + `no-cache` + `X-Accel-Buffering: no`).
/// Replays any events already buffered (so a late subscriber still sees the
/// `start` event) and then streams live events from the broadcast channel
/// until the reindex completes. Lagged subscribers receive a
/// `{"type":"lag","skipped":N}` frame. A 20 s keep-alive heartbeat (SSE
/// comment frame, ignored by clients) prevents the OS from tearing down the
/// idle TCP connection when the sidecar stalls between batches.
///
/// What: builds a merged stream of (a) broadcast events and (b) 20 s interval
/// heartbeats. The broadcast path produces `data:` frames; the heartbeat path
/// produces ``: heartbeat\n\n`` comment frames. The merged stream is wrapped
/// in `Body::from_stream` and returned as `text/event-stream`.
///
/// Test: `reindex_stream_handler` is exercised by the full-reindex integration
/// path. The heartbeat interval fires independently of real events so it
/// cannot be blocked by a stalled sidecar.
pub(super) async fn reindex_stream_handler(
    State(state): State<Arc<SearchAppState>>,
    Path(id): Path<String>,
) -> Result<Response, StatusCode> {
    let index_id = IndexId::new(id);
    let progress = state
        .reindex_progress
        .get(&index_id)
        .map(|r| Arc::clone(r.value()))
        .ok_or(StatusCode::NOT_FOUND)?;

    // Snapshot the replay buffer first so we don't miss the `start` event,
    // then subscribe for live updates. New events that arrive between the
    // snapshot and subscription will appear in both — duplicates are harmless
    // for SSE consumers and rare in practice.
    let replay = progress.events.lock().await.clone();
    let initial_status = progress.status.load();
    let rx = progress.sender.subscribe();

    fn frame(line: String) -> Result<axum::body::Bytes, std::io::Error> {
        Ok(axum::body::Bytes::from(format!("data: {line}\n\n")))
    }

    let replay_stream = stream::iter(replay).map(frame);

    // If the reindex already finished before the subscriber connected, the
    // replay buffer contains the terminal `complete` event and the live
    // stream would idle forever. Return the replay only in that case.
    let body = if initial_status != ReindexStatus::Running {
        Body::from_stream(replay_stream)
    } else {
        // Live event stream from the broadcast channel.
        let live = BroadcastStream::new(rx).map(|res| match res {
            Ok(line) => frame(line),
            Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(n)) => Ok(
                axum::body::Bytes::from(format!("data: {{\"type\":\"lag\",\"skipped\":{n}}}\n\n")),
            ),
        });

        // Keep-alive heartbeat: emit `: heartbeat\n\n` every 20 s so the
        // HTTP body never goes fully idle between events. SSE comment frames
        // (lines starting with ':') are mandated by the spec to be ignored
        // by all compliant clients including `eventsource-stream`.
        //
        // Why merge rather than chain: we need interleaving, not sequencing
        // — the heartbeat must fire even while the live stream is idle, and
        // the live stream must continue after a heartbeat tick.
        let heartbeat = tokio_stream::wrappers::IntervalStream::new(tokio::time::interval(
            SSE_HEARTBEAT_INTERVAL,
        ))
        .map(|_| -> Result<axum::body::Bytes, std::io::Error> {
            Ok(axum::body::Bytes::from_static(
                SSE_HEARTBEAT_FRAME.as_bytes(),
            ))
        });

        // `stream::select` from `futures` interleaves two streams; the
        // merged stream ends when BOTH inputs end. The broadcast stream ends
        // when the sender (reindex task) drops; the interval stream runs
        // forever. Relying on the broadcast-stream termination is therefore
        // sufficient — the interval side is just a no-cost keep-alive.
        Body::from_stream(replay_stream.chain(stream::select(live, heartbeat)))
    };

    Ok(Response::builder()
        .header("Content-Type", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .header("X-Accel-Buffering", "no")
        .body(body)
        .expect("valid SSE response"))
}
