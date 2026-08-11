//! Token-cost telemetry and trajectory types.
//!
//! Why: profiling a whole org means one narrative call per contributor per
//! period, so the run's token cost has to be visible in the output rather than
//! discovered on next month's invoice.
//! What: defines [`TokenCostSummary`] and [`Trajectory`].
//! Test: `token_cost_summary_defaults_to_zero`, `token_cost_summary_accumulate`,
//! and `trajectory_serde_roundtrip` in the parent `tests` module.

use serde::{Deserialize, Serialize};

// ─── TokenCostSummary ────────────────────────────────────────────────────────

/// Aggregate model token usage and cost for one profile run.
///
/// Why: see the module doc — cost per profile run is an operational number the
/// report must carry.
/// What: accumulates input/output tokens, estimated USD cost, and summed
/// wall-clock latency. Zero on every field means no model call was made, which
/// is what the renderer keys the cost section off.
/// Test: `token_cost_summary_defaults_to_zero`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenCostSummary {
    /// Total input tokens consumed.
    pub input_tokens: u64,
    /// Total output tokens produced.
    pub output_tokens: u64,
    /// Total estimated cost in USD.
    pub cost_usd: f64,
    /// Summed wall-clock latency in milliseconds.
    pub latency_ms: u64,
}

impl TokenCostSummary {
    /// Add one call's usage into this summary.
    ///
    /// Why: the narrative pass (#5464) makes several calls per profile; the
    /// report wants their total, not a list.
    /// What: adds each field in place.
    /// Test: `token_cost_summary_accumulate`.
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

/// Overall quality-trajectory direction across the profile window.
///
/// Why: a manager reading the profile needs one routing signal — coach, leave
/// alone, or commend — before reading any detail.
/// What: three-variant enum serialised as `snake_case`, derived from the
/// quality-score slope by
/// [`derive_trajectory`](crate::profile::synthesizer::derive_trajectory).
/// Test: `trajectory_serde_roundtrip`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trajectory {
    /// Quality metrics trend upward across periods.
    Improving,
    /// Quality metrics are flat within noise.
    Stable,
    /// Quality metrics trend downward across periods.
    Declining,
}
