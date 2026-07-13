//! Bedrock Converse `cachePoint` prompt-caching helpers (#2260, ported #2407).
//!
//! Why: a caller that marks the byte-stable tools+system-prompt prefix with
//! `cache_control` (#2156, e.g. tcode's `agent_loop::build_request`) emits a
//! marker every provider's adapter is free to interpret however its wire format
//! requires. OpenRouter's `anthropic/*` passthrough serialises that marker as
//! Anthropic's `cache_control` content-block field (see
//! [`crate::inference::types::ChatMessage`]'s hand-written `Serialize`);
//! Bedrock's Converse API has NO such field — it expresses the identical
//! concept as a distinct `cachePoint` content block interleaved into the
//! `system`/`tools`/`messages` arrays (`ContentBlock::CachePoint` /
//! `SystemContentBlock::CachePoint` / `Tool::CachePoint`, all wrapping the same
//! `CachePointBlock` struct). This module is the translation: it reads the SAME
//! markers [`super::convert`] already receives on the [`crate::inference::ChatRequest`]
//! and emits the Bedrock-native block shape instead.
//! What: [`cache_point_block`] builds the one `CachePointBlock` shape ever sent
//! (`type: default`, no explicit TTL). [`system_cacheable`] and
//! [`tools_cacheable`] are the minimum-size guards: Anthropic models on Bedrock
//! only cache a prefix at or above ~1024 tokens (the Sonnet/Opus floor; Haiku's
//! is 2048, but per-model minimums are not tracked, so the conservative Sonnet
//! floor is applied universally) — marking a smaller prefix is not an error, but
//! it wastes a checkpoint slot (Bedrock allows at most 4 per request) for zero
//! benefit, so [`super::convert`] skips emission below the floor rather than
//! relying on Bedrock to silently no-op it. Token counts use the same cheap
//! chars/4 heuristic tcode's `agent_loop::estimate_tokens` uses, ported here so
//! the commons adapter is byte-for-byte behaviour-identical (the M3 bake-off
//! no-regression gate).
//! Test: `super::tests::{cache_point_block_is_default_type,
//! system_cacheable_respects_floor, tools_cacheable_respects_floor}`.

use aws_sdk_bedrockruntime::types::{CachePointBlock, CachePointType};

use crate::inference::types::ToolDefinition;

/// Minimum estimated-token size of a prefix worth a Bedrock cachePoint
/// breakpoint (the Claude Sonnet/Opus floor; see module doc).
pub(super) const MIN_CACHEABLE_TOKENS: usize = 1024;

/// Estimate the token count of a text span using a cheap chars/4 heuristic.
///
/// Why: the cache guards only need to answer "is this prefix large enough to
/// ever produce a cache hit"; a tokenizer dependency would be disproportionate.
/// This is the exact heuristic tcode's `agent_loop::estimate_tokens` applies, so
/// the ported adapter's cachePoint emission is byte-identical (bake-off gate).
/// What: returns `text.chars().count() / 4`, floor-divided.
/// Test: `super::tests::system_cacheable_respects_floor`.
fn estimate_tokens(text: &str) -> usize {
    text.chars().count() / 4
}

/// Build the one `CachePointBlock` shape ever sent.
///
/// Why: every breakpoint emitted is the same "cache everything up to here,
/// ephemeral" marker — there is no per-call variation (no custom TTL) today, so
/// a single constructor keeps [`super::convert`]'s call sites a one-liner
/// instead of repeating the builder chain.
/// What: returns `CachePointBlock { r#type: Default, ttl: None }`. The builder
/// can only fail if `r#type` is left unset, which never happens here — `expect`
/// documents that as a genuine invariant, not a call for `?` plumbing a caller
/// could ever meaningfully recover from.
/// Test: `super::tests::cache_point_block_is_default_type`.
pub(super) fn cache_point_block() -> CachePointBlock {
    CachePointBlock::builder()
        .r#type(CachePointType::Default)
        .build()
        .expect("CachePointBlock::builder always sets the only required field, `r#type`")
}

/// Whether a system-prompt text is large enough to justify a cachePoint
/// breakpoint.
///
/// Why: guards [`super::convert::build_converse_messages`] against spending one
/// of Bedrock's 4-per-request checkpoint slots on a system prompt too small for
/// Anthropic's minimum-cacheable-prefix floor to ever produce a cache hit.
/// What: `true` when `estimate_tokens(text) >= MIN_CACHEABLE_TOKENS`.
/// Test: `super::tests::system_cacheable_respects_floor`.
pub(super) fn system_cacheable(text: &str) -> bool {
    estimate_tokens(text) >= MIN_CACHEABLE_TOKENS
}

/// Whether a set of tool definitions is large enough (their combined
/// JSON-Schema bodies) to justify a cachePoint breakpoint.
///
/// Why: same floor as [`system_cacheable`], applied to the tools array — guards
/// [`super::convert::build_tool_config`] against a wasted checkpoint on a small
/// tool set (e.g. a single-tool test fixture).
/// What: serialises each `ToolDefinition.function` (name + description +
/// parameters — the JSON Schema actually sent on the wire) to a JSON string,
/// sums `estimate_tokens` over those strings, and compares against
/// `MIN_CACHEABLE_TOKENS`. A serialisation failure (should be unreachable —
/// `FunctionDefinition` is always representable as JSON) contributes zero rather
/// than aborting the guard.
/// Test: `super::tests::tools_cacheable_respects_floor`.
pub(super) fn tools_cacheable(tools: &[ToolDefinition]) -> bool {
    let total: usize = tools
        .iter()
        .map(|t| {
            let json = serde_json::to_string(&t.function).unwrap_or_default();
            estimate_tokens(&json)
        })
        .sum();
    total >= MIN_CACHEABLE_TOKENS
}
