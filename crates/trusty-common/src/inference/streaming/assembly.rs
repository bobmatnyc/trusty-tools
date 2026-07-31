//! [`StreamAssembly`] — rebuild a [`ChatResponse`] from streamed events.
//!
//! Why (#4425): [`super::buffered_stream`] turns a finished response INTO a
//! stream; every consumer that renders tokens live needs the inverse. Without
//! it, a caller who switches from `chat()` to `chat_stream()` has to hand-roll
//! delta concatenation, per-`index` tool-call fragment accumulation, and the
//! terminal finish-reason/usage carry-over — the exact three-part bookkeeping
//! that made trusty-code's streaming migration look like "a rewrite of the
//! agent loop" rather than "swap one call". Putting it here (next to the
//! decoder that produces the events) means the SECOND consumer to stream —
//! trusty-review, tga, any of epic #4429's remaining migrations — reuses a
//! tested accumulator instead of writing a fourth copy.
//! What: [`StreamAssembly`] is a pure, synchronous accumulator: push
//! [`ChatStreamEvent`]s in arrival order, then [`StreamAssembly::into_response`]
//! yields the same [`ChatResponse`] the buffered `chat()` path would have
//! returned for that turn. It is deliberately NOT a `Stream` combinator and
//! takes no callback, so the caller keeps control of the poll loop and may
//! `await` arbitrary async work (rendering a delta to a UI sink) between
//! pushes.
//! Test: inline `tests` — text concatenation, fragmented tool-call
//! accumulation, terminal usage/finish-reason carry-over, the no-`Done` case,
//! and a `buffered_stream` → assembly round-trip.

use std::collections::BTreeMap;

use super::{ChatStreamEvent, StreamCompletion, ToolCallDelta};
use crate::inference::types::{
    AssistantMessage, ChatChoice, ChatResponse, FunctionCall, StopReason, ToolCall, UsageBlock,
};

/// One tool call being rebuilt from its streamed fragments.
///
/// Why: an OpenAI-dialect provider introduces a call's `id`/`name` on one frame
/// and then appends argument text across any number of later frames, so a
/// partially-seen call needs somewhere to live until the stream ends.
/// What: `id`/`name` are filled by whichever frame carries them (later frames
/// never overwrite a value already seen — a provider that repeats the id must
/// not be able to truncate it); `arguments` accumulates every fragment in
/// arrival order.
/// Test: `accumulates_fragmented_tool_call`.
#[derive(Debug, Default)]
struct PartialToolCall {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

/// Accumulates streamed chat events into a finished [`ChatResponse`].
///
/// Why: see the module doc — this is the inverse of [`super::buffered_stream`],
/// so a consumer can stream deltas for display AND still hand the rest of its
/// pipeline the exact same response type the non-streaming path produced. That
/// equivalence is what lets a caller adopt streaming without forking its
/// downstream transcript / usage / tool-dispatch code.
/// What: [`Self::push`] folds one event in; [`Self::text`] exposes the text
/// accumulated so far; [`Self::into_response`] finalises. Tool calls are keyed
/// by [`ToolCallDelta::index`] in a [`BTreeMap`] so the rebuilt order is the
/// provider's slot order regardless of the order fragments arrived in.
/// Test: `concatenates_text_deltas`, `accumulates_fragmented_tool_call`,
/// `carries_terminal_usage_and_finish_reason`.
#[derive(Debug, Default)]
pub struct StreamAssembly {
    text: String,
    calls: BTreeMap<usize, PartialToolCall>,
    completion: Option<StreamCompletion>,
}

impl StreamAssembly {
    /// Start an empty assembly.
    ///
    /// Why: one obvious entry point; the `Default` impl is the same thing for
    /// callers that build it in a struct literal.
    /// What: all three accumulators empty.
    /// Test: `concatenates_text_deltas`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold one streamed event into the accumulator.
    ///
    /// Why: the caller drives the poll loop (so it can `await` between events),
    /// so the accumulator must accept events one at a time rather than owning
    /// the stream.
    /// What: [`ChatStreamEvent::Delta`] appends to the text;
    /// [`ChatStreamEvent::ToolCall`] merges into the slot named by its `index`;
    /// [`ChatStreamEvent::Done`] records the terminal summary. A second `Done`
    /// (which a healthy stream never sends) overwrites the first rather than
    /// panicking — a malformed stream must not be able to abort the caller.
    /// Test: `concatenates_text_deltas`, `accumulates_fragmented_tool_call`.
    pub fn push(&mut self, event: ChatStreamEvent) {
        match event {
            ChatStreamEvent::Delta(chunk) => self.text.push_str(&chunk),
            ChatStreamEvent::ToolCall(delta) => self.merge_tool_call(delta),
            ChatStreamEvent::Done(completion) => self.completion = Some(completion),
        }
    }

    /// Merge one tool-call fragment into its slot.
    ///
    /// Why: `id` and `name` arrive once, `arguments` arrives in pieces; keeping
    /// the merge rule in one place stops a later frame's `None` from erasing an
    /// identifier the caller needs to answer the call.
    /// What: creates the slot on first sight; sets `id`/`name` only when this
    /// frame carries them AND the slot has none yet; always appends `arguments`.
    /// Test: `accumulates_fragmented_tool_call`.
    fn merge_tool_call(&mut self, delta: ToolCallDelta) {
        let slot = self.calls.entry(delta.index).or_default();
        if slot.id.is_none() && delta.id.is_some() {
            slot.id = delta.id;
        }
        if slot.name.is_none() && delta.name.is_some() {
            slot.name = delta.name;
        }
        slot.arguments.push_str(&delta.arguments);
    }

    /// The assistant text accumulated so far.
    ///
    /// Why: a caller that already forwarded each delta to a UI still needs the
    /// full turn text for its transcript, and reading it without consuming the
    /// assembly lets it do so mid-stream (e.g. to render a running preview).
    /// What: the concatenation of every [`ChatStreamEvent::Delta`] pushed.
    /// Test: `concatenates_text_deltas`.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Finalise into the [`ChatResponse`] the buffered path would have returned.
    ///
    /// Why: downstream code (transcript recording, usage accrual, tool
    /// dispatch) must not care whether the turn was streamed — handing it the
    /// same type is what makes streaming a transport detail instead of a second
    /// code path.
    /// What: builds a single-choice response. `content` is `None` for a
    /// text-free (tool-only) turn — matching the wire shape the non-streaming
    /// parser produces — and `Some(text)` otherwise. Tool calls are emitted in
    /// slot order; a fragment that never carried an `id`/`name` yields empty
    /// strings rather than being dropped, so a malformed stream surfaces as a
    /// visible bad call instead of a silently missing one. `finish_reason` and
    /// `usage` come from the terminal [`ChatStreamEvent::Done`]; a stream that
    /// ended without one yields `None`/zeroed usage.
    /// Test: `carries_terminal_usage_and_finish_reason`, `no_done_event_yields_defaults`.
    pub fn into_response(self, id: impl Into<String>, model: impl Into<String>) -> ChatResponse {
        let tool_calls: Vec<ToolCall> = self
            .calls
            .into_values()
            .map(|c| ToolCall {
                id: c.id.unwrap_or_default(),
                kind: "function".to_string(),
                function: FunctionCall {
                    name: c.name.unwrap_or_default(),
                    arguments: c.arguments,
                },
            })
            .collect();

        let content = if self.text.is_empty() {
            None
        } else {
            Some(self.text)
        };

        let (finish_reason, usage) = match self.completion {
            Some(done) => {
                let u = done.usage;
                // The wire `UsageBlock` is built directly (rather than via a
                // `Usage → UsageBlock` conversion) because the normalized
                // `Usage` has already merged the two cache shapes: re-splitting
                // it into both the flat and the nested fields would
                // DOUBLE-COUNT cache tokens in `UsageBlock::into_usage`. The
                // flat fields are the canonical target; `prompt_tokens_details`
                // stays `None`.
                let block = UsageBlock {
                    prompt_tokens: u.prompt_tokens,
                    completion_tokens: u.completion_tokens,
                    total_tokens: u.total_tokens(),
                    cache_read_input_tokens: u.cache_read_tokens,
                    cache_creation_input_tokens: u.cache_creation_tokens,
                    prompt_tokens_details: None,
                    cost: u.cost_usd,
                };
                (done.finish_reason.map(stop_reason_to_wire), block)
            }
            None => (None, UsageBlock::default()),
        };

        ChatResponse {
            id: id.into(),
            model: model.into(),
            choices: vec![ChatChoice {
                message: AssistantMessage {
                    content,
                    tool_calls,
                },
                finish_reason,
            }],
            usage,
        }
    }
}

/// Render a [`StopReason`] back into its wire spelling.
///
/// Why: [`ChatChoice::finish_reason`] is the raw wire string, and a consumer
/// that re-parses it via [`StopReason::from_wire`] must get the same variant
/// back — otherwise a streamed `tool_calls` turn would be indistinguishable
/// from a natural stop and the agent loop would end the run with pending calls.
/// What: the exact inverse of [`StopReason::from_wire`]; `Other` is preserved
/// verbatim.
/// Test: `finish_reason_round_trips_through_wire`.
fn stop_reason_to_wire(reason: StopReason) -> String {
    match reason {
        StopReason::Stop => "stop".to_string(),
        StopReason::ToolCalls => "tool_calls".to_string(),
        StopReason::Length => "length".to_string(),
        StopReason::ContentFilter => "content_filter".to_string(),
        StopReason::Other(other) => other,
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inference::streaming::buffered_stream;
    use crate::inference::types::Usage;
    use futures_util::StreamExt;

    /// Text deltas concatenate in arrival order into one `content`.
    ///
    /// Why: out-of-order or dropped concatenation is the single most visible
    /// streaming bug — the user reads scrambled prose.
    /// What: push three deltas, assert `text()` and the finalised content.
    /// Test: this test.
    #[test]
    fn concatenates_text_deltas() {
        let mut a = StreamAssembly::new();
        a.push(ChatStreamEvent::Delta("Hel".into()));
        a.push(ChatStreamEvent::Delta("lo, ".into()));
        a.push(ChatStreamEvent::Delta("world".into()));
        assert_eq!(a.text(), "Hello, world");
        let resp = a.into_response("gen-1", "openai/gpt-4o-mini");
        assert_eq!(resp.first_text().as_deref(), Some("Hello, world"));
        assert_eq!(resp.id, "gen-1");
        assert_eq!(resp.model, "openai/gpt-4o-mini");
    }

    /// A tool call split across frames rebuilds into one complete call.
    ///
    /// Why: OpenAI-dialect providers send `id`/`name` once and then stream the
    /// argument JSON; losing either identifier or truncating the arguments
    /// makes the call undispatchable.
    /// What: push an introducing frame plus two argument fragments (the later
    /// frames carrying no id/name), assert the rebuilt call.
    /// Test: this test.
    #[test]
    fn accumulates_fragmented_tool_call() {
        let mut a = StreamAssembly::new();
        a.push(ChatStreamEvent::ToolCall(ToolCallDelta {
            index: 0,
            id: Some("call_1".into()),
            name: Some("read_file".into()),
            arguments: "{\"path\":".into(),
        }));
        a.push(ChatStreamEvent::ToolCall(ToolCallDelta {
            index: 0,
            id: None,
            name: None,
            arguments: "\"src/".into(),
        }));
        a.push(ChatStreamEvent::ToolCall(ToolCallDelta {
            index: 0,
            id: None,
            name: None,
            arguments: "lib.rs\"}".into(),
        }));
        let resp = a.into_response("gen-2", "m");
        let calls = resp.first_tool_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].kind, "function");
        assert_eq!(calls[0].function.name, "read_file");
        assert_eq!(calls[0].function.arguments, "{\"path\":\"src/lib.rs\"}");
        // A tool-only turn has no text content, matching the wire shape.
        assert!(resp.first_text().is_none());
    }

    /// Two concurrent tool-call slots rebuild in index order.
    ///
    /// Why: a model may request several tools in one turn and the frames
    /// interleave; keying by `index` (not arrival) is what keeps them apart.
    /// What: interleave frames for index 1 and index 0, assert slot order.
    /// Test: this test.
    #[test]
    fn keeps_tool_call_slots_separate_and_ordered() {
        let mut a = StreamAssembly::new();
        a.push(ChatStreamEvent::ToolCall(ToolCallDelta {
            index: 1,
            id: Some("b".into()),
            name: Some("second".into()),
            arguments: "{}".into(),
        }));
        a.push(ChatStreamEvent::ToolCall(ToolCallDelta {
            index: 0,
            id: Some("a".into()),
            name: Some("first".into()),
            arguments: "{}".into(),
        }));
        let resp = a.into_response("id", "m");
        let calls = resp.first_tool_calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].id, "a");
        assert_eq!(calls[1].id, "b");
    }

    /// The terminal `Done` supplies finish reason and usage.
    ///
    /// Why: cost accounting and the loop's stop/continue decision both read
    /// these off the response; dropping them makes a streamed turn look free
    /// and reason-less.
    /// What: push a delta then a `Done`, assert usage fields and stop reason.
    /// Test: this test.
    #[test]
    fn carries_terminal_usage_and_finish_reason() {
        let mut a = StreamAssembly::new();
        a.push(ChatStreamEvent::Delta("ok".into()));
        let mut usage = Usage::new(120, 30, 90, 10);
        usage.cost_usd = Some(0.0042);
        a.push(ChatStreamEvent::Done(StreamCompletion {
            finish_reason: Some(StopReason::ToolCalls),
            usage,
        }));
        let resp = a.into_response("gen-3", "m");
        assert_eq!(resp.stop_reason(), Some(StopReason::ToolCalls));
        assert_eq!(resp.usage.prompt_tokens, 120);
        assert_eq!(resp.usage.completion_tokens, 30);
        assert_eq!(resp.usage.cache_read_input_tokens, 90);
        assert_eq!(resp.usage.cache_creation_input_tokens, 10);
        assert_eq!(resp.usage.cost, Some(0.0042));
        // The normalized usage must survive a re-normalisation without the
        // cache buckets being double-counted.
        let normalized = resp.usage();
        assert_eq!(normalized.cache_read_tokens, 90);
        assert_eq!(normalized.cache_creation_tokens, 10);
    }

    /// Every stop reason survives the round trip through the wire string.
    ///
    /// Why: the loop branches on `tool_calls` vs `stop`; a lossy rendering
    /// would silently change control flow for streamed turns only.
    /// What: assemble each variant and re-read it typed.
    /// Test: this test.
    #[test]
    fn finish_reason_round_trips_through_wire() {
        for reason in [
            StopReason::Stop,
            StopReason::ToolCalls,
            StopReason::Length,
            StopReason::ContentFilter,
            StopReason::Other("provider_specific".into()),
        ] {
            let mut a = StreamAssembly::new();
            a.push(ChatStreamEvent::Done(StreamCompletion {
                finish_reason: Some(reason.clone()),
                usage: Usage::default(),
            }));
            let resp = a.into_response("id", "m");
            assert_eq!(resp.stop_reason(), Some(reason));
        }
    }

    /// A stream that ended without a `Done` still finalises.
    ///
    /// Why: a transport that drops mid-stream must not make the assembly
    /// unusable — the caller needs whatever text arrived so it can report a
    /// partial turn.
    /// What: push only a delta, assert defaults for the terminal fields.
    /// Test: this test.
    #[test]
    fn no_done_event_yields_defaults() {
        let mut a = StreamAssembly::new();
        a.push(ChatStreamEvent::Delta("partial".into()));
        let resp = a.into_response("id", "m");
        assert_eq!(resp.first_text().as_deref(), Some("partial"));
        assert_eq!(resp.stop_reason(), None);
        assert_eq!(resp.usage.prompt_tokens, 0);
    }

    /// `buffered_stream` → `StreamAssembly` is the identity on a response.
    ///
    /// Why: this is the contract that makes streaming safe to adopt — the
    /// buffered fallback (used by every adapter without native SSE, incl. the
    /// Bedrock transport until #4426) must round-trip a response unchanged, or
    /// a non-streaming provider would behave differently under `chat_stream`.
    /// What: build a response with text + a tool call + usage, replay it as a
    /// stream, reassemble, and compare the observable fields.
    /// Test: this test.
    #[tokio::test]
    async fn buffered_stream_round_trips_through_assembly() {
        let original = ChatResponse {
            id: "gen-rt".into(),
            model: "anthropic/claude-sonnet-4-5".into(),
            choices: vec![ChatChoice {
                message: AssistantMessage {
                    content: Some("hello".into()),
                    tool_calls: vec![ToolCall {
                        id: "call_x".into(),
                        kind: "function".into(),
                        function: FunctionCall {
                            name: "search".into(),
                            arguments: "{\"q\":\"rust\"}".into(),
                        },
                    }],
                },
                finish_reason: Some("tool_calls".into()),
            }],
            usage: UsageBlock {
                prompt_tokens: 10,
                completion_tokens: 4,
                total_tokens: 14,
                cache_read_input_tokens: 2,
                cache_creation_input_tokens: 1,
                prompt_tokens_details: None,
                cost: Some(0.01),
            },
        };

        let mut stream = buffered_stream(original.clone());
        let mut assembly = StreamAssembly::new();
        while let Some(event) = stream.next().await {
            assembly.push(event.expect("buffered stream never errors"));
        }
        let rebuilt = assembly.into_response(original.id.clone(), original.model.clone());

        assert_eq!(rebuilt.first_text(), original.first_text());
        assert_eq!(rebuilt.first_tool_calls(), original.first_tool_calls());
        assert_eq!(rebuilt.stop_reason(), original.stop_reason());
        assert_eq!(rebuilt.usage(), original.usage());
    }
}
