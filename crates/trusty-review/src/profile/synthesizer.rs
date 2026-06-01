//! Cross-period synthesis for the contributor-profile pipeline (#566).
//!
//! Why: per-period findings (from `batch_reviewer`) are independent snapshots;
//! synthesis merges them across all periods to identify recurrence, resolution,
//! and worsening trends — the core longitudinal signal of the profile feature.
//! What: `Synthesizer` performs two steps:
//!   1. Deterministic dedup + trend-tag assignment using Jaccard token-set
//!      similarity (≥ 0.7 → Recurring/Resolved/Worsening; < 0.7 → New).
//!   2. One LLM "profiler" call that receives the deduped finding list,
//!      period quality scores, and frequency counts, and returns strengths,
//!      recurring_weaknesses, improvement_trajectory, and a narrative.
//!
//! Fail-safe: if the LLM call fails, the profile is still emitted with the
//! deterministic parts populated and an empty/placeholder narrative.
//!
//! Test: `tests` module below covers dedup, trend-tag assignment, quality_trend
//! derivation, trajectory from slope, and fail-safe narrative.

use std::sync::Arc;
use std::time::Instant;

use serde::Deserialize;
use tracing::{debug, warn};

use crate::llm::{LlmProvider, LlmResponse};
use crate::profile::types::{
    ContributorProfile, LongitudinalFinding, PeriodBatch, Trajectory, TrendTag,
};

pub use synthesizer_prompt::build_synthesizer_prompt;

#[path = "synthesizer_prompt.rs"]
mod synthesizer_prompt;

// ─── Prompt constants ─────────────────────────────────────────────────────────

const SYNTHESIZER_TEMPERATURE: f32 = 0.3;
const SYNTHESIZER_MAX_TOKENS: u32 = 2048;

/// Jaccard similarity threshold above which two findings are considered the
/// same conceptual issue.
const JACCARD_THRESHOLD: f64 = 0.7;

// ─── Synthesizer ──────────────────────────────────────────────────────────────

/// Cross-period finding synthesiser.
///
/// Why: individual period findings need to be merged and annotated with trend
/// tags before a coherent longitudinal narrative can be written.
/// What: holds an `LlmProvider` for the final narrative pass.  `synthesize`
/// performs deterministic dedup + trend-tag assignment then calls the LLM
/// for the narrative.  If the LLM call fails, a fallback narrative is used.
/// Test: see `tests` module below.
pub struct Synthesizer {
    llm: Arc<dyn LlmProvider>,
    model: String,
}

impl Synthesizer {
    /// Create a `Synthesizer` from an injected provider and model slug.
    ///
    /// Why: dependency injection allows tests to supply a fake provider.
    /// What: stores the provider and model slug for the narrative LLM call.
    /// Test: exercised by all synthesizer tests.
    pub fn new(llm: Arc<dyn LlmProvider>, model: impl Into<String>) -> Self {
        Self {
            llm,
            model: model.into(),
        }
    }

    /// Synthesise per-period findings into a complete `ContributorProfile`.
    ///
    /// Why: the profile pipeline needs one coherent output that combines the
    /// deterministic trend analysis with the LLM narrative.
    /// What:
    ///  1. Populates `profile.quality_trend` from `periods` stats.
    ///  2. Assigns `trend_tag` to every finding via Jaccard dedup.
    ///  3. Derives `improvement_trajectory` from quality score slope.
    ///  4. Calls the LLM for narrative + strengths + recurring_weaknesses.
    ///     On LLM failure: trajectory + findings are still emitted; narrative is
    ///     set to a fallback message.
    ///  5. Accumulates telemetry into `profile.token_cost`.
    ///
    /// Test: `tests::synthesizer_dedup_assigns_recurring`,
    /// `tests::synthesizer_quality_trend_populated`,
    /// `tests::synthesizer_fail_safe_narrative`.
    pub async fn synthesize(
        &self,
        mut profile: ContributorProfile,
        all_period_findings: Vec<Vec<LongitudinalFinding>>,
        periods: &[PeriodBatch],
    ) -> ContributorProfile {
        // Step 1: populate quality_trend deterministically.
        profile.quality_trend = periods
            .iter()
            .map(|b| (b.stats.period_label.clone(), b.stats.quality_score))
            .collect();

        // Step 2: flatten + dedup + assign trend_tags.
        let flat: Vec<LongitudinalFinding> = all_period_findings.into_iter().flatten().collect();
        let tagged = assign_trend_tags(flat);
        profile.all_findings = tagged;

        // Step 3: derive trajectory from quality score slope.
        profile.improvement_trajectory = derive_trajectory(&profile.quality_trend);

        // Step 4: LLM narrative pass.
        let start = Instant::now();
        let req = build_synthesizer_prompt(&profile, &self.model);

        match self.llm.complete(req).await {
            Ok(resp) => {
                let latency = start.elapsed().as_millis() as u64;
                profile.token_cost.accumulate(
                    resp.input_tokens as u64,
                    resp.output_tokens as u64,
                    resp.cost_usd,
                    latency,
                );
                debug!(
                    input_tokens = resp.input_tokens,
                    output_tokens = resp.output_tokens,
                    "synthesizer: LLM call complete"
                );
                apply_llm_synthesis(&mut profile, &resp);
            }
            Err(e) => {
                warn!(
                    error = %e,
                    "synthesizer: LLM call failed — using deterministic fallback (fail-safe)"
                );
                apply_fallback_narrative(&mut profile);
            }
        }

        profile
    }
}

// ─── Jaccard dedup + trend-tag assignment ─────────────────────────────────────

/// Assign `TrendTag` values to a flat list of findings from all periods.
///
/// Why: raw per-period findings have `trend_tag = None`; this function performs
/// cross-period dedup by Jaccard similarity and assigns the appropriate tag.
/// What:
///  - Two findings are considered "the same" if their description token-set
///    Jaccard similarity is ≥ `JACCARD_THRESHOLD`.
///  - A group of similar findings spanning ≥2 periods → `Recurring`.
///  - A group appearing only in the latest period (and not earlier) → `New`.
///  - A group that appeared in earlier periods but NOT the latest → `Resolved`.
///  - A group where the latest occurrence has higher severity than earlier → `Worsening`.
///
/// Returns the same findings with `trend_tag` set on every item.
///
/// Test: `tests::synthesizer_dedup_assigns_recurring`,
///   `tests::synthesizer_dedup_assigns_new`,
///   `tests::synthesizer_dedup_assigns_resolved`.
pub fn assign_trend_tags(findings: Vec<LongitudinalFinding>) -> Vec<LongitudinalFinding> {
    if findings.is_empty() {
        return findings;
    }

    // Collect unique period labels in insertion order.
    let mut period_order: Vec<String> = Vec::new();
    for f in &findings {
        if !period_order.contains(&f.period_label) {
            period_order.push(f.period_label.clone());
        }
    }
    let latest_period = period_order.last().cloned().unwrap_or_default();

    // Group findings by description-similarity clusters.
    let mut clusters: Vec<Vec<usize>> = Vec::new(); // indices into `findings`
    let mut assigned = vec![false; findings.len()];

    for i in 0..findings.len() {
        if assigned[i] {
            continue;
        }
        let mut cluster = vec![i];
        assigned[i] = true;
        for j in (i + 1)..findings.len() {
            if assigned[j] {
                continue;
            }
            if jaccard_similarity(
                &findings[i].finding.description,
                &findings[j].finding.description,
            ) >= JACCARD_THRESHOLD
            {
                cluster.push(j);
                assigned[j] = true;
            }
        }
        clusters.push(cluster);
    }

    // Assign trend tags based on cluster period coverage.
    let mut tagged = findings;
    for cluster in &clusters {
        let periods_in_cluster: Vec<&str> = cluster
            .iter()
            .map(|&idx| tagged[idx].period_label.as_str())
            .collect();

        let in_latest = periods_in_cluster.contains(&latest_period.as_str());
        let in_earlier = periods_in_cluster
            .iter()
            .any(|&p| p != latest_period.as_str());

        // Detect worsening: does the most recent instance have higher severity
        // than the first instance?  We use confidence as a proxy for severity
        // since findings from different providers may not have an explicit
        // severity field.
        let worsening = if in_latest && in_earlier && cluster.len() >= 2 {
            let first_conf = tagged[cluster[0]].finding.confidence;
            let last_idx = cluster[cluster.len() - 1];
            let last_conf = tagged[last_idx].finding.confidence;
            last_conf > first_conf + 0.1
        } else {
            false
        };

        let tag = if worsening {
            TrendTag::Worsening
        } else if in_latest && in_earlier {
            TrendTag::Recurring
        } else if in_latest && !in_earlier {
            TrendTag::New
        } else {
            // in_earlier && !in_latest
            TrendTag::Resolved
        };

        for &idx in cluster {
            tagged[idx].trend_tag = Some(tag.clone());
        }
    }

    tagged
}

/// Compute Jaccard similarity between two description strings.
///
/// Why: a simple, dependency-free metric for finding-description similarity
/// that works well for short technical phrases.
/// What: tokenises each string into lowercase words (split on whitespace +
/// punctuation), computes |intersection| / |union| of the token sets.
/// Returns 0.0 for empty inputs.
/// Test: `tests::jaccard_similarity_basic`.
pub fn jaccard_similarity(a: &str, b: &str) -> f64 {
    let tokens_a = tokenize(a);
    let tokens_b = tokenize(b);
    if tokens_a.is_empty() && tokens_b.is_empty() {
        return 1.0;
    }
    if tokens_a.is_empty() || tokens_b.is_empty() {
        return 0.0;
    }
    let mut intersection = 0usize;
    for t in &tokens_a {
        if tokens_b.contains(t) {
            intersection += 1;
        }
    }
    let union = tokens_a.len() + tokens_b.len() - intersection;
    intersection as f64 / union as f64
}

/// Tokenise a string into lowercase alphabetic/numeric tokens.
fn tokenize(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .collect()
}

// ─── Trajectory derivation ────────────────────────────────────────────────────

/// Derive a `Trajectory` from the quality score time series.
///
/// Why: the profile needs a deterministic trajectory even if the LLM call
/// fails; a linear slope over the quality_trend series provides this.
/// What: computes the slope of a least-squares linear fit over the
/// `(index, score)` pairs.  Slope > 0.1 → Improving; slope < −0.1 →
/// Declining; otherwise Stable.  Returns Stable for < 2 data points.
/// Test: `tests::synthesizer_trajectory_from_slope`.
pub fn derive_trajectory(quality_trend: &[(String, f64)]) -> Trajectory {
    if quality_trend.len() < 2 {
        return Trajectory::Stable;
    }
    let n = quality_trend.len() as f64;
    let sum_x: f64 = (0..quality_trend.len()).map(|i| i as f64).sum();
    let sum_y: f64 = quality_trend.iter().map(|(_, s)| s).sum();
    let sum_xy: f64 = quality_trend
        .iter()
        .enumerate()
        .map(|(i, (_, s))| i as f64 * s)
        .sum();
    let sum_xx: f64 = (0..quality_trend.len())
        .map(|i| (i as f64) * (i as f64))
        .sum();
    let denom = n * sum_xx - sum_x * sum_x;
    if denom.abs() < f64::EPSILON {
        return Trajectory::Stable;
    }
    let slope = (n * sum_xy - sum_x * sum_y) / denom;
    if slope > 0.1 {
        Trajectory::Improving
    } else if slope < -0.1 {
        Trajectory::Declining
    } else {
        Trajectory::Stable
    }
}

/// Apply the LLM synthesis response to the profile.
///
/// Why: the LLM response provides human-readable strengths, weaknesses,
/// trajectory, and narrative that cannot be derived deterministically.
/// What: tries direct JSON parse first (structured output path where the body
/// IS the JSON object), then falls back to fence-based extraction (legacy
/// free-text path).  If both fail, the deterministic fallback is used.
/// Test: covered transitively by synthesizer integration tests.
fn apply_llm_synthesis(profile: &mut ContributorProfile, resp: &LlmResponse) {
    #[derive(Deserialize)]
    struct SynthesisBlock {
        #[serde(default)]
        strengths: Vec<String>,
        #[serde(default)]
        recurring_weaknesses: Vec<String>,
        #[serde(default)]
        improvement_trajectory: String,
        #[serde(default)]
        narrative: String,
    }

    let body = resp.text.trim();

    // Strategy 1: direct JSON parse (structured output path).
    let block_opt: Option<SynthesisBlock> = if body.starts_with('{') {
        serde_json::from_str(body).ok()
    } else {
        None
    };

    // Strategy 2: fence-based extraction (legacy free-text path).
    let block_opt = block_opt.or_else(|| {
        let fence_start = body.rfind("```json")?;
        let after = &body[fence_start + 7..];
        let fence_end = after.find("```")?;
        let json_text = after[..fence_end].trim();
        match serde_json::from_str::<SynthesisBlock>(json_text) {
            Ok(b) => Some(b),
            Err(e) => {
                warn!(error = %e, "synthesizer: JSON parse error (fence path)");
                None
            }
        }
    });

    let block = match block_opt {
        Some(b) => b,
        None => {
            warn!("synthesizer: no parseable JSON in LLM response — applying fallback narrative");
            apply_fallback_narrative(profile);
            return;
        }
    };

    profile.strengths = block.strengths;
    profile.recurring_weaknesses = block.recurring_weaknesses;
    if !block.narrative.is_empty() {
        profile.narrative = block.narrative;
    } else {
        apply_fallback_narrative(profile);
    }
    // LLM trajectory overrides deterministic only if valid.
    let llm_traj = match block.improvement_trajectory.to_lowercase().as_str() {
        "improving" => Some(Trajectory::Improving),
        "declining" => Some(Trajectory::Declining),
        "stable" => Some(Trajectory::Stable),
        _ => None,
    };
    if let Some(t) = llm_traj {
        profile.improvement_trajectory = t;
    }
}

/// Apply a deterministic fallback narrative when the LLM call fails.
///
/// Why: the profile must be useful even without LLM narrative; a minimal
/// text derived from the deterministic parts prevents an empty profile.
/// What: sets `narrative` to a template based on trajectory and finding counts.
/// Test: `tests::synthesizer_fail_safe_narrative`.
fn apply_fallback_narrative(profile: &mut ContributorProfile) {
    let traj_str = match profile.improvement_trajectory {
        Trajectory::Improving => "improving",
        Trajectory::Stable => "stable",
        Trajectory::Declining => "declining",
    };
    let n_recurring = profile
        .all_findings
        .iter()
        .filter(|f| f.trend_tag == Some(TrendTag::Recurring))
        .count();
    profile.narrative = format!(
        "Longitudinal profile for {} ({} to {}). \
         Quality trajectory: {}. \
         {} recurring issue(s) identified across periods. \
         (Narrative generation unavailable — LLM call failed or returned invalid output.)",
        profile.canonical_name,
        profile.profiled_since,
        profile.profiled_until,
        traj_str,
        n_recurring,
    );
}

// ─── Unit tests ───────────────────────────────────────────────────────────────
// Tests live in synthesizer_tests.rs to keep this file under 500 lines.

#[cfg(test)]
#[path = "synthesizer_tests.rs"]
mod tests;
