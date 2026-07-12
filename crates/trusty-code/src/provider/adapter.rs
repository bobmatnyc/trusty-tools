//! Model-adapter factory — maps a model slug to a concrete [`Provider`].
//!
//! Why: The agent loop holds a model slug and needs the matching backend
//! behaviour (tool-choice mapping, usage extraction). A single factory keeps the
//! slug→provider decision in one place instead of scattering `if slug.starts_with`
//! branches across the loop (#1021).
//! What: [`provider_for`] returns an `Arc<dyn Provider>` — [`BedrockProvider`]
//! for `bedrock/*` slugs and [`OpenRouterProvider`] for everything else.
//! Test: `adapter::tests::*`.

use std::sync::Arc;

use super::bedrock::BedrockProvider;
use super::fireworks::FireworksProvider;
use super::openrouter::OpenRouterProvider;
use super::traits::Provider;

/// Slug prefix that routes to the Bedrock backend.
const BEDROCK_PREFIX: &str = "bedrock/";

/// Slug prefix that routes to the Fireworks backend (#2406).
const FIREWORKS_PREFIX: &str = "fireworks/";

/// Select the provider implementation for a model slug.
///
/// Why: The loop must stay backend-agnostic; this factory is the only place that
/// knows how to map a slug to a backend, so adding a backend touches one site.
/// What: Returns [`BedrockProvider`] for slugs beginning with `bedrock/`,
/// [`FireworksProvider`] for slugs beginning with `fireworks/` (#2406), and the
/// default [`OpenRouterProvider`] (carrying the slug for its
/// `supports_native_tools` decision) for everything else.
/// Test: `adapter::tests::provider_for_routes_*`.
pub fn provider_for(slug: &str) -> Arc<dyn Provider> {
    if slug.starts_with(BEDROCK_PREFIX) {
        Arc::new(BedrockProvider::new())
    } else if slug.starts_with(FIREWORKS_PREFIX) {
        Arc::new(FireworksProvider::new())
    } else {
        Arc::new(OpenRouterProvider::new(slug))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// `bedrock/*` slugs route to the Bedrock provider.
    ///
    /// Why: Per-agent routing must direct Bedrock-hosted models to the Bedrock
    /// backend, not OpenRouter.
    /// What: Resolve a `bedrock/...` slug, assert `name() == "bedrock"`.
    /// Test: this test.
    #[test]
    fn provider_for_routes_bedrock() {
        let p = provider_for("bedrock/us.anthropic.claude-sonnet-4-5");
        assert_eq!(p.name(), "bedrock");
    }

    /// `fireworks/*` slugs route to the Fireworks provider (#2406).
    ///
    /// Why: per-agent routing must direct Fireworks-hosted models to the
    /// Fireworks normalisation profile, not the OpenRouter default.
    /// What: Resolve a `fireworks/...` slug, assert `name() == "fireworks"`.
    /// Test: this test.
    #[test]
    fn provider_for_routes_fireworks() {
        let p = provider_for("fireworks/accounts/fireworks/models/llama-v3p1-70b-instruct");
        assert_eq!(p.name(), "fireworks");
    }

    /// Non-Bedrock, non-Fireworks slugs route to OpenRouter (the default).
    ///
    /// Why: Qwen/DeepSeek/Gemma/OpenAI/Anthropic-via-OR all go through
    /// OpenRouter; verify the default branch for several families.
    /// What: Resolve each slug, assert `name() == "openrouter"`.
    /// Test: this test.
    #[test]
    fn provider_for_routes_openrouter_default() {
        for slug in [
            "qwen/qwen-2.5-coder-32b-instruct",
            "deepseek/deepseek-chat",
            "google/gemma-2-27b-it",
            "openai/gpt-4o-mini",
            "anthropic/claude-sonnet-4-5",
        ] {
            assert_eq!(
                provider_for(slug).name(),
                "openrouter",
                "slug {slug} should route to OpenRouter"
            );
        }
    }

    /// The OpenRouter default carries the slug through to `supports_native_tools`.
    ///
    /// Why: The factory must build the OpenRouter provider with the slug so its
    /// native-tool decision is correct, not a blank default.
    /// What: Resolve a Claude slug and a Qwen slug; assert their native-tool
    /// flags differ as expected.
    /// Test: this test.
    #[test]
    fn provider_for_preserves_slug_for_native_tool_decision() {
        assert!(provider_for("anthropic/claude-sonnet-4-5").supports_native_tools());
        assert!(!provider_for("qwen/qwen-2.5-coder-32b-instruct").supports_native_tools());
    }
}
