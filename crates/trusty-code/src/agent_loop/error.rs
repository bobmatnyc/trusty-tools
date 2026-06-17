//! Error type for the multi-turn agent loop.
//!
//! Why: The loop has three distinct failure modes that callers handle
//! differently — an LLM transport/API failure (usually fatal), exhausting the
//! turn budget (recoverable: a partial transcript is still useful), and the
//! wall-clock timeout firing (also carries partial work). A structured enum lets
//! callers branch without string-matching, and the turn-cap variant carries the
//! partial `AgentOutput` so no work is lost.
//! What: Defines `AgentLoopError` with `Llm`, `TurnCapExceeded`, and `Timeout`
//! variants, deriving `thiserror::Error`.
//! Test: `agent_loop::tests::turn_cap_returns_partial_transcript` asserts the
//! `TurnCapExceeded` variant carries the accumulated transcript.

use thiserror::Error;

use crate::llm::LlmError;
use crate::tools::AgentOutput;

/// Failure modes of `AgentLoop::run`.
///
/// Why: Distinguishes a hard LLM failure from the two budget-exhaustion paths
/// so callers can decide whether to retry, surface partial output, or abort.
/// What: `Llm` wraps the underlying `LlmError`; `TurnCapExceeded` and `Timeout`
/// each carry the partial `AgentOutput` accumulated up to the point the limit
/// fired.
/// Test: Constructed and matched in `agent_loop::tests`.
#[derive(Debug, Error)]
pub enum AgentLoopError {
    /// An LLM chat call failed (transport, API, or deserialisation error).
    ///
    /// Why: Network/API failures are typically not recoverable within the loop;
    /// the caller decides whether to retry the whole run.
    /// What: Wraps the source `LlmError`.
    #[error("LLM call failed: {0}")]
    Llm(#[from] LlmError),

    /// The configured `max_turns` budget was exhausted before the model stopped.
    ///
    /// Why: Long tool-call chains can loop without converging; capping turns
    /// bounds cost. The partial transcript is still returned so the caller sees
    /// how far the run got.
    /// What: Carries the `AgentOutput` assembled from the turns that completed.
    #[error("turn cap of {max_turns} exceeded; returning partial transcript")]
    TurnCapExceeded {
        /// The configured turn limit that was hit.
        max_turns: u32,
        /// Partial output accumulated before the cap fired.
        partial: Box<AgentOutput>,
    },

    /// The wall-clock timeout elapsed before the loop finished.
    ///
    /// Why: A model that stalls or a tool that hangs must not block the caller
    /// indefinitely; the timeout bounds total latency.
    /// What: Carries the configured `timeout_secs` and the partial `AgentOutput`
    /// assembled before the deadline.
    #[error("wall-clock timeout of {timeout_secs}s elapsed; returning partial transcript")]
    Timeout {
        /// The configured timeout in seconds that elapsed.
        timeout_secs: u64,
        /// Partial output accumulated before the deadline.
        partial: Box<AgentOutput>,
    },
}
