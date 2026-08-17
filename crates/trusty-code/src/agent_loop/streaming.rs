//! Token-level streaming for one assistant turn (#4425 — tcode streaming epic
//! #3696's Gap B), with #2272's bounded in-turn inference retry.
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
//! `chat` call. A sink attached (the daemon/TUI path) streams. Both branches
//! run under one three-attempt retry policy for transient provider failures.
//! The turn id is minted by the caller and shared by every delta of the turn.
//! Test: `agent_loop::tests::sink_events` — delta sequencing (`done: false` per
//! chunk, exactly one `done: true`), the tool-only turn emitting nothing, and
//! equivalence of the assembled response with the blocking path's.
//! `agent_loop::tests::inference_retry` — the retry budget, backoff, classifier,
//! exposure boundary, and deadline containment.

use std::time::Duration;

use futures_util::StreamExt;

use super::{AgentLoop, AgentLoopError};
use crate::llm::{ChatRequest, ChatResponse, ChatStreamEvent, InferenceError, StreamAssembly};

// #2272: three attempts bound transient provider disruption inside one logical
// turn, so a flaky 503 does not restart a delegated engineer from scratch.
const INFERENCE_MAX_ATTEMPTS: u8 = 3;
const INFERENCE_RETRY_DELAYS: [Duration; 2] =
    [Duration::from_millis(100), Duration::from_millis(200)];

// Every attempt except the last is followed by exactly one delay, so the two
// constants must stay in step; `wait_before_inference_retry` indexes the delay
// table directly and this makes an out-of-step edit a compile error rather than
// a runtime panic.
const _: () = assert!(INFERENCE_MAX_ATTEMPTS as usize == INFERENCE_RETRY_DELAYS.len() + 1);

/// Whether any text from the current stream attempt has reached the sink.
///
/// Why: a retry is only invisible while the user has seen nothing. Once one
/// non-empty delta is rendered, replaying the turn would duplicate visible text
/// in the transcript bubble, so a mid-stream failure must propagate instead.
/// What: `Unexposed` until the first non-empty [`ChatStreamEvent::Delta`] is
/// forwarded, `Exposed` from then on. Reset per attempt, because a fresh stream
/// starts from a fresh (still invisible) assembly.
/// Test: `agent_loop::tests::inference_retry::stream_failure_after_text_delta_is_not_retried`,
/// `agent_loop::tests::inference_retry::stream_exposed_on_a_later_attempt_is_not_retried_again`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResponseExposure {
    Unexposed,
    Exposed,
}

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
    /// delta of this turn and MUST be unique across all agents in the session,
    /// and is shared by every retry attempt: a retry is one logical turn.
    ///
    /// (#2272) Both transports run the same three-attempt policy. Only
    /// [`InferenceError::is_retryable`] failures retry, after 100 ms then
    /// 200 ms, replaying the identical request. A streaming attempt retries only
    /// while [`ResponseExposure::Unexposed`]; after that the failure propagates,
    /// because replaying would duplicate text the user already read. Retries do
    /// NOT extend the caller's wall-clock deadline — `run_with_transcript`
    /// wraps this method in `tokio::time::timeout`, which stays the outer bound
    /// and covers the backoff sleeps.
    ///
    /// A `chat_stream` handshake failure is never silently retried as a
    /// BLOCKING call: the shared adapter deliberately declines to degrade to
    /// buffered on its own, and a silent fallback here would make "is streaming
    /// working?" unanswerable from the outside — the operator would see a
    /// working, non-streaming tcode and no signal that anything failed. A
    /// retried handshake reopens a STREAM, so that property holds.
    /// Test: `agent_loop::tests::sink_events::*`,
    /// `agent_loop::tests::inference_retry::*`.
    pub(super) async fn chat_turn(
        &self,
        request: &ChatRequest,
        turn_id: &str,
    ) -> Result<ChatResponse, AgentLoopError> {
        let Some(sink) = self.sink.clone() else {
            let mut attempt = 1;
            loop {
                match self.llm.chat(request).await {
                    Ok(response) => return Ok(response),
                    Err(error) if error.is_retryable() && attempt < INFERENCE_MAX_ATTEMPTS => {
                        self.wait_before_inference_retry(attempt, &error).await;
                        attempt += 1;
                    }
                    Err(error) => {
                        if error.is_retryable() {
                            self.warn_inference_retry_exhausted(attempt, &error);
                        }
                        return Err(AgentLoopError::Llm(error));
                    }
                }
            }
        };

        let mut attempt = 1;
        loop {
            let mut stream = match self.llm.chat_stream(request).await {
                Ok(stream) => stream,
                Err(error) if error.is_retryable() && attempt < INFERENCE_MAX_ATTEMPTS => {
                    self.wait_before_inference_retry(attempt, &error).await;
                    attempt += 1;
                    continue;
                }
                Err(error) => {
                    if error.is_retryable() {
                        self.warn_inference_retry_exhausted(attempt, &error);
                    }
                    return Err(AgentLoopError::Llm(error));
                }
            };
            // Both are per-attempt: a retry discards the partial assembly along
            // with the exposure it never produced, so the next attempt assembles
            // a whole response rather than splicing two half-streams together.
            let mut assembly = StreamAssembly::new();
            let mut exposure = ResponseExposure::Unexposed;
            let mut retry_error = None;

            while let Some(event) = stream.next().await {
                let event = match event {
                    Ok(event) => event,
                    Err(error)
                        if error.is_retryable()
                            && exposure == ResponseExposure::Unexposed
                            && attempt < INFERENCE_MAX_ATTEMPTS =>
                    {
                        retry_error = Some(error);
                        break;
                    }
                    Err(error) => {
                        if error.is_retryable() && exposure == ResponseExposure::Unexposed {
                            self.warn_inference_retry_exhausted(attempt, &error);
                        }
                        return Err(AgentLoopError::Llm(error));
                    }
                };
                if let ChatStreamEvent::Delta(chunk) = &event
                    && !chunk.is_empty()
                {
                    exposure = ResponseExposure::Exposed;
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

            if let Some(error) = retry_error {
                self.wait_before_inference_retry(attempt, &error).await;
                attempt += 1;
                continue;
            }

            // The terminal `done: true` carries no new text — the deltas above
            // already delivered every character. It exists so a subscriber knows
            // the bubble is complete and can stop rendering a caret.
            if exposure == ResponseExposure::Exposed {
                sink.agent_message(self.agent_name(), self.agent_id_str(), turn_id, "", true)
                    .await;
            }

            // The response id is not reported by the OpenAI-dialect SSE terminal
            // frame, so it is left empty; `model` echoes the requested slug (see
            // `crate::llm::resolved_model`, which falls back to exactly this value
            // whenever a provider omits its own).
            return Ok(assembly.into_response(String::new(), request.model.clone()));
        }
    }

    /// Log the failed attempt and sleep its backoff before the next one.
    ///
    /// Why: an operator watching a slow turn needs to see that it is retrying
    /// rather than hung, and which provider is misbehaving.
    /// What: warns with the attempt number, provider, and error class, then
    /// sleeps `INFERENCE_RETRY_DELAYS[failed_attempt - 1]` — in range for every
    /// `failed_attempt < INFERENCE_MAX_ATTEMPTS`, which is the only condition
    /// under which the callers invoke this.
    /// Test: `agent_loop::tests::inference_retry::blocking_retry_delays_are_100_then_200_milliseconds`.
    async fn wait_before_inference_retry(&self, failed_attempt: u8, error: &InferenceError) {
        let delay = INFERENCE_RETRY_DELAYS[usize::from(failed_attempt - 1)];
        tracing::warn!(
            attempt = failed_attempt,
            max_attempts = INFERENCE_MAX_ATTEMPTS,
            provider = self.llm.name(),
            error_class = inference_error_class(error),
            delay_ms = delay.as_millis(),
            "retryable inference attempt failed; retrying"
        );
        tokio::time::sleep(delay).await;
    }

    /// Warn that a transient failure escaped because the budget ran out.
    ///
    /// Why: a caller sees only the last error, which looks identical to a
    /// first-attempt failure — this line is what distinguishes "the provider
    /// blipped once" from "the provider is down".
    /// Test: covered indirectly by
    /// `agent_loop::tests::inference_retry::blocking_retry_exhaustion_returns_third_original_error`.
    fn warn_inference_retry_exhausted(&self, attempt: u8, error: &InferenceError) {
        tracing::warn!(
            attempt,
            max_attempts = INFERENCE_MAX_ATTEMPTS,
            provider = self.llm.name(),
            error_class = inference_error_class(error),
            "retryable inference attempts exhausted"
        );
    }
}

/// A stable, low-cardinality label for an inference failure.
///
/// Why: the error's `Display` embeds response bodies and provider ids, which is
/// wrong for a log field an operator groups by.
/// Test: exercised by every `agent_loop::tests::inference_retry` case that logs.
fn inference_error_class(error: &InferenceError) -> &'static str {
    match error {
        InferenceError::Transport(_) => "transport",
        InferenceError::Api { .. } => "api",
        InferenceError::Deserialise { .. } => "deserialise",
        InferenceError::MissingCredential { .. } => "missing_credential",
        InferenceError::NoAdapterRegistered { .. } => "no_adapter_registered",
        InferenceError::Provider(_) => "provider",
        InferenceError::Unsupported(_) => "unsupported",
        InferenceError::MissingConfig(_) => "missing_config",
    }
}
