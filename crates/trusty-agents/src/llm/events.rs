//! LLM lifecycle event + usage-record emission helpers.
//!
//! Why: `chat()` and `chat_with_tools_gated()` don't thread a session id or
//! agent id through their signatures, but the SSE event stream and the
//! per-dispatch usage log both need them. Centralizing the emit shapes here
//! keeps every call site consistent (env-var fallbacks, field conventions)
//! and out of the request-orchestration code.
//! What: Resolves correlation ids from env, publishes paired
//! `LlmRequested`/`LlmResponded` events, appends `UsageRecord`s, and derives
//! the `runner` label + `task_prefix` for those records.
//! Test: Side-effect-only (event bus + detached file write). Covered
//! indirectly by `crate::usage::tests` and end-to-end dispatch integration.

use async_openai::types::ChatCompletionRequestMessage;

use super::adapter::{self, ModelAdapter};
use crate::events::{self, Event};

/// Resolve the active session id for LLM lifecycle events.
///
/// Why: `chat()` and `chat_with_tools_gated()` don't take a session id
/// parameter, but `Event::LlmRequested` / `Event::LlmResponded` need one so
/// SSE subscribers can scope by task. The harness already stamps
/// `TAGENT_RUN_ID` on the process at startup (see `main.rs`), so falling
/// back to it gives every LLM call the right correlation id without
/// threading it through dozens of signatures.
/// What: Returns `TAGENT_RUN_ID` if set, else an empty string (events with
/// empty session_id remain visible — SSE just doesn't filter them).
fn current_session_id() -> String {
    crate::env_compat::env_var("TAGENT_RUN_ID", "OPEN_MPM_RUN_ID").unwrap_or_default()
}

/// Emit `Event::LlmRequested` and return the start instant for latency
/// measurement.
///
/// Why: Centralises the emit shape so every call site uses the same field
/// conventions (empty `agent_name` for PM calls, TAGENT_RUN_ID fallback).
/// What: Publishes the event and returns `Instant::now()` so the caller
/// can compute `latency_ms` for the paired `LlmResponded`.
/// Test: Side-effect-only; exercised by dispatch integration.
pub(super) fn emit_llm_requested(model: &str, prompt_tokens: Option<u32>) -> std::time::Instant {
    events::publish(Event::LlmRequested {
        session_id: current_session_id(),
        agent_name: crate::env_compat::env_var("TAGENT_AGENT_ID", "OPEN_MPM_AGENT_ID")
            .unwrap_or_default(),
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
/// Test: Side-effect-only; exercised by dispatch integration.
pub(super) fn emit_llm_responded(
    model: &str,
    started: std::time::Instant,
    completion_tokens: Option<u32>,
    prompt_tokens: Option<u32>,
) {
    let latency_ms = started.elapsed().as_millis() as u64;
    events::publish(Event::LlmResponded {
        session_id: current_session_id(),
        agent_name: crate::env_compat::env_var("TAGENT_AGENT_ID", "OPEN_MPM_AGENT_ID")
            .unwrap_or_default(),
        model: model.to_string(),
        completion_tokens,
        latency_ms,
    });
    if prompt_tokens.is_some() {
        events::publish(Event::LlmRequested {
            session_id: current_session_id(),
            agent_name: crate::env_compat::env_var("TAGENT_AGENT_ID", "OPEN_MPM_AGENT_ID")
                .unwrap_or_default(),
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
/// What: Resolves the agent name from `TAGENT_AGENT_ID` (falling back to
/// `"unknown"`), constructs a `UsageRecord` with `chrono::Utc::now()`, and
/// appends to `.trusty-agents/state/usage.jsonl` via `crate::usage::append_usage`.
/// Spawns the actual file write as a detached tokio task so we never block
/// the dispatch loop on disk I/O.
/// Test: Indirectly via `crate::usage::tests::append_usage_*` covering the
/// append helper itself; the call-site spawn is exercised end-to-end by
/// any integration that triggers a dispatch.
pub(super) fn record_dispatch_usage(
    model: &str,
    runner: &str,
    input_tokens: u32,
    output_tokens: u32,
    duration_ms: u64,
    task_for_prefix: &str,
) {
    let agent = crate::env_compat::env_var("TAGENT_AGENT_ID", "OPEN_MPM_AGENT_ID")
        .unwrap_or_else(|_| "unknown".to_string());
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
/// Test: Trivial branch mapping; exercised via dispatch integration.
pub(super) fn runner_label_for(
    adapter: &dyn ModelAdapter,
    route_native_anthropic: bool,
) -> &'static str {
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
///
/// Why: The usage log entry should be human-recognizable by the task it
/// belongs to; the first user message is the most stable identifier.
/// What: Serializes each message to JSON, returns the text content of the
/// first `role:user` message (string or joined array parts).
/// Test: Pure JSON walk; exercised via dispatch integration.
pub(super) fn first_user_text_for_prefix(messages: &[ChatCompletionRequestMessage]) -> String {
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
