//! Activity state types and per-session LLM verdict cache.
//!
//! Why: checking whether a Claude Code session is working, idle, or blocked
//! involves an LLM call that costs tokens. Caching the verdict when the pane
//! content has not changed eliminates redundant LLM round-trips and makes the
//! activity monitor affordable to run frequently.
//! What: defines the activity state machine ([`ActivityState`]), the LLM verdict
//! ([`ActivityVerdict`]), per-check metrics ([`CheckMetrics`]), cumulative cost
//! tallying ([`CostTally`]), and the cache itself ([`ActivityCache`]).
//! Test: `cache_hit_on_same_hash`, `cache_miss_on_new_hash`,
//! `tally_accumulates_llm_calls`, `tally_accumulates_cache_hits`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The inferred activity state of a Claude Code session.
///
/// Why: operators and the daemon need a concise, machine-readable label for
/// what a session is doing so they can decide whether to intervene or wait.
/// What: an enum covering the states the LLM classifier can return, plus
/// `Unknown` for when classification fails or the API key is absent.
/// Test: serde round-tripped in `activity_state_round_trip`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityState {
    /// The session is actively processing or writing code.
    Working,
    /// The session is present but shows no recent activity.
    Idle,
    /// The session is awaiting a human permission decision.
    BlockedOnPermission,
    /// The session has encountered an error and stopped.
    Errored,
    /// The session has completed its task.
    Done,
    /// Classification was not possible (missing key, transient failure, etc.).
    Unknown,
}

/// LLM-produced verdict about a session's current activity.
///
/// Why: a verdict bundles the state classification with a human-readable
/// summary and a confidence score so callers can decide how much to trust it.
/// What: `state` is the classification; `summary` is a ≤ 120-char human note;
/// `confidence` is in [0.0, 1.0].
/// Test: `verdict_serde_round_trip`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityVerdict {
    /// The inferred activity state.
    pub state: ActivityState,
    /// Short human-readable summary of the classification rationale.
    pub summary: String,
    /// Classifier confidence, in [0.0, 1.0].
    pub confidence: f32,
}

/// Token and latency metrics captured for a single activity check.
///
/// Why: the operator dashboard and cost-reporting surfaces need per-check data
/// so they can compute usage and latency distributions over time.
/// What: captures the session id, timestamp, model, token counts, latency, and
/// whether the LLM was actually called (vs. a cache hit returning the prior verdict).
/// Test: recorded in `ActivityCache::update_llm_hit` / `update_cache_hit`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckMetrics {
    /// The session this check was for.
    pub session_id: String,
    /// When the check completed.
    pub at: DateTime<Utc>,
    /// The model used for classification (empty string on cache hit).
    pub model: String,
    /// Input tokens consumed (0 on cache hit).
    pub input_tokens: u32,
    /// Output tokens consumed (0 on cache hit).
    pub output_tokens: u32,
    /// Wall-clock latency of the check in milliseconds.
    pub latency_ms: u64,
    /// True if the result was served from the content-hash cache.
    pub cache_hit: bool,
    /// The activity state returned by this check.
    pub verdict_state: ActivityState,
}

/// Running totals for all activity checks across a session.
///
/// Why: the operator dashboard needs aggregate cost/usage data without
/// iterating over every individual `CheckMetrics` record.
/// What: accumulates total checks, LLM calls, and token consumption.
/// Test: `tally_accumulates_llm_calls`, `tally_accumulates_cache_hits`.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct CostTally {
    /// Total number of `check()` calls made (cache hits + LLM calls).
    pub total_checks: u64,
    /// Number of checks that actually called the LLM (cache misses).
    pub llm_calls_made: u64,
    /// Cumulative input tokens consumed across all LLM calls.
    pub total_input_tokens: u64,
    /// Cumulative output tokens consumed across all LLM calls.
    pub total_output_tokens: u64,
}

/// Per-session activity verdict cache keyed by pane-content hash.
///
/// Why: pane content rarely changes between check intervals, so hashing the
/// last N lines and skipping the LLM call on a match eliminates redundant
/// spend without reducing verdict freshness.
/// What: stores the last seen content hash, the last verdict, and a running
/// cost tally; exposes methods for callers to check and update the cache.
/// Test: `cache_hit_on_same_hash`, `cache_miss_on_new_hash`.
#[derive(Debug)]
pub struct ActivityCache {
    /// SHA-256 hex digest of the last seen pane tail.
    pub last_hash: Option<String>,
    /// The LLM verdict that corresponds to `last_hash`.
    pub last_verdict: Option<ActivityVerdict>,
    /// Cumulative cost tally for this session.
    pub tally: CostTally,
    /// Model identifier used for LLM calls.
    pub model: String,
}

impl ActivityCache {
    /// Construct a fresh cache for a session.
    ///
    /// Why: each new session starts with no history; constructing an empty
    /// cache avoids Option-wrapping the entire struct at the call site.
    /// What: zeroes all fields; stores the model string for metrics.
    /// Test: used in every cache unit test.
    pub fn new(model: &str) -> Self {
        Self {
            last_hash: None,
            last_verdict: None,
            tally: CostTally::default(),
            model: model.to_owned(),
        }
    }

    /// Return `true` if the given hash matches the last cached hash.
    ///
    /// Why: this is the cache-hit predicate; callers call this before deciding
    /// whether to issue an LLM request.
    /// What: compares `hash` against `self.last_hash`; `None` never matches.
    /// Test: `cache_hit_on_same_hash`, `cache_miss_on_new_hash`.
    pub fn check_unchanged(&self, hash: &str) -> bool {
        self.last_hash.as_deref() == Some(hash)
    }

    /// Record a cache hit: the hash matched, verdict served from cache.
    ///
    /// Why: the tally must count cache hits so the dashboard can show the
    /// hit/miss ratio and confirm the cache is effective.
    /// What: bumps `total_checks`; does NOT bump `llm_calls_made` or tokens.
    /// Test: `tally_accumulates_cache_hits`.
    pub fn update_cache_hit(&mut self, hash: &str, _metrics: CheckMetrics) {
        // Ensure the hash pointer stays fresh even on a hit (defensive).
        self.last_hash = Some(hash.to_owned());
        self.tally.total_checks += 1;
    }

    /// Record an LLM hit: the hash was new, the LLM was called.
    ///
    /// Why: caches the new verdict and hash, and updates the tally so the
    /// dashboard reflects the actual LLM spend.
    /// What: replaces `last_hash`, `last_verdict`; bumps all tally counters.
    /// Test: `tally_accumulates_llm_calls`.
    pub fn update_llm_hit(&mut self, hash: &str, verdict: ActivityVerdict, metrics: CheckMetrics) {
        self.last_hash = Some(hash.to_owned());
        self.last_verdict = Some(verdict);
        self.tally.total_checks += 1;
        self.tally.llm_calls_made += 1;
        self.tally.total_input_tokens += u64::from(metrics.input_tokens);
        self.tally.total_output_tokens += u64::from(metrics.output_tokens);
    }

    /// Return the last cached verdict, if any.
    ///
    /// Why: cache-hit callers need to retrieve the verdict without mutating
    /// the cache.
    /// What: returns a reference to `self.last_verdict`.
    /// Test: `cache_hit_on_same_hash`.
    pub fn last_verdict(&self) -> Option<&ActivityVerdict> {
        self.last_verdict.as_ref()
    }

    /// Return a reference to the running cost tally.
    ///
    /// Why: the monitor surfaces the tally in `ActivityCheckResult` so the
    /// caller can include it in API responses and dashboards.
    /// What: borrows `self.tally`.
    /// Test: `tally_accumulates_llm_calls`, `tally_accumulates_cache_hits`.
    pub fn tally(&self) -> &CostTally {
        &self.tally
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_metrics(cache_hit: bool) -> CheckMetrics {
        CheckMetrics {
            session_id: "s1".into(),
            at: Utc::now(),
            model: "openai/gpt-4o-mini".into(),
            input_tokens: 100,
            output_tokens: 20,
            latency_ms: 200,
            cache_hit,
            verdict_state: ActivityState::Working,
        }
    }

    fn working_verdict() -> ActivityVerdict {
        ActivityVerdict {
            state: ActivityState::Working,
            summary: "Session is writing code".into(),
            confidence: 0.9,
        }
    }

    #[test]
    fn cache_hit_on_same_hash() {
        let mut cache = ActivityCache::new("gpt-4o-mini");
        let hash = "abc123";
        let verdict = working_verdict();
        cache.update_llm_hit(hash, verdict.clone(), fake_metrics(false));
        assert!(cache.check_unchanged(hash));
        assert_eq!(cache.last_verdict().unwrap().summary, verdict.summary);
    }

    #[test]
    fn cache_miss_on_new_hash() {
        let mut cache = ActivityCache::new("gpt-4o-mini");
        cache.update_llm_hit("old_hash", working_verdict(), fake_metrics(false));
        assert!(!cache.check_unchanged("new_hash"));
    }

    #[test]
    fn tally_accumulates_llm_calls() {
        let mut cache = ActivityCache::new("gpt-4o-mini");
        cache.update_llm_hit("h1", working_verdict(), fake_metrics(false));
        cache.update_llm_hit("h2", working_verdict(), fake_metrics(false));
        assert_eq!(cache.tally().total_checks, 2);
        assert_eq!(cache.tally().llm_calls_made, 2);
        assert_eq!(cache.tally().total_input_tokens, 200);
        assert_eq!(cache.tally().total_output_tokens, 40);
    }

    #[test]
    fn tally_accumulates_cache_hits() {
        let mut cache = ActivityCache::new("gpt-4o-mini");
        cache.update_llm_hit("h1", working_verdict(), fake_metrics(false));
        cache.update_cache_hit("h1", fake_metrics(true));
        cache.update_cache_hit("h1", fake_metrics(true));
        assert_eq!(cache.tally().total_checks, 3);
        assert_eq!(cache.tally().llm_calls_made, 1);
    }

    #[test]
    fn activity_state_round_trip() {
        let states = [
            ActivityState::Working,
            ActivityState::Idle,
            ActivityState::BlockedOnPermission,
            ActivityState::Errored,
            ActivityState::Done,
            ActivityState::Unknown,
        ];
        for state in &states {
            let json = serde_json::to_string(state).expect("serialize");
            let back: ActivityState = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(&back, state);
        }
    }

    #[test]
    fn verdict_serde_round_trip() {
        let v = working_verdict();
        let json = serde_json::to_string(&v).expect("serialize");
        let back: ActivityVerdict = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.summary, v.summary);
        assert_eq!(back.state, v.state);
    }
}
