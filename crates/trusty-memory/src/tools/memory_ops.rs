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
    list_drawers_in_wing, recall_across_palaces, recall_deep_scoped, recall_scoped, scope_admits,
    PalaceHandle, RecallScope, RememberOptions,
};
use trusty_common::memory_core::timeouts;
use uuid::Uuid;

use super::wing_ops::resolve_wing_arg;

use super::bm25::{
    bm25_delete_document, bm25_hits_to_recall_results, bm25_search_optional, fuse_bm25_into_recall,
    serialize_recall,
};
use super::helpers::{
    apply_tier_c, attach_mcp_attribution, blocklist_gate, content_gate, dedup_gate,
    mcp_remember_opts, open_palace_handle, parse_tags, resolve_palace, resolve_tier_c, room_label,
    skipped_envelope, write_drawer, WriteDrawerParams,
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
    // Issue #2520 (two-tier force): `force` now bypasses QUALITY gates only
    // (this gate, the blocklist gate below, dedup, and the short-content
    // check). The SECRET gate inside the deep `FilterConfig`/`check_secret`
    // pass always runs — even under `force` — unless the caller ALSO sets
    // the separate `allow_secret_like` opt-in, a deliberate override for
    // callers that genuinely need to persist secret-shaped content.
    let force = args.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
    let allow_secret_like = args
        .get("allow_secret_like")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

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
    // ADR-0027 T3: one room parser for every transport (`RoomType::parse`).
    let room = args
        .get("room")
        .and_then(|v| v.as_str())
        .map(RoomType::parse)
        .unwrap_or(RoomType::General);
    let mut tags = parse_tags(&args);
    // Submission-logging Part B: attach `creator:*` attribution so every
    // MCP-origin drawer carries the writer identity (client =
    // `trusty-memory-mcp`, source = `mcp`, version, and caller-supplied
    // cwd/workstream when the request carried them — DOC-53 §4.3 critical
    // fix: NEVER this shared daemon's own env/cwd, see
    // `helpers::attach_mcp_attribution`'s doc comment). Issue #202: also
    // project a bare-UUID session tag (when present in the caller's tags)
    // into the reserved `creator:session=<first-8>` slot so the activity
    // panel can surface it without inspecting every tag.
    attach_mcp_attribution(&mut tags, &args);

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
    // ADR-0027 T9: optional wing scope. Absent (the overwhelmingly common
    // case) means the palace's default wing, which is byte-identical to the
    // pre-wing write path. Present means this room belongs to that wing, so
    // `engineer`/`Planning` and `pm`/`Planning` are two different rooms.
    // Without this the write path could never reach a non-default wing and
    // wing-scoped recall would be permanently empty outside the default.
    let wing_handle = open_palace_handle(state, palace)?;
    let wing_id = resolve_wing_arg(&wing_handle, &args, "memory_remember")?;
    // #4886: ADR-0028 D3/D4. `fact_key` is what makes this a Tier C write; a
    // slot that fails admission degrades to an ordinary drawer, and the
    // envelope below reports which tier the write actually landed in.
    let admission = resolve_tier_c(&args, "memory_remember")?;
    let mut opts = RememberOptions {
        wing_id,
        ..mcp_remember_opts(force, defer_embedding, allow_secret_like)
    };
    let (tier, refusal) = apply_tier_c(&admission, &mut opts);
    let drawer_id = write_drawer(
        state,
        WriteDrawerParams {
            palace_id: palace,
            content: text,
            tags,
            room,
            importance: 0.5,
            opts,
            room_label_for_kg,
        },
    )
    .await?;
    let mut out = json!({
        "drawer_id": drawer_id.to_string(),
        "palace": palace,
        "status": "stored",
        "tier": tier,
    });
    if let Some(reason) = refusal {
        out["tier_c_refused"] = json!(reason);
    }
    Ok(out)
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
    // Submission-logging Part B: same attribution as memory_remember
    // (caller-supplied cwd/workstream, never the daemon's own — DOC-53
    // §4.3). Issue #202: project a bare-UUID session tag (when present)
    // into the reserved `creator:session=<first-8>` slot.
    attach_mcp_attribution(&mut tags, &args);
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
    // ADR-0027 T5 (#4804): the pin to `General` is lifted. `memory_note` takes
    // the same optional `room` as `memory_remember`, through the same single
    // parser (`RoomType::parse`, ADR-0027 D4.1), and still defaults to
    // `General` — so a caller that passes nothing is unaffected.
    let room = args
        .get("room")
        .and_then(|v| v.as_str())
        .map(RoomType::parse)
        .unwrap_or(RoomType::General);
    // Mirror the resolved room for the KG extractor so the auto-extracted
    // triples carry the same room label as the drawer.
    let room_label_for_kg = room_label(&room);
    // note() preset skips the token threshold; we keep the default
    // filter for noise patterns. No MCP-stricter min_tokens override
    // is needed because `enforce_min_tokens = false`.
    // #4886: same Tier C surface as `memory_remember`. `memory_note` is the
    // more likely home for a current fact — it is the short, curated, pinned
    // `importance = 1.0` path, which is exactly the privilege ADR-0028 §C5
    // measured at 8.25x median injection lift and D4 requires be paired with a
    // retirement condition.
    let admission = resolve_tier_c(&args, "memory_note")?;
    let mut opts = RememberOptions {
        defer_embedding,
        ..RememberOptions::note()
    };
    let (tier, refusal) = apply_tier_c(&admission, &mut opts);
    let drawer_id = write_drawer(
        state,
        WriteDrawerParams {
            palace_id: palace,
            content,
            tags,
            room,
            importance: 1.0,
            opts,
            room_label_for_kg,
        },
    )
    .await
    .context("PalaceHandle::remember_with_options (note)")?;
    let mut out = json!({
        "drawer_id": drawer_id.to_string(),
        "palace": palace,
        "status": "stored",
        "drawer_type": "UserFact",
        "tier": tier,
    });
    if let Some(reason) = refusal {
        out["tier_c_refused"] = json!(reason);
    }
    Ok(out)
}

/// Whether a recall may use the vector lane, or must take the degraded
/// L0/L1 + BM25 fallback.
///
/// Why (#4836): the degraded fallback ignores the query except through an
/// optional BM25 lane, so taking it when the embedder is in fact live makes
/// `memory_recall` answer every query with the same drawers — which is what it
/// did for the whole life of any daemon whose startup warm-up failed once. The
/// readiness latch alone cannot be trusted for this decision: a recall that
/// degrades never calls [`AppState::embedder`], so it can never observe the
/// recovery that would clear the latch. Asking the embedder cell directly
/// breaks that deadlock, and costs one atomic load.
/// What: `true` when readiness is `Ready`, or when the shared embedder is
/// already initialised regardless of the latch. Never triggers a cold init, so
/// issue #1970's "never block a recall on embedder warm-up" guarantee holds.
/// Test: `different_queries_return_different_drawers`,
/// `deep_recall_also_discriminates_by_query` (tests/recall_query_discrimination.rs)
/// — both drive recall with a live embedder behind a stranded `Warming` latch.
fn vector_lane_available(state: &AppState) -> bool {
    state.readiness() != DaemonReadiness::Warming
        || trusty_common::memory_core::retrieval::shared_embedder_initialized()
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
/// ADR-0027 T7: `room` scopes the lexical lane here for the same reason it
/// scopes L2/L3 — a caller who narrowed to one room must never be handed
/// another room's drawer just because the daemon happened to be warming. L0/L1
/// are deliberately left unfiltered, matching `retrieval::recall_in_room`.
/// #5036: the palace is read off `handle`, never passed in. This is the one
/// path where the lexical lane is the ONLY lane, so keying it on a caller's
/// slug — which `open_palace` may have redirected through an alias — searched a
/// socket the backfill never writes and left the caller with L0/L1 alone.
/// Taking no `palace` parameter makes that disagreement unrepresentable.
/// Test: `recall_degrades_to_l0_l1_when_the_embedder_is_genuinely_cold`,
/// `recall_deep_degrades_to_l0_l1_when_the_embedder_is_genuinely_cold`,
/// `bm25_alias_recall.rs::an_aliased_recall_reads_the_corpus_the_backfill_wrote`.
async fn recall_without_embedder(
    state: &AppState,
    handle: &trusty_common::memory_core::retrieval::PalaceHandle,
    query: &str,
    scope: &RecallScope,
    top_k: usize,
) -> Vec<trusty_common::memory_core::retrieval::RecallResult> {
    let palace = handle.id.as_str();
    let mut results = trusty_common::memory_core::retrieval::retrieve_l0_l1(handle);
    // ADR-0027 T7 filtered this lane by room; T9 (#4809) widens it to the whole
    // scope so a WING-scoped recall issued while the embedder is warming is
    // filtered too. Without that, the warming path would return unscoped
    // results — a scope boundary failing open, which is a leak, not a
    // degraded result.
    let allowed = scope.allowed_room_ids(&handle.kg);
    if let Some(bm25_hits) = bm25_search_optional(state, palace, query, top_k).await {
        for hydrated in bm25_hits_to_recall_results(handle, &bm25_hits) {
            if !scope_admits(&allowed, hydrated.drawer.room_id) {
                continue;
            }
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

/// The optional `room` scope on a recall call.
///
/// Why (ADR-0027 T7 / #4806): `retrieve_l2` has enforced a room filter since
/// #3274, but the MCP recall schema never carried the argument, so room-scoped
/// recall was reachable only through `memory_list` or the HTTP `/recall` route.
/// Parsing it in one helper keeps `memory_recall` and `memory_recall_deep`
/// from drifting into two spellings of the same option (ADR-0027 D4.1).
/// What: `RoomType::parse` over `args["room"]`; `None` means every room.
/// Test: `dispatch_recall_room_filter_scopes_results`.
fn recall_room_filter(args: &Value) -> Option<RoomType> {
    args.get("room")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(RoomType::parse)
}

/// Combine the optional `room` (T7) and `wing` (T9) arguments into one scope.
///
/// Why (ADR-0027 #4806 + #4809): a room is a topic and a wing is an owner, so
/// the two arguments are different axes and every read path must agree on what
/// they mean together. Resolving them in ONE helper is what keeps
/// `memory_recall`, `memory_recall_deep`, and `memory_list` from drifting into
/// three spellings of the same option (ADR-0027 D4.1).
/// What: `RecallScope::Wing` when `wing` is present, `RecallScope::Room` when
/// only `room` is, `RecallScope::All` when neither. Supplying BOTH is an error
/// — "that topic inside that scope" is a real query this ticket does not
/// implement, and honouring one while dropping the other is exactly the
/// invisible failure ADR-0027 exists to remove. An unknown wing errors here
/// too, via `resolve_wing_arg`.
/// Test: `recall_rejects_wing_and_room_together`,
/// `recall_rejects_an_unknown_wing`.
fn recall_scope(handle: &PalaceHandle, args: &Value, tool: &str) -> Result<RecallScope> {
    let room = recall_room_filter(args);
    match resolve_wing_arg(handle, args, tool)? {
        Some(wing_id) => {
            if room.is_some() {
                return Err(anyhow!(
                    "{tool}: 'wing' and 'room' together are not supported yet — \
                     pass one or the other"
                ));
            }
            Ok(RecallScope::Wing(wing_id))
        }
        None => Ok(RecallScope::from_room_filter(room)),
    }
}

pub(crate) async fn handle_memory_recall(state: &AppState, args: Value) -> Result<Value> {
    let palace = resolve_palace(state, &args, "memory_recall")?;
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("memory_recall: missing 'query'"))?;
    let top_k = args.get("top_k").and_then(|v| v.as_u64()).unwrap_or(10) as usize;

    let handle = open_palace_handle(state, &palace)?;
    // ADR-0027 T7 + T9: resolved BEFORE the warming short-circuit below, so a
    // wing- or room-scoped recall is filtered on every path. Resolving it after
    // that early return would let a scoped recall issued during embedder warmup
    // come back unfiltered.
    let scope = recall_scope(&handle, &args, "memory_recall")?;

    // Issue #1970: while the embedder is still warming up, return BM25 +
    // L0/L1 results immediately instead of blocking/erroring on embedder
    // state.
    // #4836: gate on the embedder's real state, not the readiness latch alone —
    // this fallback ignores the query, so entering it while the embedder is live
    // makes every query return the same drawers.
    if !vector_lane_available(state) {
        let results = recall_without_embedder(state, &handle, query, &scope, top_k).await;
        return Ok(serialize_recall(&palace, query, results));
    }

    let embedder = state.embedder().await?;
    // Issue #156: when the BM25 lane is enabled, run it in parallel
    // with the vector recall and RRF-fuse the two ranked lists.
    // When the daemon is unavailable or the env var is unset, the
    // helper returns `None` and we return the vector-only results
    // verbatim — zero behavioural change for existing deployments.
    // ADR-0027 T7: the BM25 lane needs no scope filter of its own here —
    // `fuse_bm25_into_recall` only *boosts* drawers already present in the
    // vector list (it never appends BM25-only hits), and that list is already
    // scoped, so no out-of-scope drawer can enter through the lexical
    // lane. The warming path above is different: it hydrates BM25-only hits,
    // which is why it filters explicitly.
    let vector_fut = recall_scoped(&handle, embedder.as_ref(), query, &scope, top_k);
    // #5036: key the lexical lane on the RESOLVED palace id. `open_palace`
    // follows aliases, so the requested slug can address a different palace
    // than the vector lane just searched.
    let bm25_fut = bm25_search_optional(state, handle.id.as_str(), query, top_k);
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
    // ADR-0027 T7 + T9: deep recall is no longer the odd one out — it takes the
    // same `room`/`wing` scope as `memory_recall`, resolved before the warming
    // short-circuit so no path can return unscoped results.
    let scope = recall_scope(&handle, &args, "memory_recall_deep")?;

    // Issue #1970: same warming-fallback posture as memory_recall.
    // #4836: and the same embedder-state gate, for the same reason.
    if !vector_lane_available(state) {
        let results = recall_without_embedder(state, &handle, query, &scope, top_k).await;
        return Ok(serialize_recall(&palace, query, results));
    }

    let embedder = state.embedder().await?;
    let results = recall_deep_scoped(&handle, embedder.as_ref(), query, &scope, top_k)
        .await
        .context("recall_deep")?;
    Ok(serialize_recall(&palace, query, results))
}

pub(crate) async fn handle_memory_list(state: &AppState, args: Value) -> Result<Value> {
    let palace = resolve_palace(state, &args, "memory_list")?;
    let handle = open_palace_handle(state, &palace)?;
    let tag = args
        .get("tag")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
    // ADR-0027 T9: `wing` narrows to one scope's rooms; absent, this is the
    // unchanged pre-wing listing. Same shared resolver the recall paths use, so
    // all three agree on what `wing` and `room` mean together.
    let drawers = match recall_scope(&handle, &args, "memory_list")? {
        RecallScope::Wing(wing_id) => list_drawers_in_wing(&handle, wing_id, tag, limit),
        RecallScope::Room(room) => handle.list_drawers(Some(room), tag, limit),
        RecallScope::All => handle.list_drawers(None, tag, limit),
    };
    let payload: Vec<Value> = drawers
        .iter()
        .map(|d| {
            json!({
                "drawer_id": d.id.to_string(),
                "content": d.content(),
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

/// Delete one drawer from every lane that can surface it.
///
/// Why (#5053): "forget" used to mean redb plus the vector store, and the
/// lexical lane kept the text — so a drawer a user explicitly deleted went on
/// matching BM25 queries and went on contributing to RRF fusion. A deletion
/// that covers some of the places the content lives is not a deletion, so the
/// BM25 document is removed here rather than left to a repair pass that only
/// ever adds.
/// What: parses the id, forgets through the palace handle (redb + vector +
/// in-memory tables), then deletes the BM25 document via
/// [`bm25_delete_document`] and propagates its failure — the caller must not
/// read "deleted" off a call that could not finish. The BM25 delete runs
/// whatever `forget` reported: an id already absent from redb is exactly the
/// shape a retry after a half-completed forget takes, and skipping it there
/// would strand the stale document forever. `handle.id`, not the requested
/// slug, keys the lane — `open_palace` follows aliases and the writer indexed
/// under the resolved id (#5036).
/// Test: `tests/bm25_forget_delete.rs::a_forgotten_drawer_leaves_the_lexical_corpus`,
/// `forget_fails_loudly_when_the_lexical_lane_cannot_confirm_the_delete`,
/// `forget_succeeds_when_the_lexical_lane_is_disabled`.
pub(crate) async fn handle_memory_forget(state: &AppState, args: Value) -> Result<Value> {
    let palace = resolve_palace(state, &args, "memory_forget")?;
    let drawer_id_str = args
        .get("drawer_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("memory_forget: missing 'drawer_id'"))?;
    let drawer_id = Uuid::parse_str(drawer_id_str)
        .map_err(|e| anyhow!("memory_forget: invalid drawer_id UUID: {e}"))?;
    let handle = open_palace_handle(state, &palace)?;
    let outcome = handle.forget(drawer_id).await.context("forget")?;
    // #5053: the lexical lane is the other place this drawer's text lives.
    bm25_delete_document(state, handle.id.as_str(), drawer_id).await?;
    // #5231: only a real deletion emits DrawerDeleted and reports "deleted".
    // A no-op used to do both, so an audit loop saw N delete events for zero
    // deletions.
    if outcome.is_deleted() {
        // Issue #96: emit so MCP-driven deletes are visible in the feed.
        let drawer_count = handle.drawers.read().len();
        state.emit(DaemonEvent::DrawerDeleted {
            palace_id: palace.clone(),
            drawer_count,
            source: ActivitySource::Mcp,
        });
    }
    // Issue #228: skip the per-write `StatusChanged` emit — the ticker
    // handles aggregate roll-ups.
    Ok(json!({ "status": outcome.as_str(), "drawer_id": drawer_id_str, "palace": palace }))
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
        let hits = recall_without_embedder(state, handle, query, &RecallScope::All, top_k).await;
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
    let palaces = crate::service::helpers::list_palaces_blocking(state).await?;

    // #4637: open_palace (not peek) is deliberate — recall must see every
    // palace; the shared helper keeps the blocking opens off the async executor.
    let handles =
        crate::service::helpers::open_palaces_blocking(state, &palaces, "memory_recall_all").await;

    // Issue #1970: BM25 + L0/L1 fallback across every palace while warming.
    // #4836: gated on the embedder's real state, as the per-palace paths are.
    let results = if !vector_lane_available(state) {
        recall_all_without_embedder(state, &handles, query, top_k).await
    } else {
        // #4836: `embedder()` now yields the type-erased shared embedder
        // directly, so the local re-erasure this used to need is gone.
        let embedder = state.embedder().await?;
        recall_across_palaces(&handles, &embedder, query, top_k, deep)
            .await
            .context("recall_across_palaces")?
    };

    let payload: Vec<Value> = results
        .iter()
        .map(|r| {
            json!({
                "palace_id":  r.palace_id,
                "drawer_id":  r.result.drawer.id.to_string(),
                "content":    r.result.drawer.content(),
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
    // DOC-53 §4.3 critical fix: this handler runs inside the shared daemon
    // (same hazard as `attach_mcp_attribution` — see its doc comment), so
    // attribution must come from caller-supplied `args`, never
    // `CreatorInfo::new_self`'s daemon-own env/cwd.
    let caller_cwd = args
        .get("cwd")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let caller_workstream = args
        .get("workstream")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let drawer_id = crate::messaging::send_message_to_palace(
        &state.registry,
        &state.data_root,
        &from_palace,
        &to_palace,
        &purpose,
        content,
        CreatorInfo::new_for_caller(
            MCP_CLIENT_NAME,
            CreatorSource::Mcp,
            caller_cwd,
            caller_workstream,
        ),
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
