//! Incremental token-streaming for the unified inference adapter (epic #3696,
//! Gap B — OpenAI-compat lane).
//!
//! Why: the demo needs the assistant's tokens to appear as they are produced,
//! not after the whole turn buffers. `chat()` returns one final
//! [`super::ChatResponse`]; this module adds the *streaming* seam every consumer
//! (trusty-agents' chat first) drives to render deltas live. The wire mechanics
//! for OpenAI-dialect providers (OpenRouter foremost) are a single SSE grammar —
//! `data: {chunk}\n\n` frames carrying `choices[].delta.content` fragments, a
//! `[DONE]` terminator, and (with `stream_options.include_usage`) a final
//! usage-only chunk — so the parser lives here once and every `openai_compat`
//! provider lights up through it.
//! What: the provider-neutral event model ([`ChatStreamEvent`], [`ToolCallDelta`],
//! [`StreamCompletion`]) and the boxed [`ChatStream`] the trait yields; the
//! byte-level [`SseDecoder`] (partial-UTF-8-safe, keep-alive/comment tolerant,
//! `[DONE]`- and error-chunk aware); [`decode_event_stream`] turning any
//! byte-chunk stream into a [`ChatStreamEvent`] stream; and [`buffered_stream`],
//! the fallback that replays a completed [`super::ChatResponse`] as a stream so
//! every adapter has a working `chat_stream` even without native streaming.
//! Test: `mod tests` (sibling `streaming/tests.rs`) — happy-path deltas,
//! split-mid-token chunks, `[DONE]`, usage terminal, error chunk, keep-alives,
//! partial UTF-8 across chunk boundaries, and the buffered fallback.

use std::collections::VecDeque;
use std::pin::Pin;

use futures_util::stream::{self, Stream, StreamExt};
use serde::Deserialize;

use crate::inference::error::InferenceError;
use crate::inference::types::{ChatResponse, StopReason, Usage, UsageBlock};

/// The boxed, `Send` stream of streaming events an adapter yields.
///
/// Why: `async fn chat_stream` is dispatched through `Box<dyn InferenceAdapter>`,
/// so its item stream must be object-safe — a concrete `impl Stream` cannot cross
/// the trait boundary. Boxing + pinning erases the provider-specific stream type
/// while keeping it pollable and shareable across `tokio` tasks (`Send`).
/// What: a pinned, heap-boxed [`Stream`] yielding
/// `Result<ChatStreamEvent, InferenceError>` — ordered text/tool deltas followed
/// by exactly one terminal [`ChatStreamEvent::Done`] (unless the transport errors
/// mid-stream, which surfaces as a terminal `Err`).
/// Test: `decode_yields_deltas_then_done`.
pub type ChatStream = Pin<Box<dyn Stream<Item = Result<ChatStreamEvent, InferenceError>> + Send>>;

/// One event in a streamed chat completion.
///
/// Why: consumers render as tokens arrive and must also receive the authoritative
/// finish reason + usage the non-streaming path returns — so the stream carries
/// both incremental fragments and a single terminal summary rather than forcing
/// the caller to reconstruct usage from raw chunks.
/// What: [`Self::Delta`] is a text-content fragment (append in order);
/// [`Self::ToolCall`] is a streamed tool-call fragment (accumulate by `index`);
/// [`Self::Done`] is the terminal event carrying the finish reason and usage.
/// Exactly one `Done` ends a healthy stream, and it is always the last item.
/// Test: `decode_yields_deltas_then_done`, `decode_accumulates_tool_call`.
#[derive(Debug, Clone, PartialEq)]
pub enum ChatStreamEvent {
    /// An incremental assistant text fragment, in emission order.
    Delta(String),
    /// A streamed tool-call fragment (OpenAI streams calls across frames).
    ToolCall(ToolCallDelta),
    /// Terminal event: the model finished; carries the finish reason + usage.
    Done(StreamCompletion),
}

/// A single streamed tool-call fragment.
///
/// Why: OpenAI-dialect providers stream tool calls incrementally — the first
/// frame at an `index` carries the `id` and function `name`, later frames append
/// argument text — so the consumer must accumulate fragments by `index` to
/// rebuild each call. Forwarding the raw fragments (rather than buffering the
/// whole call here) keeps this module a pure parser and lets the caller decide
/// when a call is complete.
/// What: `index` identifies the call slot; `id`/`name` are set on the frame that
/// carries them (`None` on continuation frames); `arguments` is the argument-text
/// fragment from this frame (possibly empty).
/// Test: `decode_accumulates_tool_call`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallDelta {
    /// The tool-call slot this fragment belongs to (frames share an index).
    pub index: usize,
    /// The call id, present only on the frame that introduces the call.
    pub id: Option<String>,
    /// The function name, present only on the frame that introduces the call.
    pub name: Option<String>,
    /// The argument-text fragment from this frame (concatenate across frames).
    pub arguments: String,
}

/// The terminal summary of a streamed completion.
///
/// Why: the caller needs the same finish reason + normalized [`Usage`] the
/// buffered `chat()` path returns; collecting them into one terminal event means
/// downstream cost/telemetry code is identical whether or not streaming was used.
/// What: `finish_reason` is the typed stop condition (`None` when the provider
/// never sent one); `usage` is the normalized token/cost accounting (zeroed when
/// the provider did not include a usage chunk).
/// Test: `decode_carries_usage_in_terminal`.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamCompletion {
    /// Why the model stopped, when reported.
    pub finish_reason: Option<StopReason>,
    /// Normalized token + cost accounting for the streamed call.
    pub usage: Usage,
}

// ── Wire chunk shapes ─────────────────────────────────────────────────────────

/// One `data:` chunk from an OpenAI-compatible SSE stream.
///
/// Why: each streamed frame is a partial completion object; deserialising into a
/// permissive struct (every field defaulted) lets a content-only frame, a
/// usage-only final frame, and an error frame all parse without branching on
/// shape first.
/// What: `choices` carries the per-choice deltas (empty on a usage-only frame);
/// `usage` is the optional final accounting (present when
/// `stream_options.include_usage` was requested); `error` is the in-band error
/// object OpenRouter emits mid-stream instead of an HTTP status.
/// Test: `decode_surfaces_error_chunk`, `decode_carries_usage_in_terminal`.
#[derive(Debug, Default, Deserialize)]
struct StreamChunk {
    #[serde(default)]
    choices: Vec<StreamChoice>,
    #[serde(default)]
    usage: Option<UsageBlock>,
    #[serde(default)]
    error: Option<StreamError>,
}

/// The in-band error object a provider may emit as a `data:` frame.
///
/// Why: OpenRouter can report a mid-stream failure as a JSON error chunk rather
/// than a non-2xx status (the connection already succeeded); the decoder must
/// surface it as a terminal `Err` so the caller stops rather than hanging.
/// What: `message` is the human-readable failure; `code` is the optional
/// provider status code (mapped into [`InferenceError::Api`] when numeric).
/// Test: `decode_surfaces_error_chunk`.
#[derive(Debug, Default, Deserialize)]
struct StreamError {
    #[serde(default)]
    message: String,
    #[serde(default)]
    code: Option<u16>,
}

/// One choice's per-frame delta.
///
/// Why: mirrors the wire `choices[i]` object in a streaming frame — the delta
/// payload plus this choice's finish reason once the model stops.
/// What: `delta` holds the content/tool-call fragments; `finish_reason` is the
/// raw wire stop string on the final content frame for this choice.
/// Test: `decode_yields_deltas_then_done`.
#[derive(Debug, Default, Deserialize)]
struct StreamChoice {
    #[serde(default)]
    delta: WireDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

/// The `delta` payload within a streamed choice.
///
/// Why: a frame carries either a text fragment, one or more tool-call fragments,
/// or neither (role-only opening frame / usage-only closing frame); all fields
/// default so any of those parses cleanly.
/// What: `content` is the text fragment; `tool_calls` are the tool-call
/// fragments for this frame.
/// Test: `decode_yields_deltas_then_done`, `decode_accumulates_tool_call`.
#[derive(Debug, Default, Deserialize)]
struct WireDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<WireToolCall>,
}

/// A tool-call fragment inside a streamed `delta`.
///
/// Why: the wire spreads one call across frames keyed by `index`; parsing the
/// per-frame shape here keeps [`ToolCallDelta`] (the public event) free of serde
/// concerns.
/// What: `index` is the call slot; `id` and `function` appear on the introducing
/// frame; later frames carry only the `function.arguments` fragment.
/// Test: `decode_accumulates_tool_call`.
#[derive(Debug, Default, Deserialize)]
struct WireToolCall {
    #[serde(default)]
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<WireFunction>,
}

/// The `function` object within a streamed tool-call fragment.
///
/// Why: name and argument text arrive separately across frames; a defaulted
/// struct absorbs whichever a given frame carries.
/// What: `name` on the introducing frame; `arguments` is the (possibly partial)
/// argument-text fragment.
/// Test: `decode_accumulates_tool_call`.
#[derive(Debug, Default, Deserialize)]
struct WireFunction {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

// ── SSE decoder ───────────────────────────────────────────────────────────────

/// A stateful, byte-level decoder for one OpenAI-compatible SSE stream.
///
/// Why: byte chunks off the socket split at arbitrary boundaries — mid-line,
/// mid-JSON, even mid-UTF-8-codepoint. Buffering *bytes* and only decoding
/// complete `\n`-terminated lines makes the parser correct across every split:
/// `\n` is ASCII so it never falls inside a multi-byte codepoint, so any line we
/// dispatch is guaranteed to be whole, valid UTF-8. This is the fix over the
/// legacy pump's per-chunk `from_utf8` (which silently dropped a chunk that split
/// a codepoint).
/// What: [`Self::feed`] pushes bytes and returns any events completed by them;
/// [`Self::finish`] flushes the terminal [`ChatStreamEvent::Done`] at EOF. The
/// decoder tracks the running finish reason + usage so the terminal event carries
/// the authoritative summary regardless of which frame reported it.
/// Test: `decode_yields_deltas_then_done`, `decode_handles_split_chunks`,
/// `decode_tolerates_keepalives`, `decode_partial_utf8_across_chunks`.
#[derive(Debug, Default)]
pub struct SseDecoder {
    /// Undecoded bytes carried across `feed` calls (a partial trailing line).
    buf: Vec<u8>,
    /// Accumulated `data:` field for the SSE event currently being built.
    data: String,
    /// The most recent finish reason seen (carried into the terminal event).
    finish_reason: Option<StopReason>,
    /// The most recent usage block seen (carried into the terminal event).
    usage: Usage,
    /// Set once `[DONE]` (or a terminal error) has been dispatched — further
    /// input is ignored so a duplicated sentinel cannot emit two `Done`s.
    done: bool,
}

impl SseDecoder {
    /// Create an empty decoder.
    ///
    /// Why: the transport constructs one per streamed request.
    /// What: all buffers empty, no terminal seen.
    /// Test: `decode_yields_deltas_then_done`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a byte chunk and return every event it completed.
    ///
    /// Why: one socket read can complete zero, one, or many SSE events; returning
    /// a batch lets the stream adapter enqueue them without re-buffering.
    /// What: appends `chunk` to the byte buffer, extracts each complete
    /// `\n`-terminated line, and dispatches SSE fields — `data:` lines accumulate
    /// (a blank line, `[DONE]`, or a parsed chunk flushes them). Comment lines
    /// (`:`-prefixed keep-alives) and non-`data` fields are ignored. Returns the
    /// ordered events produced; a parse error inside a frame is skipped (best
    /// effort), but a provider `error` chunk yields a terminal `Err`.
    /// Test: `decode_yields_deltas_then_done`, `decode_handles_split_chunks`,
    /// `decode_partial_utf8_across_chunks`.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<Result<ChatStreamEvent, InferenceError>> {
        let mut out = Vec::new();
        if self.done {
            return out;
        }
        self.buf.extend_from_slice(chunk);

        while let Some(nl) = self.buf.iter().position(|&b| b == b'\n') {
            // Drain the line including its trailing `\n`; the byte after the last
            // `\n` (if any) stays buffered as the next partial line.
            let line_bytes: Vec<u8> = self.buf.drain(..=nl).collect();
            // A `\n`-terminated line never splits a UTF-8 codepoint, so this is
            // always whole, valid UTF-8 (lossy is belt-and-braces only).
            let line = String::from_utf8_lossy(&line_bytes);
            let line = line.trim_end_matches(['\r', '\n']);
            self.dispatch_line(line, &mut out);
            if self.done {
                break;
            }
        }
        out
    }

    /// Interpret one complete SSE line, pushing any resulting events.
    ///
    /// Why: isolating per-line handling keeps [`Self::feed`]'s buffering loop
    /// small and makes the SSE grammar (blank-line dispatch, `data:` accrual,
    /// comment skip) readable in one place.
    /// What: a blank line flushes the accumulated `data:` payload; a `:` comment
    /// or a non-`data` field is ignored; a `data:` line appends to the payload
    /// (newline-joined for multi-line data). On flush, `[DONE]` produces the
    /// terminal event and an in-band `error` chunk produces a terminal `Err`.
    /// Test: `decode_tolerates_keepalives`, `decode_surfaces_error_chunk`.
    fn dispatch_line(
        &mut self,
        line: &str,
        out: &mut Vec<Result<ChatStreamEvent, InferenceError>>,
    ) {
        if line.is_empty() {
            self.flush_event(out);
            return;
        }
        if line.starts_with(':') {
            return; // SSE comment / keep-alive.
        }
        let Some(rest) = line.strip_prefix("data:") else {
            return; // `event:`/`id:`/`retry:` — irrelevant to chat streaming.
        };
        let value = rest.strip_prefix(' ').unwrap_or(rest);
        if !self.data.is_empty() {
            self.data.push('\n');
        }
        self.data.push_str(value);
        // OpenAI providers terminate each frame with a blank line, but some emit
        // back-to-back `data:` lines; flush eagerly so a missing blank line never
        // strands a frame.
        self.flush_event(out);
    }

    /// Flush the accumulated `data:` payload as an event (if any).
    ///
    /// Why: the payload is only meaningful once complete; centralising the flush
    /// keeps the `[DONE]`/error/chunk decision in one place and guarantees the
    /// payload buffer is always cleared.
    /// What: takes the accumulated payload; empties do nothing. `[DONE]` marks the
    /// stream done and emits the terminal event. A parseable chunk updates the
    /// running finish reason/usage and emits any content/tool-call deltas; an
    /// `error` field emits a terminal `Err` and marks done. An unparseable
    /// payload is skipped.
    /// Test: `decode_yields_deltas_then_done`, `decode_surfaces_error_chunk`.
    fn flush_event(&mut self, out: &mut Vec<Result<ChatStreamEvent, InferenceError>>) {
        if self.data.is_empty() {
            return;
        }
        let payload = std::mem::take(&mut self.data);
        let payload = payload.trim();
        if payload.is_empty() {
            return;
        }
        if payload == "[DONE]" {
            out.push(Ok(self.terminal_event()));
            self.done = true;
            return;
        }
        let chunk: StreamChunk = match serde_json::from_str(payload) {
            Ok(c) => c,
            Err(_) => return, // Best-effort: skip a malformed frame.
        };
        if let Some(err) = chunk.error {
            let status = err.code.unwrap_or(0);
            out.push(Err(InferenceError::Api {
                status,
                body: err.message,
            }));
            self.done = true;
            return;
        }
        if let Some(block) = chunk.usage {
            self.usage = block.into_usage();
        }
        for choice in chunk.choices {
            if let Some(reason) = choice.finish_reason.as_deref() {
                self.finish_reason = Some(StopReason::from_wire(reason));
            }
            if let Some(text) = choice.delta.content.filter(|s| !s.is_empty()) {
                out.push(Ok(ChatStreamEvent::Delta(text)));
            }
            for tc in choice.delta.tool_calls {
                let (name, arguments) = tc
                    .function
                    .map(|f| (f.name, f.arguments.unwrap_or_default()))
                    .unwrap_or((None, String::new()));
                out.push(Ok(ChatStreamEvent::ToolCall(ToolCallDelta {
                    index: tc.index,
                    id: tc.id,
                    name,
                    arguments,
                })));
            }
        }
    }

    /// Build the terminal [`ChatStreamEvent::Done`] from accumulated state.
    ///
    /// Why: the finish reason and usage may arrive in different frames than the
    /// `[DONE]` sentinel; the decoder always reports the latest of each.
    /// What: clones the running finish reason + usage into a [`StreamCompletion`].
    /// Test: `decode_carries_usage_in_terminal`.
    fn terminal_event(&self) -> ChatStreamEvent {
        ChatStreamEvent::Done(StreamCompletion {
            finish_reason: self.finish_reason.clone(),
            usage: self.usage,
        })
    }

    /// Flush the terminal event at end-of-stream if the provider never sent
    /// `[DONE]`.
    ///
    /// Why: not every provider emits the `[DONE]` sentinel; a clean socket EOF
    /// must still yield exactly one terminal event so the consumer's loop ends
    /// normally rather than truncating.
    /// What: returns the terminal [`ChatStreamEvent::Done`] once if no terminal
    /// was already dispatched; `None` afterwards (so EOF after `[DONE]` adds
    /// nothing).
    /// Test: `decode_eof_without_done_still_terminates`.
    pub fn finish(&mut self) -> Option<ChatStreamEvent> {
        if self.done {
            return None;
        }
        self.done = true;
        Some(self.terminal_event())
    }
}

// ── Stream adapters ───────────────────────────────────────────────────────────

/// Turn a stream of raw byte chunks into a stream of [`ChatStreamEvent`]s.
///
/// Why: the transport hands us `resp.bytes_stream()` (chunks off the socket); the
/// consumer wants ordered events. This wraps the [`SseDecoder`] in a pull-based
/// [`Stream`] so back-pressure and cancellation work naturally — dropping the
/// returned stream drops `byte_stream`, which aborts the underlying HTTP request.
/// What: drives `byte_stream`, feeding each chunk to an [`SseDecoder`] and
/// yielding the events it produces in order; a transport error becomes a terminal
/// `Err`; a clean EOF flushes the decoder's terminal event. Generic over the
/// chunk type (`B: AsRef<[u8]>`) and the transport error (`E: Display`) so it is
/// unit-testable with an in-memory byte stream and reusable beyond reqwest.
/// Test: `decode_event_stream_end_to_end`, `decode_event_stream_surfaces_transport_error`.
pub fn decode_event_stream<S, B, E>(byte_stream: S) -> ChatStream
where
    S: Stream<Item = Result<B, E>> + Send + 'static,
    B: AsRef<[u8]>,
    E: std::fmt::Display,
{
    struct State<S> {
        inner: S,
        decoder: SseDecoder,
        queue: VecDeque<Result<ChatStreamEvent, InferenceError>>,
        done: bool,
    }

    let init = State {
        inner: Box::pin(byte_stream),
        decoder: SseDecoder::new(),
        queue: VecDeque::new(),
        done: false,
    };

    let s = stream::unfold(init, |mut st| async move {
        loop {
            if let Some(ev) = st.queue.pop_front() {
                return Some((ev, st));
            }
            if st.done {
                return None;
            }
            match st.inner.next().await {
                Some(Ok(bytes)) => {
                    st.queue.extend(st.decoder.feed(bytes.as_ref()));
                }
                Some(Err(e)) => {
                    st.done = true;
                    return Some((Err(InferenceError::Transport(e.to_string())), st));
                }
                None => {
                    st.done = true;
                    if let Some(ev) = st.decoder.finish() {
                        return Some((Ok(ev), st));
                    }
                    return None;
                }
            }
        }
    });

    Box::pin(s)
}

/// Replay a completed [`ChatResponse`] as a one-shot [`ChatStream`].
///
/// Why: adapters without native streaming (Anthropic-direct, Bedrock, the test
/// double) must still satisfy `chat_stream`; buffering the finished response into
/// a single delta + terminal event gives the consumer one uniform interface. It
/// is also the trait's default `chat_stream` body.
/// What: yields the first choice's text as one [`ChatStreamEvent::Delta`] (when
/// non-empty), each of its tool calls as a [`ChatStreamEvent::ToolCall`], then a
/// terminal [`ChatStreamEvent::Done`] carrying the response's stop reason + usage.
/// Test: `buffered_stream_replays_response`.
pub fn buffered_stream(response: ChatResponse) -> ChatStream {
    let mut events: Vec<Result<ChatStreamEvent, InferenceError>> = Vec::new();
    if let Some(text) = response.first_text().filter(|s| !s.is_empty()) {
        events.push(Ok(ChatStreamEvent::Delta(text)));
    }
    for (index, call) in response.first_tool_calls().iter().enumerate() {
        events.push(Ok(ChatStreamEvent::ToolCall(ToolCallDelta {
            index,
            id: Some(call.id.clone()),
            name: Some(call.function.name.clone()),
            arguments: call.function.arguments.clone(),
        })));
    }
    events.push(Ok(ChatStreamEvent::Done(StreamCompletion {
        finish_reason: response.stop_reason(),
        usage: response.usage(),
    })));
    Box::pin(stream::iter(events))
}

#[cfg(test)]
mod tests;
