//! Bedrock Converse `cachePoint` prompt-caching helpers (#2260).
//!
//! Why: `agent_loop::build_request` already marks the byte-stable
//! tools+system-prompt prefix with `cache_control` (#2156) — a marker every
//! provider's transport is free to interpret however its wire format
//! requires. OpenRouter's `anthropic/*` passthrough serialises that marker
//! as Anthropic's `cache_control` content-block field
//! (`llm::message::ChatMessage::serialize`); Bedrock's Converse API has NO
//! such field — it expresses the identical concept as a distinct
//! `cachePoint` content block interleaved into the `system`/`tools`/
//! `messages` arrays (`ContentBlock::CachePoint` / `SystemContentBlock::
//! CachePoint` / `Tool::CachePoint`, all wrapping the same `CachePointBlock`
//! struct). This module is the translation: it reads the SAME `build_request`
//! markers `super::convert` already receives on the `ChatRequest` and emits
//! the Bedrock-native block shape instead.
//! What: [`cache_point_block`] builds the one `CachePointBlock` shape tcode
//! ever sends (`type: default`, no explicit TTL). [`system_cacheable`] and
//! [`tools_cacheable`] are the minimum-size guards: Anthropic models on
//! Bedrock only cache a prefix at or above ~1024 tokens (the Sonnet/Opus
//! floor; Haiku's is 2048, but tcode does not yet track per-model minimums,
//! so the conservative Sonnet floor is applied universally) — marking a
//! smaller prefix is not an error, but it wastes a checkpoint slot (Bedrock
//! allows at most 4 per request) for zero benefit, so `super::convert` skips
//! emission below the floor rather than relying on Bedrock to silently
//! no-op it.
//! Test: `bedrock::tests::*`.

use aws_sdk_bedrockruntime::types::{CachePointBlock, CachePointType};

use crate::agent_loop::estimate_tokens;
use crate::llm::ToolDefinition;

/// Minimum estimated-token size of a prefix worth a Bedrock cachePoint
/// breakpoint (the Claude Sonnet/Opus floor; see module doc).
pub(super) const MIN_CACHEABLE_TOKENS: usize = 1024;

/// Build the one `CachePointBlock` shape tcode ever sends.
///
/// Why: Every breakpoint tcode emits is the same "cache everything up to
/// here, ephemeral" marker — there is no per-call variation (no custom TTL)
/// today, so a single constructor keeps `super::convert`'s call sites a
/// one-liner instead of repeating the builder chain.
/// What: Returns `CachePointBlock { r#type: Default, ttl: None }`. The
/// builder can only fail if `r#type` is left unset, which never happens
/// here — `expect` documents that as a genuine invariant, not a call for
/// `?` plumbing a caller could ever meaningfully recover from.
/// Test: `bedrock::tests::cache_point_block_is_default_type`.
pub(super) fn cache_point_block() -> CachePointBlock {
    CachePointBlock::builder()
        .r#type(CachePointType::Default)
        .build()
        .expect("CachePointBlock::builder always sets the only required field, `r#type`")
}

/// Whether a system-prompt text is large enough to justify a cachePoint
/// breakpoint.
///
/// Why: Guards [`super::convert::build_converse_messages`] against spending
/// one of Bedrock's 4-per-request checkpoint slots on a system prompt too
/// small for Anthropic's minimum-cacheable-prefix floor to ever produce a
/// cache hit.
/// What: `true` when `estimate_tokens(text) >= MIN_CACHEABLE_TOKENS`.
/// Test: `bedrock::tests::system_cacheable_respects_floor`.
pub(super) fn system_cacheable(text: &str) -> bool {
    estimate_tokens(text) >= MIN_CACHEABLE_TOKENS
}

/// Whether a set of tool definitions is large enough (their combined
/// JSON-Schema bodies) to justify a cachePoint breakpoint.
///
/// Why: Same floor as [`system_cacheable`], applied to the tools array —
/// guards [`super::convert::build_tool_config`] against a wasted checkpoint
/// on a small tool set (e.g. a single-tool test fixture).
/// What: Serialises each `ToolDefinition.function` (name + description +
/// parameters — the JSON Schema actually sent on the wire) to a JSON string,
/// sums `estimate_tokens` over those strings, and compares against
/// `MIN_CACHEABLE_TOKENS`. A serialisation failure (should be unreachable —
/// `FunctionDefinition` is always representable as JSON) contributes zero
/// rather than aborting the guard.
/// Test: `bedrock::tests::tools_cacheable_respects_floor`.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::FunctionDefinition;

    #[test]
    fn cache_point_block_is_default_type() {
        let block = cache_point_block();
        assert_eq!(block.r#type(), &CachePointType::Default);
        assert!(block.ttl().is_none());
    }

    #[test]
    fn system_cacheable_respects_floor() {
        let small = "short prompt";
        let large = "x".repeat(MIN_CACHEABLE_TOKENS * 4 + 100);
        assert!(!system_cacheable(small));
        assert!(system_cacheable(&large));
    }

    #[test]
    fn tools_cacheable_respects_floor() {
        let tiny_tool = ToolDefinition::function(FunctionDefinition {
            name: "ping".into(),
            description: None,
            parameters: None,
            cache_control: None,
        });
        assert!(!tools_cacheable(&[tiny_tool]));

        let big_tool = ToolDefinition::function(FunctionDefinition {
            name: "write_file".into(),
            description: Some("x".repeat(MIN_CACHEABLE_TOKENS * 4 + 100)),
            parameters: None,
            cache_control: None,
        });
        assert!(tools_cacheable(&[big_tool]));
    }
}
