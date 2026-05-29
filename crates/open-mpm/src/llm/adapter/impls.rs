//! Concrete `ModelAdapter` implementations, one per provider family.
//!
//! Why: The trait definition, endpoint types, and factory live in `mod.rs`;
//! the per-provider behavior (tool-choice shapes, cache-control injection,
//! usage parsing, endpoint routing) is voluminous enough to warrant its own
//! file so the trait contract stays easy to read.
//! What: `impl ModelAdapter for {Anthropic,OpenAi,Bedrock,Ollama,Generic}Adapter`.
//! Test: Behavior is covered by the parent module's `tests`.

use serde_json::{Value, json};

use super::{
    AnthropicAdapter, ApiEndpoint, AuthSource, BedrockAdapter, GenericAdapter, ModelAdapter,
    OllamaAdapter, OpenAiAdapter, Provider, openrouter_endpoint,
};
use crate::perf::TokenUsage;

impl ModelAdapter for AnthropicAdapter {
    fn provider(&self) -> Provider {
        Provider::Anthropic
    }

    fn tool_choice_any(&self) -> Option<Value> {
        Some(json!({"type": "any"}))
    }

    fn tool_choice_auto(&self) -> Option<Value> {
        Some(json!("auto"))
    }

    fn inject_cache_control(&self, request_body: &mut Value, enabled: bool) {
        if !enabled {
            return;
        }

        // 1. Top-level `system` field (Anthropic-native shape). Some callers
        //    send the system prompt here directly.
        if let Some(system) = request_body.get_mut("system") {
            match system {
                Value::String(s) => {
                    let text = s.clone();
                    *system = json!([{
                        "type": "text",
                        "text": text,
                        "cache_control": {"type": "ephemeral"}
                    }]);
                }
                Value::Array(parts) => {
                    if let Some(last) = parts.last_mut()
                        && let Some(obj) = last.as_object_mut()
                    {
                        obj.insert("cache_control".to_string(), json!({"type": "ephemeral"}));
                    }
                }
                _ => {}
            }
            return;
        }

        // 2. OpenAI-style messages array — patch the first system message.
        if let Some(msgs) = request_body
            .get_mut("messages")
            .and_then(|v| v.as_array_mut())
        {
            for m in msgs.iter_mut() {
                if m.get("role").and_then(|v| v.as_str()) != Some("system") {
                    continue;
                }
                let text_val = match m.get("content").cloned() {
                    Some(Value::String(s)) => s,
                    Some(Value::Array(_)) => {
                        if let Some(arr) = m.get_mut("content").and_then(|v| v.as_array_mut())
                            && let Some(first) = arr.first_mut()
                            && let Some(obj) = first.as_object_mut()
                        {
                            obj.insert("cache_control".to_string(), json!({"type": "ephemeral"}));
                        }
                        return;
                    }
                    _ => return,
                };
                m["content"] = json!([{
                    "type": "text",
                    "text": text_val,
                    "cache_control": {"type": "ephemeral"}
                }]);
                return;
            }
        }
    }

    fn parse_usage(&self, response: &Value) -> TokenUsage {
        let usage = &response["usage"];
        // Anthropic may respond with either input_tokens/output_tokens OR the
        // OpenAI-compatible prompt_tokens/completion_tokens when routed via
        // OpenRouter — try both.
        let prompt = usage["input_tokens"]
            .as_u64()
            .or_else(|| usage["prompt_tokens"].as_u64())
            .unwrap_or(0) as u32;
        let completion = usage["output_tokens"]
            .as_u64()
            .or_else(|| usage["completion_tokens"].as_u64())
            .unwrap_or(0) as u32;
        let cache_read = usage["cache_read_input_tokens"]
            .as_u64()
            .or_else(|| {
                usage["prompt_tokens_details"]
                    .get("cached_tokens")
                    .and_then(|v| v.as_u64())
            })
            .unwrap_or(0) as u32;
        let cache_creation = usage["cache_creation_input_tokens"].as_u64().unwrap_or(0) as u32;
        TokenUsage::new(prompt, completion, cache_read, cache_creation)
    }

    fn supports_thinking(&self) -> bool {
        true
    }

    fn api_endpoint(&self, use_direct: bool) -> ApiEndpoint {
        // #62: Direct API mode requires ANTHROPIC_API_KEY from console.anthropic.com.
        // CLAUDE_CODE_OAUTH_TOKEN (sk-ant-oat01-*) is only valid for
        // runner="claude-code" agents (ClaudeCodeAgentRunner subprocess path).
        // Anthropic's REST API rejects OAuth tokens with 401 — do NOT attempt
        // direct API calls with them.
        //
        // Priority 1: ANTHROPIC_API_KEY → api.anthropic.com with x-api-key header.
        // Fallback: OpenRouter — preserves existing deployments unchanged.
        if use_direct
            && let Ok(key) = std::env::var("ANTHROPIC_API_KEY")
            && !key.is_empty()
        {
            return ApiEndpoint {
                base_url: std::env::var("ANTHROPIC_BASE_URL")
                    .unwrap_or_else(|_| "https://api.anthropic.com/v1".to_string()),
                auth_header_name: "x-api-key".to_string(),
                auth_header_value: key,
                extra_headers: vec![("anthropic-version".to_string(), "2023-06-01".to_string())],
                auth_source: AuthSource::AnthropicApiKey,
            };
        }
        openrouter_endpoint()
    }

    fn uses_native_format(&self) -> bool {
        true
    }
}

impl ModelAdapter for OpenAiAdapter {
    fn provider(&self) -> Provider {
        Provider::OpenAI
    }
    fn tool_choice_any(&self) -> Option<Value> {
        Some(json!("required"))
    }
    fn tool_choice_auto(&self) -> Option<Value> {
        Some(json!("auto"))
    }
    fn inject_cache_control(&self, _: &mut Value, _: bool) {
        // OpenAI has no equivalent; silently no-op.
    }
    fn parse_usage(&self, response: &Value) -> TokenUsage {
        let usage = &response["usage"];
        let prompt = usage["prompt_tokens"].as_u64().unwrap_or(0) as u32;
        let completion = usage["completion_tokens"].as_u64().unwrap_or(0) as u32;
        let cache_read = usage["prompt_tokens_details"]
            .get("cached_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        TokenUsage::new(prompt, completion, cache_read, 0)
    }
}

impl ModelAdapter for BedrockAdapter {
    fn provider(&self) -> Provider {
        Provider::Bedrock
    }

    fn tool_choice_any(&self) -> Option<Value> {
        // Bedrock Converse expresses tool choice via a `ToolChoice` struct on
        // the request, not a JSON `tool_choice` field on the message body.
        // The Bedrock client constructs that struct from the agent's
        // `ToolChoice` enum directly, so we return None here.
        None
    }

    fn tool_choice_auto(&self) -> Option<Value> {
        None
    }

    fn inject_cache_control(&self, _: &mut Value, _: bool) {
        // Bedrock has its own `CachePoint` block type; unused for now.
    }

    fn parse_usage(&self, response: &Value) -> TokenUsage {
        // Used only when Bedrock results are normalized through a JSON
        // response (currently never — the SDK returns typed values which
        // are translated in `crate::llm::bedrock`). Keep a defensive impl.
        let usage = &response["usage"];
        let prompt = usage["inputTokens"]
            .as_u64()
            .or_else(|| usage["prompt_tokens"].as_u64())
            .unwrap_or(0) as u32;
        let completion = usage["outputTokens"]
            .as_u64()
            .or_else(|| usage["completion_tokens"].as_u64())
            .unwrap_or(0) as u32;
        TokenUsage::new(prompt, completion, 0, 0)
    }

    fn api_endpoint(&self, _use_direct: bool) -> ApiEndpoint {
        // Bedrock uses SigV4 + the SDK; no HTTP base URL/auth-header
        // mechanism applies. Return a sentinel whose `auth_source` flags
        // the chat loop to take the SDK path.
        ApiEndpoint {
            base_url: "https://bedrock-runtime.amazonaws.com".to_string(),
            auth_header_name: String::new(),
            auth_header_value: String::new(),
            extra_headers: vec![],
            auth_source: AuthSource::Bedrock,
        }
    }
}

impl ModelAdapter for OllamaAdapter {
    fn provider(&self) -> Provider {
        Provider::Generic
    }
    fn tool_choice_any(&self) -> Option<Value> {
        // ollama's OpenAI-compat layer accepts "auto"/"required" but not all
        // models honor it. Return None to keep the request payload minimal.
        None
    }
    fn tool_choice_auto(&self) -> Option<Value> {
        None
    }
    fn inject_cache_control(&self, _: &mut Value, _: bool) {
        // ollama has no cache_control concept.
    }
    fn parse_usage(&self, response: &Value) -> TokenUsage {
        let usage = &response["usage"];
        let prompt = usage["prompt_tokens"]
            .as_u64()
            .or_else(|| usage["input_tokens"].as_u64())
            .unwrap_or(0) as u32;
        let completion = usage["completion_tokens"]
            .as_u64()
            .or_else(|| usage["output_tokens"].as_u64())
            .unwrap_or(0) as u32;
        TokenUsage::new(prompt, completion, 0, 0)
    }
    fn api_endpoint(&self, _use_direct: bool) -> ApiEndpoint {
        let host =
            std::env::var("OLLAMA_HOST").unwrap_or_else(|_| "http://localhost:11434".to_string());
        ApiEndpoint {
            base_url: format!("{}/v1", host.trim_end_matches('/')),
            // ollama needs no auth — leave the header empty so the LLM HTTP
            // layer skips Authorization (or sends an empty header which ollama
            // ignores).
            auth_header_name: "Authorization".to_string(),
            auth_header_value: String::new(),
            extra_headers: vec![],
            // Tag with OpenRouter so the existing routing/credential paths
            // treat it as a generic OpenAI-compatible endpoint.
            auth_source: AuthSource::OpenRouter,
        }
    }
}

impl ModelAdapter for GenericAdapter {
    fn provider(&self) -> Provider {
        Provider::Generic
    }
    fn tool_choice_any(&self) -> Option<Value> {
        None
    }
    fn tool_choice_auto(&self) -> Option<Value> {
        None
    }
    fn inject_cache_control(&self, _: &mut Value, _: bool) {}
    fn parse_usage(&self, response: &Value) -> TokenUsage {
        let usage = &response["usage"];
        let prompt = usage["prompt_tokens"]
            .as_u64()
            .or_else(|| usage["input_tokens"].as_u64())
            .unwrap_or(0) as u32;
        let completion = usage["completion_tokens"]
            .as_u64()
            .or_else(|| usage["output_tokens"].as_u64())
            .unwrap_or(0) as u32;
        TokenUsage::new(prompt, completion, 0, 0)
    }
}
