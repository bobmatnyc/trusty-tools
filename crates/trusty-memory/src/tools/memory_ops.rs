//! Memory-tool handlers for the trusty-memory MCP surface.
//!
//! Why: the `memory_*` tool handlers (remember/note/recall/recall_deep/list/
//! forget/recall_all/send_message) form one cohesive group split out of the
//! former monolithic `tools.rs` (issue #607).
//! What: per-tool `pub(crate) async fn handle_*` functions moved verbatim;
//! visibility widened to `pub(crate)` so the dispatcher (in `tools::mod`) and
//! the test module can reach them.
//! Test: `dispatch_remember_then_recall`, `dispatch_note_*`,
//! `dispatch_memory_list_*`, `dispatch_memory_recall_all_*` in `tools::tests`.

use crate::attribution::{CreatorInfo, CreatorSource, MCP_CLIENT_NAME};
use crate::{ActivitySource, AppState, DaemonEvent, DaemonReadiness};
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use trusty_common::memory_core::palace::RoomType;
use trusty_common::memory_core::retrieval::{
    recall, recall_across_palaces, recall_deep, RememberOptions,
};
use trusty_common::memory_core::timeouts;
use uuid::Uuid;

use super::bm25::{
    bm25_hits_to_recall_results, bm25_search_optional, fuse_bm25_into_recall, serialize_recall,
};
use super::helpers::{
    attach_mcp_attribution, blocklist_gate, content_gate, dedup_gate, mcp_remember_opts,
    open_palace_handle, parse_room, parse_tags, resolve_palace, room_label, skipped_envelope,
    write_drawer, WriteDrawerParams,
};

pub(crate) async fn handle_memory_remember(state: &AppState, args: Value) -> Result<Value> {
    // Issue #1970: writes no longer hard-block on embedder readiness. The
    // text/KG/BM25 path below never touches the embedder; when the daemon
    // is still `Warming`, `defer_embedding` tells `write_drawer` to
    // background the embed + vector-store upsert instead of blocking this
    // call behind a 30-120s ONNX/CoreML cold compile (mirrors trusty-search's
    // lexical-first, vector-later staged pipeline).
    let defer_embedding = state.readiness() == DaemonReadiness::Warming;
    let palace = resolve_palace(state, &args, "memory_remember")?;
    let palace = palace.as_str();
    let raw_text = args
        .get("text")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("memory_remember: missing 'text'"))?
        .to_string();

    // Issue #2442: `force` must be parsed BEFORE any content-quality gate
    // runs so it can bypass every one of them uniformly — it is an explicit
    // operator override ("app-managed writers need deterministic storage"),
    // not just a dedup-window bypass. Previously `force` was parsed after
    // `blocklist_gate`/`content_gate` had already run unconditionally, so
    // `force = true` writes of blocklisted or short standalone content were
    // still silently dropped, contradicting the tool schema's documented
    // "bypass all content-quality gates" contract.
    let force = args.get("force").and_then(|v| v.as_bool()).unwrap_or(false);

    // Issue #220: blocklist gate — silently drop content matching
    // known low-value auto-capture patterns (e.g. `Tool use: Bash`,
    // `Claude Code session ended: …`). Logged at debug so operators
    // can audit when investigating missing writes. Issue #2442: `force`
    // bypasses this gate, matching the other content-quality gates below.
    if !force {
        if let Some(pattern) = blocklist_gate(&raw_text) {
            // Issue #1481: name the matched pattern so callers can see exactly
            // what tripped the gate instead of an opaque "blocked pattern".
            let reason = format!("content gate: skipped (blocked pattern: {pattern:?})");
            tracing::debug!(palace = %palace, pattern = %pattern, "{reason}");
            return Ok(skipped_envelope(palace, &reason));
        }
    }
    // Issue #215: content gate — drop very short standalone content
    // unless the caller supplied a `context` wrapper or `force = true`
    // (issue #2442). When skipped, return a success envelope with an
    // explanatory status so the caller can see the write was a no-op
    // without having to parse a custom error shape.
    let ctx = args.get("context").and_then(|v| v.as_str());
    let text = match content_gate(&raw_text, ctx, force) {
        Some(t) => t,
        None => {
            return Ok(skipped_envelope(
                palace,
                "content gate: skipped (short prompt, no context)",
            ));
        }
    };
    let room = parse_room(args.get("room").and_then(|v| v.as_str()));
    let mut tags = parse_tags(&args);
    // Submission-logging Part B: attach `creator:*` attribution so
    // every MCP-origin drawer carries the writer identity (client
    // = `trusty-memory-mcp`, source = `mcp`, version + cwd of the
    // MCP server process). Issue #202: also project a bare-UUID
    // session tag (when present in the caller's tags) into the
    // reserved `creator:session=<first-8>` slot so the activity
    // panel can surface it without inspecting every tag.
    attach_mcp_attribution(&mut tags);

    // Issue #230: serialise the dedup-check + write sequence per-palace
    // so two concurrent identical writes can't both pass the gate. The
    // lock is scoped to the palace id — writes to different palaces
    // still run in parallel. The guard is held across the gate check
    // and the `write_drawer` call so the redb write inside
    // `remember_with_options` happens with the gate snapshot still
    // visible to subsequent waiters.
    // Issue #906: bound the acquisition so a stuck embedder on a prior writer
    // does not cascade an indefinite queue of callers waiting for this lock.
    let write_lock = state.palace_write_lock(palace);
    let _write_guard =
        timeouts::lock_with_timeout(&write_lock, timeouts::write_lock_timeout(), palace)
            .await
            .map_err(|e| anyhow::anyhow!("memory_remember: {e:#}"))?;

    // Issue #220: rolling dedup window — skip when a near-duplicate
    // landed in the same palace within the last 5 minutes. The
    // `force=true` operator override bypasses the gate so
    // intentional re-writes are not silently dropped.
    if !force {
        let handle = open_palace_handle(state, palace)?;
        if dedup_gate(&handle, &text) {
            tracing::debug!(
                palace = %palace,
                "content gate: skipped (duplicate within window)",
            );
            return Ok(skipped_envelope(
                palace,
                "content gate: skipped (duplicate within window)",
            ));
        }
    }
    let room_label_for_kg = room_label(&room);
    let drawer_id = write_drawer(
        state,
        WriteDrawerParams {
            palace_id: palace,
            content: text,
            tags,
            room,
            importance: 0.5,
            opts: mcp_remember_opts(force, defer_embedding),
            room_label_for_kg,
        },
    )
    .await?;
    Ok(json!({
        "drawer_id": drawer_id.to_string(),
        "palace": palace,
        "status": "stored",
    }))
}

pub(crate) async fn handle_memory_note(state: &AppState, args: Value) -> Result<Value> {
    // Issue #1970: same non-blocking write posture as memory_remember — see
    // that handler's comment for the rationale.
    let defer_embedding = state.readiness() == DaemonReadiness::Warming;
    // Issue #61: curated short-fact shortcut. Bypasses the token
    // threshold (so "User prefers snake_case" is accepted) but still
    // applies noise-pattern rejects so the tool can't be used to
    // smuggle in auto-capture garbage. Pinned `DrawerType::UserFact`
    // and `importance = 1.0` so the entry surfaces in L1 essentials.
    let palace = resolve_palace(state, &args, "memory_note")?;
    let palace = palace.as_str();
    let raw_content = args
        .get("content")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("memory_note: missing 'content'"))?
        .to_string();
    // Issue #220: blocklist gate — silently drop content matching
    // known low-value auto-capture patterns. Same filter as
    // `memory_remember` so the gate is uniform across the write
    // surface.
    if let Some(pattern) = blocklist_gate(&raw_content) {
        // Issue #1481: name the matched pattern (see memory_remember).
        let reason = format!("content gate: skipped (blocked pattern: {pattern:?})");
        tracing::debug!(palace = %palace, pattern = %pattern, "{reason}");
        return Ok(skipped_envelope(palace, &reason));
    }
    // Issue #215: same content gate as `memory_remember`. A `context`
    // arg can be passed to wrap a one-word answer; otherwise short
    // standalone content is silently dropped with an explanatory
    // status envelope.
    let ctx = args.get("context").and_then(|v| v.as_str());
    // memory_note has no `force` arg (see the dedup-gate comment above), so
    // the content gate always runs with `force = false`.
    let content = match content_gate(&raw_content, ctx, false) {
        Some(c) => c,
        None => {
            return Ok(skipped_envelope(
                palace,
                "content gate: skipped (short prompt, no context)",
            ));
        }
    };
    let mut tags = parse_tags(&args);
    // Submission-logging Part B: same attribution as memory_remember.
    // Issue #202: project a bare-UUID session tag (when present)
    // into the reserved `creator:session=<first-8>` slot.
    attach_mcp_attribution(&mut tags);
    // Issue #230: serialise the dedup-check + write sequence per-palace
    // so two concurrent identical writes can't both pass the gate. The
    // lock is scoped to the palace id — writes to different palaces
    // still run in parallel. Held across the gate check and the
    // `write_drawer` call so the redb write inside
    // `remember_with_options` is visible to subsequent waiters before
    // they snapshot.
    // Issue #906: bound the acquisition to prevent cascading hangs.
    let write_lock = state.palace_write_lock(palace);
    let _write_guard =
        timeouts::lock_with_timeout(&write_lock, timeouts::write_lock_timeout(), palace)
            .await
            .map_err(|e| anyhow::anyhow!("memory_note: {e:#}"))?;
    // Issue #220: rolling dedup window — same gate as
    // `memory_remember`. `memory_note` has no `force` arg, so the
    // gate is unconditional: curated short-fact writes that happen
    // to duplicate an existing recent note are still skipped.
    {
        let handle = open_palace_handle(state, palace)?;
        if dedup_gate(&handle, &content) {
            tracing::debug!(
                palace = %palace,
                "content gate: skipped (duplicate within window)",
            );
            return Ok(skipped_envelope(
                palace,
                "content gate: skipped (duplicate within window)",
            ));
        }
    }
    // note() preset skips the token threshold; we keep the default
    // filter for noise patterns. No MCP-stricter min_tokens override
    // is needed because `enforce_min_tokens = false`.
    let drawer_id = write_drawer(
        state,
        WriteDrawerParams {
            palace_id: palace,
            content,
            tags,
            room: RoomType::General,
            importance: 1.0,
            opts: RememberOptions {
                defer_embedding,
                ..RememberOptions::note()
            },
            // memory_note is pinned to the General room; mirror that for
            // the KG extractor so the auto-extracted triples carry the
            // same room label as the drawer.
            room_label_for_kg: Some("General".to_string()),
        },
    )
    .await
    .context("PalaceHandle::remember_with_options (note)")?;
    Ok(json!({
        "drawer_id": drawer_id.to_string(),
        "palace": palace,
        "status": "stored",
        "drawer_type": "UserFact",
    }))
}

/// Degraded-embedder recall fallback (issue #1970): L0/L1 + BM25 only.
///
/// Why: mirrors trusty-search's staged-pipeline degradation — rather than
/// blocking/erroring while the shared embedder cold-inits, every recall
/// variant should return identity + essential drawers plus whatever the
/// BM25 lexical lane can resolve, with the semantic (vector) layer simply
/// omitted until the embedder warms up.
/// What: seeds the result set with `retrieve_l0_l1`, joins the optional BM25
/// lane (query runs regardless of embedder state — BM25 has no embedder
/// dependency), hydrates any BM25-only hits via `bm25_hits_to_recall_results`,
/// merges without duplicating drawers already present, re-sorts by score
/// descending, and truncates to `top_k`.
/// Test: `recall_falls_back_to_bm25_and_l0_l1_while_warming`,
/// `recall_deep_falls_back_to_bm25_and_l0_l1_while_warming`.
async fn recall_without_embedder(
    state: &AppState,
    handle: &trusty_common::memory_core::retrieval::PalaceHandle,
    palace: &str,
    query: &str,
    top_k: usize,
) -> Vec<trusty_common::memory_core::retrieval::RecallResult> {
    let mut results = trusty_common::memory_core::retrieval::retrieve_l0_l1(handle);
    if let Some(bm25_hits) = bm25_search_optional(state, palace, query, top_k).await {
        for hydrated in bm25_hits_to_recall_results(handle, &bm25_hits) {
            if !results.iter().any(|r| r.drawer.id == hydrated.drawer.id) {
                results.push(hydrated);
            }
        }
    }
    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results.truncate(top_k);
    results
}

pub(crate) async fn handle_memory_recall(state: &AppState, args: Value) -> Result<Value> {
    let palace = resolve_palace(state, &args, "memory_recall")?;
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("memory_recall: missing 'query'"))?;
    let top_k = args.get("top_k").and_then(|v| v.as_u64()).unwrap_or(10) as usize;

    let handle = open_palace_handle(state, &palace)?;

    // Issue #1970: while the embedder is still warming up, return BM25 +
    // L0/L1 results immediately instead of blocking/erroring on embedder
    // state.
    if state.readiness() == DaemonReadiness::Warming {
        let results = recall_without_embedder(state, &handle, &palace, query, top_k).await;
        return Ok(serialize_recall(&palace, query, results));
    }

    let embedder = state.embedder().await?;
    // Issue #156: when the BM25 lane is enabled, run it in parallel
    // with the vector recall and RRF-fuse the two ranked lists.
    // When the daemon is unavailable or the env var is unset, the
    // helper returns `None` and we return the vector-only results
    // verbatim — zero behavioural change for existing deployments.
    let vector_fut = recall(&handle, embedder.as_ref(), query, top_k);
    let bm25_fut = bm25_search_optional(state, &palace, query, top_k);
    let (vector_res, bm25_res) = tokio::join!(vector_fut, bm25_fut);
    let mut results = vector_res.context("recall")?;
    if let Some(bm25_hits) = bm25_res {
        fuse_bm25_into_recall(&mut results, &bm25_hits, top_k);
    }
    Ok(serialize_recall(&palace, query, results))
}

pub(crate) async fn handle_memory_recall_deep(state: &AppState, args: Value) -> Result<Value> {
    let palace = resolve_palace(state, &args, "memory_recall_deep")?;
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("memory_recall_deep: missing 'query'"))?;
    let top_k = args.get("top_k").and_then(|v| v.as_u64()).unwrap_or(10) as usize;

    let handle = open_palace_handle(state, &palace)?;

    // Issue #1970: same warming-fallback posture as memory_recall.
    if state.readiness() == DaemonReadiness::Warming {
        let results = recall_without_embedder(state, &handle, &palace, query, top_k).await;
        return Ok(serialize_recall(&palace, query, results));
    }

    let embedder = state.embedder().await?;
    let results = recall_deep(&handle, embedder.as_ref(), query, top_k)
        .await
        .context("recall_deep")?;
    Ok(serialize_recall(&palace, query, results))
}

pub(crate) async fn handle_memory_list(state: &AppState, args: Value) -> Result<Value> {
    let palace = resolve_palace(state, &args, "memory_list")?;
    let handle = open_palace_handle(state, &palace)?;
    let room = args
        .get("room")
        .and_then(|v| v.as_str())
        .map(|s| parse_room(Some(s)));
    let tag = args
        .get("tag")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
    let drawers = handle.list_drawers(room, tag, limit);
    let payload: Vec<Value> = drawers
        .iter()
        .map(|d| {
            json!({
                "drawer_id": d.id.to_string(),
                "content": d.content,
                "importance": d.importance,
                "tags": d.tags,
                "created_at": d.created_at.to_rfc3339(),
                "drawer_type": d.drawer_type.as_str(),
                "expires_at": d.expires_at.map(|t| t.to_rfc3339()),
            })
        })
        .collect();
    Ok(json!({ "palace": palace, "drawers": payload }))
}

pub(crate) async fn handle_memory_forget(state: &AppState, args: Value) -> Result<Value> {
    let palace = resolve_palace(state, &args, "memory_forget")?;
    let drawer_id_str = args
        .get("drawer_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("memory_forget: missing 'drawer_id'"))?;
    let drawer_id = Uuid::parse_str(drawer_id_str)
        .map_err(|e| anyhow!("memory_forget: invalid drawer_id UUID: {e}"))?;
    let handle = open_palace_handle(state, &palace)?;
    handle.forget(drawer_id).await.context("forget")?;
    // Issue #96: emit so MCP-driven deletes are visible in the feed.
    let drawer_count = handle.drawers.read().len();
    state.emit(DaemonEvent::DrawerDeleted {
        palace_id: palace.clone(),
        drawer_count,
        source: ActivitySource::Mcp,
    });
    // Issue #228: skip the per-write `StatusChanged` emit — the ticker
    // handles aggregate roll-ups.
    Ok(json!({ "status": "deleted", "drawer_id": drawer_id_str, "palace": palace }))
}

/// Cross-palace counterpart of `recall_without_embedder` (issue #1970).
///
/// Why: `memory_recall_all` must degrade the same way the single-palace
/// recall handlers do — BM25 + L0/L1 per palace, no vector lane — while the
/// embedder is warming up.
/// What: runs `recall_without_embedder` against every handle, tags each hit
/// with its source palace id, then merges/re-sorts/truncates exactly like
/// `recall_across_palaces` does for the vector-backed path.
/// Test: `recall_all_falls_back_to_bm25_and_l0_l1_while_warming`.
async fn recall_all_without_embedder(
    state: &AppState,
    handles: &[std::sync::Arc<trusty_common::memory_core::retrieval::PalaceHandle>],
    query: &str,
    top_k: usize,
) -> Vec<trusty_common::memory_core::retrieval::CrossPalaceResult> {
    let mut merged = Vec::new();
    for handle in handles {
        let palace_id = handle.id.as_str().to_string();
        let hits = recall_without_embedder(state, handle, &palace_id, query, top_k).await;
        merged.extend(hits.into_iter().map(|result| {
            trusty_common::memory_core::retrieval::CrossPalaceResult {
                palace_id: palace_id.clone(),
                result,
            }
        }));
    }
    merged.sort_by(|a, b| {
        b.result
            .score
            .partial_cmp(&a.result.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    merged.truncate(top_k);
    merged
}

pub(crate) async fn handle_memory_recall_all(state: &AppState, args: Value) -> Result<Value> {
    let query = args
        .get("q")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("memory_recall_all: missing 'q'"))?;
    let top_k = args.get("top_k").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
    let deep = args.get("deep").and_then(|v| v.as_bool()).unwrap_or(false);

    // List every palace on disk and open a handle for each. Palaces
    // that fail to open are skipped with a warning so a single bad
    // namespace cannot fail the whole fan-out.
    let root = state.data_root.clone();
    let palaces = tokio::task::spawn_blocking(move || {
        trusty_common::memory_core::PalaceRegistry::list_palaces(&root)
    })
    .await
    .context("join list_palaces")??;

    let mut handles = Vec::with_capacity(palaces.len());
    for p in &palaces {
        match state.registry.open_palace(&state.data_root, &p.id) {
            Ok(h) => handles.push(h),
            Err(e) => {
                tracing::warn!(palace = %p.id, "memory_recall_all: open failed: {e:#}")
            }
        }
    }

    // Issue #1970: BM25 + L0/L1 fallback across every palace while warming.
    let results = if state.readiness() == DaemonReadiness::Warming {
        recall_all_without_embedder(state, &handles, query, top_k).await
    } else {
        let embedder = state.embedder().await?;
        let erased: std::sync::Arc<dyn trusty_common::memory_core::embed::Embedder + Send + Sync> =
            embedder;
        recall_across_palaces(&handles, &erased, query, top_k, deep)
            .await
            .context("recall_across_palaces")?
    };

    let payload: Vec<Value> = results
        .iter()
        .map(|r| {
            json!({
                "palace_id":  r.palace_id,
                "drawer_id":  r.result.drawer.id.to_string(),
                "content":    r.result.drawer.content,
                "importance": r.result.drawer.importance,
                "tags":       r.result.drawer.tags,
                "score":      r.result.score,
                "layer":      r.result.layer,
                "drawer_type": r.result.drawer.drawer_type.as_str(),
            })
        })
        .collect();
    Ok(json!({ "query": query, "results": payload }))
}

pub(crate) async fn handle_memory_send_message(state: &AppState, args: Value) -> Result<Value> {
    // Issue #99: inter-project messaging via palace memories.
    let to_palace = args
        .get("to_palace")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("memory_send_message: missing 'to_palace'"))?
        .to_string();
    let purpose = args
        .get("purpose")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("memory_send_message: missing 'purpose'"))?
        .to_string();
    let content = args
        .get("content")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("memory_send_message: missing 'content'"))?
        .to_string();
    // from_palace defaults to the explicit `from_palace` arg, then
    // the server's --palace default, then the cwd-derived slug.
    let from_palace = if let Some(s) = args.get("from_palace").and_then(|v| v.as_str()) {
        s.to_string()
    } else if let Some(d) = state.default_palace.clone() {
        d
    } else {
        crate::messaging::cwd_palace_slug()
            .context("memory_send_message: derive from_palace from cwd")?
    };
    let drawer_id = crate::messaging::send_message_to_palace(
        &state.registry,
        &state.data_root,
        &from_palace,
        &to_palace,
        &purpose,
        content,
        CreatorInfo::new_self(MCP_CLIENT_NAME, CreatorSource::Mcp),
    )
    .await
    .context("memory_send_message")?;
    Ok(json!({
        "drawer_id": drawer_id.to_string(),
        "from_palace": from_palace,
        "to_palace": to_palace,
        "purpose": purpose,
        "status": "sent",
    }))
}
