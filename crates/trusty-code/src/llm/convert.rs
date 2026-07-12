//! Lossless conversions between trusty-code's LLM wire types and the shared
//! `trusty_common::inference` types, plus error mapping (#2406, epic #2400).
//!
//! Why: #2406 migrates tcode's OpenRouter/OpenAI-compatible transport onto the
//! shared `trusty_common::inference` adapter while keeping every existing call
//! site — `LlmClientTrait`, `AgentLoop`, the `ScriptedLlm` test doubles, the
//! recorder — depending on tcode's own `ChatRequest`/`ChatResponse`/`LlmError`
//! (which are deeply integrated with `crate::perf::TokenUsage`, the Bedrock
//! transport, and the prompt-cache request shaping). Rather than ripple those
//! types through the whole crate, the shared adapter is bridged at exactly one
//! seam: this module. The two type families are structurally identical (both
//! are the OpenAI `/chat/completions` schema, incl. the #2156 `cache_control`
//! content-block wire shape), so the mapping is a field-by-field copy — NOT a
//! serde round-trip, because the request-side `cache_control` marker is a
//! `#[serde(skip)]` struct field that only becomes a content block at
//! serialisation time; a `to_value`/`from_value` round-trip would silently drop
//! it and disable prompt caching (a #2406 merge-gate concern).
//! What: `to_shared_request` (with an explicit wire model so the caller can
//! strip a provider routing prefix), `from_shared_response`, and `map_error`.
//! Test: `convert::tests::*` — request/response field round-trips, the
//! cache-control carry-through, and the error-variant mapping.

use trusty_common::inference as tci;

use super::error::LlmError;
use super::message::ChatMessage;
use super::request::{
    CacheControl, ChatRequest, FunctionCall, RequestUsageConfig, ToolCall, ToolDefinition,
};
use super::response::{AssistantMessage, ChatChoice, ChatResponse};
use super::usage::{PromptTokensDetails, UsageBlock};

/// Convert a tcode [`ChatRequest`] into the shared [`tci::ChatRequest`], using
/// `wire_model` as the outbound `model` field.
///
/// Why: the shared adapters send the `ChatRequest.model` verbatim on the wire,
/// but tcode routes on a prefixed slug (e.g. `fireworks/…`) that must be
/// stripped to the provider-native id before it is sent — so the wire model is
/// a parameter here, decoupled from the routing decision the transport already
/// made.
/// What: copies every field across; `messages`/`tools` map element-wise
/// (carrying the `cache_control` breakpoint), `stop` has no tcode equivalent so
/// it is `None`, and the OpenRouter detailed-usage directive maps 1:1.
/// Test: `to_shared_request_maps_all_fields`, `cache_control_carries_through`.
pub fn to_shared_request(req: &ChatRequest, wire_model: String) -> tci::ChatRequest {
    tci::ChatRequest {
        model: wire_model,
        messages: req.messages.iter().map(to_shared_message).collect(),
        temperature: req.temperature,
        max_tokens: req.max_tokens,
        tools: req
            .tools
            .as_ref()
            .map(|tools| tools.iter().map(to_shared_tool_def).collect()),
        tool_choice: req.tool_choice.clone(),
        stop: None,
        usage: req.usage.as_ref().map(to_shared_usage_directive),
    }
}

/// Convert a shared [`tci::ChatResponse`] back into tcode's [`ChatResponse`].
///
/// Why: every caller downstream of the transport (the agent loop, the recorder,
/// `perf`) speaks tcode's response type and its `TokenUsage` conversion; the
/// shared response is translated back at this one seam.
/// What: copies `id`/`model`, maps each choice element-wise, and reshapes the
/// usage block field-for-field (the two `UsageBlock`s are identical, incl. the
/// nested OpenRouter `prompt_tokens_details` cache counters and the
/// authoritative `cost`).
/// Test: `from_shared_response_maps_all_fields`, `usage_block_round_trips`.
pub fn from_shared_response(resp: tci::ChatResponse) -> ChatResponse {
    let u = &resp.usage;
    // The two `UsageBlock`s are field-identical (incl. the nested OpenRouter
    // `prompt_tokens_details` cache counters and the authoritative `cost`); this
    // is a verbatim copy that preserves tcode's `into_token_usage` cost path.
    // Inlined (rather than a named-type helper) because the shared `UsageBlock`
    // is not re-exported at the `inference` root — only its fields are read.
    let usage = UsageBlock {
        prompt_tokens: u.prompt_tokens,
        completion_tokens: u.completion_tokens,
        total_tokens: u.total_tokens,
        cache_read_input_tokens: u.cache_read_input_tokens,
        cache_creation_input_tokens: u.cache_creation_input_tokens,
        prompt_tokens_details: u
            .prompt_tokens_details
            .as_ref()
            .map(|d| PromptTokensDetails {
                cached_tokens: d.cached_tokens,
                cache_write_tokens: d.cache_write_tokens,
            }),
        cost: u.cost,
    };
    ChatResponse {
        id: resp.id,
        model: resp.model,
        choices: resp.choices.iter().map(from_shared_choice).collect(),
        usage,
    }
}

/// Map a shared [`tci::InferenceError`] onto tcode's [`LlmError`].
///
/// Why: the agent loop, recorder, and telemetry all speak `LlmError`; the two
/// shared errors that carry actionable, structured detail — a non-2xx API
/// status and a missing credential — are preserved as the matching
/// status-carrying / config-missing tcode variants so 401/429 handling and the
/// "set this key" operator guidance survive the migration. Everything else
/// (transport blips, parse failures, unregistered/unsupported alarms) is carried
/// verbatim as [`LlmError::Inference`] rather than lossily reshaped.
/// What: `Api → ApiError`, `MissingCredential → MissingConfig` (with a resolver
/// hint), all other variants → `Inference(display)`.
/// Test: `map_error_preserves_api_status`, `map_error_maps_missing_credential`,
/// `map_error_wraps_other`.
pub fn map_error(err: tci::InferenceError) -> LlmError {
    match err {
        tci::InferenceError::Api { status, body } => LlmError::ApiError { status, body },
        tci::InferenceError::MissingCredential { provider } => LlmError::MissingConfig(format!(
            "no API key resolved for provider '{provider}' (checked its env var, .env.local, \
             and the secure credential store)"
        )),
        other => LlmError::Inference(other.to_string()),
    }
}

// ── Element mappers (tcode → shared) ─────────────────────────────────────────

/// Map a tcode [`ChatMessage`] to the shared one, carrying the cache breakpoint.
///
/// Why: the `cache_control` field is the #2156 prompt-cache marker; it must
/// survive the conversion as a struct field so the shared `ChatMessage`'s own
/// hand-written `Serialize` emits the identical content-block wire shape.
/// What: copies `role`/`content`/`tool_calls`/`tool_call_id`/`name` and maps
/// `cache_control`.
/// Test: `cache_control_carries_through`.
fn to_shared_message(msg: &ChatMessage) -> tci::ChatMessage {
    tci::ChatMessage {
        role: msg.role.clone(),
        content: msg.content.clone(),
        tool_calls: msg
            .tool_calls
            .as_ref()
            .map(|calls| calls.iter().map(to_shared_tool_call).collect()),
        tool_call_id: msg.tool_call_id.clone(),
        name: msg.name.clone(),
        cache_control: msg.cache_control.as_ref().map(to_shared_cache_control),
    }
}

/// Map a tcode [`ToolCall`] to the shared one (assistant-emitted call echoed in
/// history).
fn to_shared_tool_call(call: &ToolCall) -> tci::ToolCall {
    tci::ToolCall {
        id: call.id.clone(),
        kind: call.kind.clone(),
        function: tci::FunctionCall {
            name: call.function.name.clone(),
            arguments: call.function.arguments.clone(),
        },
    }
}

/// Map a tcode [`ToolDefinition`] to the shared one, carrying the last-tool
/// cache breakpoint.
fn to_shared_tool_def(def: &ToolDefinition) -> tci::ToolDefinition {
    tci::ToolDefinition {
        kind: def.kind.clone(),
        function: tci::FunctionDefinition {
            name: def.function.name.clone(),
            description: def.function.description.clone(),
            parameters: def.function.parameters.clone(),
            cache_control: def
                .function
                .cache_control
                .as_ref()
                .map(to_shared_cache_control),
        },
    }
}

/// Map the ephemeral cache breakpoint across (both are `{ kind }`).
fn to_shared_cache_control(cc: &CacheControl) -> tci::CacheControl {
    tci::CacheControl {
        kind: cc.kind.clone(),
    }
}

/// Map the OpenRouter detailed-usage directive across.
fn to_shared_usage_directive(cfg: &RequestUsageConfig) -> tci::RequestUsageConfig {
    tci::RequestUsageConfig {
        include: cfg.include,
    }
}

// ── Element mappers (shared → tcode) ─────────────────────────────────────────

/// Map a shared choice back to tcode's [`ChatChoice`].
fn from_shared_choice(choice: &tci::ChatChoice) -> ChatChoice {
    ChatChoice {
        message: AssistantMessage {
            content: choice.message.content.clone(),
            tool_calls: choice
                .message
                .tool_calls
                .iter()
                .map(from_shared_tool_call)
                .collect(),
        },
        finish_reason: choice.finish_reason.clone(),
    }
}

/// Map a shared [`tci::ToolCall`] back to tcode's [`ToolCall`].
fn from_shared_tool_call(call: &tci::ToolCall) -> ToolCall {
    ToolCall {
        id: call.id.clone(),
        kind: call.kind.clone(),
        function: FunctionCall {
            name: call.function.name.clone(),
            arguments: call.function.arguments.clone(),
        },
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::super::request::FunctionDefinition;
    use super::*;

    /// Every tcode request field must map onto the shared request, with the
    /// caller-supplied wire model replacing the routing slug.
    ///
    /// Why: a dropped field (tools, tool_choice, max_tokens, usage) would
    /// silently change the wire payload and regress behaviour/cost.
    /// What: build a fully-populated request, convert with a distinct wire
    /// model, assert every field.
    /// Test: this test.
    #[test]
    fn to_shared_request_maps_all_fields() {
        let req = ChatRequest {
            model: "fireworks/accounts/fireworks/models/llama".into(),
            messages: vec![ChatMessage::system("sys"), ChatMessage::user("hi")],
            temperature: Some(0.0),
            max_tokens: Some(4096),
            tools: Some(vec![ToolDefinition::function(FunctionDefinition {
                name: "search".into(),
                description: Some("d".into()),
                parameters: Some(serde_json::json!({"type": "object"})),
                cache_control: None,
            })]),
            tool_choice: Some(serde_json::json!("auto")),
            usage: Some(RequestUsageConfig::detailed()),
        };
        let shared = to_shared_request(&req, "accounts/fireworks/models/llama".into());
        assert_eq!(shared.model, "accounts/fireworks/models/llama");
        assert_eq!(shared.messages.len(), 2);
        assert_eq!(shared.messages[0].role, "system");
        assert_eq!(shared.temperature, Some(0.0));
        assert_eq!(shared.max_tokens, Some(4096));
        assert_eq!(shared.tools.as_ref().unwrap().len(), 1);
        assert_eq!(shared.tools.as_ref().unwrap()[0].function.name, "search");
        assert_eq!(shared.tool_choice, Some(serde_json::json!("auto")));
        assert!(shared.stop.is_none());
        assert_eq!(
            shared.usage,
            Some(tci::RequestUsageConfig { include: true })
        );
    }

    /// The #2156 cache breakpoint must survive the conversion as a struct field
    /// on BOTH the system message and the last tool definition — proving it
    /// still serialises to the Anthropic content-block / sibling shape.
    ///
    /// Why: a serde round-trip would drop `#[serde(skip)]` `cache_control`; this
    /// pins that the field-by-field copy preserves it (prompt-caching merge
    /// gate).
    /// What: set `cache_control` on a message and a tool, convert, assert the
    /// shared values are present AND that the shared message serialises to the
    /// cached content-block shape.
    /// Test: this test.
    #[test]
    fn cache_control_carries_through() {
        let mut msg = ChatMessage::system("prefix");
        msg.cache_control = Some(CacheControl::ephemeral());
        let tool = ToolDefinition::function(FunctionDefinition {
            name: "write_file".into(),
            description: None,
            parameters: None,
            cache_control: Some(CacheControl::ephemeral()),
        });
        let req = ChatRequest {
            model: "anthropic/claude-sonnet-4-5".into(),
            messages: vec![msg],
            temperature: None,
            max_tokens: None,
            tools: Some(vec![tool]),
            tool_choice: None,
            usage: None,
        };
        let shared = to_shared_request(&req, "anthropic/claude-sonnet-4-5".into());
        assert!(shared.messages[0].cache_control.is_some());
        assert!(
            shared.tools.as_ref().unwrap()[0]
                .function
                .cache_control
                .is_some()
        );

        // The shared message must serialise to the Anthropic cached-content-block
        // shape (not a bare string) — the #2156 wire contract.
        let v = serde_json::to_value(&shared.messages[0]).expect("serialise");
        assert_eq!(
            v["content"],
            serde_json::json!([{
                "type": "text",
                "text": "prefix",
                "cache_control": {"type": "ephemeral"}
            }])
        );
    }

    /// A shared response must map back onto tcode's response with text, tool
    /// calls, finish reason, and usage all intact.
    ///
    /// Why: the agent loop reads `first_text`/`first_tool_calls`/`finish_reason`
    /// and the recorder reads usage; a dropped field breaks the loop.
    /// What: deserialise a shared response fixture, convert, assert accessors.
    /// Test: this test.
    #[test]
    fn from_shared_response_maps_all_fields() {
        let fixture = r#"{
          "id": "gen-1",
          "model": "openai/gpt-4o-mini",
          "choices": [{"message": {"role": "assistant", "content": null,
              "tool_calls": [{"id": "c1", "type": "function",
                  "function": {"name": "bash", "arguments": "{}"}}]},
              "finish_reason": "tool_calls"}],
          "usage": {"prompt_tokens": 12, "completion_tokens": 3, "total_tokens": 15,
                    "cache_read_input_tokens": 4, "cache_creation_input_tokens": 1}
        }"#;
        let shared: tci::ChatResponse = serde_json::from_str(fixture).expect("deserialise");
        let resp = from_shared_response(shared);
        assert_eq!(resp.id, "gen-1");
        assert_eq!(resp.resolved_model("asked"), "openai/gpt-4o-mini");
        assert!(resp.first_text().is_none());
        assert_eq!(resp.first_tool_calls().len(), 1);
        assert_eq!(resp.first_tool_calls()[0].function.name, "bash");
        assert_eq!(resp.finish_reason(), Some("tool_calls"));
        let usage = resp.token_usage();
        assert_eq!(usage.prompt_tokens, 12);
        assert_eq!(usage.cache_read_tokens, 4);
        assert_eq!(usage.cache_creation_tokens, 1);
    }

    /// OpenRouter's nested cache accounting + authoritative cost must survive
    /// the usage-block conversion (the exact fields the token-efficiency thesis
    /// depends on).
    ///
    /// Why: cost/telemetry capture is a #2406 preservation requirement.
    /// What: deserialise an OpenRouter-shaped usage fixture, convert, assert the
    /// nested counters and cost flow through into `TokenUsage`.
    /// Test: this test.
    #[test]
    fn usage_block_round_trips() {
        let fixture = r#"{
          "id": "z", "choices": [],
          "usage": {"prompt_tokens": 1200, "completion_tokens": 80,
                    "prompt_tokens_details": {"cached_tokens": 900, "cache_write_tokens": 300},
                    "cost": 0.00123}
        }"#;
        let shared: tci::ChatResponse = serde_json::from_str(fixture).expect("deserialise");
        let usage = from_shared_response(shared).token_usage();
        assert_eq!(usage.cache_read_tokens, 900);
        assert_eq!(usage.cache_creation_tokens, 300);
        assert_eq!(usage.cost_usd, Some(0.00123));
    }

    /// A non-2xx API status maps to `ApiError` preserving the code + body.
    ///
    /// Why: 401/429 handling and telemetry key off the preserved status.
    /// Test: this test.
    #[test]
    fn map_error_preserves_api_status() {
        let err = map_error(tci::InferenceError::Api {
            status: 429,
            body: "slow down".into(),
        });
        match err {
            LlmError::ApiError { status, body } => {
                assert_eq!(status, 429);
                assert_eq!(body, "slow down");
            }
            other => panic!("expected ApiError, got {other:?}"),
        }
    }

    /// A missing credential maps to `MissingConfig` with a resolver hint.
    ///
    /// Why: preserves the actionable "set this key" operator guidance.
    /// Test: this test.
    #[test]
    fn map_error_maps_missing_credential() {
        let err = map_error(tci::InferenceError::MissingCredential {
            provider: tci::ProviderId::OpenRouter,
        });
        match err {
            LlmError::MissingConfig(msg) => assert!(msg.contains("openrouter"), "{msg}"),
            other => panic!("expected MissingConfig, got {other:?}"),
        }
    }

    /// Every other shared error is carried verbatim as `Inference`.
    ///
    /// Why: transport/parse/alarm failures keep their honest display text.
    /// Test: this test.
    #[test]
    fn map_error_wraps_other() {
        let err = map_error(tci::InferenceError::Transport("connection refused".into()));
        match err {
            LlmError::Inference(msg) => assert!(msg.contains("connection refused"), "{msg}"),
            other => panic!("expected Inference, got {other:?}"),
        }
    }
}
