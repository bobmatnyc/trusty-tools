//! Shared helpers for the trusty-memory MCP tool surface.
//!
//! Why: Gate logic (content/blocklist/dedup), palace resolution, the shared
//! write-drawer pipeline, and attribution helpers are used across every
//! tool handler; collecting them in one submodule keeps the per-tool
//! handler files focused (issue #607).
//! What: free functions + small structs/consts moved verbatim out of the
//! former monolithic `tools.rs`. Visibility widened to `pub(crate)` where a
//! sibling submodule or the test module needs the item.
//! Test: `content_gate_*`, `blocklist_gate_*`, `dedup_*`, and the dispatch
//! tests in `tools::tests`.

use super::bm25::bm25_index_enqueue;
use crate::attribution::{session_tag_from_tags, CreatorInfo, CreatorSource, MCP_CLIENT_NAME};
use crate::kg_extract::{extract_triples, ExtractInput};
use crate::{ActivitySource, AppState, DaemonEvent};
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use trusty_common::memory_core::filter::{FilterConfig, MCP_MIN_TOKENS};
use trusty_common::memory_core::palace::{PalaceId, RoomType};
use trusty_common::memory_core::retrieval::RememberOptions;
use uuid::Uuid;

/// Look up the friendly palace name (Palace.name) from the in-memory cache,
/// falling back to the id when the cache misses.
///
/// Why (issue #96): MCP-side emit calls need the same `palace_name` field
/// the HTTP path emits so the activity feed renders identical labels
/// regardless of origin.
/// Why (issue #228): the previous implementation called
/// `PalaceRegistry::list_palaces` — a synchronous filesystem walk — on every
/// `memory_remember` / `memory_note` write. With N palaces on disk that was
/// O(N) opendirs + `palace.json` reads per write, blocking the async runtime
/// thread (this helper had no `spawn_blocking` wrapper, unlike `palace_list`).
/// The replacement reads `state.palace_names` (a `DashMap` populated at
/// hydration / create time), which is a lock-free read and never touches
/// disk.
/// What: looks up `palace_id` in `state.palace_names`; on miss, returns the
/// id verbatim so emit calls never fail. Cache misses are non-fatal —
/// rename / create paths keep the cache in sync, but a fresh-after-restart
/// palace will hit the miss branch only until hydration completes.
/// Test: implicit — the MCP emit tests assert the `palace_id` matches; the
/// fallback is the same id-as-name behaviour the HTTP path uses. The cache
/// invariant is covered by `palace_name_cache_populated_after_hydration`
/// and `palace_name_cache_updates_on_create` in lib.rs.
pub(crate) fn lookup_palace_name(state: &AppState, palace_id: &str) -> String {
    state
        .palace_names
        .get(palace_id)
        .map(|entry| entry.value().clone())
        .unwrap_or_else(|| palace_id.to_string())
}

/// Minimum standalone-content word count enforced by [`content_gate`].
///
/// Why (issue #215): single-word user replies ("yes", "ok", "no thanks") have
/// no standalone memory value when the surrounding turn isn't captured
/// alongside them — they end up in the palace as orphan fragments that
/// pollute recall results. Requiring at least four whitespace-separated tokens
/// is a cheap heuristic that matches the natural boundary between "just a
/// reaction" and "an actual statement".
/// What: the threshold the gate compares against. Tokens are counted via
/// `split_whitespace().count()`, so punctuation does not inflate the count.
/// Test: `content_gate_blocks_short_no_context`, `content_gate_keeps_long`.
pub(crate) const CONTENT_GATE_MIN_WORDS: usize = 4;

/// Gate short standalone content unless a `context` wrapper is supplied.
///
/// Why: single-word or very-short standalone user responses ("yes", "ok")
/// have no standalone memory value (issue #215). Gate them unless a context
/// is provided.
/// What: returns `None` if `content` has fewer than [`CONTENT_GATE_MIN_WORDS`]
/// whitespace-separated tokens AND `context` is `None` (the write should be
/// skipped). Returns `Some(combined)` where `combined = "<context>\n\n---\n\n<content>"`
/// when `context` is `Some` and non-empty after trimming. Returns
/// `Some(content)` unchanged when `content` has at least
/// [`CONTENT_GATE_MIN_WORDS`] tokens. Tokens are counted on the trimmed
/// `content` so trailing whitespace doesn't inflate the count.
/// Test: `content_gate_blocks_short_no_context`,
/// `content_gate_wraps_short_with_context`,
/// `content_gate_keeps_long`, `content_gate_blank_context_treated_as_none`.
pub(crate) fn content_gate(content: &str, context: Option<&str>) -> Option<String> {
    let trimmed = content.trim();
    let word_count = trimmed.split_whitespace().count();
    // Treat a context that is empty or whitespace-only as "no context" — a
    // caller passing `""` should not unlock a write the gate would otherwise
    // drop, and the combined output would otherwise begin with a meaningless
    // separator.
    let context_clean = context.map(str::trim).filter(|s| !s.is_empty());
    if let Some(ctx) = context_clean {
        return Some(format!("{ctx}\n\n---\n\n{content}"));
    }
    if word_count < CONTENT_GATE_MIN_WORDS {
        return None;
    }
    Some(content.to_string())
}

/// Patterns whose content should never be stored as standalone memories.
///
/// Why (issue #220): the activity panel was being flooded with low-value
/// Claude Code auto-captures — `Tool use: Bash`, `Tool use: Edit File: …`,
/// `Claude Code session ended: <uuid>` — that carry no semantic value once
/// the surrounding turn is gone. They pollute recall results and burn UI
/// real estate. A blocklist is the cheapest way to filter them at write
/// time without coordinating with the auto-capture hook source.
/// What: substring patterns (not regexes) checked via `str::contains` so
/// the matcher stays branch-predictable and never panics on malformed
/// input. Patterns are intentionally lower-case-friendly but matched
/// case-sensitively because the auto-capture hooks always emit the exact
/// English prefix.
/// Test: `blocklist_gate_blocks_tool_use`,
/// `blocklist_gate_blocks_session_ended`,
/// `blocklist_gate_passes_normal_content`.
pub(crate) const BLOCKLIST_PATTERNS: &[&str] = &[
    "Tool use: ",          // Claude Code tool-use captures
    "Claude Code session", // Session lifecycle events
];

/// Rolling-window horizon for the dedup gate.
///
/// Why (issue #220): identical content is often emitted multiple times in
/// quick succession (auto-capture hook bursts, retries, copy-paste). A
/// 5-minute window catches the burst without rejecting deliberate user
/// re-statements hours later.
/// What: `chrono::Duration` value. Drawers created before
/// `now - DEDUP_WINDOW` are ignored by the dedup pass.
/// Test: indirect via `dedup_skips_near_duplicate` and
/// `dedup_allows_different_content` (use the helper directly).
pub(crate) const DEDUP_WINDOW_MINUTES: i64 = 5;

/// Maximum number of recent drawers the dedup pass scans.
///
/// Why: a palace can hold tens of thousands of drawers; we never need to
/// compare the new write against more than the most-recent handful to
/// catch the bursty-duplicate case. Capping the scan keeps the hot path
/// O(1) in the palace size.
/// What: ceiling on the candidate list pulled from
/// `PalaceHandle::list_drawers` before the time-window filter.
/// Test: `dedup_skips_near_duplicate` exercises the scan against a small
/// candidate set; the cap is enforced by `list_drawers`'s `limit` arg.
pub(crate) const DEDUP_SCAN_LIMIT: usize = 50;

/// Jaro-Winkler similarity threshold above which a candidate counts as a
/// near-duplicate of the new content.
///
/// Why: 0.92 is the empirically-chosen cutoff documented in the issue —
/// high enough to allow distinct facts to coexist, low enough to catch
/// trivial whitespace / punctuation / suffix variation. Jaro-Winkler is
/// preferred over plain Jaro because the auto-capture noise tends to share
/// the same prefix (`Tool use: …`, `Edit File: …`), which Jaro-Winkler
/// weights heavily.
/// What: `f64` threshold compared against `strsim::jaro_winkler`'s output.
/// Test: `dedup_skips_near_duplicate`, `dedup_allows_different_content`.
pub(crate) const DEDUP_SIMILARITY_THRESHOLD: f64 = 0.92;

/// Blocklist gate: returns true when the content should be silently
/// skipped because it matches a known low-value auto-capture pattern.
///
/// Why (issue #220): Centralises the pattern-match logic so both
/// `memory_remember` and `memory_note` go through the same filter. Trims
/// leading whitespace before matching so indented variants still hit.
/// What: returns `true` iff `content.contains(pat)` for any pattern in
/// `BLOCKLIST_PATTERNS`. Trimming uses `str::trim_start` to keep the
/// substring check predictable (the suffixes after the prefix can vary).
/// Test: `blocklist_gate_blocks_tool_use`,
/// `blocklist_gate_blocks_session_ended`,
/// `blocklist_gate_passes_normal_content`.
pub(crate) fn blocklist_gate(content: &str) -> bool {
    let trimmed = content.trim_start();
    BLOCKLIST_PATTERNS.iter().any(|pat| trimmed.contains(pat))
}

/// Dedup gate: returns true when the new content is a near-duplicate of a
/// drawer written to the same palace within the rolling window.
///
/// Why (issue #220): bursts of identical or near-identical content (auto-
/// capture retries, hook re-emissions, copy-paste artefacts) were
/// inflating the palace with no recall benefit. A short rolling window
/// catches the burst without rejecting deliberate re-statements hours
/// later.
/// What: pulls up to `DEDUP_SCAN_LIMIT` recent drawers from the live
/// in-memory table via `list_drawers` (a cheap snapshot, no I/O), filters
/// to those created within `DEDUP_WINDOW_MINUTES` of `now`, then computes
/// `strsim::jaro_winkler` against each. Returns `true` on the first match
/// above `DEDUP_SIMILARITY_THRESHOLD`. Returns `false` if `content` is
/// empty after trimming (the content gate handles that case separately)
/// or if the palace has no recent drawers.
/// Test: `dedup_skips_near_duplicate`, `dedup_allows_different_content`.
pub(crate) fn dedup_gate(handle: &trusty_common::memory_core::PalaceHandle, content: &str) -> bool {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return false;
    }
    let now = chrono::Utc::now();
    let window_start = now - chrono::Duration::minutes(DEDUP_WINDOW_MINUTES);
    let recent = handle.list_drawers(None, None, DEDUP_SCAN_LIMIT);
    recent
        .iter()
        .filter(|d| d.created_at >= window_start)
        .any(|d| strsim::jaro_winkler(trimmed, d.content.trim()) > DEDUP_SIMILARITY_THRESHOLD)
}

/// Build the strict MCP-level `RememberOptions`.
///
/// Why: Issue #61 — the MCP boundary is where auto-capture hooks deposit
/// raw tool/commit/prompt data; we want the 8-token threshold there even
/// though the library default is more permissive for direct callers.
/// What: Clones the default filter and bumps `min_tokens` to `MCP_MIN_TOKENS`.
/// Test: `dispatch_remember_rejects_short_content`.
pub(crate) fn mcp_remember_opts(force: bool) -> RememberOptions {
    let filter = FilterConfig {
        min_tokens: MCP_MIN_TOKENS,
        ..FilterConfig::default()
    };
    RememberOptions {
        filter,
        force,
        ..RememberOptions::default()
    }
}

/// Reverse of `parse_room`: produce a stable label for KG `in-room`
/// extraction.
///
/// Why: The auto-extractor wants the same friendly label the caller passed
/// (`"Backend"`, `"General"`, …) so the graph stays consistent across
/// remember calls regardless of how the MCP client spelled the argument.
/// What: Returns the canonical enum-name string for the built-in variants
/// and the inner string for `Custom`.
/// Test: Indirect — `auto_kg_extraction_hooks_into_memory_remember`
/// round-trips a known room label.
pub(crate) fn room_label(room: &RoomType) -> Option<String> {
    let label = match room {
        RoomType::Frontend => "Frontend",
        RoomType::Backend => "Backend",
        RoomType::Testing => "Testing",
        RoomType::Planning => "Planning",
        RoomType::Documentation => "Documentation",
        RoomType::Research => "Research",
        RoomType::Configuration => "Configuration",
        RoomType::Meetings => "Meetings",
        RoomType::General => "General",
        RoomType::Custom(s) => return Some(s.clone()),
    };
    Some(label.to_string())
}

/// Parse a `RoomType` from an optional string (`"Backend"`, `"Frontend"`,
/// etc.) — falls back to `RoomType::General` when unset or unknown.
///
/// Why: MCP arguments are JSON; we accept the friendly enum-name forms so
/// callers don't have to learn an internal serialization.
/// What: Match-on-string returning the corresponding `RoomType`.
/// Test: Indirectly via `dispatch_remember_then_recall`.
pub(crate) fn parse_room(s: Option<&str>) -> RoomType {
    match s.unwrap_or("General") {
        "Frontend" => RoomType::Frontend,
        "Backend" => RoomType::Backend,
        "Testing" => RoomType::Testing,
        "Planning" => RoomType::Planning,
        "Documentation" => RoomType::Documentation,
        "Research" => RoomType::Research,
        "Configuration" => RoomType::Configuration,
        "Meetings" => RoomType::Meetings,
        "General" => RoomType::General,
        other => RoomType::Custom(other.to_string()),
    }
}

/// Resolve (or lazily open) the palace handle for a tool call.
pub(crate) fn open_palace_handle(
    state: &AppState,
    palace_id: &str,
) -> Result<std::sync::Arc<trusty_common::memory_core::PalaceHandle>> {
    let pid = PalaceId::new(palace_id);
    state
        .registry
        .open_palace(&state.data_root, &pid)
        .with_context(|| format!("open palace {palace_id}"))
}

/// Run deterministic KG extraction over a freshly-written drawer and assert
/// any resulting triples through the palace's `KnowledgeGraph`.
///
/// Why: Issue #97 — `memory_remember` and `memory_note` should auto-populate
/// the KG so palaces with drawers always have a graph. The extractor is pure
/// and offline so the write hot path stays fast; failures *must never* fail
/// the parent write (the drawer is already on disk), so this function logs
/// and swallows every error.
/// What: Builds an `ExtractInput`, runs `extract_triples`, then calls
/// `handle.kg.assert` for each triple. Any failure during assertion is
/// captured as a `tracing::warn!` and the rest of the triples are still
/// attempted; the function returns nothing.
/// Test: `auto_kg_extraction_hooks_into_memory_remember`,
/// `auto_kg_extraction_no_op_does_not_fail_remember`,
/// `web::tests::http_create_drawer_runs_auto_kg_extraction`.
pub(crate) async fn auto_extract_and_assert(
    handle: &std::sync::Arc<trusty_common::memory_core::PalaceHandle>,
    drawer_id: Uuid,
    content: &str,
    tags: &[String],
    room: Option<&str>,
) {
    let input = ExtractInput {
        drawer_id,
        content,
        tags,
        room,
    };
    let triples = extract_triples(&input);
    if triples.is_empty() {
        return;
    }
    for triple in triples {
        let s = triple.subject.clone();
        let p = triple.predicate.clone();
        if let Err(e) = handle.kg.assert(triple).await {
            tracing::warn!(
                drawer_id = %drawer_id,
                subject = %s,
                predicate = %p,
                "auto kg extraction: assert failed (non-fatal): {e:#}",
            );
        }
    }
}

/// Resolve a palace argument, falling back to `state.default_palace` when
/// the caller omitted `palace`.
///
/// Why: `serve --palace <name>` lets the operator bind a process to a single
/// project namespace; tool calls then no longer need to repeat the palace
/// every time. This helper centralises the precedence rule (explicit arg
/// wins over default) and produces a uniform error when neither is set.
/// What: Returns the explicit `args["palace"]` string if present, otherwise
/// `state.default_palace`. Errors with a helpful message if both are absent.
/// Test: `default_palace_used_when_arg_omitted` and
/// `dispatch_unknown_tool_errors`.
pub(crate) fn resolve_palace<'a>(
    state: &'a AppState,
    args: &'a Value,
    tool: &str,
) -> Result<String> {
    if let Some(p) = args.get("palace").and_then(|v| v.as_str()) {
        return Ok(p.to_string());
    }
    state
        .default_palace
        .clone()
        .ok_or_else(|| anyhow!("{tool}: missing 'palace' (no --palace default configured)"))
}

/// Inputs to the shared write-drawer pipeline.
///
/// Why (issue #227): `memory_remember` and `memory_note` share the same
/// "open palace → write drawer → fan-out side effects" tail. Capturing those
/// inputs in one struct keeps the handler call sites flat and makes the
/// shared pipeline a single function — every behavioural divergence between
/// the two tools is now visible in their handlers, not buried in a
/// 60-line block of duplicated post-write fan-out.
/// What: bundles every value the post-gate pipeline needs. `room_label_for_kg`
/// is pre-computed by the handler (memory_note pins it to `"General"`;
/// memory_remember derives it from `RoomType` via [`room_label`]).
/// Test: exercised end-to-end by `dispatch_remember_then_recall`,
/// `dispatch_remember_with_context_writes_combined`, and the note tests.
pub(crate) struct WriteDrawerParams<'a> {
    pub(crate) palace_id: &'a str,
    pub(crate) content: String,
    pub(crate) tags: Vec<String>,
    pub(crate) room: RoomType,
    pub(crate) importance: f32,
    pub(crate) opts: RememberOptions,
    pub(crate) room_label_for_kg: Option<String>,
}

/// Run the shared write pipeline after content has been gated and attribution
/// applied.
///
/// Why (issue #227): centralises the open-palace → remember → BM25 → emit →
/// auto-KG-extract tail that `memory_remember` and `memory_note` both run.
/// Hosting it in one place keeps the side-effect ordering identical across
/// the two tools and makes future write-side hooks land in one location.
/// What: opens the palace handle, calls `remember_with_options`, fires the
/// BM25 index task, emits `DrawerAdded` + the aggregate status event, and
/// runs the auto-KG-extraction pass (best-effort). Returns the new drawer
/// id on success; any underlying error propagates via `anyhow::Result`.
/// Test: covered through `dispatch_remember_then_recall`,
/// `dispatch_remember_with_context_writes_combined`,
/// `dispatch_note_skips_short_no_context` (negative path before this runs),
/// and `auto_kg_extraction_hooks_into_memory_remember`.
pub(crate) async fn write_drawer(state: &AppState, params: WriteDrawerParams<'_>) -> Result<Uuid> {
    let WriteDrawerParams {
        palace_id,
        content,
        tags,
        room,
        importance,
        opts,
        room_label_for_kg,
    } = params;

    let handle = open_palace_handle(state, palace_id)?;
    // Snapshot the preview before `content` is moved into the write so the
    // activity feed shows what landed on disk (matches the HTTP path).
    let preview = crate::service::drawer_content_preview(&content);
    // Issue #97: keep originals so the auto-KG extractor sees the same
    // content / tags that landed in the drawer. `remember_with_options`
    // consumes them, so clone before the call.
    let content_for_kg = content.clone();
    let tags_for_kg = tags.clone();
    let drawer_id = handle
        .remember_with_options(content, room, tags, importance, opts)
        .await
        .context("PalaceHandle::remember_with_options")?;
    // Issue #156 + #231: opt-in BM25 lexical lane. Enqueue onto the
    // bounded indexer channel so the redb write returns immediately;
    // a full queue is dropped + logged rather than allowed to grow
    // unbounded behind a slow daemon (#231). Daemon errors observed
    // by the worker are logged but never block the MCP response.
    bm25_index_enqueue(state, palace_id, drawer_id, &content_for_kg);
    // Issue #96: emit a DrawerAdded so the activity feed shows
    // MCP-origin writes with `source = Mcp`.
    let palace_name = lookup_palace_name(state, palace_id);
    let drawer_count = handle.drawers.read().len();
    state.emit(DaemonEvent::DrawerAdded {
        palace_id: palace_id.to_string(),
        palace_name,
        drawer_count,
        timestamp: chrono::Utc::now(),
        content_preview: preview,
        source: ActivitySource::Mcp,
    });
    // Issue #228: do NOT emit `StatusChanged` on every write — the
    // aggregate-recompute was O(N palaces) of disk I/O on the hot path.
    // The periodic ticker spawned by `run_http_on` refreshes dashboard
    // totals on a fixed cadence; mutations themselves still surface via
    // the `DrawerAdded` SSE frame above.
    // Issue #97: best-effort auto-extraction. Failures never fail the
    // write — the drawer is already on disk.
    auto_extract_and_assert(
        &handle,
        drawer_id,
        &content_for_kg,
        &tags_for_kg,
        room_label_for_kg.as_deref(),
    )
    .await;
    Ok(drawer_id)
}

/// Build a JSON "skipped" envelope used by both write handlers when a gate
/// rejects the input.
///
/// Why (issue #227): keeps the three skip reasons (`blocked pattern`,
/// `short prompt, no context`, `duplicate within window`) emitted as a
/// uniform shape so callers can parse the envelope without per-tool
/// branching.
/// What: returns `{"palace": <id>, "status": "skipped", "reason": <reason>}`.
/// Test: exercised by `dispatch_remember_skips_short_no_context`,
/// `dispatch_note_skips_short_no_context`,
/// `dispatch_remember_blocks_blocklist_pattern`.
pub(crate) fn skipped_envelope(palace_id: &str, reason: &str) -> Value {
    json!({
        "palace": palace_id,
        "status": "skipped",
        "reason": reason,
    })
}

/// Extract a `tags` argument (JSON array of strings) into a `Vec<String>`.
///
/// Why: every write-side handler accepts an optional `tags` argument with
/// identical shape; centralising the parse keeps the handlers focused on
/// their tool-specific logic.
/// What: returns the strings in order; non-string entries are silently
/// dropped (matches pre-refactor behaviour).
/// Test: covered indirectly by `dispatch_remember_then_recall` and
/// `auto_kg_extraction_hooks_into_memory_remember`.
pub(crate) fn parse_tags(args: &Value) -> Vec<String> {
    args.get("tags")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| t.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// Attach the MCP attribution tags (`creator:*` and the bare-UUID session
/// projection) to the caller-supplied tag list.
///
/// Why (Submission-logging Part B + issue #202): every MCP-origin drawer must
/// carry the writer identity so the activity panel and audit logs can attribute
/// the write. Issue #202 also projects a bare-UUID session tag into the
/// reserved `creator:session=<first-8>` slot when present.
/// What: appends the session-tag projection (when one is found in the input
/// tags) then merges the canonical `CreatorInfo::new_self(MCP, Mcp)` into the
/// vec. Mutates in place to match the original code path.
/// Test: covered indirectly by `dispatch_remember_then_recall`.
pub(crate) fn attach_mcp_attribution(tags: &mut Vec<String>) {
    if let Some(session_tag) = session_tag_from_tags(tags) {
        tags.push(session_tag);
    }
    CreatorInfo::new_self(MCP_CLIENT_NAME, CreatorSource::Mcp).merge_into(tags);
}
