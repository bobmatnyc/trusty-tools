//! Error type for the multi-turn agent loop.
//!
//! Why: The loop has five distinct failure modes that callers handle
//! differently — an LLM transport/API failure (usually fatal), exhausting the
//! turn budget (recoverable: a partial transcript is still useful), the
//! wall-clock timeout firing (also carries partial work), (#2056)
//! cooperative cancellation via `AgentLoop::with_cancel_flag` (a
//! `session.cancel` on an in-flight daemon-driven run), and (#2265 fix #5)
//! an external stop signal firing via `AgentLoop::with_stop_signal` (e.g. the
//! `run_task` PM loop refusing to issue another `delegate_to_agent` call once
//! `run_task::redelegation::RedelegationCapSignal` has latched). A structured
//! enum lets callers branch without string-matching, and every abort variant
//! carries the partial `AgentOutput` so no work is lost.
//! What: Defines `AgentLoopError` with `Llm`, `TurnCapExceeded`, `Timeout`,
//! `Cancelled`, and `StoppedBySignal` variants, deriving `thiserror::Error`.
//! Test: `agent_loop::tests::turn_cap_returns_partial_transcript` asserts the
//! `TurnCapExceeded` variant carries the accumulated transcript;
//! `cancel_flag_aborts_before_next_turn` covers `Cancelled`;
//! `stop_signal_aborts_before_next_turn` covers `StoppedBySignal`.

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
    ///
    /// Note: the `partial` output is for INSPECTION / diagnostics ONLY. The cap
    /// can fire mid-turn — e.g. on an assistant turn that requested tool calls
    /// whose results were never appended — so the transcript may be internally
    /// inconsistent (dangling tool calls, no final answer). It is NOT safe to
    /// replay against an LLM or to resume from; treat it as a snapshot.
    #[error("turn cap of {max_turns} exceeded; returning partial transcript")]
    TurnCapExceeded {
        /// The configured turn limit that was hit.
        max_turns: u32,
        /// Partial output accumulated before the cap fired.
        ///
        /// Inspection/diagnostics only — see the variant note. May reflect an
        /// incomplete turn (e.g. tool calls without matching results); do NOT
        /// replay or resume from it.
        partial: Box<AgentOutput>,
    },

    /// The wall-clock timeout elapsed before the loop finished.
    ///
    /// Why: A model that stalls or a tool that hangs must not block the caller
    /// indefinitely; the timeout bounds total latency.
    /// What: Carries the configured `timeout_secs` and the partial `AgentOutput`
    /// assembled before the deadline.
    ///
    /// Note: the `partial` output is for INSPECTION / diagnostics ONLY. The
    /// deadline can elapse at any point — including between an assistant turn's
    /// tool calls and their results — so the transcript may be internally
    /// inconsistent (dangling tool calls, no final answer). It is NOT safe to
    /// replay against an LLM or to resume from; treat it as a snapshot.
    #[error("wall-clock timeout of {timeout_secs}s elapsed; returning partial transcript")]
    Timeout {
        /// The configured timeout in seconds that elapsed.
        timeout_secs: u64,
        /// Partial output accumulated before the deadline.
        ///
        /// Inspection/diagnostics only — see the variant note. May reflect an
        /// incomplete turn (e.g. tool calls without matching results); do NOT
        /// replay or resume from it.
        partial: Box<AgentOutput>,
    },

    /// The loop's `with_cancel_flag` cancellation flag was observed set
    /// (#2056: `session.cancel` on an in-flight daemon-driven run).
    ///
    /// Why: `session.cancel` must stop an in-flight task cooperatively rather
    /// than aborting the process or leaking the background task. The flag is
    /// checked once per turn boundary (before the next LLM call) — per the
    /// vision spec §12 (11.6), cancellation is NOT instantaneous: a tool call
    /// already in flight always completes first.
    /// What: Carries the partial `AgentOutput` assembled before cancellation
    /// was observed, for the same inspection-only purpose as the other two
    /// abort variants.
    #[error("cancelled; returning partial transcript")]
    Cancelled {
        /// Partial output accumulated before cancellation was observed.
        ///
        /// Inspection/diagnostics only — see the variant note on the other
        /// abort arms. Do NOT replay or resume from it.
        partial: Box<AgentOutput>,
    },

    /// The loop's `with_stop_signal` external stop condition was observed
    /// `true` at a turn boundary (#2265 fix #5).
    ///
    /// Why: A caller may know, independently of this loop's own turn/timeout
    /// budget, that continuing is pointless — e.g. `run_task`'s PM loop shares
    /// a `RedelegationCapSignal` with the engineer-delegation retry decorator;
    /// once that cap latches, every further `delegate_to_agent` call is a
    /// guaranteed dead end, so burning the PM's remaining turns issuing them
    /// (previously bounded only by `AgentLoopConfig::max_turns`) wasted up to
    /// 7 turns per run in the bake-off L1 evidence for zero additional
    /// engineer work. Checking the signal at the SAME turn boundary as the
    /// (#2056) cancellation flag — before the next `chat` call, never
    /// mid-tool-call — stops the loop the turn AFTER the condition becomes
    /// true, without dropping any in-flight tool dispatch.
    /// What: Carries the partial `AgentOutput` assembled before the signal
    /// was observed, for the same inspection-only purpose as the other abort
    /// variants.
    #[error("external stop signal observed; returning partial transcript")]
    StoppedBySignal {
        /// Partial output accumulated before the stop signal was observed.
        ///
        /// Inspection/diagnostics only — see the variant note on the other
        /// abort arms. Do NOT replay or resume from it.
        partial: Box<AgentOutput>,
    },
}
