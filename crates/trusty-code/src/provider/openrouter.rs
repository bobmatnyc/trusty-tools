//! OpenRouter provider — the default backend for trusty-code.
//!
//! Why: OpenRouter fronts dozens of model families (Qwen, DeepSeek, Gemma,
//! OpenAI, Anthropic, Gemini, …) behind one OpenAI-compatible chat-completions
//! schema. It is the right default for every slug that is not explicitly routed
//! elsewhere (#1021).
//! What: [`OpenRouterProvider`] implements [`Provider`] using OpenAI-style
//! `tool_choice` JSON and the standard `usage` block. `supports_native_tools`
//! is conservative: it returns `true` only for well-known native-capable model
//! families and `false` otherwise (#1023 refines this per the strategy matrix).
//! Test: `openrouter::tests::*`.

use serde_json::{Value, json};

use super::traits::{Provider, ToolChoice};
use crate::llm::ChatResponse;
use crate::perf::TokenUsage;

/// The default OpenRouter-backed provider.
///
/// Why: Carries the model slug so `supports_native_tools` can decide per-model
/// without the loop having to pass the slug on every call.
/// What: Holds the model slug it was constructed for; the trait methods read it.
/// Test: `openrouter::tests::*`.
#[derive(Debug, Clone)]
pub struct OpenRouterProvider {
    /// The OpenRouter model slug this provider instance was built for.
    slug: String,
}

impl OpenRouterProvider {
    /// Construct an `OpenRouterProvider` for a given model slug.
    ///
    /// Why: `supports_native_tools` is slug-dependent, so the provider must
    /// remember which model it routes for.
    /// What: Stores the slug; all trait behaviour derives from it.
    /// Test: `openrouter::tests::supports_native_tools_claude`.
    pub fn new(slug: impl Into<String>) -> Self {
        Self { slug: slug.into() }
    }
}

/// Whether a model slug belongs to a family with native function-calling.
///
/// Why: Sending a `tools` array to a model that ignores it wastes tokens and
/// produces malformed output; the loop needs a conservative yes/no per slug.
/// What: Returns `true` when the slug names a known native-capable family
/// (Claude, OpenAI GPT, Gemini); `false` otherwise. Matching is
/// case-insensitive and substring-based so both bare slugs (`gpt-4o-mini`) and
/// OpenRouter-namespaced slugs (`openai/gpt-4o-mini`) are recognised.
/// Test: `openrouter::tests::supports_native_tools_*`.
fn slug_supports_native_tools(slug: &str) -> bool {
    let lower = slug.to_ascii_lowercase();
    // Well-known native-tool-capable families. Conservative by design (#1023).
    const NATIVE_MARKERS: [&str; 3] = ["claude-", "gpt-", "gemini-"];
    NATIVE_MARKERS.iter().any(|m| lower.contains(m))
}

impl Provider for OpenRouterProvider {
    fn name(&self) -> &str {
        "openrouter"
    }

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

    fn extract_usage(&self, response: &ChatResponse) -> TokenUsage {
        response.clone().token_usage()
    }

    fn supports_native_tools(&self) -> bool {
        slug_supports_native_tools(&self.slug)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// `name()` returns the stable `"openrouter"` identifier.
    ///
    /// Why: Telemetry keys off this string; a change would break log filters.
    /// What: Construct a provider, assert `name() == "openrouter"`.
    /// Test: this test.
    #[test]
    fn name_is_stable() {
        let p = OpenRouterProvider::new("openai/gpt-4o-mini");
        assert_eq!(p.name(), "openrouter");
    }

    /// `map_tool_choice` maps the three scalar policies to OpenAI strings.
    ///
    /// Why: The wire format requires exact `"none"`/`"auto"`/`"required"`
    /// strings; a typo would be silently rejected by the API.
    /// What: Map each scalar variant, assert the JSON string value.
    /// Test: this test.
    #[test]
    fn map_tool_choice_scalars() {
        let p = OpenRouterProvider::new("openai/gpt-4o-mini");
        assert_eq!(p.map_tool_choice(ToolChoice::None), json!("none"));
        assert_eq!(p.map_tool_choice(ToolChoice::Auto), json!("auto"));
        assert_eq!(p.map_tool_choice(ToolChoice::Required), json!("required"));
    }

    /// `map_tool_choice(Function)` produces the OpenAI function-selector object.
    ///
    /// Why: Forcing a specific function requires the nested
    /// `{type:function, function:{name}}` shape, not a bare string.
    /// What: Map `Function("search")`, assert the object structure.
    /// Test: this test.
    #[test]
    fn map_tool_choice_function() {
        let p = OpenRouterProvider::new("openai/gpt-4o-mini");
        let v = p.map_tool_choice(ToolChoice::Function("search_code".into()));
        assert_eq!(v["type"], "function");
        assert_eq!(v["function"]["name"], "search_code");
    }

    /// `extract_usage` maps the standard usage block into `TokenUsage`.
    ///
    /// Why: Cost accounting depends on correct field mapping; verify the
    /// provider delegates to the canonical conversion.
    /// What: Deserialise a response fixture with a usage block, extract usage,
    /// assert each field.
    /// Test: this test.
    #[test]
    fn extract_usage_maps_fields() {
        let fixture = r#"{
          "id": "gen-1",
          "choices": [{"message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}],
          "usage": {"prompt_tokens": 12, "completion_tokens": 5, "total_tokens": 17,
                    "cache_read_input_tokens": 3, "cache_creation_input_tokens": 1}
        }"#;
        let resp: ChatResponse = serde_json::from_str(fixture).expect("deserialise");
        let p = OpenRouterProvider::new("anthropic/claude-sonnet-4-5");
        let usage = p.extract_usage(&resp);
        assert_eq!(usage.prompt_tokens, 12);
        assert_eq!(usage.completion_tokens, 5);
        assert_eq!(usage.cache_read_tokens, 3);
        assert_eq!(usage.cache_creation_tokens, 1);
    }

    /// Claude / GPT / Gemini slugs report native-tool support.
    ///
    /// Why: These families support native function-calling; the loop should send
    /// the `tools` array directly.
    /// What: Construct providers for representative native slugs, assert `true`.
    /// Test: this test.
    #[test]
    fn supports_native_tools_native_families() {
        for slug in [
            "anthropic/claude-sonnet-4-5",
            "openai/gpt-4o-mini",
            "google/gemini-2.5-pro",
        ] {
            assert!(
                OpenRouterProvider::new(slug).supports_native_tools(),
                "expected native-tool support for {slug}"
            );
        }
    }

    /// Qwen / DeepSeek / Gemma slugs do NOT report native-tool support.
    ///
    /// Why: These are conservatively treated as non-native until #1023 refines
    /// the matrix; the loop must use prompt-based fallback guidance for them.
    /// What: Construct providers for these slugs, assert `false`.
    /// Test: this test.
    #[test]
    fn supports_native_tools_non_native_families() {
        for slug in [
            "qwen/qwen-2.5-coder-32b-instruct",
            "deepseek/deepseek-chat",
            "google/gemma-2-27b-it",
        ] {
            assert!(
                !OpenRouterProvider::new(slug).supports_native_tools(),
                "expected NO native-tool support for {slug}"
            );
        }
    }
}
