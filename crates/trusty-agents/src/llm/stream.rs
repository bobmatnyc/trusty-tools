//! Token-level streaming for the conversational / persona chat turn.
//!
//! Why: The chat GUI's default surface (the generic Assistant persona and the
//! no-tools conversational fast path) previously waited for the ENTIRE model
//! response before rendering anything — a multi-second dead stare on long
//! replies. `trusty_common::chat::OpenRouterProvider` and (issue #3767)
//! `trusty_common::chat::BedrockProvider` both speak native token streaming
//! and yield `ChatEvent::Delta` fragments as tokens arrive; this module
//! adapts that provider stream onto the crate's own event bus so the browser
//! can render the reply as it is produced, while still returning the
//! fully-assembled text so history, `PmResponse`, and responder attribution
//! behave EXACTLY as the non-streaming path.
//! What: [`drive_delta_stream`] is the pure, testable batching core — it
//! consumes a `ChatEvent` receiver, coalesces text deltas to a wall-clock
//! cadence (so a fast provider produces ~10-20 UI frames/sec instead of one
//! per token), invokes an `emit(text, done)` sink per flush, and returns the
//! assembled content plus any [`ChatEvent::Usage`] the provider reported (#3767
//! — Bedrock's `ConverseStream` reports it exactly once, in a terminal event,
//! so it is captured here rather than dropped). [`stream_reply`] picks a live
//! provider by the model's routed adapter — `BedrockProvider` for
//! `bedrock/*`, `OpenRouterProvider` otherwise — and publishes each flush as
//! an [`Event::AgentMessageDelta`] on the process bus (relayed to the browser
//! by the `/api/events` SSE stream). [`streaming_supported`] is the
//! capability gate: streaming is used for every adapter family except
//! Fireworks (no streaming adapter yet) and Anthropic-direct, and can be
//! disabled wholesale via `TAGENT_CHAT_STREAMING=0`. Non-streaming providers
//! keep their current behavior unchanged — callers fall back to the blocking
//! chat path.
//! Test: `drive_delta_stream_*` unit tests feed a mocked `ChatEvent` channel
//! (ordered fragments, done flag, assembled-text equality, batching); the
//! gate is covered by `streaming_supported_*`.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use tokio::sync::mpsc;
use trusty_common::ChatMessage;
use trusty_common::chat::{
    ChatEvent, ChatProvider, ChatUsage, OpenRouterProvider, SamplingParams, ToolCall,
};

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
/// fully-accumulated provider tool invocations; `usage` is the provider's
/// token tally when it reported one (#3767 — some providers, e.g. Bedrock's
/// `ConverseStream`, report usage exactly once in a terminal event rather
/// than per-delta; `None` when the provider never emitted `ChatEvent::Usage`,
/// which remains true for OpenRouter today); `error` is `Some` when the
/// stream terminated with `ChatEvent::Error`.
#[derive(Debug, Default, Clone)]
pub struct StreamAssembly {
    /// Full assistant text — the concatenation of every `Delta` in order.
    pub content: String,
    /// Tool invocations the provider streamed (empty for a tools-off turn).
    pub tool_calls: Vec<ToolCall>,
    /// Token usage the provider reported, if any (#3767).
    pub usage: Option<ChatUsage>,
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
/// Why: `OpenRouterProvider` and `BedrockProvider` (issue #3767 — Bedrock's
/// `ConverseStream`, wired via `stream_reply`'s provider dispatch below) both
/// speak native streaming; Fireworks (own base URL / credential, no streaming
/// adapter yet) and the Anthropic-direct path reach the model through a
/// different client that doesn't. The `TAGENT_CHAT_STREAMING` env var is a
/// global kill switch for the demo.
/// What: Returns `true` only when streaming is enabled AND `use_anthropic_direct`
/// is false AND the model's adapter family has a streaming provider
/// (`Bedrock` as of #3767, plus everything except `Fireworks`).
/// Test: `streaming_supported_gates_on_provider`,
/// `streaming_supported_allows_bedrock`,
/// `streaming_supported_respects_anthropic_direct`, `parse_streaming_flag_table`.
pub fn streaming_supported(model: &str, use_anthropic_direct: bool) -> bool {
    if use_anthropic_direct || !streaming_enabled() {
        return false;
    }
    !matches!(adapter_for_model(model).provider(), Provider::Fireworks)
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
/// elapsed. `ToolCall`s accumulate; `Usage` (#3767) is recorded onto
/// `assembly.usage` — never dropped, so a downstream cost-reporting consumer
/// sees it — without affecting batching or the terminal marker; `Error`
/// records the message and stops; `Done` (or a closed channel) stops. On exit
/// it flushes any residual buffer as a non-terminal delta, then emits exactly
/// one terminal `emit("", true)` so the consumer has an unambiguous
/// end-of-stream signal (carrying empty text so it never double-appends).
/// Returns the [`StreamAssembly`].
/// Test: `drive_delta_stream_flushes_each_delta_at_zero_cadence`,
/// `drive_delta_stream_batches_under_cadence`,
/// `drive_delta_stream_assembles_full_text`,
/// `drive_delta_stream_surfaces_error`,
/// `drive_delta_stream_collects_tool_calls`,
/// `drive_delta_stream_captures_usage`.
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
            // #3767: never silently drop usage — a downstream cost-reporting
            // consumer depends on it surviving the streaming path.
            ChatEvent::Usage(usage) => assembly.usage = Some(usage),
            ChatEvent::Error(message) => {
                assembly.error = Some(message);
                break;
            }
            ChatEvent::Done => break,
            // `ChatEvent` is `#[non_exhaustive]` (trusty-common 0.27.0): a wildcard
            // keeps a future variant from breaking this crate's build.
            _ => {}
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

/// Stream a conversational reply, publishing deltas on the bus.
///
/// Why: The live wiring — picks a provider by the model's routed adapter,
/// spawns its event pump, and drives [`drive_delta_stream`] with a sink that
/// publishes each flushed batch as an [`Event::AgentMessageDelta`]. The
/// `agent` label rides every delta so per-message speaker attribution (#3739)
/// stays truthful mid-stream. Returns the assembled full text so the caller's
/// downstream behavior (history append, `PmResponse`, attribution) is
/// byte-for-byte identical to the blocking path.
/// What: `bedrock/*` models (issue #3767) route to [`stream_reply_bedrock`];
/// everything else builds an [`OpenRouterProvider`] and runs `chat_stream`
/// with an empty tools list (tools-off conversational turn). Errors (empty
/// key, HTTP failure, mid-stream error, missing AWS credentials) propagate as
/// `Err` so the caller can fall back to the blocking chat path.
///
/// `sampling` (issue #3758, and #3767 for the Bedrock branch) MUST carry the
/// same temperature / token ceiling / stop sequences the caller's blocking
/// path sends for this turn. Without it the streamed reply silently ran on
/// provider defaults, so its style, verbosity, and stopping behaviour were
/// not equivalent to the fallback — and `LlmConfig::stop_sequences`
/// documents itself as forwarded on the OpenRouter path, which the streaming
/// path was quietly not honouring.
/// Test: exercised end-to-end against a live provider; the batching/assembly
/// contract is unit-tested via [`drive_delta_stream`]; the provider-dispatch
/// branch is covered by `streaming_supported_allows_bedrock` at the gate
/// level (constructing a live `BedrockProvider` here needs network/credential
/// access, out of scope for a unit test — see `bedrock_impl`'s own tests for
/// the Bedrock-specific event mapping).
pub async fn stream_reply(
    model: &str,
    messages: Vec<ChatMessage>,
    session_id: &str,
    agent: &str,
    cadence: Duration,
    sampling: SamplingParams,
) -> Result<StreamAssembly> {
    if adapter_for_model(model).provider() == Provider::Bedrock {
        return stream_reply_bedrock(model, messages, session_id, agent, cadence, sampling).await;
    }

    let api_key = trusty_common::credentials::resolve_key("openrouter").unwrap_or_default();
    if api_key.is_empty() {
        return Err(anyhow!(
            "streaming requires an OpenRouter API key; none resolved"
        ));
    }

    // #3758: forward the blocking path's sampling knobs onto the stream.
    let provider = OpenRouterProvider::new(api_key, model.to_string()).with_sampling(sampling);
    stream_with_provider(provider, messages, session_id, agent, cadence).await
}

/// Stream a conversational reply from Bedrock's `ConverseStream` (issue
/// #3767), publishing deltas on the bus.
///
/// Why: mirrors [`stream_reply`]'s OpenRouter branch, but Bedrock is reached
/// through the AWS SDK client (standard credential chain: env vars,
/// `~/.aws/credentials`, IAM roles, SSO) rather than an HTTP request with a
/// bearer token, so construction needs the `bedrock/` prefix stripped to the
/// bare model id instead of an API key.
/// What: strips the `bedrock/` prefix (mirrors
/// `adapter::adapter_for_model`'s `BedrockAdapter` routing), builds a
/// [`trusty_common::chat::BedrockProvider`] via
/// [`trusty_common::chat::BedrockProvider::new`], forwards `sampling` (#3758
/// parity) via `with_sampling`, and delegates to [`stream_with_provider`] for
/// the shared batching/failure-guard/usage-recording logic. `BedrockProvider`
/// construction itself does not require live credentials — only the
/// subsequent `chat_stream` call does — so a caller with no AWS credentials
/// configured (as on this machine — `AWS_PROFILE`/`AWS_REGION` are
/// set-but-empty) still gets `Err` from *this* function (via
/// `stream_with_provider`'s pump-join propagation), letting the caller fall
/// back to the blocking chat path exactly like any other stream failure.
/// Test: exercised end-to-end against a live Bedrock endpoint (not runnable
/// on this machine — no usable AWS credentials); the Bedrock event → `ChatEvent`
/// mapping is unit-tested in `trusty_common::chat::bedrock_impl`
/// (`handle_stream_event` tests), and the batching/assembly/usage-capture
/// contract this function shares with the OpenRouter branch is unit-tested
/// via [`drive_delta_stream`] and `stream_with_provider_*`.
async fn stream_reply_bedrock(
    model: &str,
    messages: Vec<ChatMessage>,
    session_id: &str,
    agent: &str,
    cadence: Duration,
    sampling: SamplingParams,
) -> Result<StreamAssembly> {
    let model_id = model.strip_prefix("bedrock/").unwrap_or(model);
    let provider = trusty_common::chat::BedrockProvider::new(model_id, None)
        .await
        .context("build BedrockProvider for streaming")?
        .with_sampling(sampling);
    stream_with_provider(provider, messages, session_id, agent, cadence).await
}

/// Provider-generic streaming core: drive any [`ChatProvider`], publish deltas,
/// and enforce the failed-stream guard.
///
/// Why: Split out of [`stream_reply`] so the error branches can be unit-tested
/// with a controllable fake provider (no live API key) — and so the
/// failed-stream detection lives in exactly one place. The upstream SSE pump
/// (`trusty_common::chat`) now emits `ChatEvent::Error` for a mid-stream error
/// frame, a truncated EOF, and an unusable non-SSE `200` (issue #3757 — those
/// three previously arrived as a clean `Done`), so the partial-then-truncated
/// case IS caught here: the error surfaces through `assembly.error` and the
/// caller falls back to the blocking chat path instead of rendering a cut-off
/// answer as complete. The empty-stream guard below remains as the backstop for
/// any remaining way a stream can yield NO content and NO tool calls without
/// saying so.
/// What: Spawns `provider.chat_stream` into a bounded channel, drives
/// [`drive_delta_stream`] with a sink that publishes each batch as an
/// [`Event::AgentMessageDelta`] tagged with `session_id`/`agent`, joins the
/// pump (propagating its `Err` and panics), surfaces an explicit
/// `ChatEvent::Error`, then applies the empty-stream guard. Whatever the
/// provider reported (content, tool calls, and — #3767 — usage) rides through
/// on the returned [`StreamAssembly`] unmodified, so a caller that wires
/// usage into per-dispatch accounting (mirroring the blocking path's
/// `record_dispatch_usage`) has it available; this function does not write to
/// that log itself, since a background-spawned disk write triggered by every
/// streamed call — including from this very test suite — is exactly the kind
/// of surprising side effect this layer should not introduce silently.
/// Test: `stream_with_provider_propagates_error_frame`,
/// `stream_with_provider_propagates_provider_err`,
/// `stream_with_provider_guards_empty_stream`,
/// `stream_with_provider_propagates_panic`,
/// `stream_with_provider_returns_assembled_content`,
/// `stream_with_provider_surfaces_usage_in_assembly`,
/// `stream_with_provider_usage_absent_when_not_reported`.
pub async fn stream_with_provider<P>(
    provider: P,
    messages: Vec<ChatMessage>,
    session_id: &str,
    agent: &str,
    cadence: Duration,
) -> Result<StreamAssembly>
where
    P: ChatProvider + Send + 'static,
{
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
        Ok(Err(e)) => return Err(e.context("chat_stream failed")),
        Err(join_err) => return Err(anyhow!("streaming task panicked: {join_err}")),
    }
    if let Some(message) = &assembly.error {
        return Err(anyhow!("stream terminated with error: {message}"));
    }
    // Failed-stream guard (see fn docs): a stream that produced nothing usable
    // is a silent failure — force the blocking fallback.
    if assembly.content.is_empty() && assembly.tool_calls.is_empty() {
        return Err(anyhow!(
            "stream produced no content (treating as a failed stream so the blocking path takes over)"
        ));
    }

    Ok(assembly)
}

#[cfg(test)]
#[path = "stream_tests.rs"]
mod tests;
