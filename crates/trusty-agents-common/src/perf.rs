//! Portable perf value types shared between `trusty-agents` and external crates.
//!
//! Why: (#47) Tracking per-phase token counts, costs, and durations is a
//! cross-cutting concern used by the workflow engine, the PM loop, the
//! session record, and future external analysis tools. Placing the plain-data
//! types here (without stateful collection or async I/O) lets every layer
//! reference them without depending on the full `trusty-agents` binary crate.
//! What: Defines the four portable value types:
//! - `TokenUsage` — per-LLM-call token counter (prompt/completion/cache)
//! - `PhaseRecord` — per-phase duration + token snapshot
//! - `PerfTotals` — rolled-up totals across all phases in a run
//! - `PerfRecord` — full run record (persisted to `docs/performance/runs/`)
//! `PerfCollector` (stateful, tokio-dependent) is NOT here — it stays in
//! `trusty-agents::perf`.
//! Test: `token_usage_default_is_zeros`, `token_usage_accumulates` in
//! `trusty-agents::perf::tests`; compile-tested via `trusty-agents-common`
//! unit tests.

use serde::{Deserialize, Serialize};

/// Token usage captured from a single LLM round-trip.
///
/// Why: (#50) Anthropic's OpenRouter responses carry cache_read_input_tokens
/// and cache_creation_input_tokens alongside the standard prompt/completion
/// counts. Exposing those as first-class fields lets `PerfCollector` track
/// cache effectiveness over time. For non-Anthropic models these stay 0.
/// What: Plain struct; additive (`+`-style) accumulation is done inline in
/// `PerfCollector::totals`.
/// Test: `token_usage_default_is_zeros`, `token_usage_accumulates` in
/// `trusty-agents::perf::tests`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_creation_tokens: u32,
}

impl TokenUsage {
    /// Construct a `TokenUsage` from the four individual counts.
    pub fn new(prompt: u32, completion: u32, cache_read: u32, cache_creation: u32) -> Self {
        Self {
            prompt_tokens: prompt,
            completion_tokens: completion,
            cache_read_tokens: cache_read,
            cache_creation_tokens: cache_creation,
        }
    }

    /// Add another usage record into this one (in place).
    ///
    /// Why: Each phase may comprise multiple LLM turns (tool loop); we sum
    /// them into a single `PhaseRecord`.
    /// What: Field-wise saturating add.
    /// Test: `token_usage_accumulates` in `trusty-agents::perf::tests`.
    pub fn add(&mut self, other: &TokenUsage) {
        self.prompt_tokens = self.prompt_tokens.saturating_add(other.prompt_tokens);
        self.completion_tokens = self
            .completion_tokens
            .saturating_add(other.completion_tokens);
        self.cache_read_tokens = self
            .cache_read_tokens
            .saturating_add(other.cache_read_tokens);
        self.cache_creation_tokens = self
            .cache_creation_tokens
            .saturating_add(other.cache_creation_tokens);
    }
}

/// One phase's measured performance.
///
/// Why: Recording per-phase data (not just totals) lets post-hoc analysis
/// pinpoint which phase consumed the most tokens or time.
/// What: Name, duration, the four token buckets from `TokenUsage`, and the
/// computed USD cost.
/// Test: Constructed by `PerfCollector::record_phase` in `trusty-agents::perf`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseRecord {
    pub name: String,
    pub duration_ms: u64,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_creation_tokens: u32,
    pub cost_usd: f64,
}

/// Totals rolled up from every `PhaseRecord` in a run.
///
/// Why: Convenience aggregate so consumers don't have to sum phases themselves.
/// What: Field-wise sums of the four token buckets plus total cost.
/// Test: `collector_totals_sum_phases` in `trusty-agents::perf::tests`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PerfTotals {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_creation_tokens: u32,
    pub cost_usd: f64,
}

/// Full performance record persisted to `docs/performance/runs/`.
///
/// Why: One JSON file per run under `runs/` gives greppable history without a
/// database; the `runs.log` one-liner makes it easy to spot regressions.
/// What: Build number, version, workflow name, task preview, ISO start time,
/// total duration, all phases, rolled-up totals, run status, skills used/considered,
/// and optional QA test counts.
/// Test: `collector_flush_writes_json_and_log`, `test_run_record_serializes_test_counts`
/// in `trusty-agents::perf::tests`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerfRecord {
    pub build: u64,
    pub version: String,
    pub workflow: String,
    pub task_preview: String,
    pub started_at: String,
    pub total_duration_ms: u64,
    pub phases: Vec<PhaseRecord>,
    pub totals: PerfTotals,
    /// Run outcome (#56): "success" | "partial" | "failed".
    ///
    /// Why: When a phase fails mid-workflow, we still want to persist the
    /// perf record so we can see where/how the run died. A separate `status`
    /// field distinguishes clean completions from partial runs in tooling
    /// that scans `runs/*.json`.
    /// What: Defaults to "success" for back-compat with older fixtures.
    #[serde(default = "default_status")]
    pub status: String,
    /// Name of the phase that failed, when `status != "success"` (#56).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_phase: Option<String>,
    /// Distinct skill names injected into any phase prompt during this run
    /// (#171).
    ///
    /// Why: Post-run persistence needs to know which skills were used so it
    /// can increment `use_count` and refresh `last_used`. Carrying the list
    /// through `PerfRecord` keeps the data in one place that's already
    /// flushed to disk for analysis.
    /// What: Deduplicated, insertion-order-preserved list of skill names.
    /// Defaults to empty for back-compat with older fixtures.
    #[serde(default)]
    pub skills_used: Vec<String>,
    /// Distinct skill names matched by the pre-plan discovery step (#173).
    ///
    /// Why: Discovery surfaces every skill the engine considered relevant for
    /// a task — even if downstream prompt assembly only injected a subset (or
    /// none). Recording the broader candidate set separately from
    /// `skills_used` lets us measure recall vs. precision over time and
    /// audit which signals the discovery step matched on.
    /// What: Deduplicated, insertion-order-preserved list of skill names.
    /// Defaults to empty for back-compat with older fixtures.
    #[serde(default)]
    pub skills_considered: Vec<String>,
    /// Tests passed in the QA phase, when the QA agent emitted a parsable
    /// JSON envelope with a `passed` count.
    ///
    /// Why: Run-over-run comparisons want raw test counts, not just the
    /// pass/fail status. Tracking `tests_passed`/`tests_failed` separately
    /// from `status` lets dashboards show "1118/1118 passing" without
    /// re-parsing QA output.
    /// What: `Some(N)` when QA returned a JSON envelope with `passed`,
    /// `None` otherwise. Defaults to `None` for back-compat with older
    /// fixtures and for runs that skip the QA phase.
    /// Test: `test_run_record_serializes_test_counts` in `trusty-agents::perf::tests`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tests_passed: Option<u64>,
    /// Tests failed in the QA phase, paired with `tests_passed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tests_failed: Option<u64>,
}

fn default_status() -> String {
    "success".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_usage_default_is_zeros() {
        let u = TokenUsage::default();
        assert_eq!(u.prompt_tokens, 0);
        assert_eq!(u.completion_tokens, 0);
        assert_eq!(u.cache_read_tokens, 0);
        assert_eq!(u.cache_creation_tokens, 0);
    }

    #[test]
    fn token_usage_accumulates() {
        let mut a = TokenUsage::new(10, 5, 2, 1);
        a.add(&TokenUsage::new(3, 7, 0, 4));
        assert_eq!(a, TokenUsage::new(13, 12, 2, 5));
    }

    #[test]
    fn perf_record_status_defaults_to_success() {
        let rec = PerfRecord {
            build: 1,
            version: "0.1.0".to_string(),
            workflow: "test".to_string(),
            task_preview: "task".to_string(),
            started_at: "2026-01-01T00:00:00Z".to_string(),
            total_duration_ms: 100,
            phases: vec![],
            totals: PerfTotals::default(),
            status: default_status(),
            failed_phase: None,
            skills_used: vec![],
            skills_considered: vec![],
            tests_passed: None,
            tests_failed: None,
        };
        assert_eq!(rec.status, "success");
        assert!(rec.failed_phase.is_none());
    }
}
