//! AWS Bedrock provider — STUB (real implementation deferred).
//!
//! Why: #1021 establishes the provider abstraction and the `bedrock/*` routing
//! seam so per-agent model routing can name Bedrock slugs today, while the
//! actual Bedrock wire integration (SigV4 auth, Converse API tool-choice
//! mapping, usage extraction) lands in a later ticket. The stub keeps the
//! factory total and lets routing tests pass without pulling in the AWS SDK.
//! What: [`BedrockProvider`] implements [`Provider`]. `name` and
//! `supports_native_tools` are real; `map_tool_choice` and `extract_usage` are
//! NOT yet implemented and will panic if called — the agent loop never invokes
//! a Bedrock provider until the real implementation lands, so a panic here is a
//! loud programmer-error signal rather than a silent wrong answer.
//! Test: `bedrock::tests::*` cover the implemented methods and assert the stubs
//! panic.
//!
//! NOTE: Real Bedrock implementation deferred (see #1021 acceptance criteria —
//! "Stub BedrockProvider needs no change to agent loop").

use serde_json::Value;

use super::traits::{Provider, ToolChoice};
use crate::llm::ChatResponse;
use crate::perf::TokenUsage;

/// Stub provider for AWS Bedrock-hosted models (`bedrock/*` slugs).
///
/// Why: Reserves the routing target and proves the abstraction is multi-backend
/// without committing to the AWS SDK surface yet.
/// What: Zero-sized marker type implementing [`Provider`]; tool-choice mapping
/// and usage extraction are deferred stubs.
/// Test: `bedrock::tests::*`.
#[derive(Debug, Clone, Default)]
pub struct BedrockProvider;

impl BedrockProvider {
    /// Construct a `BedrockProvider`.
    ///
    /// Why: Gives the factory a uniform constructor call site.
    /// What: Returns the zero-sized marker.
    /// Test: `bedrock::tests::name_is_bedrock`.
    pub fn new() -> Self {
        Self
    }
}

impl Provider for BedrockProvider {
    fn name(&self) -> &str {
        "bedrock"
    }

    fn map_tool_choice(&self, _choice: ToolChoice) -> Value {
        // NOTE: Real implementation deferred. Bedrock's Converse API expresses
        // tool choice differently from the OpenAI schema; mapping lands with the
        // full Bedrock integration. The agent loop does not call this stub yet.
        unimplemented!("BedrockProvider::map_tool_choice is a deferred stub (#1021)")
    }

    fn extract_usage(&self, _response: &ChatResponse) -> TokenUsage {
        // NOTE: Real implementation deferred. Bedrock reports usage in its own
        // response envelope, not the OpenAI `usage` block parsed by
        // `ChatResponse`. The agent loop does not call this stub yet.
        unimplemented!("BedrockProvider::extract_usage is a deferred stub (#1021)")
    }

    fn supports_native_tools(&self) -> bool {
        // Bedrock-hosted Anthropic/other models support native tool use; the
        // strategy matrix (#1023) may refine this per-model later.
        true
    }

    fn supports_prompt_caching(&self) -> bool {
        // Bedrock's Converse API expresses prompt caching via its own
        // `cachePoint` block shape, not Anthropic's `cache_control` marker
        // (#2156) — a different wire format this stub does not implement.
        // `false` until the real Bedrock integration lands so
        // `agent_loop::build_request` never emits a marker Bedrock can't
        // interpret.
        false
    }

    fn wants_detailed_usage(&self) -> bool {
        // `RequestUsageConfig`/`usage: {"include": true}` is an
        // OpenRouter-specific OpenAI-compat wire directive; Bedrock's
        // Converse API reports usage in its own response envelope entirely,
        // so this must stay `false` until the real Bedrock integration lands.
        false
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// `name()` returns `"bedrock"`.
    ///
    /// Why: Routing and telemetry distinguish Bedrock from OpenRouter by name.
    /// What: Construct the stub, assert the name.
    /// Test: this test.
    #[test]
    fn name_is_bedrock() {
        assert_eq!(BedrockProvider::new().name(), "bedrock");
    }

    /// Bedrock reports native-tool support.
    ///
    /// Why: Bedrock-hosted Anthropic models accept the native `tools` array.
    /// What: Assert `supports_native_tools()` is `true`.
    /// Test: this test.
    #[test]
    fn bedrock_supports_native_tools() {
        assert!(BedrockProvider::new().supports_native_tools());
    }

    /// Bedrock does NOT report Anthropic-style prompt-caching support
    /// (#2156).
    ///
    /// Why: Bedrock's Converse API caching uses a different wire shape
    /// (`cachePoint` blocks); until that integration lands, `build_request`
    /// must never emit an Anthropic `cache_control` marker for a Bedrock
    /// route.
    /// What: Assert `supports_prompt_caching()` is `false`.
    /// Test: this test.
    #[test]
    fn bedrock_does_not_support_prompt_caching() {
        assert!(!BedrockProvider::new().supports_prompt_caching());
    }

    /// Bedrock does NOT request OpenRouter-style detailed usage accounting
    /// (response-side cache-usage fix).
    ///
    /// Why: `usage: {"include": true}` is an OpenRouter-only OpenAI-compat
    /// directive; sending it to Bedrock (once implemented) would be a
    /// meaningless no-op at best.
    /// What: Assert `wants_detailed_usage()` is `false`.
    /// Test: this test.
    #[test]
    fn bedrock_does_not_want_detailed_usage() {
        assert!(!BedrockProvider::new().wants_detailed_usage());
    }

    /// `map_tool_choice` panics while stubbed.
    ///
    /// Why: Document and lock in the deferred-stub contract so a future caller
    /// hits a loud error rather than a silent wrong mapping.
    /// What: Expect the call to panic.
    /// Test: this test.
    #[test]
    #[should_panic(expected = "deferred stub")]
    fn map_tool_choice_is_stubbed() {
        let _ = BedrockProvider::new().map_tool_choice(ToolChoice::Auto);
    }

    /// `extract_usage` panics while stubbed.
    ///
    /// Why: Same deferred-stub contract for usage extraction.
    /// What: Expect the call to panic.
    /// Test: this test.
    #[test]
    #[should_panic(expected = "deferred stub")]
    fn extract_usage_is_stubbed() {
        let fixture = r#"{"id":"x","choices":[],"usage":{}}"#;
        let resp: ChatResponse = serde_json::from_str(fixture).expect("deserialise");
        let _ = BedrockProvider::new().extract_usage(&resp);
    }
}
