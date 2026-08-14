//! Token-cost telemetry and the overall trajectory signal.
//!
//! Why: profiling many contributors multiplies LLM spend, so the run must
//! report what it cost; and every consumer of a profile wants one
//! high-level direction before reading any detail.
//! What: defines [`TokenCostSummary`] and [`Trajectory`].
//! Test: `token_cost_summary_defaults_to_zero`, `token_cost_summary_accumulate`,
//! and `trajectory_serde_roundtrip` in the parent `types` test module.

use serde::{Deserialize, Serialize};

// ─── TokenCostSummary ─────────────────────────────────────────────────────────

/// Aggregate LLM token usage and cost for one profile run.
///
/// Populated by the narrative pass; all-zero on a deterministic-only run.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenCostSummary {
    /// Total input tokens across all calls.
    pub input_tokens: u64,
    /// Total output tokens across all calls.
    pub output_tokens: u64,
    /// Total estimated cost in USD.
    pub cost_usd: f64,
    /// Summed wall-clock latency in milliseconds.
    pub latency_ms: u64,
}

impl TokenCostSummary {
    /// Add one call's usage into this summary.
    ///
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

// ─── Trajectory ───────────────────────────────────────────────────────────────

/// Direction of a contributor's quality scores over the profile window.
///
/// Why: this single value is what routes action — a `Declining` profile is
/// worth a conversation, a `Stable` one usually is not. Deriving it from the
/// score slope (`derive_trajectory`) rather than from the narrative means it
/// is still correct when the LLM pass is skipped or fails.
/// What: three variants, serialised as `snake_case`.
/// Test: `trajectory_serde_roundtrip`, `synthesizer_trajectory_from_slope`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trajectory {
    /// Quality scores trend upward across periods.
    Improving,
    /// Quality scores are flat within noise.
    Stable,
    /// Quality scores trend downward across periods.
    Declining,
}
