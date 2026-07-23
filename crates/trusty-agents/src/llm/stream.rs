//! Token-level streaming for the conversational / persona chat turn.
//!
//! Why: The chat GUI's default surface (the generic Assistant persona and the
//! no-tools conversational fast path) previously waited for the ENTIRE model
//! response before rendering anything — a multi-second dead stare on long
//! replies. `trusty_common::chat::OpenRouterProvider` already speaks the
//! streaming `/chat/completions` SSE protocol and yields `ChatEvent::Delta`
//! fragments as tokens arrive; this module adapts that provider stream onto
//! the crate's own event bus so the browser can render the reply as it is
//! produced, while still returning the fully-assembled text so history,
//! `PmResponse`, and responder attribution behave EXACTLY as the
//! non-streaming path.
//! What: [`drive_delta_stream`] is the pure, testable batching core — it
//! consumes a `ChatEvent` receiver, coalesces text deltas to a wall-clock
//! cadence (so a fast provider produces ~10-20 UI frames/sec instead of one
//! per token), invokes an `emit(text, done)` sink per flush, and returns the
//! assembled content. [`stream_reply`] wires that core to a live
//! `OpenRouterProvider` and publishes each flush as an
//! [`Event::AgentMessageDelta`] on the process bus (relayed to the browser by
//! the `/api/events` SSE stream). [`streaming_supported`] is the capability
//! gate: streaming is used only for OpenRouter-transport models (not Bedrock,
//! not Fireworks, not Anthropic-direct) and can be disabled wholesale via
//! `TAGENT_CHAT_STREAMING=0`. Non-streaming providers keep their current
//! behavior unchanged — callers fall back to the blocking chat path.
//! Test: `drive_delta_stream_*` unit tests feed a mocked `ChatEvent` channel
//! (ordered fragments, done flag, assembled-text equality, batching); the
//! gate is covered by `streaming_supported_*`.

use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use tokio::sync::mpsc;
use trusty_common::ChatMessage;
use trusty_common::chat::{ChatEvent, ChatProvider, OpenRouterProvider, ToolCall};

use crate::ctrl::state::ConversationTurn;
use crate::events::{self, Event};
use crate::llm::adapter::{Provider, adapter_for_model};

/// Default flush cadence for streamed deltas.
///
/// Why: Publishing one bus event per token would saturate the broadcast
/// channel and the SSE stream on a fast provider (hundreds of frames/sec).
/// Coalescing to ~60ms keeps the reply visually smooth (~16 frames/sec) while
/// bounding bus pressure. Tunable per call; tests pass `Duration::ZERO` to get
/// one flush per delta deterministically.
/// What: 60 milliseconds.
pub const DEFAULT_STREAM_CADENCE: Duration = Duration::from_millis(60);

/// Bounded capacity of the provider→consumer `ChatEvent` channel.
///
/// Why: Back-pressures a provider that outruns the consumer without unbounded
/// memory growth. 256 comfortably absorbs bursty SSE frames between flushes.
const STREAM_CHANNEL_CAPACITY: usize = 256;

/// Assembled result of consuming a streaming chat response.
///
/// Why: The caller needs the full text (to store in history / return in
/// `PmResponse`) plus any tool calls the provider surfaced and a terminal
/// error, without re-deriving them from the emitted deltas.
/// What: `content` is every `Delta` concatenated in order; `tool_calls` are
/// fully-accumulated provider tool invocations; `error` is `Some` when the
/// stream terminated with `ChatEvent::Error`.
#[derive(Debug, Default, Clone)]
pub struct StreamAssembly {
    /// Full assistant text — the concatenation of every `Delta` in order.
    pub content: String,
    /// Tool invocations the provider streamed (empty for a tools-off turn).
    pub tool_calls: Vec<ToolCall>,
    /// Human-readable message when the stream ended via `ChatEvent::Error`.
    pub error: Option<String>,
}

/// Build the provider message list for a single conversational turn.
///
/// Why: Both the persona path and the no-tools conversational fast path have
/// the same three ingredients — a fully-rendered system prompt, prior
/// `ConversationTurn` history, and the current user input — so message
/// construction lives here once.
/// What: Emits `system`, then interleaved `user`/`assistant` history turns, then
/// the current `user` message, as `trusty_common::ChatMessage`s (the wire type
/// `ChatProvider::chat_stream` consumes).
/// Test: `build_messages_shapes_roles` asserts the role ordering.
pub fn build_messages(
    system_prompt: &str,
    history: &[ConversationTurn],
    user_input: &str,
) -> Vec<ChatMessage> {
    let mut messages = Vec::with_capacity(2 + history.len() * 2);
    messages.push(text_message("system", system_prompt));
    for turn in history {
        messages.push(text_message("user", &turn.user));
        messages.push(text_message("assistant", &turn.assistant));
    }
    messages.push(text_message("user", user_input));
    messages
}

/// Construct a plain text `ChatMessage` (no tool fields).
fn text_message(role: &str, content: &str) -> ChatMessage {
    ChatMessage {
        role: role.to_string(),
        content: content.to_string(),
        tool_call_id: None,
        tool_calls: None,
    }
}

/// Whether the token-streaming path applies to `model`.
///
/// Why: `OpenRouterProvider` only speaks to the OpenRouter transport. Bedrock
/// (AWS SDK), Fireworks (own base URL / credential), and the Anthropic-direct
/// path all reach the model through a different client, so streaming through
/// the OpenRouter provider would target the wrong endpoint. The
/// `TAGENT_CHAT_STREAMING` env var is a global kill switch for the demo.
/// What: Returns `true` only when streaming is enabled AND `use_anthropic_direct`
/// is false AND the model's adapter family routes over the OpenRouter client
/// (everything except `Bedrock` / `Fireworks`).
/// Test: `streaming_supported_gates_on_provider`,
/// `streaming_supported_respects_anthropic_direct`, `parse_streaming_flag_table`.
pub fn streaming_supported(model: &str, use_anthropic_direct: bool) -> bool {
    if use_anthropic_direct || !streaming_enabled() {
        return false;
    }
    !matches!(
        adapter_for_model(model).provider(),
        Provider::Bedrock | Provider::Fireworks
    )
}

/// Read the `TAGENT_CHAT_STREAMING` kill switch (default ON).
///
/// Why: Lets an operator disable streaming globally without a rebuild if a
/// provider misbehaves during the demo.
/// What: Delegates parsing to [`parse_streaming_flag`] so the policy is pure
/// and unit-testable without mutating process env.
fn streaming_enabled() -> bool {
    parse_streaming_flag(std::env::var("TAGENT_CHAT_STREAMING").ok().as_deref())
}

/// Pure policy for the `TAGENT_CHAT_STREAMING` kill switch.
///
/// Why: Isolating the parse keeps `streaming_enabled` a one-liner and lets the
/// enable/disable table be pinned by tests without env-var races (which this
/// crate serializes precisely because they are flaky).
/// What: `None` (unset) → enabled. Any of `0`/`false`/`off`/`no`
/// (case-insensitive, trimmed) → disabled. Everything else → enabled.
/// Test: `parse_streaming_flag_table`.
fn parse_streaming_flag(value: Option<&str>) -> bool {
    match value {
        None => true,
        Some(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        ),
    }
}

/// Consume a `ChatEvent` stream, batching text deltas and invoking `emit`.
///
/// Why: This is the pure heart of streaming — separated from the live provider
/// and the event bus so it can be unit-tested deterministically with a mocked
/// channel. It owns the batching policy (flush when `cadence` has elapsed since
/// the last flush) and the terminal-marker contract the GUI relies on.
/// What: For each `Delta`, appends to both the running assembly and a pending
/// buffer; flushes the buffer via `emit(text, false)` once `cadence` has
/// elapsed. `ToolCall`s accumulate; `Error` records the message and stops;
/// `Done` (or a closed channel) stops. On exit it flushes any residual buffer
/// as a non-terminal delta, then emits exactly one terminal `emit("", true)`
/// so the consumer has an unambiguous end-of-stream signal (carrying empty
/// text so it never double-appends). Returns the [`StreamAssembly`].
/// Test: `drive_delta_stream_flushes_each_delta_at_zero_cadence`,
/// `drive_delta_stream_batches_under_cadence`,
/// `drive_delta_stream_assembles_full_text`,
/// `drive_delta_stream_surfaces_error`,
/// `drive_delta_stream_collects_tool_calls`.
pub async fn drive_delta_stream<F>(
    mut rx: mpsc::Receiver<ChatEvent>,
    cadence: Duration,
    mut emit: F,
) -> StreamAssembly
where
    F: FnMut(String, bool),
{
    let mut assembly = StreamAssembly::default();
    let mut buffer = String::new();
    let mut last_flush = Instant::now();

    while let Some(event) = rx.recv().await {
        match event {
            ChatEvent::Delta(chunk) => {
                assembly.content.push_str(&chunk);
                buffer.push_str(&chunk);
                if !buffer.is_empty() && last_flush.elapsed() >= cadence {
                    emit(std::mem::take(&mut buffer), false);
                    last_flush = Instant::now();
                }
            }
            ChatEvent::ToolCall(call) => assembly.tool_calls.push(call),
            ChatEvent::Error(message) => {
                assembly.error = Some(message);
                break;
            }
            ChatEvent::Done => break,
        }
    }

    if !buffer.is_empty() {
        emit(std::mem::take(&mut buffer), false);
    }
    // Terminal marker: empty text + done=true so the GUI finalizes without
    // appending anything.
    emit(String::new(), true);
    assembly
}

/// Stream a conversational reply from OpenRouter, publishing deltas on the bus.
///
/// Why: The live wiring — resolves the OpenRouter credential, spawns the
/// provider's SSE pump, and drives [`drive_delta_stream`] with a sink that
/// publishes each flushed batch as an [`Event::AgentMessageDelta`]. The
/// `agent` label rides every delta so per-message speaker attribution (#3739)
/// stays truthful mid-stream. Returns the assembled full text so the caller's
/// downstream behavior (history append, `PmResponse`, attribution) is
/// byte-for-byte identical to the blocking path.
/// What: Builds an [`OpenRouterProvider`] for `model`, runs `chat_stream` with
/// an empty tools list (tools-off conversational turn), and publishes deltas
/// tagged with `session_id`/`agent`. Errors (empty key, HTTP failure,
/// mid-stream error) propagate as `Err` so the caller can fall back to the
/// blocking chat path.
/// Test: exercised end-to-end against a live provider; the batching/assembly
/// contract is unit-tested via [`drive_delta_stream`].
pub async fn stream_reply(
    model: &str,
    messages: Vec<ChatMessage>,
    session_id: &str,
    agent: &str,
    cadence: Duration,
) -> Result<StreamAssembly> {
    let api_key =
        trusty_common::inference::credentials::resolve_key("openrouter").unwrap_or_default();
    if api_key.is_empty() {
        return Err(anyhow!(
            "streaming requires an OpenRouter API key; none resolved"
        ));
    }

    let provider = OpenRouterProvider::new(api_key, model.to_string());
    let (tx, rx) = mpsc::channel::<ChatEvent>(STREAM_CHANNEL_CAPACITY);

    // Spawn the provider's SSE pump; it writes `ChatEvent`s into `tx`.
    let pump = tokio::spawn(async move { provider.chat_stream(messages, Vec::new(), tx).await });

    let sid = session_id.to_string();
    let agent_label = agent.to_string();
    let assembly = drive_delta_stream(rx, cadence, |text, done| {
        events::publish(Event::AgentMessageDelta {
            session_id: sid.clone(),
            agent: agent_label.clone(),
            text,
            done,
        });
    })
    .await;

    match pump.await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(e.context("OpenRouter chat_stream failed")),
        Err(join_err) => return Err(anyhow!("streaming task panicked: {join_err}")),
    }
    if let Some(message) = &assembly.error {
        return Err(anyhow!("stream terminated with error: {message}"));
    }

    Ok(assembly)
}

#[cfg(test)]
#[path = "stream_tests.rs"]
mod tests;
