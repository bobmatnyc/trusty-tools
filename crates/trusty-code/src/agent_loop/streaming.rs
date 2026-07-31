//! Token-level streaming for one assistant turn (#4425 — tcode streaming epic
//! #3696's Gap B).
//!
//! Why: Gap A (Slice 1) made an assistant turn OBSERVABLE — `ToolEventSink::agent_message`
//! published the whole turn once, with `done: true`, after the blocking `chat`
//! call returned. That is the difference between a TUI and a Claude Code clone:
//! the user waits in silence for the entire turn, then the text appears as one
//! paste. Closing the gap needed a streaming transport, which trusty-code did
//! not have until #4425 migrated it onto `trusty_common::inference::InferenceAdapter`
//! — whose `chat_stream` already carries native SSE for the OpenAI-dialect
//! providers. This module is the loop-side half of that: it turns the shared
//! event stream into repeated `done: false` sink calls and, at the end, hands
//! the loop back the exact same `ChatResponse` the blocking path produced, so
//! nothing downstream (transcript, usage accrual, tool dispatch) changes.
//! What: [`AgentLoop::chat_turn`] — one method with two branches. NO sink
//! attached (the `run_task` CLI path, every scripted test) takes the blocking
//! `chat` call, byte-for-byte the pre-#4425 behaviour. A sink attached (the
//! daemon/TUI path) streams. The turn id is minted by the caller and shared by
//! every delta of the turn.
//! Test: `agent_loop::tests::sink_events` — delta sequencing (`done: false` per
//! chunk, exactly one `done: true`), the tool-only turn emitting nothing, and
//! equivalence of the assembled response with the blocking path's.

use futures_util::StreamExt;

use super::{AgentLoop, AgentLoopError};
use crate::llm::{ChatRequest, ChatResponse, ChatStreamEvent, StreamAssembly};

impl AgentLoop {
    /// Execute one assistant turn, streaming deltas to the sink when there is one.
    ///
    /// Why: the loop must not care whether a turn streamed. Returning a
    /// `ChatResponse` from BOTH branches is what keeps `push_response`, usage
    /// accrual, and tool dispatch on a single code path — the alternative (a
    /// separate streaming loop) is how the two paths drift until only one of
    /// them handles, say, a `finish_task` call correctly.
    /// What: with no sink, issues the blocking `chat`. With a sink, issues
    /// `chat_stream` and folds the events into a [`StreamAssembly`], forwarding
    /// each non-empty text delta to [`crate::agent_loop::ToolEventSink::agent_message`]
    /// with `done: false` and, once the stream ends, one final call with
    /// `done: true` — but ONLY if the turn produced text at all, so a tool-only
    /// turn stays silent exactly as it did before. `turn_id` correlates every
    /// delta of this turn and MUST be unique across all agents in the session.
    ///
    /// A `chat_stream` handshake failure is PROPAGATED, never silently retried
    /// as a blocking call: the shared adapter deliberately declines to degrade
    /// to buffered on its own, and a silent fallback here would make "is
    /// streaming working?" unanswerable from the outside — the operator would
    /// see a working, non-streaming tcode and no signal that anything failed.
    /// Test: `agent_loop::tests::sink_events::*`.
    pub(super) async fn chat_turn(
        &self,
        request: &ChatRequest,
        turn_id: &str,
    ) -> Result<ChatResponse, AgentLoopError> {
        let Some(sink) = self.sink.clone() else {
            return Ok(self.llm.chat(request).await?);
        };

        let mut stream = self.llm.chat_stream(request).await?;
        let mut assembly = StreamAssembly::new();
        let mut emitted_text = false;

        while let Some(event) = stream.next().await {
            let event = event?;
            if let ChatStreamEvent::Delta(chunk) = &event
                && !chunk.is_empty()
            {
                emitted_text = true;
                sink.agent_message(
                    self.agent_name(),
                    self.agent_id_str(),
                    turn_id,
                    chunk,
                    false,
                )
                .await;
            }
            assembly.push(event);
        }

        // The terminal `done: true` carries no new text — the deltas above
        // already delivered every character. It exists so a subscriber knows
        // the bubble is complete and can stop rendering a caret.
        if emitted_text {
            sink.agent_message(self.agent_name(), self.agent_id_str(), turn_id, "", true)
                .await;
        }

        // The response id is not reported by the OpenAI-dialect SSE terminal
        // frame, so it is left empty; `model` echoes the requested slug (see
        // `crate::llm::resolved_model`, which falls back to exactly this value
        // whenever a provider omits its own).
        Ok(assembly.into_response(String::new(), request.model.clone()))
    }
}
