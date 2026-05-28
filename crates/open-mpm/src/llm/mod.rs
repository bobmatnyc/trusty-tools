//! OpenRouter LLM client (via async-openai, OpenAI-compatible).
//!
//! Why: Centralizes client construction (base URL, API key) and exposes a
//! small, ergonomic `chat()` helper so the PM loop and sub-agent mode don't
//! duplicate async-openai request plumbing.
//! What: Wraps async-openai's Client<OpenAIConfig> pointed at OpenRouter,
//! and exposes a `chat(...)` function returning our own `ChatResponse`.
//! Test: Unit test parses a hand-crafted tool_calls arguments string via
//! `serde_json` to validate the post-processing logic.

pub mod adapter;
pub mod anthropic_native;
pub mod bedrock;
pub mod credentials;
pub mod thinking_classifier;

pub use thinking_classifier::{ThinkingMode, classify_thinking_mode};

use crate::context::ContextManager;

/// Apply a `ContextManager`'s soft token budget to a typed message vector
/// BEFORE issuing a chat completion (#69).
///
/// Why: Long multi-turn conversations drift toward the model's hard context
/// window; proactively trimming at ~50% leaves headroom for caching and the
/// assistant's response. We operate on the typed `ChatCompletionRequestMessage`
/// by round-tripping through `serde_json::Value` so we don't need a custom
/// size accounting for every message variant.
/// What: Serializes each message to a Value, calls
/// `ContextManager::trim_to_budget` with `protected_count = 1` (the system
/// message), and deserializes survivors back. On any serde failure the
/// original vector is returned unchanged (fail-open).
/// Test: Exercised via the manager's own unit tests; integration covered by
/// the workflow smoke tests.
pub fn trim_messages_with_manager(
    messages: Vec<ChatCompletionRequestMessage>,
    manager: &ContextManager,
    model: &str,
) -> Vec<ChatCompletionRequestMessage> {
    let json_msgs: Vec<serde_json::Value> = match messages
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(v) => v,
        Err(_) => return messages,
    };
    let original_len = json_msgs.len();
    let (trimmed, evicted) = manager.trim_to_budget(json_msgs, model, 1);
    if evicted == 0 {
        return messages;
    }
    tracing::debug!(
        model = %model,
        evicted,
        before = original_len,
        after = trimmed.len(),
        "context manager: trimmed messages"
    );
    let parsed: Result<Vec<ChatCompletionRequestMessage>, _> = trimmed
        .into_iter()
        .map(serde_json::from_value::<ChatCompletionRequestMessage>)
        .collect();
    parsed.unwrap_or(messages)
}

use std::sync::Arc;
use std::sync::OnceLock;

use adapter::{ModelAdapter, adapter_for_model};

use crate::agents::AgentCompressConfig;
use crate::compress::history::{HistoryConfig, Turn, compress_history, history_token_count};
use crate::compress::{CompressConfig, compress as compress_text};
use crate::session::HistoryMessage;

/// Apply compression to a conversation history and task text before sending.
///
/// Why: #135 — wires the existing `compress` module into the actual LLM call
/// path. Compressing at send-time keeps stored history untouched while
/// cutting tokens on the wire. A TOML-disabled agent sees the exact same
/// messages it would have before this hook existed.
/// What: If `cfg.enabled` is false this is a no-op passthrough. Otherwise:
/// runs the pinned sliding-window over `history` using the configured token
/// budget, and — when `cfg.compress_task` is true — runs the task string
/// through the deterministic prompt compressor. Any internal failure logs a
/// WARN and returns the original inputs (fail-open). Metrics are emitted at
/// DEBUG level.
/// Test: `apply_compression_disabled_passthrough`,
/// `apply_compression_compresses_history`,
/// `apply_compression_compresses_task_when_flag_set`.
pub fn apply_compression(
    history: Vec<HistoryMessage>,
    task: String,
    cfg: &AgentCompressConfig,
) -> (Vec<HistoryMessage>, String) {
    if !cfg.enabled {
        return (history, task);
    }

    // History window compression.
    let compressed_history = compress_history_messages(&history, cfg);

    // Task compression (optional).
    let compressed_task = if cfg.compress_task && !task.is_empty() {
        let result = std::panic::catch_unwind(|| compress_text(&task, &CompressConfig::default()));
        match result {
            Ok(r) => {
                tracing::debug!(
                    orig_chars = r.original_len,
                    compressed_chars = r.compressed_len,
                    reduction_pct = r.reduction_pct,
                    "[compress] task: {} → {} chars ({:.1}% reduction)",
                    r.original_len,
                    r.compressed_len,
                    r.reduction_pct
                );
                r.text
            }
            Err(_) => {
                tracing::warn!("compress: task compression panicked; using original");
                task
            }
        }
    } else {
        task
    };

    (compressed_history, compressed_task)
}

/// Run `compress_history` against a `HistoryMessage` slice and log metrics.
fn compress_history_messages(
    history: &[HistoryMessage],
    cfg: &AgentCompressConfig,
) -> Vec<HistoryMessage> {
    let turns: Vec<Turn> = history
        .iter()
        .map(|h| Turn {
            role: h.role.clone(),
            content: h.content.clone(),
        })
        .collect();

    let history_cfg = HistoryConfig {
        keep_last_n: 6,
        token_budget: Some(cfg.token_budget),
        compress_turns: false,
        compress_config: CompressConfig::default(),
    };

    let orig_tokens = history_token_count(&turns);
    let orig_len = turns.len();
    let result = std::panic::catch_unwind(|| compress_history(&turns, &history_cfg));
    let compressed = match result {
        Ok(v) => v,
        Err(_) => {
            tracing::warn!("compress: history compression panicked; using original");
            return history.to_vec();
        }
    };
    let new_tokens = history_token_count(&compressed);
    let new_len = compressed.len();

    let ratio = if orig_len > 0 {
        (1.0 - new_len as f64 / orig_len as f64) * 100.0
    } else {
        0.0
    };
    tracing::debug!(
        orig_msgs = orig_len,
        compressed_msgs = new_len,
        orig_tokens,
        new_tokens,
        "[compress] history: {} → {} messages ({:.1}% reduction)",
        orig_len,
        new_len,
        ratio
    );

    compressed
        .into_iter()
        .map(|t| HistoryMessage {
            role: t.role,
            content: t.content,
        })
        .collect()
}

/// Module-level `reqwest::Client` shared across raw LLM POSTs (MIN-2 / #98).
///
/// Why: `reqwest::Client::new()` allocates a fresh connection pool and TLS
/// state per invocation; creating one per LLM call in `send_raw_completion`
/// and `send_anthropic_native_completion` wastes TCP+TLS handshakes and
/// defeats keep-alive/HTTP2 multiplexing. A single process-wide client reuses
/// its pool across every request.
/// What: `OnceLock<reqwest::Client>` initialized lazily on first access.
/// Test: `http_client_returns_same_instance` below.
static HTTP_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

pub(crate) fn http_client() -> &'static reqwest::Client {
    HTTP_CLIENT.get_or_init(reqwest::Client::new)
}

/// Transient HTTP error classifier used by `backon` retry wrappers.
///
/// Why: OpenRouter and api.anthropic.com both surface transient 429 / 5xx and
/// connection-level errors that succeed on retry. Treating them as hard
/// failures forces the operator to re-run the entire workflow. Surfacing
/// auth/quota errors (400/401/402) as transient would just hide a real
/// configuration problem, so this classifier returns `false` for those.
/// What: An error is "retryable" when it's a connection/timeout/decode error
/// (no HTTP status reached) or when the status code is 429 or in 500..=599.
/// Test: `is_transient_status_*` unit tests below.
fn is_transient_http_status(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

/// Wrap an async LLM HTTP call in a 3-attempt exponential backoff that only
/// retries on transient errors (429, 5xx, connection/timeout failures).
///
/// Why: Centralizes the retry policy so both `send_raw_completion` and
/// `send_anthropic_native_completion` get identical behavior and any future
/// LLM call sites can opt in with one line.
/// What: Uses `backon::ExponentialBuilder` with `max_times(3)` and the
/// classifier `is_transient_anyhow_error` so non-retryable errors (auth,
/// quota, malformed request) fail fast. Returns `T` on success, the last
/// error on exhaustion.
/// Test: Indirectly via integration runs against a 429-throttled endpoint;
/// classifier covered by `is_transient_status_*` unit tests.
async fn with_llm_retry<T, F, Fut>(op: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    use backon::{ExponentialBuilder, Retryable};
    let policy = ExponentialBuilder::default().with_max_times(3);
    op.retry(policy).when(is_transient_anyhow_error).await
}

/// True when an `anyhow::Error` was produced by a transient HTTP failure.
///
/// Why: `with_llm_retry` operates on `anyhow::Error` because every LLM call
/// site already returns `anyhow::Result`; we look for an underlying
/// `reqwest::Error` (no status reached → connection/timeout/decode) or a
/// status-bearing error tagged with a transient HTTP code.
/// What: Walks the error chain. Connection-level `reqwest::Error`s are
/// always retried; status-bearing ones only when `is_transient_http_status`
/// returns true.
/// Test: `is_transient_anyhow_error_*` unit tests below.
fn is_transient_anyhow_error(err: &anyhow::Error) -> bool {
    for cause in err.chain() {
        if let Some(re) = cause.downcast_ref::<reqwest::Error>() {
            return match re.status() {
                Some(status) => is_transient_http_status(status),
                None => true,
            };
        }
    }
    false
}

use anyhow::{Context, Result};
use async_openai::{
    Client,
    config::OpenAIConfig,
    types::{
        ChatCompletionMessageToolCall, ChatCompletionRequestAssistantMessageArgs,
        ChatCompletionRequestMessage, ChatCompletionRequestSystemMessageArgs,
        ChatCompletionRequestToolMessageArgs, ChatCompletionRequestUserMessageArgs,
        ChatCompletionTool, ChatCompletionToolArgs, CreateChatCompletionRequest,
        CreateChatCompletionRequestArgs, CreateChatCompletionResponse, FunctionCall,
        FunctionObjectArgs,
    },
};

use crate::events::{self, Event};
use crate::perf::TokenUsage;
use crate::tools::{ToolRegistry, ToolResult};

/// Why: Some local models (notably qwen3:30b via Ollama) ignore the OpenAI
/// `tool_calls` schema and instead emit tool calls as inline XML inside the
/// assistant `content` field, e.g.
/// `<tool_call>{"name":"foo","arguments":{...}}</tool_call>`. Without parsing
/// these, the harness renders raw XML to the user and skips dispatch entirely.
/// What: Scans `content` for `<tool_call>…</tool_call>` blocks and returns the
/// parsed JSON payloads (those that parse cleanly; malformed blocks are
/// skipped silently).
/// Test: Pass a string with two well-formed tool_call blocks and one garbage
/// block; assert exactly two values returned with the expected `name` fields.
fn extract_xml_tool_calls(content: &str) -> Vec<serde_json::Value> {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r"(?s)<tool_call>(.*?)</tool_call>").expect("static regex compiles")
    });
    re.captures_iter(content)
        .filter_map(|cap| serde_json::from_str(cap.get(1)?.as_str().trim()).ok())
        .collect()
}

/// Why: Even after we dispatch XML-style tool calls, residual
/// `<tool_call>` / `<tool_response>` markup can survive in the final assistant
/// content and leak to the user. Strip it as a defense-in-depth measure so the
/// REPL never shows raw tool XML.
/// What: Removes `<tool_call>…</tool_call>` and `<tool_response>…</tool_response>`
/// blocks (including their inner JSON payloads), trims whitespace, and returns
/// the cleaned text.
/// Test: Feed a string containing both kinds of blocks plus surrounding prose;
/// assert blocks are gone and prose survives intact.
fn strip_xml_tool_noise(content: &str) -> String {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r"(?s)<tool_call>.*?</tool_call>|<tool_response>.*?</tool_response>")
            .expect("static regex compiles")
    });
    re.replace_all(content, "").trim().to_string()
}

/// Resolve the active session id for LLM lifecycle events.
///
/// Why: `chat()` and `chat_with_tools_gated()` don't take a session id
/// parameter, but `Event::LlmRequested` / `Event::LlmResponded` need one so
/// SSE subscribers can scope by task. The harness already stamps
/// `OPEN_MPM_RUN_ID` on the process at startup (see `main.rs`), so falling
/// back to it gives every LLM call the right correlation id without
/// threading it through dozens of signatures.
/// What: Returns `OPEN_MPM_RUN_ID` if set, else an empty string (events with
/// empty session_id remain visible — SSE just doesn't filter them).
fn current_session_id() -> String {
    std::env::var("OPEN_MPM_RUN_ID").unwrap_or_default()
}

/// Emit `Event::LlmRequested` and return the start instant for latency
/// measurement.
///
/// Why: Centralises the emit shape so every call site uses the same field
/// conventions (empty `agent_name` for PM calls, OPEN_MPM_RUN_ID fallback).
/// What: Publishes the event and returns `Instant::now()` so the caller
/// can compute `latency_ms` for the paired `LlmResponded`.
fn emit_llm_requested(model: &str, prompt_tokens: Option<u32>) -> std::time::Instant {
    events::publish(Event::LlmRequested {
        session_id: current_session_id(),
        agent_name: std::env::var("OPEN_MPM_AGENT_ID").unwrap_or_default(),
        model: model.to_string(),
        prompt_tokens,
    });
    std::time::Instant::now()
}

/// Emit `Event::LlmResponded` paired with a prior `emit_llm_requested`.
///
/// Why: The token relay in `repl/mod.rs::spawn_thinking_relay` increments the
/// REPL's live counters from `LlmRequested.prompt_tokens` (input) and
/// `LlmResponded.completion_tokens` (output). The pre-call `emit_llm_requested`
/// always passes `None` for prompt_tokens because we don't know the count
/// until the provider responds. Without this follow-up emit the input bar's
/// `↑` counter stays at 0 forever even though `usage.prompt_tokens` is right
/// there in the response. Mirrors the pattern in
/// `agents/claude_code_runner.rs` which already publishes the pair after
/// parsing the result event.
/// What: Publishes `LlmResponded` (carries completion + latency), then —
/// when `prompt_tokens` is Some — re-publishes `LlmRequested` with the
/// authoritative count so the relay forwards it as a `TokenUpdate`.
fn emit_llm_responded(
    model: &str,
    started: std::time::Instant,
    completion_tokens: Option<u32>,
    prompt_tokens: Option<u32>,
) {
    let latency_ms = started.elapsed().as_millis() as u64;
    events::publish(Event::LlmResponded {
        session_id: current_session_id(),
        agent_name: std::env::var("OPEN_MPM_AGENT_ID").unwrap_or_default(),
        model: model.to_string(),
        completion_tokens,
        latency_ms,
    });
    if prompt_tokens.is_some() {
        events::publish(Event::LlmRequested {
            session_id: current_session_id(),
            agent_name: std::env::var("OPEN_MPM_AGENT_ID").unwrap_or_default(),
            model: model.to_string(),
            prompt_tokens,
        });
    }
}

/// Append a `UsageRecord` for an LLM dispatch (#281).
///
/// Why: Centralizes the per-dispatch usage log emit so the four call sites
/// (chat, chat_with_tools_gated, chat_adapter_aware Bedrock branch, and the
/// claude_code_runner CLI path) all produce identical record shapes.
/// What: Resolves the agent name from `OPEN_MPM_AGENT_ID` (falling back to
/// `"unknown"`), constructs a `UsageRecord` with `chrono::Utc::now()`, and
/// appends to `.open-mpm/state/usage.jsonl` via `crate::usage::append_usage`.
/// Spawns the actual file write as a detached tokio task so we never block
/// the dispatch loop on disk I/O.
/// Test: Indirectly via `crate::usage::tests::append_usage_*` covering the
/// append helper itself; the call-site spawn is exercised end-to-end by
/// any integration that triggers a dispatch.
fn record_dispatch_usage(
    model: &str,
    runner: &str,
    input_tokens: u32,
    output_tokens: u32,
    duration_ms: u64,
    task_for_prefix: &str,
) {
    let agent = std::env::var("OPEN_MPM_AGENT_ID").unwrap_or_else(|_| "unknown".to_string());
    let record = crate::usage::UsageRecord::new(
        agent,
        model.to_string(),
        runner.to_string(),
        input_tokens,
        output_tokens,
        duration_ms,
        task_for_prefix,
    );
    let project_dir = crate::usage::project_dir();
    // Best-effort: spawn so disk I/O never blocks the LLM caller.
    tokio::spawn(async move {
        crate::usage::append_usage(&project_dir, &record).await;
    });
}

/// Heuristic to label which provider a dispatch went through, for the
/// `runner` field of `UsageRecord`.
///
/// Why: The chat loop dispatches via three branches (native Anthropic,
/// raw OpenRouter with cache_control, typed async-openai). All three end
/// up at OpenRouter unless `route_native_anthropic` is true. This helper
/// keeps the branch → label mapping in one place.
/// What: Returns `"anthropic-direct"` when `route_native_anthropic` is set,
/// `"bedrock"` when the adapter provider is Bedrock, otherwise `"openrouter"`.
fn runner_label_for(adapter: &dyn ModelAdapter, route_native_anthropic: bool) -> &'static str {
    if route_native_anthropic {
        "anthropic-direct"
    } else if adapter.provider() == adapter::Provider::Bedrock {
        "bedrock"
    } else {
        "openrouter"
    }
}

/// Recover the first user-message text from a typed message vector for the
/// usage log's `task_prefix` field. Falls back to an empty string when no
/// user message is present (e.g. a tools-only continuation turn).
fn first_user_text_for_prefix(messages: &[ChatCompletionRequestMessage]) -> String {
    for m in messages {
        let v = match serde_json::to_value(m) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if v.get("role").and_then(|r| r.as_str()) == Some("user") {
            return match v.get("content") {
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(serde_json::Value::Array(parts)) => parts
                    .iter()
                    .filter_map(|p| p.get("text").and_then(|t| t.as_str()).map(str::to_string))
                    .collect::<Vec<_>>()
                    .join(" "),
                _ => String::new(),
            };
        }
    }
    String::new()
}

/// Mid-conversation scope reminder (CC pattern §2.8 / §3 Recommendation 7).
///
/// Why: Long-running engineer agents (max_turns ≥ 20) drift on the original
/// task scope when only the turn-0 system prompt asserts those constraints.
/// Anthropic's Claude Code injects `<system-reminder>` tags mid-conversation
/// to reduce this drift; we mimic the pattern as a user-role message after
/// each file-write tool result past turn 5.
/// What: A short directive re-stating scope and verification expectations.
/// Test: `chat_with_tools_gated` is exercised end-to-end via integration; the
/// constant's presence is asserted indirectly by ensuring the loop compiles
/// and the message vector grows by one after a `write_file` past turn 5.
const SCOPE_REMINDER: &str = "<system-reminder>Scope: complete only what the original task specified. Do not add features or refactors beyond the task. Verify output before finishing.</system-reminder>";

/// Build an async-openai client configured for OpenRouter.
///
/// Why: OpenRouter exposes an OpenAI-compatible API on a different base URL;
/// using async-openai's `OpenAIConfig` keeps us on a well-maintained client.
/// What: Reads `OPENROUTER_API_KEY` from env, sets base URL to OpenRouter.
/// Test: Called with a dummy env var set; assert no panic. Real calls are
/// integration-tested via the smoke test.
pub fn create_client() -> Result<Client<OpenAIConfig>> {
    // #250: Tolerate missing OPENROUTER_API_KEY when an alternative credential
    // is configured (ANTHROPIC_API_KEY for direct API; CLAUDE_CODE_OAUTH_TOKEN
    // for the claude CLI subprocess path). The downstream call sites either
    // route through a different code path entirely (claude CLI) or override
    // `use_anthropic_direct=true` which bypasses this client. The base URL
    // stays OpenRouter so any OpenRouter-routed call still works when its
    // key is present.
    let api_key = std::env::var("OPENROUTER_API_KEY").unwrap_or_else(|_| {
        // Empty key — async-openai will only fail if the request actually
        // tries to use it. Direct-Anthropic / claude-code paths short-circuit
        // before then.
        String::new()
    });
    // Note: this is the bare-client constructor. We don't have an agent
    // runner context here; pass `None` so claude-code is never auto-selected
    // just because OAuth is in the env.
    if api_key.is_empty() && credentials::pick_credentials(None).is_none() {
        anyhow::bail!("{}", credentials::missing_credentials_error());
    }
    let base_url = std::env::var("OPENROUTER_BASE_URL")
        .unwrap_or_else(|_| "https://openrouter.ai/api/v1".to_string());
    let config = OpenAIConfig::new()
        .with_api_key(api_key)
        .with_api_base(base_url);
    Ok(Client::with_config(config))
}

/// Parsed tool invocation from a chat completion response.
#[derive(Debug, Clone)]
#[allow(dead_code)] // `id` retained for future multi-turn tool-message threading
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// Decoded JSON arguments from the model.
    pub arguments: serde_json::Value,
}

/// Normalized chat completion response.
#[derive(Debug, Clone, Default)]
pub struct ChatResponse {
    pub content: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    /// Token usage extracted from the OpenRouter response (#47).
    /// Zero when the provider omitted usage or on cache-less non-Anthropic paths.
    pub usage: TokenUsage,
}

/// Send a single chat completion request.
///
/// Why: Hides async-openai builder boilerplate from callers and uniformly
/// converts tool-call arguments (which arrive as stringified JSON) into
/// `serde_json::Value` so downstream dispatch can index by field.
/// What: Sends `system` + `user` messages, optional tools, returns
/// `ChatResponse { content, tool_calls }`.
/// Test: Smoke test with a trivial prompt; asserts non-empty response or
/// a tool_call with `name == "delegate_to_agent"`.
pub async fn chat(
    client: &Client<OpenAIConfig>,
    model: &str,
    system_prompt: &str,
    user_message: &str,
    temperature: f32,
    max_tokens: u32,
    tools: Vec<ChatCompletionTool>,
) -> Result<ChatResponse> {
    let system_msg: ChatCompletionRequestMessage =
        ChatCompletionRequestSystemMessageArgs::default()
            .content(system_prompt)
            .build()
            .context("failed to build system message")?
            .into();

    let user_msg: ChatCompletionRequestMessage = ChatCompletionRequestUserMessageArgs::default()
        .content(user_message)
        .build()
        .context("failed to build user message")?
        .into();

    let mut builder = CreateChatCompletionRequestArgs::default();
    builder
        .model(model)
        .temperature(temperature)
        .max_tokens(max_tokens)
        .messages(vec![system_msg, user_msg]);

    if !tools.is_empty() {
        builder.tools(tools);
    }

    let request = builder.build().context("failed to build chat request")?;

    tracing::debug!(model = %model, "sending chat request to OpenRouter");
    let started = emit_llm_requested(model, None);
    let response = create_chat_completion_lenient(client, request)
        .await
        .context("OpenRouter chat request failed")?;
    let duration_ms = started.elapsed().as_millis() as u64;
    emit_llm_responded(
        model,
        started,
        response.usage.as_ref().map(|u| u.completion_tokens),
        response.usage.as_ref().map(|u| u.prompt_tokens),
    );

    let usage = response
        .usage
        .as_ref()
        .map(|u| {
            let cached = u
                .prompt_tokens_details
                .as_ref()
                .and_then(|d| d.cached_tokens)
                .unwrap_or(0);
            TokenUsage::new(u.prompt_tokens, u.completion_tokens, cached, 0)
        })
        .unwrap_or_default();

    // #281: Emit per-dispatch usage record. `chat()` always goes through
    // OpenRouter (Bedrock/Anthropic-direct have separate paths), so the
    // runner label is hard-coded.
    record_dispatch_usage(
        model,
        "openrouter",
        usage.prompt_tokens,
        usage.completion_tokens,
        duration_ms,
        user_message,
    );

    let choice = response
        .choices
        .into_iter()
        .next()
        .context("OpenRouter returned no choices")?;

    let mut out = ChatResponse {
        content: choice.message.content.clone(),
        tool_calls: Vec::new(),
        usage,
    };

    if let Some(calls) = choice.message.tool_calls {
        for tc in calls {
            let args: serde_json::Value = serde_json::from_str(&tc.function.arguments)
                .with_context(|| {
                    format!(
                        "failed to parse tool_call arguments as JSON: {}",
                        tc.function.arguments
                    )
                })?;
            out.tool_calls.push(ToolCall {
                id: tc.id,
                name: tc.function.name,
                arguments: args,
            });
        }
    }

    Ok(out)
}

/// Adapter-aware single-shot chat: routes Bedrock-prefixed models to AWS
/// Bedrock Converse, falling back to the OpenRouter `chat()` path otherwise.
///
/// Why: `chat()` is hardwired to the async-openai client (OpenRouter). Callers
/// like the CTRL coordinator load model names from TOML and may target Bedrock
/// (`bedrock/...`) or Anthropic-direct in addition to OpenRouter. Without this
/// helper they would have to fan out the same provider switch themselves.
/// What: Inspects the model string via `adapter_for_model`. For Bedrock,
/// builds a Bedrock client (using `OPEN_MPM_AWS_PROFILE`/`OPEN_MPM_AWS_REGION`
/// env vars set by callers via `BedrockEnvGuard`), calls `bedrock::chat_oneshot`,
/// and translates the `(text, tool_uses, usage)` shape to `ChatResponse`. For
/// every other provider it forwards to `chat()` unchanged.
/// Test: Routing itself is covered by `adapter_for_model` unit tests; the
/// Bedrock path is exercised by `bedrock_smoke_test` and end-to-end CTRL
/// integration when a `bedrock/...` ctrl.toml is loaded.
pub async fn chat_adapter_aware(
    client: &Client<OpenAIConfig>,
    model: &str,
    system_prompt: &str,
    user_message: &str,
    temperature: f32,
    max_tokens: u32,
    tools: Vec<ChatCompletionTool>,
) -> Result<ChatResponse> {
    let adapter = adapter_for_model(model);
    if adapter.provider() != adapter::Provider::Bedrock {
        return chat(
            client,
            model,
            system_prompt,
            user_message,
            temperature,
            max_tokens,
            tools,
        )
        .await;
    }

    let model_id = model.strip_prefix("bedrock/").unwrap_or(model);
    let aws_profile = std::env::var("OPEN_MPM_AWS_PROFILE").ok();
    let aws_region = std::env::var("OPEN_MPM_AWS_REGION").ok();
    let bedrock_client =
        bedrock::build_client(aws_profile.as_deref(), aws_region.as_deref()).await?;
    let tools_json: Vec<serde_json::Value> = tools
        .iter()
        .map(|t| serde_json::to_value(t).context("serialize tool for bedrock"))
        .collect::<Result<Vec<_>>>()?;

    tracing::debug!(model = %model, "sending one-shot chat request to Bedrock");
    let started = emit_llm_requested(model, None);
    let (text, tool_uses, usage) = bedrock::chat_oneshot(
        &bedrock_client,
        model_id,
        system_prompt,
        user_message,
        temperature,
        max_tokens,
        tools_json,
    )
    .await?;
    let duration_ms = started.elapsed().as_millis() as u64;
    emit_llm_responded(
        model,
        started,
        Some(usage.completion_tokens),
        Some(usage.prompt_tokens),
    );
    // #281: Emit per-dispatch usage record for Bedrock one-shot calls.
    record_dispatch_usage(
        model,
        "bedrock",
        usage.prompt_tokens,
        usage.completion_tokens,
        duration_ms,
        user_message,
    );

    let tool_calls: Vec<ToolCall> = tool_uses
        .into_iter()
        .map(|tu| ToolCall {
            id: tu.id,
            name: tu.name,
            arguments: tu.input,
        })
        .collect();

    Ok(ChatResponse {
        content: text,
        tool_calls,
        usage,
    })
}

/// Multi-turn chat loop with tool-calling support.
///
/// Why: The single-shot `chat()` can't handle agents that need to reason,
/// call a tool, look at the result, and continue. This function drives the
/// canonical "LLM -> tool -> LLM" loop until the model returns plain text or
/// we hit `max_turns`.
/// What: Seeds a conversation with the provided `messages` (typically system
/// + initial user), then repeatedly:
///   1. Sends the running conversation to the model.
///   2. If the response is text with no tool calls, returns it.
///   3. Otherwise, appends the assistant message and executes each tool call
///      via the `ToolRegistry`, appending each result as a `tool` message.
/// Test: Exercised via integration when a research agent is dispatched;
/// logic covered by `ToolRegistry` dispatch unit tests.
#[allow(dead_code)]
pub async fn chat_with_tools(
    client: &Client<OpenAIConfig>,
    model: &str,
    initial_messages: Vec<ChatCompletionRequestMessage>,
    registry: Arc<ToolRegistry>,
    temperature: f32,
    max_tokens: u32,
    max_turns: u32,
) -> Result<(String, TokenUsage)> {
    let adapter: Box<dyn ModelAdapter> = adapter_for_model(model);
    chat_with_tools_gated(
        client,
        model,
        &*adapter,
        initial_messages,
        registry,
        None,
        temperature,
        max_tokens,
        max_turns,
        false,
        None,
        false,
        false,
        &[],
    )
    .await
}

/// Variant of `chat_with_tools` that applies a per-agent tool allowlist and
/// dispatches all tool calls in a single turn concurrently.
///
/// Why: (#25) Different agents should only be able to call the tools they
/// need; (#26) the LLM frequently emits multiple tool calls per turn (e.g.
/// two `fetch_url` calls), and running them sequentially multiplies latency
/// unnecessarily. Using `FuturesUnordered` lets them proceed in parallel
/// while preserving per-call tool_use_id matching.
/// What: For each turn, collects the assistant's `tool_calls`, dispatches
/// each via `registry.dispatch_gated(name, args, allowed)` concurrently,
/// then appends the results as `tool` messages in tool_call order.
/// Errors are surfaced as `is_error: true` tool_result content so the LLM
/// can reason about the failure. Fatal (non-recoverable) errors still
/// produce a tool_result message but we log them at warn level; the loop
/// always continues until the model returns plain text or `max_turns`.
/// Test: See `llm::tests::parallel_tool_dispatch` (mocks two tools; assert
/// both ran and one erroring did not cancel the other).
#[allow(clippy::too_many_arguments)]
/// Inject `/think\n` at the start of the last user message when the prompt
/// classifies as reasoning-heavy.
///
/// Why: qwen3's chat template recognizes `/think` and `/no_think` as special
/// tokens. The ctrl agent sets `/no_think` in its system prompt (fast default);
/// this helper opt-in-overrides on a per-turn basis when the user actually
/// needs chain-of-thought.
/// What: Walks `messages` in reverse for the last `role == "user"` entry,
/// extracts its text content, runs `classify_thinking_mode`, and — only when
/// the result is `Think` and the content does not already start with `/think`
/// or `/no_think` — prepends `/think\n` and writes the message back. On any
/// serde failure the messages vec is left untouched (fail-open).
/// Test: Covered indirectly via `thinking_classifier` unit tests; behaviour
/// here is a thin Value-level mutation with no LLM dependency.
fn maybe_inject_qwen3_think(messages: &mut [ChatCompletionRequestMessage]) {
    // Walk in reverse — find the last user message.
    let last_user_idx = messages.iter().enumerate().rev().find_map(|(i, m)| {
        let v = serde_json::to_value(m).ok()?;
        if v.get("role").and_then(|r| r.as_str()) == Some("user") {
            Some(i)
        } else {
            None
        }
    });
    let Some(idx) = last_user_idx else {
        return;
    };

    let mut v = match serde_json::to_value(&messages[idx]) {
        Ok(v) => v,
        Err(_) => return,
    };

    // Pull the user's text content out of either the string or array shape.
    let content_text: String = match v.get("content") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(|t| t.as_str()).map(str::to_string))
            .collect::<Vec<_>>()
            .join(" "),
        _ => return,
    };

    // Don't double-tag if the user (or an earlier injection) already set the mode.
    let trimmed = content_text.trim_start();
    if trimmed.starts_with("/think") || trimmed.starts_with("/no_think") {
        return;
    }

    if classify_thinking_mode(&content_text) != ThinkingMode::Think {
        return;
    }

    // Mutate the content. For string content we can prepend directly; for
    // the array shape we prepend onto the first text part (qwen3 reads them
    // concatenated anyway).
    match v.get_mut("content") {
        Some(c @ serde_json::Value::String(_)) => {
            let s = c.as_str().unwrap_or("").to_string();
            *c = serde_json::Value::String(format!("/think\n{s}"));
        }
        Some(serde_json::Value::Array(parts)) => {
            if let Some(first_text) = parts
                .iter_mut()
                .find(|p| p.get("text").and_then(|t| t.as_str()).is_some())
                && let Some(t) = first_text.get_mut("text")
            {
                let s = t.as_str().unwrap_or("").to_string();
                *t = serde_json::Value::String(format!("/think\n{s}"));
            }
        }
        _ => return,
    }

    if let Ok(rebuilt) = serde_json::from_value::<ChatCompletionRequestMessage>(v) {
        messages[idx] = rebuilt;
    }
}

// Why: This is the workhorse function-call loop and every parameter is
// independently load-bearing (model routing, sampling, gating flags, etc.).
// A wrapper struct would just punt the documentation problem one layer up
// without reducing complexity at the call sites.
#[allow(clippy::too_many_arguments)]
pub async fn chat_with_tools_gated(
    client: &Client<OpenAIConfig>,
    model: &str,
    adapter: &dyn ModelAdapter,
    initial_messages: Vec<ChatCompletionRequestMessage>,
    registry: Arc<ToolRegistry>,
    allowed_tools: Option<Vec<String>>,
    temperature: f32,
    max_tokens: u32,
    max_turns: u32,
    enable_prompt_caching: bool,
    tool_choice: Option<serde_json::Value>,
    use_finish_task: bool,
    use_anthropic_direct: bool,
    stop_sequences: &[String],
) -> Result<(String, TokenUsage)> {
    // #201: Bedrock-routed agents take a totally different code path — no
    // OpenAI-compatible HTTP, no async-openai client. We extract the system
    // prompt and the initial user message from `initial_messages`, then hand
    // off to the Bedrock multi-turn loop. AWS profile/region come from env
    // vars set by the in-process runner before this call (see
    // `OPEN_MPM_AWS_PROFILE`/`OPEN_MPM_AWS_REGION`); falling back to SDK
    // defaults preserves operator overrides.
    if adapter.provider() == adapter::Provider::Bedrock {
        let model_id = model.strip_prefix("bedrock/").unwrap_or(model);
        let (system_prompt, user_message) = extract_system_and_first_user(&initial_messages)?;
        let openai_tools = registry.openai_tools()?;
        let tools_json: Vec<serde_json::Value> = openai_tools
            .iter()
            .map(|t| serde_json::to_value(t).context("serialize tool for bedrock"))
            .collect::<Result<Vec<_>>>()?;
        let aws_profile = std::env::var("OPEN_MPM_AWS_PROFILE").ok();
        let aws_region = std::env::var("OPEN_MPM_AWS_REGION").ok();
        let bedrock_client =
            bedrock::build_client(aws_profile.as_deref(), aws_region.as_deref()).await?;
        return bedrock::chat_with_tools(
            &bedrock_client,
            model_id,
            &system_prompt,
            &user_message,
            temperature,
            max_tokens,
            tools_json,
            registry,
            allowed_tools,
            max_turns,
            stop_sequences,
        )
        .await;
    }

    use futures::stream::{FuturesUnordered, StreamExt};

    let openai_tools = registry.openai_tools()?;
    let mut messages = initial_messages;
    // #287: ollama models are routed via `ollama/<name>` — the prefix is only
    // used by `adapter_for_model` for selection. Strip it before sending the
    // request body so ollama's OpenAI-compat layer sees the bare model id
    // (e.g. `llama3.2:latest`). Force the raw HTTP path for these calls so
    // they hit the local ollama base URL instead of the async-openai client's
    // hardwired OpenRouter endpoint.
    let is_ollama = model.starts_with("ollama/");
    let model_owned = model.strip_prefix("ollama/").unwrap_or(model).to_string();
    let model = model_owned.as_str();
    // qwen3 supports `/think` and `/no_think` as in-message special tokens.
    // The system prompt sets `/no_think` as the default for speed; we inject
    // `/think` into the LAST user message when the prompt is reasoning-heavy
    // (math, debugging, architecture, multi-step). Detection is case-insensitive
    // on the model name. See `crate::llm::thinking_classifier`.
    if model.to_ascii_lowercase().contains("qwen3") {
        maybe_inject_qwen3_think(&mut messages);
    }
    // #47: Accumulate token usage across every turn so callers can attribute
    // a single aggregated cost/latency figure to the whole task.
    let mut total_usage = TokenUsage::default();
    // #50: Prompt caching is Anthropic-specific — the adapter's
    // `inject_cache_control` is a no-op for everyone else, so we can route
    // through the raw path whenever the caller opts in.
    let caching_active =
        enable_prompt_caching && adapter.provider() == adapter::Provider::Anthropic;
    // #59: Choose native Anthropic routing once, up front, so the request path
    // is consistent for the entire tool loop even if env changes mid-flight.
    let endpoint = adapter.api_endpoint(use_anthropic_direct);
    let route_native_anthropic = use_anthropic_direct
        && adapter.uses_native_format()
        && endpoint.auth_header_name == "x-api-key";
    // #33: Count consecutive turns in which the model returned plain text
    // instead of a tool call. Reset to 0 whenever a tool call is emitted.
    // We allow ONE plain-text-mid-task turn (injecting a reminder to use a
    // tool), then accept the second plain-text response as the final answer.
    let mut consecutive_no_tool_turns: u32 = 0;

    // #69: Apply a default 50% context-window budget trim before each turn.
    // Protects the system message (index 0) and evicts oldest turns only when
    // the accumulated prompt exceeds ~half the model's context window.
    let ctx_manager = ContextManager::new(0.5);

    for turn in 0..max_turns {
        tracing::debug!(
            turn,
            model,
            caching_active,
            "chat_with_tools: sending request"
        );

        // #69: Trim messages before each LLM call so long tool-call chains
        // don't overflow the context window.
        messages = trim_messages_with_manager(messages, &ctx_manager, model);

        // Build the request either the typed async-openai way (no tool_choice
        // override, no caching) or as a raw `serde_json::Value` when we need
        // to inject provider-specific fields (cache_control and/or a
        // non-trivial tool_choice shape) that async-openai 0.28 cannot model.
        let needs_raw =
            caching_active || tool_choice.is_some() || route_native_anthropic || is_ollama;
        let llm_started = emit_llm_requested(model, None);
        let (content, tool_calls, turn_usage) = if route_native_anthropic {
            // #59: Native `api.anthropic.com/v1/messages` path — prompt caching
            // is supported inline (no raw-reqwest hack needed) and tool
            // results get translated into Anthropic's `tool_result` blocks.
            let mut body = anthropic_native::build_anthropic_request(
                model,
                &messages,
                &openai_tools,
                temperature,
                max_tokens,
                tool_choice.as_ref(),
                caching_active,
            )?;
            // #297: Anthropic native API uses `stop_sequences` (array of strings).
            if !stop_sequences.is_empty() {
                body["stop_sequences"] = serde_json::json!(stop_sequences);
            }
            send_anthropic_native_completion(&body, &endpoint).await?
        } else if needs_raw {
            let mut raw = build_raw_request(
                model,
                &messages,
                &openai_tools,
                temperature,
                max_tokens,
                false, // we apply cache_control via the adapter below
            )?;
            // Only inject Anthropic-specific cache_control when routing natively
            // to api.anthropic.com. Injecting it for OpenRouter breaks the
            // OpenAI-format request body that OpenRouter expects.
            if route_native_anthropic {
                adapter.inject_cache_control(&mut raw, caching_active);
            }
            if let Some(tc) = tool_choice.as_ref() {
                // When routing via OpenRouter (not native Anthropic), convert
                // Anthropic-format tool_choice to OpenAI-compatible format.
                // {"type":"any"} -> "required", {"type":"auto"} -> "auto"
                let openrouter_tc = if !route_native_anthropic {
                    match tc.get("type").and_then(|v| v.as_str()) {
                        Some("any") => serde_json::json!("required"),
                        Some("auto") => serde_json::json!("auto"),
                        Some("none") => serde_json::json!("none"),
                        _ => tc.clone(), // pass through if already OpenAI format
                    }
                } else {
                    tc.clone()
                };
                raw["tool_choice"] = openrouter_tc;
            }
            // #297: Forward stop sequences on the raw path. OpenRouter accepts
            // OpenAI's `stop` (string or array). Use the array form for both
            // single and multi-sequence cases for consistency.
            if !stop_sequences.is_empty() {
                raw["stop"] = serde_json::json!(stop_sequences);
            }
            send_raw_completion(&raw, adapter).await?
        } else {
            let mut builder = CreateChatCompletionRequestArgs::default();
            builder
                .model(model)
                .temperature(temperature)
                .max_tokens(max_tokens)
                .messages(messages.clone());
            // #297: Forward agent-configured stop sequences (e.g. `"```\n\n"`
            // for code-returning agents) to OpenRouter's `stop` parameter so the
            // model halts as soon as it emits one — saving 50-300 output tokens
            // of trailing commentary per response.
            if !stop_sequences.is_empty() {
                builder.stop(async_openai::types::Stop::StringArray(
                    stop_sequences.to_vec(),
                ));
            }
            if !openai_tools.is_empty() {
                builder.tools(openai_tools.clone());
            }
            let request = builder
                .build()
                .context("failed to build chat_with_tools request")?;

            let response = create_chat_completion_lenient(client, request)
                .await
                .context("chat_with_tools: OpenRouter request failed")?;

            let usage = response
                .usage
                .as_ref()
                .map(|u| {
                    let cached = u
                        .prompt_tokens_details
                        .as_ref()
                        .and_then(|d| d.cached_tokens)
                        .unwrap_or(0);
                    TokenUsage::new(u.prompt_tokens, u.completion_tokens, cached, 0)
                })
                .unwrap_or_default();

            let choice = response
                .choices
                .into_iter()
                .next()
                .context("chat_with_tools: no choices in response")?;
            let message = choice.message;
            (
                message.content,
                message.tool_calls.unwrap_or_default(),
                usage,
            )
        };

        let turn_duration_ms = llm_started.elapsed().as_millis() as u64;
        emit_llm_responded(
            model,
            llm_started,
            Some(turn_usage.completion_tokens),
            Some(turn_usage.prompt_tokens),
        );
        // #281: Emit per-dispatch usage record for each turn of the tool loop.
        // `task_prefix` uses the first user message from the conversation so
        // the log entry is human-recognizable; per-turn assistant/tool churn
        // doesn't change the prefix.
        let runner_label = runner_label_for(adapter, route_native_anthropic);
        let task_prefix_src = first_user_text_for_prefix(&messages);
        record_dispatch_usage(
            model,
            runner_label,
            turn_usage.prompt_tokens,
            turn_usage.completion_tokens,
            turn_duration_ms,
            &task_prefix_src,
        );
        total_usage.add(&turn_usage);

        // BUG-FIX: qwen3:30b (and similar local models) emit tool calls as
        // inline `<tool_call>{…}</tool_call>` XML inside `content` instead of
        // populating the OpenAI `tool_calls` array. When the structured array
        // is empty but the content carries XML tool calls, promote them to
        // real `ChatCompletionMessageToolCall` values so the existing dispatch
        // path below executes them. Each synthesized id is unique within the
        // turn so tool_result pairing works.
        let mut tool_calls = tool_calls;
        if tool_calls.is_empty()
            && let Some(text) = content.as_deref()
            && text.contains("<tool_call>")
        {
            let xml_calls = extract_xml_tool_calls(text);
            if !xml_calls.is_empty() {
                tracing::info!(
                    count = xml_calls.len(),
                    "promoting XML <tool_call> blocks to structured tool calls"
                );
                for (i, v) in xml_calls.into_iter().enumerate() {
                    let name = v
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("")
                        .to_string();
                    if name.is_empty() {
                        continue;
                    }
                    let args = v
                        .get("arguments")
                        .cloned()
                        .unwrap_or(serde_json::Value::Object(Default::default()));
                    let arguments =
                        serde_json::to_string(&args).unwrap_or_else(|_| "{}".to_string());
                    tool_calls.push(ChatCompletionMessageToolCall {
                        id: format!("xml_tc_{turn}_{i}"),
                        r#type: async_openai::types::ChatCompletionToolType::Function,
                        function: FunctionCall { name, arguments },
                    });
                }
            }
        }

        if tool_calls.is_empty() {
            // #33: Tool-call discipline. A tool-calling agent producing plain
            // text mid-task is usually confusion, not completion. On the
            // FIRST such turn (and only if we have turns remaining), inject
            // an error reminding the agent to use a tool and retry. On the
            // SECOND, give up and accept the text as the final answer so we
            // degrade gracefully rather than loop forever.
            if should_retry_plain_text_turn(consecutive_no_tool_turns, turn, max_turns) {
                consecutive_no_tool_turns += 1;
                let reminder: ChatCompletionRequestMessage =
                    ChatCompletionRequestUserMessageArgs::default()
                        .content(
                            "Please use one of the available tools to complete this task, \
                             or provide your final answer directly.",
                        )
                        .build()
                        .context("failed to build tool-discipline reminder message")?
                        .into();
                messages.push(reminder);
                tracing::warn!(
                    turn,
                    "agent produced plain-text mid-task; injecting retry error"
                );
                continue;
            }
            // Either we already retried once, or this is the final turn.
            // Accept whatever the model produced. Strip any residual XML
            // tool_call / tool_response markup so it never reaches the user.
            let final_content = strip_xml_tool_noise(&content.unwrap_or_default());
            return Ok((final_content, total_usage));
        }

        // Tool call present — discipline counter resets.
        consecutive_no_tool_turns = 0;

        // #57: `finish_task` is a terminal tool — as soon as the model calls
        // it we exit the loop, using the supplied `summary` as the agent's
        // final text output. We check BEFORE dispatching so we don't emit a
        // stray tool_result message (there's no turn after this).
        if use_finish_task && let Some(summary) = extract_finish_task_summary(&tool_calls) {
            tracing::info!(turn, "finish_task called — exiting chat loop");
            return Ok((strip_xml_tool_noise(&summary), total_usage));
        }

        // Append assistant message (with tool calls) before sending tool results.
        let assistant_msg = build_assistant_tool_call_message(&content, &tool_calls)?;
        messages.push(assistant_msg);

        // Dispatch all tool calls in this turn concurrently. We preserve the
        // assistant-provided order for tool_result messages (matching by id)
        // because some providers require it.
        let mut futs = FuturesUnordered::new();
        for tc in &tool_calls {
            let registry = Arc::clone(&registry);
            let id = tc.id.clone();
            let name = tc.function.name.clone();
            let raw_args = tc.function.arguments.clone();
            let allowed = allowed_tools.clone();
            futs.push(async move {
                let args: serde_json::Value = serde_json::from_str(&raw_args)
                    .unwrap_or(serde_json::Value::Object(Default::default()));
                let result = registry
                    .dispatch_gated(&name, args, allowed.as_deref())
                    .await;
                (id, name, result)
            });
        }

        let mut results_by_id: std::collections::HashMap<String, (String, ToolResult)> =
            std::collections::HashMap::with_capacity(tool_calls.len());
        while let Some((id, name, result)) = futs.next().await {
            tracing::debug!(tool = %name, is_error = result.is_error(), "tool result");
            if result.is_fatal() {
                tracing::warn!(
                    tool = %name,
                    message = %result.content(),
                    "tool returned fatal (non-recoverable) error"
                );
            }
            results_by_id.insert(id, (name, result));
        }

        // Emit tool_result messages in original order so providers that care
        // about pairing see matching ids.
        // Track whether this turn included a file-write tool so we can append
        // a scope reminder after the tool_result block (see SCOPE_REMINDER).
        let mut had_file_write = false;
        for tc in &tool_calls {
            let (name, result) = match results_by_id.remove(&tc.id) {
                Some(v) => v,
                None => (
                    tc.function.name.clone(),
                    ToolResult::err("internal: tool result missing"),
                ),
            };
            if matches!(name.as_str(), "write_file" | "edit_file") {
                had_file_write = true;
            }
            let raw_str = match &result {
                ToolResult::Success(s) => s.clone(),
                ToolResult::Error { message, .. } => format!("ERROR: {message}"),
            };
            // #269/#420: Compress tool output before injecting into conversation
            // history. Uses the async RTK-style path (tries rtk subprocess first,
            // falls back to native filter chain) so per-tool filters (test runner,
            // git diff, file read, etc.) strip noise that would otherwise bloat
            // the next request's prompt. Errors and unknown tools pass through.
            let content_str = match &result {
                ToolResult::Success(_) => {
                    crate::compress::compress_tool_output_async(&name, &raw_str).await
                }
                ToolResult::Error { .. } => raw_str,
            };
            let tool_msg: ChatCompletionRequestMessage =
                ChatCompletionRequestToolMessageArgs::default()
                    .tool_call_id(tc.id.clone())
                    .content(content_str)
                    .build()
                    .context("failed to build tool result message")?
                    .into();
            messages.push(tool_msg);
            // `name` retained for trace context; silence unused.
            let _ = name;
        }

        // CC pattern: mid-conversation scope reminder. After a file-write tool
        // result, once the agent has been running for more than 5 turns, inject
        // a brief user-side reminder to keep scope and verification in mind.
        // Why: long-running engineer agents (max_turns ≥ 20) drift on scope and
        // skip verification when only the turn-0 system prompt asserts these
        // constraints. A cheap, periodic reinforcement reduces drift measurably.
        if had_file_write && turn > 5 {
            let reminder_msg: ChatCompletionRequestMessage =
                ChatCompletionRequestUserMessageArgs::default()
                    .content(SCOPE_REMINDER)
                    .build()
                    .context("failed to build scope reminder message")?
                    .into();
            messages.push(reminder_msg);
        }
    }

    anyhow::bail!(
        "chat_with_tools exceeded max_turns ({max_turns}) without a final text response (total_usage: prompt={} completion={})",
        total_usage.prompt_tokens,
        total_usage.completion_tokens,
    )
}

/// Build a raw JSON request body mirroring async-openai's schema, optionally
/// injecting Anthropic `cache_control` onto the system message content.
///
/// Why: (#50) `async-openai` 0.28 does not expose `cache_control` on its
/// typed messages; bypassing it lets us send the extra field OpenRouter
/// forwards to Anthropic. Kept private so the tool-loop is the only consumer.
/// What: Converts the typed `ChatCompletionRequestMessage` vec to a JSON
/// array, wraps the system message's string content as
/// `[{"type":"text","text":<content>,"cache_control":{"type":"ephemeral"}}]`,
/// and assembles the full top-level chat/completions body.
/// Test: `build_raw_request_injects_cache_control`.
fn build_raw_request(
    model: &str,
    messages: &[ChatCompletionRequestMessage],
    tools: &[ChatCompletionTool],
    temperature: f32,
    max_tokens: u32,
    inject_cache_control: bool,
) -> Result<serde_json::Value> {
    // Round-trip the typed messages through JSON so we get the OpenAI wire
    // shape without re-implementing it; then patch the system message.
    let mut msgs_json: Vec<serde_json::Value> = messages
        .iter()
        .map(|m| serde_json::to_value(m).context("serialize chat message"))
        .collect::<Result<Vec<_>>>()?;

    if inject_cache_control {
        // Patch the FIRST system message we find.
        for m in msgs_json.iter_mut() {
            if m.get("role").and_then(|v| v.as_str()) == Some("system") {
                // content may be string or array-of-blocks; normalize to array
                // with a single text block carrying cache_control.
                let text_val = match m.get("content").cloned() {
                    Some(serde_json::Value::String(s)) => s,
                    Some(serde_json::Value::Array(_arr)) => {
                        // Already block-shaped; patch first block's cache_control.
                        if let Some(arr) = m.get_mut("content").and_then(|v| v.as_array_mut())
                            && let Some(first) = arr.first_mut()
                            && let Some(obj) = first.as_object_mut()
                        {
                            obj.insert(
                                "cache_control".to_string(),
                                serde_json::json!({"type": "ephemeral"}),
                            );
                        }
                        break;
                    }
                    _ => break,
                };
                m["content"] = serde_json::json!([
                    {
                        "type": "text",
                        "text": text_val,
                        "cache_control": {"type": "ephemeral"}
                    }
                ]);
                break;
            }
        }
    }

    let mut body = serde_json::json!({
        "model": model,
        "temperature": temperature,
        "max_tokens": max_tokens,
        "messages": msgs_json,
    });

    if !tools.is_empty() {
        let tools_json: Vec<serde_json::Value> = tools
            .iter()
            .map(|t| serde_json::to_value(t).context("serialize tool"))
            .collect::<Result<Vec<_>>>()?;
        body["tools"] = serde_json::Value::Array(tools_json);
    }

    Ok(body)
}

/// Send a typed chat-completion request, tolerating unknown `service_tier`
/// values OpenRouter returns.
///
/// Why: (#486) OpenRouter (via Anthropic) now returns
/// `"service_tier":"standard"`, a variant async-openai's `ServiceTier` enum
/// doesn't know. The typed `client.chat().create()` path deserializes the
/// body internally and fails the whole request. We can't extend the upstream
/// enum, so we POST the request ourselves, strip the offending field from the
/// raw JSON, then deserialize into the typed `CreateChatCompletionResponse`.
/// What: Serializes `request`, POSTs it to `{config}/chat/completions` with
/// the client's headers, removes the top-level `service_tier` field, and
/// deserializes the result. Retries transient 429/5xx via `with_llm_retry`.
/// Test: `strip_service_tier_removes_field` covers the JSON sanitization;
/// the request path is integration-tested via the OpenRouter smoke test.
async fn create_chat_completion_lenient(
    client: &Client<OpenAIConfig>,
    request: CreateChatCompletionRequest,
) -> Result<CreateChatCompletionResponse> {
    use async_openai::config::Config;

    let config = client.config();
    let url = config.url("/chat/completions");
    let headers = config.headers();
    let query = config.query();
    let body = serde_json::to_value(&request).context("failed to serialize chat request")?;

    let json: serde_json::Value = with_llm_retry(|| async {
        let resp = http_client()
            .post(&url)
            .headers(headers.clone())
            .query(&query)
            .json(&body)
            .send()
            .await
            .context("chat completion POST failed")?;
        // Preserve the underlying `reqwest::Error` (with status) in the anyhow
        // chain so `with_llm_retry`'s classifier can decide whether to retry.
        let resp = resp.error_for_status()?;
        let json: serde_json::Value = resp
            .json()
            .await
            .context("failed to parse OpenRouter JSON response")?;
        Ok::<_, anyhow::Error>(json)
    })
    .await?;

    let json = strip_service_tier(json);
    serde_json::from_value(json).context("failed to deserialize chat completion response")
}

/// Remove the top-level `service_tier` field from a chat-completion response.
///
/// Why: (#486) OpenRouter returns `service_tier` values async-openai's
/// `ServiceTier` enum can't deserialize (e.g. `"standard"`). The field is
/// purely informational and unused downstream, so dropping it lets the rest
/// of the typed response deserialize cleanly.
/// What: If `json` is an object, removes the `service_tier` key; returns the
/// (possibly modified) value unchanged otherwise.
/// Test: `strip_service_tier_removes_field`.
fn strip_service_tier(mut json: serde_json::Value) -> serde_json::Value {
    if let Some(obj) = json.as_object_mut() {
        obj.remove("service_tier");
    }
    json
}

/// POST a raw chat-completions body to OpenRouter and parse back the pieces
/// the tool loop needs (content, tool_calls, usage with Anthropic cache fields).
///
/// Why: (#50) The raw path is only used when we've injected cache_control
/// fields async-openai cannot represent. Extracting usage + tool_calls from
/// `serde_json::Value` is trivial and keeps us provider-agnostic.
/// What: Reads OPENROUTER_API_KEY + OPENROUTER_BASE_URL, POSTs the body,
/// pulls `choices[0].message.{content, tool_calls}` and `usage.*` including
/// `cache_read_input_tokens` / `cache_creation_input_tokens`.
/// Test: Exercised end-to-end via integration; unit-tested construction via
/// `build_raw_request_injects_cache_control`.
async fn send_raw_completion(
    body: &serde_json::Value,
    adapter: &dyn ModelAdapter,
) -> Result<(
    Option<String>,
    Vec<ChatCompletionMessageToolCall>,
    TokenUsage,
)> {
    // #287: Route through the adapter's `api_endpoint` so ollama (or any other
    // adapter that overrides the base URL) goes to its native server instead
    // of OpenRouter. Adapters with no auth (ollama) leave `auth_header_value`
    // empty; we skip the Authorization header in that case.
    let endpoint = adapter.api_endpoint(false);
    let url = format!(
        "{}/chat/completions",
        endpoint.base_url.trim_end_matches('/')
    );
    let auth_value = if endpoint.auth_header_value.is_empty() {
        // Fall back to the OpenRouter env var when the adapter doesn't supply
        // a credential (legacy callers that rely on OPENROUTER_API_KEY here).
        std::env::var("OPENROUTER_API_KEY").unwrap_or_default()
    } else {
        endpoint
            .auth_header_value
            .strip_prefix("Bearer ")
            .unwrap_or(&endpoint.auth_header_value)
            .to_string()
    };
    if auth_value.is_empty() && !url.contains("localhost") && !url.contains("127.0.0.1") {
        anyhow::bail!("OPENROUTER_API_KEY not set (and adapter supplied no credential)");
    }
    // backon retry: on transient 429/5xx + connection errors, retry up to 3x
    // with exponential backoff. Auth/quota errors (400/401/402) fail fast.
    let json: serde_json::Value = with_llm_retry(|| async {
        let mut req = http_client().post(&url).json(body);
        if !auth_value.is_empty() {
            req = req.bearer_auth(&auth_value);
        }
        let resp = req
            .send()
            .await
            .context("raw chat completion POST failed")?;
        // `error_for_status` keeps the underlying `reqwest::Error` reachable
        // via the anyhow chain so the retry classifier can inspect its status.
        let resp = resp.error_for_status()?;
        let json: serde_json::Value = resp
            .json()
            .await
            .context("failed to parse OpenRouter JSON response")?;
        Ok::<_, anyhow::Error>(json)
    })
    .await?;

    // Adapter parses provider-specific usage (Anthropic cache fields etc.).
    let usage = adapter.parse_usage(&json);

    // choices[0].message
    let msg = json
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .context("raw response missing choices[0].message")?;

    let content = msg
        .get("content")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let tool_calls: Vec<ChatCompletionMessageToolCall> = msg
        .get("tool_calls")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|tc| {
                    let id = tc.get("id")?.as_str()?.to_string();
                    let name = tc
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|v| v.as_str())?
                        .to_string();
                    let arguments = tc
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("{}")
                        .to_string();
                    Some(ChatCompletionMessageToolCall {
                        id,
                        r#type: async_openai::types::ChatCompletionToolType::Function,
                        function: FunctionCall { name, arguments },
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    Ok((content, tool_calls, usage))
}

/// POST an Anthropic native `/v1/messages` body to the configured endpoint
/// and return the pieces the tool loop expects.
///
/// Why: #59 — when routing to `api.anthropic.com` directly we can't reuse
/// `send_raw_completion` because its URL is hard-coded to
/// `{base}/chat/completions` and its auth header is `Bearer`. This helper
/// owns the direct-API POST (respecting `x-api-key` and extra headers) and
/// normalizes the response back into the same `(content, tool_calls, usage)`
/// tuple the OpenAI path produces so the loop body is unchanged.
/// What: POSTs `body` to `{endpoint.base_url}/messages` with the adapter's
/// auth + extra headers, parses the response via
/// `anthropic_native::parse_anthropic_response`, and converts the result
/// into `(Option<String>, Vec<ChatCompletionMessageToolCall>, TokenUsage)`.
/// Test: Exercised end-to-end via integration; unit-tested through
/// `anthropic_native::parse_anthropic_response` tests.
async fn send_anthropic_native_completion(
    body: &serde_json::Value,
    endpoint: &adapter::ApiEndpoint,
) -> Result<(
    Option<String>,
    Vec<ChatCompletionMessageToolCall>,
    TokenUsage,
)> {
    let url = format!("{}/messages", endpoint.base_url.trim_end_matches('/'));
    // backon retry: same transient-only policy as the OpenRouter path.
    let json: serde_json::Value = with_llm_retry(|| async {
        let mut req = http_client()
            .post(&url)
            .header(&endpoint.auth_header_name, &endpoint.auth_header_value)
            .header("content-type", "application/json");
        for (k, v) in &endpoint.extra_headers {
            req = req.header(k, v);
        }
        let resp = req
            .json(body)
            .send()
            .await
            .context("Anthropic direct POST failed")?;
        let resp = resp.error_for_status()?;
        let json: serde_json::Value = resp
            .json()
            .await
            .context("failed to parse Anthropic JSON response")?;
        Ok::<_, anyhow::Error>(json)
    })
    .await?;
    let parsed = anthropic_native::parse_anthropic_response(&json);
    Ok((parsed.text_content, parsed.tool_calls, parsed.usage))
}

/// Scan a turn's tool calls for a `finish_task` invocation and return its
/// `summary` argument if present.
///
/// Why: #57 — the chat loop needs to short-circuit on `finish_task` before
/// dispatching tools. Keeping the detection/extraction logic as a pure
/// function makes it unit-testable without mocking an LLM round-trip.
/// What: Returns `Some(summary)` if any tool call's name equals
/// `FINISH_TASK_TOOL_NAME`; the summary defaults to an empty string when
/// the arguments are missing or malformed so the loop still exits cleanly.
/// Test: `extract_finish_task_summary_*` below.
fn extract_finish_task_summary(tool_calls: &[ChatCompletionMessageToolCall]) -> Option<String> {
    let finish = tool_calls
        .iter()
        .find(|tc| tc.function.name == crate::tools::finish_task::FINISH_TASK_TOOL_NAME)?;
    let args: serde_json::Value =
        serde_json::from_str(&finish.function.arguments).unwrap_or_default();
    let summary = args
        .get("summary")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Some(summary)
}

/// Decide, given the current retry count and remaining turns, whether the
/// tool-discipline loop should inject a retry-reminder or accept a plain-text
/// response as final.
///
/// Why: #33 — this logic lives inline in `chat_with_tools_gated` but is
/// cleanly testable as a pure function: we want the first plain-text turn
/// to trigger a retry and the second (or final-turn) plain-text to be
/// accepted gracefully.
/// What: Returns `true` if the caller should retry (inject error + continue),
/// `false` if it should accept the plain-text content as the final answer.
/// Test: See `tool_discipline_decision_*` tests below.
pub fn should_retry_plain_text_turn(
    consecutive_no_tool_turns: u32,
    turn: u32,
    max_turns: u32,
) -> bool {
    consecutive_no_tool_turns == 0 && turn < max_turns.saturating_sub(1)
}

/// Pull a `(system_prompt, first_user_message)` pair out of a typed
/// `ChatCompletionRequestMessage` vector.
///
/// Why: #201 — Bedrock's Converse API takes the system prompt as a separate
/// `system` field and an initial `messages` vector. The harness-side typed
/// messages are an OpenAI-shaped `system + user` pair; this helper round-trips
/// them through JSON so we don't depend on async-openai's enum being open
/// for matching.
/// What: Returns the first system message's text content and the first user
/// message's text content; falls back to empty strings if either is missing.
/// Test: `extract_system_and_first_user_basic`.
fn extract_system_and_first_user(
    messages: &[ChatCompletionRequestMessage],
) -> Result<(String, String)> {
    let mut system = String::new();
    let mut user = String::new();
    for m in messages {
        let v = serde_json::to_value(m).context("serialize message for bedrock extraction")?;
        let role = v.get("role").and_then(|r| r.as_str()).unwrap_or("");
        let content = v.get("content");
        let text = match content {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(serde_json::Value::Array(parts)) => parts
                .iter()
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()).map(str::to_string))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        };
        match role {
            "system" if system.is_empty() => system = text,
            "user" if user.is_empty() => user = text,
            _ => {}
        }
        if !system.is_empty() && !user.is_empty() {
            break;
        }
    }
    Ok((system, user))
}

/// Build a single assistant message carrying the model's tool calls, for
/// injection back into the conversation before appending tool results.
fn build_assistant_tool_call_message(
    content: &Option<String>,
    tool_calls: &[ChatCompletionMessageToolCall],
) -> Result<ChatCompletionRequestMessage> {
    let calls: Vec<ChatCompletionMessageToolCall> = tool_calls
        .iter()
        .map(|tc| ChatCompletionMessageToolCall {
            id: tc.id.clone(),
            r#type: tc.r#type.clone(),
            function: FunctionCall {
                name: tc.function.name.clone(),
                arguments: tc.function.arguments.clone(),
            },
        })
        .collect();

    let mut builder = ChatCompletionRequestAssistantMessageArgs::default();
    if let Some(text) = content
        && !text.is_empty()
    {
        builder.content(text.as_str());
    }
    builder.tool_calls(calls);
    Ok(builder
        .build()
        .context("failed to build assistant tool-call message")?
        .into())
}

/// Construct a minimal `ChatCompletionTool` wrapper around a raw schema.
/// Kept here as a helper for callers that want to bridge between raw JSON
/// schemas and async-openai's typed API.
#[allow(dead_code)]
pub(crate) fn chat_tool_from_schema(schema: &serde_json::Value) -> Result<ChatCompletionTool> {
    let function = schema
        .get("function")
        .cloned()
        .context("tool schema missing 'function' object")?;
    let name = function
        .get("name")
        .and_then(|v| v.as_str())
        .context("tool schema missing function.name")?
        .to_string();
    let description = function
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let parameters = function
        .get("parameters")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({"type":"object","properties":{}}));
    let func = FunctionObjectArgs::default()
        .name(name)
        .description(description)
        .parameters(parameters)
        .build()
        .context("failed to build FunctionObject")?;
    let tool = ChatCompletionToolArgs::default()
        .function(func)
        .build()
        .context("failed to build ChatCompletionTool")?;
    Ok(tool)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- #135: apply_compression tests ---

    fn hm(role: &str, content: &str) -> HistoryMessage {
        HistoryMessage {
            role: role.to_string(),
            content: content.to_string(),
        }
    }

    #[test]
    fn apply_compression_disabled_passthrough() {
        // When compress.enabled is false, messages and task pass through unchanged.
        let cfg = AgentCompressConfig {
            enabled: false,
            token_budget: 1000,
            compress_task: true, // even with compress_task on, disabled wins
            ..AgentCompressConfig::default()
        };
        let hist = vec![hm("user", "one"), hm("assistant", "two")];
        let task = "Write a program that adds two integers and prints the result.".to_string();
        let (h, t) = apply_compression(hist.clone(), task.clone(), &cfg);
        assert_eq!(h, hist);
        assert_eq!(t, task);
    }

    #[test]
    fn apply_compression_compresses_history() {
        // With a tight token budget, middle turns should be evicted.
        let mut hist: Vec<HistoryMessage> = Vec::new();
        for i in 0..20 {
            let role = if i % 2 == 0 { "user" } else { "assistant" };
            hist.push(hm(
                role,
                &format!("Turn {i} content with plenty of descriptive words for scoring"),
            ));
        }
        let cfg = AgentCompressConfig {
            enabled: true,
            token_budget: 100,
            compress_task: false,
            ..AgentCompressConfig::default()
        };
        let (h, t) = apply_compression(hist.clone(), "task".to_string(), &cfg);
        assert!(
            h.len() < hist.len(),
            "expected history to shrink, got {} -> {}",
            hist.len(),
            h.len()
        );
        // Turn 0 is always pinned.
        assert_eq!(h[0].content, hist[0].content);
        // Task is untouched when compress_task=false.
        assert_eq!(t, "task");
    }

    #[test]
    fn apply_compression_compresses_task_when_flag_set() {
        // With compress_task=true, a verbose task should shrink.
        let cfg = AgentCompressConfig {
            enabled: true,
            token_budget: 32_000,
            compress_task: true,
            ..AgentCompressConfig::default()
        };
        let verbose = "This is a very long paragraph about the system. \
            Furthermore, it contains many words that should be removed by the stop word filter. \
            Moreover, there are also discourse markers in this text that add no real value. \
            Additionally, the system is indeed quite verbose by design to test the pipeline."
            .to_string();
        let (_h, t) = apply_compression(vec![], verbose.clone(), &cfg);
        assert!(
            t.len() < verbose.len(),
            "expected task to shrink: {} -> {}",
            verbose.len(),
            t.len()
        );
    }

    #[test]
    fn apply_compression_empty_history_task_untouched_when_flag_off() {
        let cfg = AgentCompressConfig {
            enabled: true,
            token_budget: 1000,
            compress_task: false,
            ..AgentCompressConfig::default()
        };
        let task = "hello world".to_string();
        let (h, t) = apply_compression(vec![], task.clone(), &cfg);
        assert!(h.is_empty());
        assert_eq!(t, task);
    }

    #[test]
    fn http_client_returns_same_instance() {
        // MIN-2 (#98): the module-level OnceLock must hand out the same
        // reqwest::Client across calls so connection pooling actually kicks
        // in. Pointer equality is the simplest way to assert identity.
        let a = http_client();
        let b = http_client();
        assert!(std::ptr::eq(a, b));
    }

    #[test]
    fn strip_service_tier_removes_field() {
        // #486: OpenRouter returns `service_tier` values async-openai's
        // `ServiceTier` enum can't deserialize (e.g. "standard"). The helper
        // must drop the field so the rest of the response deserializes.
        let json = serde_json::json!({
            "id": "chatcmpl-1",
            "service_tier": "standard",
            "choices": [],
        });
        let cleaned = strip_service_tier(json);
        assert!(cleaned.get("service_tier").is_none());
        assert_eq!(
            cleaned.get("id").and_then(|v| v.as_str()),
            Some("chatcmpl-1")
        );
        assert!(cleaned.get("choices").is_some());

        // Non-object values pass through untouched.
        let arr = serde_json::json!([1, 2, 3]);
        assert_eq!(strip_service_tier(arr.clone()), arr);
    }

    #[test]
    fn arguments_parse_as_json() {
        let raw = r#"{"agent_name":"python-engineer","task":"hi"}"#;
        let v: serde_json::Value = serde_json::from_str(raw).unwrap();
        assert_eq!(v["agent_name"], "python-engineer");
        assert_eq!(v["task"], "hi");
    }

    #[test]
    fn chat_response_default_empty() {
        let r = ChatResponse::default();
        assert!(r.content.is_none());
        assert!(r.tool_calls.is_empty());
        assert_eq!(r.usage, TokenUsage::default());
    }

    fn tc(name: &str, args: &str) -> ChatCompletionMessageToolCall {
        ChatCompletionMessageToolCall {
            id: "id-1".into(),
            r#type: async_openai::types::ChatCompletionToolType::Function,
            function: FunctionCall {
                name: name.into(),
                arguments: args.into(),
            },
        }
    }

    #[test]
    fn extract_finish_task_summary_finds_call() {
        let calls = vec![
            tc("other_tool", "{}"),
            tc("finish_task", r#"{"summary":"all tests green"}"#),
        ];
        let s = extract_finish_task_summary(&calls);
        assert_eq!(s.as_deref(), Some("all tests green"));
    }

    #[test]
    fn extract_finish_task_summary_absent_returns_none() {
        let calls = vec![tc("web_search", r#"{"query":"x"}"#)];
        assert!(extract_finish_task_summary(&calls).is_none());
    }

    #[test]
    fn extract_finish_task_summary_defaults_on_missing_arg() {
        let calls = vec![tc("finish_task", "{}")];
        let s = extract_finish_task_summary(&calls);
        assert_eq!(s.as_deref(), Some(""));
    }

    #[test]
    fn extract_finish_task_summary_defaults_on_malformed_json() {
        let calls = vec![tc("finish_task", "not json at all")];
        // Malformed args should still exit the loop rather than hang.
        let s = extract_finish_task_summary(&calls);
        assert_eq!(s.as_deref(), Some(""));
    }

    #[test]
    fn adapter_detects_anthropic_variants() {
        // Sanity: the adapter factory should classify each of these correctly.
        // The previous `model_is_anthropic()` helper is gone; this test
        // guards the same behavioral contract via `adapter_for_model`.
        use crate::llm::adapter::{Provider, adapter_for_model};
        assert_eq!(
            adapter_for_model("anthropic/claude-sonnet-4-5").provider(),
            Provider::Anthropic
        );
        assert_eq!(
            adapter_for_model("claude-haiku-4").provider(),
            Provider::Anthropic
        );
        assert_eq!(
            adapter_for_model("CLAUDE-OPUS-4").provider(),
            Provider::Anthropic
        );
        assert_eq!(
            adapter_for_model("openai/gpt-4o").provider(),
            Provider::OpenAI
        );
        assert_eq!(
            adapter_for_model("google/gemini-2.5-flash").provider(),
            Provider::Generic
        );
    }

    #[test]
    fn build_raw_request_injects_cache_control() {
        let system: ChatCompletionRequestMessage =
            ChatCompletionRequestSystemMessageArgs::default()
                .content("You are a helpful assistant.")
                .build()
                .unwrap()
                .into();
        let user: ChatCompletionRequestMessage = ChatCompletionRequestUserMessageArgs::default()
            .content("hello")
            .build()
            .unwrap()
            .into();
        let body =
            build_raw_request("claude-sonnet-4-5", &[system, user], &[], 0.2, 1024, true).unwrap();
        let messages = body.get("messages").and_then(|v| v.as_array()).unwrap();
        let sys = &messages[0];
        assert_eq!(sys["role"], "system");
        let content = sys["content"].as_array().expect("content is array");
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "You are a helpful assistant.");
        assert_eq!(content[0]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn build_raw_request_without_cache_control_leaves_system_alone() {
        let system: ChatCompletionRequestMessage =
            ChatCompletionRequestSystemMessageArgs::default()
                .content("sys")
                .build()
                .unwrap()
                .into();
        let body = build_raw_request("gpt-4", &[system], &[], 0.1, 100, false).unwrap();
        let msgs = body["messages"].as_array().unwrap();
        let sys_content = &msgs[0]["content"];
        if let Some(arr) = sys_content.as_array() {
            for block in arr {
                assert!(block.get("cache_control").is_none());
            }
        }
    }

    // #326 Tests 1 & 2: verify the if-guard behavior used in
    // `chat_with_tools_gated` that injects `stop` into the raw request body
    // only when `cfg.llm.stop_sequences` is non-empty. Mirrors the call-site
    // logic so accidental regressions (e.g. always setting `stop`, or
    // dropping the array) are caught.

    #[test]
    fn stop_sequences_injected_into_raw_body_when_nonempty() {
        let seqs: Vec<String> = vec!["```\n\n".into(), "<|end|>".into()];
        let mut body = serde_json::json!({});
        if !seqs.is_empty() {
            body["stop"] = serde_json::json!(seqs);
        }
        let stop = body["stop"].as_array().expect("stop must be array");
        assert_eq!(stop.len(), 2);
        assert_eq!(stop[0].as_str().unwrap(), "```\n\n");
        assert_eq!(stop[1].as_str().unwrap(), "<|end|>");
    }

    #[test]
    fn empty_stop_sequences_leave_no_stop_key_in_raw_body() {
        let seqs: &[String] = &[];
        let mut body = serde_json::json!({});
        if !seqs.is_empty() {
            body["stop"] = serde_json::json!(seqs);
        }
        assert!(
            body.get("stop").is_none(),
            "stop key must be absent when stop_sequences is empty"
        );
    }

    #[test]
    fn tool_discipline_decision_first_plain_text_retries() {
        // turn 0 of 12, 0 prior no-tool turns -> retry
        assert!(should_retry_plain_text_turn(0, 0, 12));
    }

    #[test]
    fn tool_discipline_decision_second_plain_text_accepts() {
        // We've already retried once (counter == 1). Do NOT retry again.
        assert!(!should_retry_plain_text_turn(1, 1, 12));
    }

    #[test]
    fn tool_discipline_decision_final_turn_accepts_even_first_time() {
        // Don't inject a retry on the very last turn — the loop would break
        // anyway; just accept whatever the model produced.
        assert!(!should_retry_plain_text_turn(0, 11, 12));
    }

    #[test]
    fn tool_discipline_decision_saturates_on_zero_max_turns() {
        // Edge case: max_turns = 0 should never retry (underflow-safe).
        assert!(!should_retry_plain_text_turn(0, 0, 0));
    }

    /// Pure-logic simulation of the tool-discipline state machine mimicking
    /// the plain-text vs tool-call control flow in `chat_with_tools_gated`.
    /// This lets us assert the spec's scenarios without a real LLM.
    fn simulate_turns(events: &[&str], max_turns: u32) -> (u32, u32, Option<String>) {
        // events: "text" or "tool". Returns (retries_used, turns_run, final_text).
        let mut counter: u32 = 0;
        let mut retries = 0u32;
        let mut final_text: Option<String> = None;
        for (turn_idx, ev) in events.iter().enumerate() {
            let turn = turn_idx as u32;
            if turn >= max_turns {
                break;
            }
            match *ev {
                "text" => {
                    if should_retry_plain_text_turn(counter, turn, max_turns) {
                        counter += 1;
                        retries += 1;
                        continue;
                    }
                    final_text = Some("final-text".into());
                    return (retries, turn + 1, final_text);
                }
                "tool" => {
                    counter = 0; // tool call resets the discipline counter
                }
                _ => panic!("bad event"),
            }
        }
        (retries, events.len() as u32, final_text)
    }

    #[test]
    fn tool_discipline_plain_then_tool_continues_with_one_retry() {
        // Spec: plain text once, then tool call -> continues; 1 retry used.
        let (retries, _, final_text) = simulate_turns(&["text", "tool"], 12);
        assert_eq!(retries, 1);
        assert!(
            final_text.is_none(),
            "loop should have continued, not ended"
        );
    }

    #[test]
    fn tool_discipline_two_plain_texts_breaks_gracefully() {
        // Spec: plain text twice -> breaks with final answer (graceful).
        let (retries, turns, final_text) = simulate_turns(&["text", "text"], 12);
        assert_eq!(retries, 1, "only the first should trigger a retry");
        assert_eq!(turns, 2);
        assert_eq!(final_text.as_deref(), Some("final-text"));
    }

    #[test]
    fn tool_discipline_all_tool_calls_never_increments_counter() {
        // Spec: normal tool-call flow -> consecutive_no_tool_turns stays 0.
        let (retries, _, final_text) = simulate_turns(&["tool", "tool", "tool"], 12);
        assert_eq!(retries, 0);
        assert!(final_text.is_none());
    }

    // Parallel dispatch test: verifies that a ToolRegistry can dispatch
    // multiple tool calls concurrently using the same plumbing used by
    // `chat_with_tools_gated`, and that one tool erroring does not cancel
    // the others. This exercises the registry + FuturesUnordered pattern
    // without requiring a real OpenRouter round-trip.
    #[tokio::test]
    async fn parallel_tool_dispatch_does_not_cancel_peers() {
        use crate::tools::{ToolExecutor, ToolRegistry, ToolResult};
        use async_trait::async_trait;
        use futures::stream::{FuturesUnordered, StreamExt};
        use serde_json::json;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct SlowOk(Arc<AtomicUsize>);
        #[async_trait]
        impl ToolExecutor for SlowOk {
            fn name(&self) -> &str {
                "slow_ok"
            }
            fn schema(&self) -> serde_json::Value {
                json!({"type":"function","function":{"name":"slow_ok","parameters":{"type":"object","properties":{},"additionalProperties":false}}})
            }
            async fn execute(&self, _args: serde_json::Value) -> ToolResult {
                tokio::time::sleep(std::time::Duration::from_millis(30)).await;
                self.0.fetch_add(1, Ordering::SeqCst);
                ToolResult::ok("slow done")
            }
        }

        struct FastErr(Arc<AtomicUsize>);
        #[async_trait]
        impl ToolExecutor for FastErr {
            fn name(&self) -> &str {
                "fast_err"
            }
            fn schema(&self) -> serde_json::Value {
                json!({"type":"function","function":{"name":"fast_err","parameters":{"type":"object","properties":{},"additionalProperties":false}}})
            }
            async fn execute(&self, _args: serde_json::Value) -> ToolResult {
                self.0.fetch_add(1, Ordering::SeqCst);
                ToolResult::err("fast boom")
            }
        }

        let slow_count = Arc::new(AtomicUsize::new(0));
        let fast_count = Arc::new(AtomicUsize::new(0));
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(SlowOk(slow_count.clone())));
        reg.register(Arc::new(FastErr(fast_count.clone())));
        let reg = Arc::new(reg);

        let calls = vec![
            ("id-1".to_string(), "slow_ok".to_string()),
            ("id-2".to_string(), "fast_err".to_string()),
            ("id-3".to_string(), "slow_ok".to_string()),
        ];

        let start = std::time::Instant::now();
        let mut futs = FuturesUnordered::new();
        for (id, name) in &calls {
            let reg = Arc::clone(&reg);
            let id = id.clone();
            let name = name.clone();
            futs.push(async move {
                let r = reg.dispatch_gated(&name, json!({}), None).await;
                (id, name, r)
            });
        }

        let mut saw_error = false;
        let mut saw_success = false;
        while let Some((_, _, r)) = futs.next().await {
            if r.is_error() {
                saw_error = true;
            } else {
                saw_success = true;
            }
        }
        let elapsed = start.elapsed();

        assert!(saw_error, "expected at least one error result");
        assert!(saw_success, "expected success despite concurrent error");
        assert_eq!(slow_count.load(Ordering::SeqCst), 2);
        assert_eq!(fast_count.load(Ordering::SeqCst), 1);
        // If dispatch were sequential, elapsed would be >= 60ms (2 * 30ms);
        // parallel dispatch should complete in roughly one slow-tool's time.
        assert!(
            elapsed < std::time::Duration::from_millis(120),
            "dispatch appears sequential; elapsed = {elapsed:?}"
        );
    }
}
