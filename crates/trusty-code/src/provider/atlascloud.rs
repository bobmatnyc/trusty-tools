//! AtlasCloud provider — request-shaping normalisation for `atlascloud/*` slugs
//! (#2536, epic #2400).
//!
//! Why: enabling AtlasCloud in tcode (#2536) is not only a transport concern —
//! `AgentLoop::build_request` consults `crate::provider::provider_for` to decide
//! whether to request OpenRouter's detailed-usage directive, whether to attach
//! prompt-cache breakpoints, and whether to send a native `tools` array. Routing
//! an `atlascloud/*` slug to the default `OpenRouterProvider` would (wrongly)
//! request `usage:{include:true}` — a directive AtlasCloud doesn't understand —
//! so AtlasCloud needs its own normalisation profile, mirroring the Together
//! precedent (#2494) and the shared capability registry's AtlasCloud seed
//! (`detailed_usage_accounting = false`, no explicit `cache_control`
//! breakpoints, native OpenAI-style function calling).
//! What: [`AtlasCloudProvider`] implements [`Provider`] with the OpenAI-dialect
//! `tool_choice` mapping (identical to OpenRouter/Fireworks/Together), the
//! standard usage extraction, native tool support, NO explicit prompt-cache
//! breakpoints, and NO detailed-usage request. The transport
//! (`crate::llm::client::OpenAiCompatClient`) is what actually reaches
//! `api.atlascloud.ai`; this type only shapes the request.
//! Test: `atlascloud::tests::*`.

use serde_json::{Value, json};

use super::traits::{Provider, ToolChoice};
use crate::llm::ChatResponse;
use crate::perf::TokenUsage;

/// Provider for AtlasCloud-hosted models (`atlascloud/*` slugs), served behind the
/// OpenAI-compatible `/chat/completions` schema.
///
/// Why: gives `build_request` an AtlasCloud-correct normalisation profile so the
/// wire payload matches what AtlasCloud accepts (no OpenRouter-only directives,
/// no `cache_control` breakpoints).
/// What: zero-sized marker type implementing [`Provider`].
/// Test: `atlascloud::tests::*`.
#[derive(Debug, Clone, Default)]
pub struct AtlasCloudProvider;

impl AtlasCloudProvider {
    /// Construct an `AtlasCloudProvider`.
    ///
    /// Why: gives the factory a uniform constructor call site.
    /// What: returns the zero-sized marker.
    /// Test: `atlascloud::tests::name_is_atlascloud`.
    pub fn new() -> Self {
        Self
    }
}

impl Provider for AtlasCloudProvider {
    fn name(&self) -> &str {
        "atlascloud"
    }

    /// Translate a neutral [`ToolChoice`] into OpenAI-dialect wire JSON.
    ///
    /// Why: AtlasCloud uses the OpenAI `tool_choice` spelling (string policies +
    /// `{type:function, function:{name}}` object), identical to OpenRouter,
    /// Fireworks, and Together.
    /// What: the OpenAI mapping.
    /// Test: `atlascloud::tests::map_tool_choice_scalars`.
    fn map_tool_choice(&self, choice: ToolChoice) -> Value {
        match choice {
            ToolChoice::None => json!("none"),
            ToolChoice::Auto => json!("auto"),
            ToolChoice::Required => json!("required"),
            ToolChoice::Function(name) => json!({
                "type": "function",
                "function": { "name": name },
            }),
        }
    }

    /// Extract canonical token usage from an AtlasCloud response.
    ///
    /// Why: AtlasCloud returns the standard OpenAI `usage` block, already
    /// normalised into `ChatResponse.usage` by the shared adapter's parse.
    /// What: delegates to `ChatResponse::token_usage` (identical to the other
    /// OpenAI-dialect providers).
    /// Test: `atlascloud::tests::extract_usage_maps_fields`.
    fn extract_usage(&self, response: &ChatResponse) -> TokenUsage {
        response.clone().token_usage()
    }

    fn supports_native_tools(&self) -> bool {
        // AtlasCloud's tool-capable models accept the native OpenAI `tools`
        // array; mirrors the shared registry's AtlasCloud `native_tool_calling =
        // true`.
        true
    }

    fn supports_prompt_caching(&self) -> bool {
        // AtlasCloud caching is provider-side/implicit: the caller cannot place
        // Anthropic-style `cache_control` breakpoints, so tcode must never mark
        // an AtlasCloud request with one (matching the Together precedent — the
        // wire body carries no `cache_control` markers).
        false
    }

    fn wants_detailed_usage(&self) -> bool {
        // `usage:{include:true}` is an OpenRouter-only directive; AtlasCloud would
        // not understand it. Mirrors the shared registry's AtlasCloud
        // `detailed_usage_accounting = false`.
        false
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// `name()` returns `"atlascloud"`.
    ///
    /// Why: routing and telemetry distinguish AtlasCloud by name; the transport's
    /// `provider_for_slug` and `dispatch`'s bedrock check both key off it.
    /// What: assert the name.
    /// Test: this test.
    #[test]
    fn name_is_atlascloud() {
        assert_eq!(AtlasCloudProvider::new().name(), "atlascloud");
    }

    /// AtlasCloud reports native-tool support.
    ///
    /// Why: `build_request` sends the native `tools` array for tool-capable
    /// AtlasCloud models.
    /// What: assert `supports_native_tools()` is `true`.
    /// Test: this test.
    #[test]
    fn atlascloud_supports_native_tools() {
        assert!(AtlasCloudProvider::new().supports_native_tools());
    }

    /// AtlasCloud does NOT request OpenRouter's detailed usage accounting.
    ///
    /// Why: this is the exact gate that would otherwise attach the
    /// OpenRouter-only `usage:{include:true}` directive to an AtlasCloud request —
    /// the core reason an AtlasCloud-specific provider is needed.
    /// What: assert `wants_detailed_usage()` is `false`.
    /// Test: this test.
    #[test]
    fn atlascloud_does_not_want_detailed_usage() {
        assert!(!AtlasCloudProvider::new().wants_detailed_usage());
    }

    /// AtlasCloud does NOT place explicit prompt-cache breakpoints.
    ///
    /// Why: `build_request` must never mark an AtlasCloud request with an
    /// Anthropic-style `cache_control` breakpoint — AtlasCloud caching is
    /// provider-side and rejects caller-placed markers.
    /// What: assert `supports_prompt_caching()` is `false`.
    /// Test: this test.
    #[test]
    fn atlascloud_does_not_support_prompt_caching() {
        assert!(!AtlasCloudProvider::new().supports_prompt_caching());
    }

    /// `map_tool_choice` maps the scalar policies to the OpenAI wire strings.
    ///
    /// Why: AtlasCloud expects the OpenAI spelling; a wrong shape breaks
    /// tool routing.
    /// What: map each scalar variant, assert the JSON value.
    /// Test: this test.
    #[test]
    fn map_tool_choice_scalars() {
        let p = AtlasCloudProvider::new();
        assert_eq!(p.map_tool_choice(ToolChoice::None), json!("none"));
        assert_eq!(p.map_tool_choice(ToolChoice::Auto), json!("auto"));
        assert_eq!(p.map_tool_choice(ToolChoice::Required), json!("required"));
        assert_eq!(
            p.map_tool_choice(ToolChoice::Function("search".into())),
            json!({"type": "function", "function": {"name": "search"}})
        );
    }

    /// `extract_usage` maps a normalised usage block into `TokenUsage`.
    ///
    /// Why: pins that AtlasCloud performs no extra (inconsistent) usage transform.
    /// What: deserialise a response fixture, extract usage, assert fields.
    /// Test: this test.
    #[test]
    fn extract_usage_maps_fields() {
        let fixture = r#"{
          "id": "ac-1",
          "choices": [{"message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}],
          "usage": {"prompt_tokens": 40, "completion_tokens": 8, "total_tokens": 48}
        }"#;
        let resp: ChatResponse = serde_json::from_str(fixture).expect("deserialise");
        let usage = AtlasCloudProvider::new().extract_usage(&resp);
        assert_eq!(usage.prompt_tokens, 40);
        assert_eq!(usage.completion_tokens, 8);
    }
}
