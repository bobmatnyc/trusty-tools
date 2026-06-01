//! Token cost summary and trajectory types.
//!
//! Why: operators need to monitor the cost of running longitudinal profiles
//! across many contributors; separating these telemetry/direction types keeps
//! each file focused and under the 500-line cap.
//! What: defines `TokenCostSummary` and `Trajectory`.
//! Test: `token_cost_summary_defaults_to_zero` and `trajectory_serde_roundtrip`
//! in the parent `tests` module.

use serde::{Deserialize, Serialize};

// ─── TokenCostSummary ─────────────────────────────────────────────────────────

/// Aggregate LLM token usage and cost for a profile generation run.
///
/// Why: operators need to monitor the cost of running longitudinal profiles
/// across many contributors, especially as the number of periods grows.
/// What: accumulates `input_tokens`, `output_tokens`, estimated `cost_usd`,
/// and wall-clock `latency_ms` across all LLM calls in a single profile run.
/// Test: see `token_cost_summary_defaults_to_zero`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenCostSummary {
    /// Total input tokens consumed across all LLM calls.
    pub input_tokens: u64,
    /// Total output tokens produced across all LLM calls.
    pub output_tokens: u64,
    /// Total estimated cost in USD.
    pub cost_usd: f64,
    /// Total wall-clock latency in milliseconds (sum of all call latencies).
    pub latency_ms: u64,
}

impl TokenCostSummary {
    /// Accumulate token/cost/latency from one LLM response into this summary.
    ///
    /// Why: each LLM call (one per period, plus the synthesiser call) returns a
    /// `LlmResponse`; accumulating into `TokenCostSummary` gives the total run cost.
    /// What: adds `input_tokens`, `output_tokens`, `cost_usd`, and `latency_ms` in-place.
    /// Test: exercised by `batch_reviewer` and `synthesizer` accumulation tests.
    pub fn accumulate(
        &mut self,
        input_tokens: u64,
        output_tokens: u64,
        cost_usd: f64,
        latency_ms: u64,
    ) {
        self.input_tokens += input_tokens;
        self.output_tokens += output_tokens;
        self.cost_usd += cost_usd;
        self.latency_ms += latency_ms;
    }
}

// ─── Trajectory ──────────────────────────────────────────────────────────────

/// Overall quality trajectory direction for a contributor over the profile
/// window.
///
/// Why: the profile narrative needs a single high-level signal that the LLM
/// and callers can use to route action (e.g. flag declining contributors for
/// coaching vs. commend improving ones).
/// What: three-variant enum serialised as `snake_case`.
/// Test: see `trajectory_serde_roundtrip`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trajectory {
    /// Quality metrics are trending upward across periods.
    Improving,
    /// Quality metrics are roughly flat (within noise).
    Stable,
    /// Quality metrics are trending downward across periods.
    Declining,
}
