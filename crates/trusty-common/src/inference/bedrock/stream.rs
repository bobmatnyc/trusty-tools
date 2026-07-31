//! Native `ConverseStream` transport for the shared Bedrock adapter (#4426).
//!
//! Why: [`InferenceAdapter::chat_stream`](crate::inference::InferenceAdapter::chat_stream)
//! had no Bedrock override, so every `bedrock/*` turn fell back to the trait's
//! buffered default — the whole answer arrived as ONE delta after the model had
//! already finished. Real token streaming for Bedrock is not new work in this
//! workspace: `crate::chat::bedrock_impl::BedrockProvider::chat_stream` has
//! driven `ConverseStream` in production (trusty-agents' chat path) since #3767.
//! This module ports that proven event handling onto the `inference` side, which
//! is the convergence target (`chat::ChatProvider` is being retired — epic
//! #4429). It deliberately does NOT reuse
//! [`crate::inference::streaming::SseDecoder`]: `ConverseStream` is a BINARY AWS
//! event-stream framed and demultiplexed by the SDK's `EventReceiver`, not the
//! `data:`-line SSE grammar that decoder speaks, and its token accounting
//! arrives exactly once in a terminal `Metadata` event rather than in a
//! usage-only final chunk.
//! What: [`ConverseStreamDecoder`] — the pure, synchronous
//! `ConverseStreamOutput` → [`ChatStreamEvent`] fold (text deltas, tool-use
//! start/argument fragments, the `MessageStop` stop reason, and the `Metadata`
//! usage tally); [`ConverseEventSource`] — a one-method seam over the SDK's
//! `EventReceiver` so [`drive`] (which produces the real [`ChatStream`] the
//! adapter returns) is unit-testable against a scripted event sequence, since
//! `EventReceiver` has no public constructor outside a live HTTP response; and
//! [`SdkEventSource`], the live implementation of that seam.
//! Test: `super::tests::stream_*` — delta ordering, structural-event skipping,
//! tool-call fragment mapping, terminal usage/stop-reason carry-over, the
//! mid-stream-error contract, and a `StreamAssembly` round-trip proving a
//! streamed turn rebuilds into the same `ChatResponse` the buffered `chat()`
//! path returns; plus the `#[ignore]`-gated `live_bedrock_converse_stream`.

use std::collections::HashMap;

use async_trait::async_trait;
use aws_sdk_bedrockruntime::operation::converse_stream::ConverseStreamOutput as ConverseStreamResponse;
use aws_sdk_bedrockruntime::types::{
    ContentBlockDelta, ContentBlockStart, ConverseStreamOutput as ConverseEvent,
};
use aws_smithy_types::error::display::DisplayErrorContext;
use futures_util::stream;

use crate::inference::error::InferenceError;
use crate::inference::streaming::{ChatStream, ChatStreamEvent, StreamCompletion, ToolCallDelta};
use crate::inference::types::{StopReason, Usage};

/// A pull source of decoded `ConverseStream` events.
///
/// Why (#4426): the AWS SDK hands the streamed turn back as an
/// `EventReceiver<ConverseStreamOutput, _>`, which has NO public constructor —
/// it can only be obtained from a live HTTP response. Testing [`drive`] (the
/// function that turns those events into the [`ChatStream`] the adapter
/// actually returns) therefore requires a seam; without one the only coverage
/// possible would be of the decoder in isolation, leaving the stream plumbing —
/// event ordering, terminal emission, error termination — unproven. One
/// `recv`-shaped method is enough because that is the entire surface [`drive`]
/// consumes.
/// What: `recv` yields the next event, `Ok(None)` at a clean end of stream, or
/// an [`InferenceError`] for a mid-stream failure (a modeled exception such as
/// throttling, or a transport fault). `Send + 'static` so the produced stream
/// satisfies [`ChatStream`]'s bounds.
/// Test: [`SdkEventSource`] is the live impl; `super::tests::ScriptedEvents` is
/// the test impl driving every `stream_*` test.
#[async_trait]
pub(super) trait ConverseEventSource: Send + 'static {
    /// Receive the next event, `None` at clean end of stream.
    async fn recv(&mut self) -> Result<Option<ConverseEvent>, InferenceError>;
}

/// The live [`ConverseEventSource`]: the AWS SDK's `ConverseStream` response.
///
/// Why: wraps the SDK response so the SDK error type is mapped to
/// [`InferenceError`] in ONE place, and so [`drive`] never names an AWS type.
/// It holds the whole operation OUTPUT rather than just its `stream` field
/// because the field's `EventReceiver<..>` type is not publicly nameable from
/// outside the SDK crate (`aws_sdk_bedrockruntime::event_receiver` is private) —
/// keeping the public wrapper is the only way to store it without a generic
/// escape hatch. The model/region are carried purely to make a mid-stream
/// failure message actionable (which model, which region) — the same context
/// [`super::BedrockAdapter::chat`] puts on its non-streaming errors.
/// What: `recv` delegates to the receiver's `recv`, mapping any `SdkError` to
/// [`InferenceError::Provider`] with the full source chain via
/// `DisplayErrorContext` (an `SdkError`'s own `Display` omits its cause, which
/// is where the real reason lives).
/// Test: exercised by the `#[ignore]`-gated `live_bedrock_converse_stream`; the
/// offline `stream_*` tests use the scripted source instead.
pub(super) struct SdkEventSource {
    response: ConverseStreamResponse,
    model: String,
    region: String,
}

impl SdkEventSource {
    /// Wrap a `ConverseStream` response with the context a failure message needs.
    ///
    /// Why: the adapter hands the operation output straight over; the
    /// model/region are captured here rather than at each error site.
    /// What: stores the response plus owned copies of the requested model slug
    /// and resolved region.
    /// Test: `live_bedrock_converse_stream` (`#[ignore]`).
    pub(super) fn new(response: ConverseStreamResponse, model: &str, region: &str) -> Self {
        Self {
            response,
            model: model.to_string(),
            region: region.to_string(),
        }
    }
}

#[async_trait]
impl ConverseEventSource for SdkEventSource {
    async fn recv(&mut self) -> Result<Option<ConverseEvent>, InferenceError> {
        self.response.stream.recv().await.map_err(|e| {
            InferenceError::Provider(format!(
                "ConverseStream failed mid-stream (model={}, region={}): {}",
                self.model,
                self.region,
                DisplayErrorContext(&e)
            ))
        })
    }
}

/// Folds `ConverseStream` events into the neutral streaming event model.
///
/// Why: this is the part of the port with real branching logic, so it is a
/// plain synchronous struct rather than being inlined into the async stream —
/// it can then be driven by a scripted event list in a unit test, exactly as
/// `chat::bedrock_impl::handle_stream_event` is. Converse spreads the terminal
/// summary across TWO events (`MessageStop` carries the stop reason,
/// `Metadata` carries usage) and neither is the last thing the receiver yields,
/// so the decoder must accumulate both and emit the single terminal
/// [`ChatStreamEvent::Done`] only at end of stream — the neutral contract's
/// "exactly one `Done`, always last".
/// What: [`Self::push`] maps one event to at most one [`ChatStreamEvent`] while
/// recording stop reason / usage / tool-call slots; [`Self::finish`] builds the
/// terminal event. Tool calls are re-indexed into DENSE slots (0, 1, 2 …) keyed
/// by Converse's `contentBlockIndex`, because that index counts every content
/// block (a leading text block makes the first tool block index 1) while
/// [`ToolCallDelta::index`] is the OpenAI-dialect call ordinal every consumer —
/// [`crate::inference::StreamAssembly`] included — accumulates by.
/// Test: `super::tests::stream_*`.
#[derive(Debug, Default)]
pub(super) struct ConverseStreamDecoder {
    /// The stop reason from `MessageStop`, carried into the terminal event.
    finish_reason: Option<StopReason>,
    /// The token tally from `Metadata`, carried into the terminal event.
    usage: Usage,
    /// Converse `contentBlockIndex` → dense tool-call slot.
    slots: HashMap<i32, usize>,
    /// Next dense slot to hand out.
    next_slot: usize,
}

impl ConverseStreamDecoder {
    /// Start an empty decoder.
    ///
    /// Why: one is built per streamed request.
    /// What: no stop reason, zeroed usage, no tool slots.
    /// Test: `super::tests::stream_forwards_text_deltas_in_order`.
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// Fold one `ConverseStream` event in, returning what to emit for it.
    ///
    /// Why: keeps the whole Converse→neutral mapping in one readable match so a
    /// new SDK event variant is a visible, single-place decision rather than a
    /// silent behaviour change spread across the stream driver.
    /// What: a `ContentBlockDelta::Text` becomes [`ChatStreamEvent::Delta`]
    /// (empty fragments are dropped — a keep-alive-shaped empty delta must not
    /// reach the consumer as a token); a `ContentBlockStart::ToolUse` becomes
    /// the introducing [`ChatStreamEvent::ToolCall`] carrying `id` + `name`;
    /// a `ContentBlockDelta::ToolUse` becomes a continuation `ToolCall` carrying
    /// only the partial-JSON `arguments` fragment; `MessageStop` records the
    /// stop reason (lowercased and parsed exactly as the non-streaming path
    /// does, so `chat` and `chat_stream` report the SAME `finish_reason`
    /// string); `Metadata` records usage when present (and fabricates nothing
    /// when absent — Bedrock omits it on some guardrail paths). Every other
    /// variant (`MessageStart`, `ContentBlockStop`, image/citation/reasoning
    /// deltas, and any SDK `Unknown` future variant) is a structural marker
    /// with nothing to forward and yields `None`.
    /// Test: `super::tests::stream_forwards_text_deltas_in_order`,
    /// `super::tests::stream_ignores_structural_events`,
    /// `super::tests::stream_maps_tool_use_fragments`,
    /// `super::tests::stream_carries_usage_and_stop_reason_in_terminal`.
    pub(super) fn push(&mut self, event: ConverseEvent) -> Option<ChatStreamEvent> {
        match event {
            ConverseEvent::ContentBlockStart(ev) => match ev.start() {
                Some(ContentBlockStart::ToolUse(tool)) => {
                    let index = self.slot_for(ev.content_block_index());
                    Some(ChatStreamEvent::ToolCall(ToolCallDelta {
                        index,
                        id: Some(tool.tool_use_id().to_string()),
                        name: Some(tool.name().to_string()),
                        arguments: String::new(),
                    }))
                }
                _ => None,
            },
            ConverseEvent::ContentBlockDelta(ev) => match ev.delta() {
                Some(ContentBlockDelta::Text(text)) if !text.is_empty() => {
                    Some(ChatStreamEvent::Delta(text.clone()))
                }
                Some(ContentBlockDelta::ToolUse(delta)) => {
                    let index = self.slot_for(ev.content_block_index());
                    Some(ChatStreamEvent::ToolCall(ToolCallDelta {
                        index,
                        id: None,
                        name: None,
                        arguments: delta.input().to_string(),
                    }))
                }
                _ => None,
            },
            ConverseEvent::MessageStop(ev) => {
                // #4426: mirror `convert::converse_output_to_chat_response`'s
                // `stop_reason().as_str().to_ascii_lowercase()` so a streamed
                // turn and a buffered one report an IDENTICAL finish reason
                // (`"end_turn"`, `"tool_use"`, `"max_tokens"` …) — a consumer
                // must never be able to tell which transport ran from it.
                self.finish_reason = Some(StopReason::from_wire(
                    &ev.stop_reason().as_str().to_ascii_lowercase(),
                ));
                None
            }
            ConverseEvent::Metadata(meta) => {
                if let Some(usage) = meta.usage() {
                    self.usage = Usage::new(
                        usage.input_tokens().max(0) as u32,
                        usage.output_tokens().max(0) as u32,
                        usage.cache_read_input_tokens().unwrap_or(0).max(0) as u32,
                        usage.cache_write_input_tokens().unwrap_or(0).max(0) as u32,
                    );
                }
                None
            }
            _ => None,
        }
    }

    /// The terminal [`ChatStreamEvent::Done`] for this stream.
    ///
    /// Why: the stop reason and usage arrive in earlier events than the end of
    /// stream, so the terminal summary can only be built once the receiver is
    /// exhausted.
    /// What: a `Done` carrying the recorded finish reason (`None` if Bedrock
    /// never sent `MessageStop`) and usage (zeroed if no `Metadata` arrived).
    /// Test: `super::tests::stream_carries_usage_and_stop_reason_in_terminal`.
    pub(super) fn finish(&self) -> ChatStreamEvent {
        ChatStreamEvent::Done(StreamCompletion {
            finish_reason: self.finish_reason.clone(),
            usage: self.usage,
        })
    }

    /// Resolve (or allocate) the dense tool-call slot for a Converse content
    /// block index.
    ///
    /// Why: see the struct doc — Converse's `contentBlockIndex` is a
    /// content-block ordinal, not a tool-call ordinal, and a consumer keyed on
    /// the raw value would number a turn's first tool call `1` whenever the
    /// model emitted any text first.
    /// What: returns the slot already assigned to `block_index`, else assigns
    /// the next slot (0, 1, 2 …) in first-seen order.
    /// Test: `super::tests::stream_tool_slots_are_dense_from_zero`.
    fn slot_for(&mut self, block_index: i32) -> usize {
        if let Some(slot) = self.slots.get(&block_index) {
            return *slot;
        }
        let slot = self.next_slot;
        self.next_slot += 1;
        self.slots.insert(block_index, slot);
        slot
    }
}

/// Drive a [`ConverseEventSource`] into the neutral [`ChatStream`].
///
/// Why: the adapter's `chat_stream` must return a pollable, `Send`, boxed
/// stream — not an ad-hoc loop — so the caller keeps back-pressure and
/// cancellation (dropping the returned stream drops the source, which aborts
/// the underlying AWS request). Generic over the source so the entire
/// production path is exercised offline by the scripted tests.
/// What: pulls events, forwards whatever [`ConverseStreamDecoder::push`]
/// produces, and terminates on either a clean end of stream (emitting exactly
/// one [`ChatStreamEvent::Done`]) or a mid-stream error (emitting a terminal
/// `Err` and NO `Done` — a failed stream must never be mistakable for a
/// complete short answer, the same contract
/// [`crate::inference::streaming::decode_event_stream`] holds for the SSE lane).
/// Test: `super::tests::stream_forwards_text_deltas_in_order`,
/// `super::tests::stream_surfaces_mid_stream_error`,
/// `super::tests::stream_assembles_into_buffered_shaped_response`.
pub(super) fn drive<S: ConverseEventSource>(source: S) -> ChatStream {
    struct State<S> {
        source: S,
        decoder: ConverseStreamDecoder,
        done: bool,
    }

    let init = State {
        source,
        decoder: ConverseStreamDecoder::new(),
        done: false,
    };

    let s = stream::unfold(init, |mut st| async move {
        loop {
            if st.done {
                return None;
            }
            match st.source.recv().await {
                Ok(Some(event)) => {
                    if let Some(out) = st.decoder.push(event) {
                        return Some((Ok(out), st));
                    }
                }
                Ok(None) => {
                    st.done = true;
                    return Some((Ok(st.decoder.finish()), st));
                }
                Err(e) => {
                    st.done = true;
                    return Some((Err(e), st));
                }
            }
        }
    });

    Box::pin(s)
}
