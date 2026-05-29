//! Model-provider adapter.
//!
//! Why: Different LLM providers reached via OpenRouter (Anthropic, OpenAI,
//! Google, ...) disagree on the exact wire shape of several fields that the
//! agent harness cares about:
//!   - `tool_choice`: Anthropic expects `{"type":"any"}` to force a tool call
//!     while OpenAI expects the string `"required"`.
//!   - `cache_control`: only Anthropic understands the
//!     `{"type":"ephemeral"}` prompt-caching field.
//!   - `usage`: Anthropic emits `input_tokens`/`output_tokens` plus
//!     `cache_read_input_tokens`/`cache_creation_input_tokens`; OpenAI emits
//!     `prompt_tokens`/`completion_tokens` with no cache fields.
//! Centralizing these differences behind a single `ModelAdapter` trait keeps
//! scattered `if model_is_anthropic(...)` branches out of the chat loop (#57).
//! What: Defines the `ModelAdapter` trait, the endpoint/auth types, the adapter
//! structs, and the factory `adapter_for_model(model_str)` that picks one
//! based on the model string. The concrete `impl` blocks live in `impls`.
//! Test: See `tests` submodule.

mod impls;
#[cfg(test)]
mod tests;

use serde_json::Value;

use crate::perf::TokenUsage;

/// Which auth mechanism was used to resolve an `ApiEndpoint`.
///
/// Why: Startup logging should tell the operator exactly which credential
/// is active for each agent so auth issues are diagnosed without reading
/// env vars manually.
/// What: Three variants — OAuth token (reserved for ClaudeCodeAgentRunner only),
/// pay-as-you-go Anthropic API key, and the OpenRouter fallback.
/// Note: `ClaudeMaxOAuth` is NOT used by `AnthropicAdapter::api_endpoint()`.
/// `CLAUDE_CODE_OAUTH_TOKEN` (sk-ant-oat01-*) tokens are rejected by
/// api.anthropic.com with 401. They are only valid for runner="claude-code"
/// agents via ClaudeCodeAgentRunner. For direct API mode, use ANTHROPIC_API_KEY
/// from console.anthropic.com.
/// Test: `oauth_token_is_not_used_for_direct_api_routing`,
/// `api_key_used_when_no_oauth_token`,
/// `falls_back_to_openrouter_when_use_direct_false`.
#[derive(Debug, Clone, PartialEq)]
pub enum AuthSource {
    /// OAuth Bearer token — reserved for ClaudeCodeAgentRunner subprocess path only.
    /// NOT used by `AnthropicAdapter::api_endpoint()` — api.anthropic.com rejects
    /// sk-ant-oat01-* tokens with 401.
    #[allow(dead_code)]
    ClaudeMaxOAuth,
    /// `ANTHROPIC_API_KEY` — pay-as-you-go direct API access via x-api-key header.
    AnthropicApiKey,
    /// OpenRouter (`OPENROUTER_API_KEY`) — the default fallback routing path.
    OpenRouter,
    /// AWS SigV4 — credentials resolved via the AWS SDK default chain
    /// (env vars, `~/.aws/credentials`, instance metadata, etc.). Used
    /// exclusively by `BedrockAdapter`.
    Bedrock,
}

impl std::fmt::Display for AuthSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ClaudeMaxOAuth => write!(f, "claude-max-oauth"),
            Self::AnthropicApiKey => write!(f, "anthropic-api-key"),
            Self::OpenRouter => write!(f, "openrouter"),
            Self::Bedrock => write!(f, "aws-bedrock"),
        }
    }
}

/// Endpoint routing information for a chat completion request.
///
/// Why: Different providers (and different auth modes within the same provider)
/// require different base URLs, auth headers, and extra headers (e.g.
/// `anthropic-version`). Centralizing this in a struct returned from
/// `ModelAdapter::api_endpoint` keeps the routing logic out of the chat loop.
/// What: Base URL, auth header name + value, extra headers, and the
/// `AuthSource` tag used for startup logging.
/// Test: `anthropic_api_endpoint_direct_when_key_set`,
/// `anthropic_api_endpoint_falls_back_to_openrouter`.
#[derive(Debug, Clone)]
pub struct ApiEndpoint {
    /// Base URL without trailing slash (e.g. `https://api.anthropic.com/v1`).
    pub base_url: String,
    /// Header name for auth — `Authorization` (Bearer) or `x-api-key`.
    pub auth_header_name: String,
    /// Header value (already includes `Bearer ` prefix when needed).
    pub auth_header_value: String,
    /// Additional headers required by the provider (e.g. `anthropic-version`).
    pub extra_headers: Vec<(String, String)>,
    /// Which credential resolved this endpoint — used for startup logging.
    pub auth_source: AuthSource,
}

/// Which provider family a `ModelAdapter` represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Provider {
    Anthropic,
    OpenAI,
    Generic,
    /// AWS Bedrock Converse API — uses the `aws-sdk-bedrockruntime` SDK
    /// instead of an OpenAI-compatible HTTP request. Activated when the
    /// model string starts with `bedrock/`.
    Bedrock,
}

/// Centralizes all model-provider-specific behavior for the chat loop.
///
/// Why: One implementation per provider family, selected at agent load time
/// from the model string. See module docs for the concrete differences
/// being abstracted.
/// What: Five methods — provider tag, the two `tool_choice` shapes
/// (forced / auto), cache-control injection on a raw request body, and
/// usage parsing from a raw response body.
/// Test: See the `tests` submodule (per-adapter unit tests).
pub trait ModelAdapter: Send + Sync + std::fmt::Debug {
    /// Identity tag for the adapter (used mostly in logs/tests).
    fn provider(&self) -> Provider;

    /// The `tool_choice` value that forces the model to call SOME tool.
    /// `None` means omit the field — some providers do not support it.
    fn tool_choice_any(&self) -> Option<Value>;

    /// The `tool_choice` value for normal ("auto") mode.
    fn tool_choice_auto(&self) -> Option<Value>;

    /// Inject Anthropic `cache_control` onto the system message of a raw
    /// request body. No-op for non-Anthropic providers.
    fn inject_cache_control(&self, request_body: &mut Value, enabled: bool);

    /// Extract token usage (including provider-specific cache fields) from
    /// a raw response JSON body.
    fn parse_usage(&self, response: &Value) -> TokenUsage;

    /// Whether this provider supports extended thinking / reasoning tokens.
    #[allow(dead_code)]
    fn supports_thinking(&self) -> bool {
        false
    }

    /// Returns the API endpoint (base URL + auth) for this provider.
    ///
    /// Why: #59 — when `use_direct=true` and the relevant direct-API key
    /// (e.g. `ANTHROPIC_API_KEY`) is set, we bypass OpenRouter and call the
    /// provider's native endpoint for lower latency/cost. For every other
    /// case (including unknown providers) this returns the OpenRouter
    /// endpoint.
    /// What: Returns `ApiEndpoint` describing `{base_url, auth headers,
    /// extra headers}`. Default impl returns OpenRouter; Anthropic overrides
    /// to honor `use_direct`.
    /// Test: `anthropic_api_endpoint_direct_when_key_set`,
    /// `anthropic_api_endpoint_falls_back_to_openrouter`,
    /// `openai_api_endpoint_always_openrouter`.
    fn api_endpoint(&self, _use_direct: bool) -> ApiEndpoint {
        openrouter_endpoint()
    }

    /// Whether this adapter's native API wire format differs from the
    /// OpenAI-compatible shape (OpenRouter's default).
    ///
    /// Why: #59 — when we route to `api.anthropic.com` directly we must emit
    /// Anthropic's native `/v1/messages` request shape (top-level `system`,
    /// `input_schema` for tools, content-block `tool_result`s, etc.) instead
    /// of OpenAI's `/v1/chat/completions`. Generic/OpenAI adapters never use
    /// a native format.
    /// What: Returns `true` only for Anthropic.
    /// Test: `anthropic_uses_native_format_true`, `openai_uses_native_format_false`.
    fn uses_native_format(&self) -> bool {
        false
    }
}

/// The default OpenRouter endpoint used by every non-direct path.
///
/// Why: Keeps the single source of truth for base URL + env-var name in one
/// place so tests and adapters agree on the fallback shape.
/// What: Reads `OPENROUTER_BASE_URL` (override for tests) and
/// `OPENROUTER_API_KEY`; builds `Authorization: Bearer <key>`.
/// Test: Indirectly via `anthropic_api_endpoint_falls_back_to_openrouter`.
pub fn openrouter_endpoint() -> ApiEndpoint {
    let base_url = std::env::var("OPENROUTER_BASE_URL")
        .unwrap_or_else(|_| "https://openrouter.ai/api/v1".to_string());
    let key = std::env::var("OPENROUTER_API_KEY").unwrap_or_default();
    ApiEndpoint {
        base_url,
        auth_header_name: "Authorization".to_string(),
        auth_header_value: format!("Bearer {key}"),
        extra_headers: vec![],
        auth_source: AuthSource::OpenRouter,
    }
}

/// Pick an adapter for the given model string.
///
/// Why: Deciding provider family from the model name (OpenRouter-style
/// `vendor/model` or loose `claude-...`/`gpt-...`) keeps callers from
/// knowing the concrete adapter types.
/// What: Heuristic substring match — Anthropic first (most common), then
/// OpenAI / o-series, else `GenericAdapter` as a safe default.
/// Test: `adapter_for_model_routes_anthropic`, `adapter_for_model_routes_openai`.
pub fn adapter_for_model(model: &str) -> Box<dyn ModelAdapter> {
    // Bedrock prefix takes precedence — the model id after `bedrock/` may
    // contain `anthropic` (e.g. `bedrock/anthropic.claude-3-5-haiku-...`),
    // and we must route those to the Bedrock SDK, not the Anthropic adapter.
    if model.starts_with("bedrock/") {
        let id = model.strip_prefix("bedrock/").unwrap_or(model).to_string();
        return Box::new(BedrockAdapter { model_id: id });
    }
    // Ollama prefix routes to a locally-running ollama server. The adapter
    // is OpenAI-compatible (ollama exposes `/v1/chat/completions` natively),
    // so the wire format matches GenericAdapter; only the base URL differs.
    if model.starts_with("ollama/") {
        let id = model.strip_prefix("ollama/").unwrap_or(model).to_string();
        return Box::new(OllamaAdapter { model_id: id });
    }
    let m = model.to_ascii_lowercase();
    if m.contains("claude") || m.contains("anthropic") {
        Box::new(AnthropicAdapter)
    } else if m.contains("gpt") || m.contains("openai") || m.contains("o1") || m.contains("o3") {
        Box::new(OpenAiAdapter)
    } else {
        Box::new(GenericAdapter)
    }
}

/// Anthropic-family adapter (Claude models).
#[derive(Debug)]
pub struct AnthropicAdapter;

/// OpenAI-family adapter (GPT, o1, o3, ...).
#[derive(Debug)]
pub struct OpenAiAdapter;

/// Provider-agnostic fallback adapter.
#[derive(Debug)]
pub struct GenericAdapter;

/// Local ollama adapter (OpenAI-compatible endpoint).
///
/// Why: `/provider local` lets the user route LLM calls to a locally-running
/// ollama instance with no auth. ollama exposes an OpenAI-compatible
/// `/v1/chat/completions` endpoint, so the wire format matches GenericAdapter;
/// only the base URL changes.
/// What: Activated by `adapter_for_model("ollama/<name>")`. The model id after
/// the prefix is forwarded as `model` in the request body. `OLLAMA_HOST` env
/// var overrides the default `http://localhost:11434`.
/// Test: `adapter_for_model_routes_ollama`, `ollama_api_endpoint_uses_host`.
#[derive(Debug)]
pub struct OllamaAdapter {
    pub model_id: String,
}

/// AWS Bedrock Converse API adapter.
///
/// Why: Bedrock is not OpenAI-compatible — it speaks SigV4 auth and a native
/// `Converse` request/response shape. We carry the `model_id` (everything
/// after the `bedrock/` prefix, e.g. `anthropic.claude-3-5-haiku-20241022-v1:0`)
/// so the Bedrock client can pass it through unchanged.
/// What: Activated by `adapter_for_model("bedrock/...")`. The chat loop
/// detects `Provider::Bedrock` and routes through `crate::llm::bedrock`
/// instead of the OpenRouter/Anthropic-native paths.
/// Test: `adapter_for_model_routes_bedrock`, `bedrock_adapter_strips_prefix`.
#[derive(Debug)]
pub struct BedrockAdapter {
    /// The raw Bedrock model id (everything after `bedrock/`).
    pub model_id: String,
}
