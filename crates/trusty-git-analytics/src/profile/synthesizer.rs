//! Cross-period synthesis — trend tagging, trajectory, and the narrative shell.
//!
//! Why: per-period findings are independent snapshots. Turning them into a
//! longitudinal signal means deciding which findings across periods are the
//! same issue, and that decision has to be reproducible — so it is done here,
//! deterministically, before any model is asked for prose.
//! What: [`apply_deterministic_synthesis`] fills `quality_trend`, tags every
//! finding via [`assign_trend_tags`], and sets the trajectory from
//! [`derive_trajectory`]. [`synthesis_output_schema`],
//! [`synthesizer_system_prompt`], and [`build_synthesizer_user_message`] build
//! the narrative request; [`apply_synthesis_json`] reads its answer, and
//! [`apply_fallback_narrative`] writes a usable one when there is no answer.
//!
//! [`Synthesizer`] is the transport, added in #5465 on the same
//! `trusty_common::inference` adapter the period review uses — which is why
//! [`apply_synthesis_json`] takes a `&str` rather than a response struct and
//! stays testable with no network.
//!
//! Test: `synthesizer_tests.rs`.

use std::cmp::Reverse;
use std::sync::Arc;
use std::time::Instant;

use serde::Deserialize;
use tracing::{debug, warn};
use trusty_common::credentials::{default_store, KeyStore};
use trusty_common::inference::{
    register_default_factories, ChatMessage, ChatRequest, Configurator, InferenceAdapter,
    InferenceError,
};

use super::types::{
    ContributorProfile, LongitudinalFinding, PeriodBatch, TokenCostSummary, Trajectory, TrendTag,
};

// ─── Request parameters ───────────────────────────────────────────────────────

/// Sampling temperature for the narrative call.
pub const SYNTHESIZER_TEMPERATURE: f32 = 0.3;

/// Output-token ceiling for the narrative call.
pub const SYNTHESIZER_MAX_TOKENS: u32 = 2048;

/// Token-set Jaccard similarity at or above which two findings are treated as
/// the same underlying issue.
///
/// Why: descriptions of one recurring problem are worded differently each time
/// a model writes them, so exact matching would report every period's version
/// as brand new and no trend would ever surface.
/// What: 0.7 — high enough that two genuinely different issues sharing generic
/// words ("missing error handling in the parser" vs. "missing tests for the
/// parser") stay apart.
/// Test: `jaccard_similarity_similar_descriptions`,
/// `synthesizer_dedup_assigns_recurring`, `synthesizer_dedup_assigns_resolved`.
pub const JACCARD_THRESHOLD: f64 = 0.7;

/// Findings listed individually in the narrative prompt.
const MAX_FINDINGS_IN_PROMPT: usize = 20;

// ─── Deterministic synthesis ──────────────────────────────────────────────────

/// Fill in everything about a profile that does not need a model.
///
/// Why: a profile must be useful even when the narrative pass is skipped or
/// fails, so the quality series, the trend tags, and the trajectory are all
/// computed first and never overwritten by a failure downstream.
/// What: sets `quality_trend` from the period stats, flattens and tags
/// `all_findings` via [`assign_trend_tags`], and sets `improvement_trajectory`
/// from [`derive_trajectory`].
/// Test: `synthesizer_quality_trend_populated`,
/// `synthesizer_deterministic_synthesis_tags_findings`.
pub fn apply_deterministic_synthesis(
    profile: &mut ContributorProfile,
    all_period_findings: Vec<Vec<LongitudinalFinding>>,
    periods: &[PeriodBatch],
) {
    profile.quality_trend = periods
        .iter()
        .map(|b| (b.stats.period_label.clone(), b.stats.quality_score))
        .collect();

    let flat: Vec<LongitudinalFinding> = all_period_findings.into_iter().flatten().collect();
    profile.all_findings = assign_trend_tags(flat);

    profile.improvement_trajectory = derive_trajectory(&profile.quality_trend);
}

/// Tag every finding with how it moved across the periods.
///
/// Why: this is the whole longitudinal signal — without it a profile is a list
/// of unrelated observations rather than a story about what is getting better
/// and what keeps coming back.
/// What: clusters findings whose descriptions score at or above
/// [`JACCARD_THRESHOLD`], then tags each cluster by which periods it spans:
/// latest only → `New`; earlier only → `Resolved`; both → `Recurring`, or
/// `Worsening` when the cluster's last confidence exceeds its first by more
/// than 0.1. Confidence stands in for severity because no severity field
/// survives the finding DTO.
///
/// Periods are ordered by first appearance in `findings`, so the caller must
/// pass them chronologically — which `apply_deterministic_synthesis` does.
///
/// Test: `synthesizer_dedup_assigns_recurring`, `synthesizer_dedup_assigns_new`,
/// `synthesizer_dedup_assigns_resolved`, `synthesizer_dedup_empty_findings`,
/// `synthesizer_dedup_assigns_worsening`.
pub fn assign_trend_tags(findings: Vec<LongitudinalFinding>) -> Vec<LongitudinalFinding> {
    if findings.is_empty() {
        return findings;
    }

    let mut period_order: Vec<String> = Vec::new();
    for f in &findings {
        if !period_order.contains(&f.period_label) {
            period_order.push(f.period_label.clone());
        }
    }
    let latest_period = period_order.last().cloned().unwrap_or_default();

    // Cluster by description similarity; `clusters` holds indices into `findings`.
    let mut clusters: Vec<Vec<usize>> = Vec::new();
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

        let worsening = if in_latest && in_earlier && cluster.len() >= 2 {
            let first_conf = tagged[cluster[0]].finding.confidence;
            let last_conf = tagged[cluster[cluster.len() - 1]].finding.confidence;
            last_conf > first_conf + 0.1
        } else {
            false
        };

        let tag = if worsening {
            TrendTag::Worsening
        } else if in_latest && in_earlier {
            TrendTag::Recurring
        } else if in_latest {
            TrendTag::New
        } else {
            TrendTag::Resolved
        };

        for &idx in cluster {
            tagged[idx].trend_tag = Some(tag);
        }
    }

    tagged
}

/// Token-set Jaccard similarity between two descriptions.
///
/// Two empty strings score 1.0; one empty string scores 0.0.
///
/// Test: `jaccard_similarity_basic`, `jaccard_similarity_similar_descriptions`.
pub fn jaccard_similarity(a: &str, b: &str) -> f64 {
    let tokens_a = tokenize(a);
    let tokens_b = tokenize(b);
    if tokens_a.is_empty() && tokens_b.is_empty() {
        return 1.0;
    }
    if tokens_a.is_empty() || tokens_b.is_empty() {
        return 0.0;
    }
    let intersection = tokens_a.iter().filter(|t| tokens_b.contains(t)).count();
    let union = tokens_a.len() + tokens_b.len() - intersection;
    intersection as f64 / union as f64
}

/// Split a string into lowercase alphanumeric tokens.
fn tokenize(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .collect()
}

/// Derive a [`Trajectory`] from the quality-score series.
///
/// Why: the trajectory has to be defensible, so it comes from the numbers
/// rather than from prose — and it is still correct when no model ran.
/// What: least-squares slope over the `(index, score)` pairs. Above 0.1 is
/// `Improving`, below -0.1 is `Declining`, otherwise `Stable`. Fewer than two
/// points is `Stable` — one period cannot show a direction.
/// Test: `synthesizer_trajectory_from_slope`.
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

// ─── Narrative request ────────────────────────────────────────────────────────

/// JSON Schema for the narrative response.
///
/// Test: `synthesis_output_schema_has_expected_properties`.
pub fn synthesis_output_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "strengths": {
                "type": "array",
                "items": {"type": "string"},
                "description": "2-4 specific strengths observed across the periods"
            },
            "recurring_weaknesses": {
                "type": "array",
                "items": {"type": "string"},
                "description": "2-4 specific recurring weaknesses or improvement areas"
            },
            "improvement_trajectory": {
                "type": "string",
                "enum": ["improving", "stable", "declining"]
            },
            "narrative": {
                "type": "string",
                "description": "2-4 paragraph engineering assessment"
            }
        },
        "required": ["strengths", "recurring_weaknesses", "improvement_trajectory", "narrative"]
    })
}

/// System prompt for the narrative role.
///
/// Test: `synthesizer_system_prompt_names_output_fields`.
pub fn synthesizer_system_prompt() -> &'static str {
    r#"You are a senior engineering lead producing a longitudinal code-quality profile for a contributor.

## Task
Given a list of recurring code-quality findings across multiple time periods and a quality score trend,
write a concise, actionable engineering profile. Be direct and specific.

## Output (REQUIRED)
Populate the structured response with:
- `strengths`: array of 2-4 specific strengths observed across the periods.
- `recurring_weaknesses`: array of 2-4 specific recurring weaknesses or improvement areas.
- `improvement_trajectory`: one of "improving", "stable", or "declining".
- `narrative`: 2-4 paragraph engineering assessment suitable for a manager review."#
}

/// Build the user-turn message for the narrative call.
///
/// Why: the model gets the deterministic results — the score series, the
/// finding frequencies, the trend tags, and the computed trajectory — so its
/// prose explains conclusions already reached rather than inventing new ones.
/// What: renders the identity header, the quality-trend table, per-kind
/// frequency counts, up to [`MAX_FINDINGS_IN_PROMPT`] tagged findings, and the
/// deterministic trajectory.
/// Test: `synthesizer_user_message_includes_trend_and_findings`.
pub fn build_synthesizer_user_message(profile: &ContributorProfile) -> String {
    let mut msg = String::with_capacity(4096);

    msg.push_str(&format!(
        "## Contributor: {} <{}>\n",
        profile.canonical_name, profile.canonical_email
    ));
    msg.push_str(&format!(
        "Profile window: {} → {}\n",
        profile.profiled_since, profile.profiled_until
    ));
    if !profile.repositories.is_empty() {
        msg.push_str(&format!(
            "Repositories: {}\n",
            profile.repositories.join(", ")
        ));
    }
    msg.push('\n');

    msg.push_str("### Quality trend\n\n");
    msg.push_str("| Period | Score |\n|--------|-------|\n");
    for (label, score) in &profile.quality_trend {
        msg.push_str(&format!("| {label} | {score:.2} |\n"));
    }
    msg.push('\n');

    if profile.all_findings.is_empty() {
        msg.push_str("### Findings\n*(no findings extracted)*\n\n");
    } else {
        msg.push_str("### Findings across all periods\n\n");

        let mut kind_counts: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::new();
        for lf in &profile.all_findings {
            *kind_counts.entry(lf.finding.kind.as_str()).or_default() += 1;
        }
        let mut kinds: Vec<(&str, usize)> = kind_counts.into_iter().collect();
        kinds.sort_by_key(|b| Reverse(b.1));

        msg.push_str("**Finding frequency by kind:**\n");
        for (kind, count) in &kinds {
            msg.push_str(&format!("- {kind}: {count}×\n"));
        }
        msg.push('\n');

        msg.push_str("**Sample findings with trend tags:**\n");
        for lf in profile.all_findings.iter().take(MAX_FINDINGS_IN_PROMPT) {
            let tag = lf
                .trend_tag
                .map(|t| format!("{t:?}"))
                .unwrap_or_else(|| "Unknown".to_string());
            msg.push_str(&format!(
                "- [{tag}] ({}) {}: {}\n",
                lf.period_label, lf.finding.kind, lf.finding.description
            ));
        }
        msg.push('\n');
    }

    msg.push_str(&format!(
        "Deterministic trajectory: {}\n\n",
        format!("{:?}", profile.improvement_trajectory).to_lowercase()
    ));

    msg.push_str(
        "Please synthesise the above data into a longitudinal engineering profile \
         and populate the structured response fields.\n",
    );

    msg
}

/// Assemble the narrative request.
///
/// Why: like the period request, [`ChatRequest`] carries no structured-output
/// field, so the schema only reaches the model if the system turn states it.
/// What: system turn = [`synthesizer_system_prompt`] plus
/// [`synthesis_output_schema`] as JSON; user turn =
/// [`build_synthesizer_user_message`]. `model` passes through unchanged so the
/// commons' prefix routing keeps working.
/// Test: `synthesis_request_preserves_routing_prefix_and_sampling`.
pub fn build_synthesis_request(profile: &ContributorProfile, model: &str) -> ChatRequest {
    // `to_string_pretty` over a `Value` this module built cannot fail.
    let schema = serde_json::to_string_pretty(&synthesis_output_schema()).unwrap_or_default();
    let system = format!(
        "{}\n\n## Response schema\nReturn ONLY a JSON object conforming to this schema:\n\
         ```json\n{schema}\n```",
        synthesizer_system_prompt()
    );

    let mut req = ChatRequest::new(
        model,
        vec![
            ChatMessage::system(system),
            ChatMessage::user(build_synthesizer_user_message(profile)),
        ],
    );
    req.temperature = Some(SYNTHESIZER_TEMPERATURE);
    req.max_tokens = Some(SYNTHESIZER_MAX_TOKENS);
    req
}

// ─── Narrative transport ──────────────────────────────────────────────────────

/// The narrative pass: one model call per profile, over the shared inference
/// stack.
///
/// Why (#5465): `tga profile` has to produce the narrative itself now that
/// profiling is tga's domain, and it routes through
/// `trusty_common::inference` for the same reason the period review does —
/// no Cargo edge to trusty-review, and one place that knows which backend and
/// whose key.
/// What: holds an [`InferenceAdapter`] and its routing slug.
/// [`Self::synthesize`] never fails the run: a provider failure leaves the
/// deterministic results intact and writes the fallback narrative.
/// Test: `synthesizer_transport_applies_the_models_narrative`,
/// `synthesizer_transport_falls_back_and_reports_the_failure`.
pub struct Synthesizer {
    adapter: Arc<dyn InferenceAdapter>,
    model: String,
}

impl Synthesizer {
    /// Resolve `model` against the shared credential store and build its adapter.
    ///
    /// # Errors
    ///
    /// [`super::ProfileError::Inference`] when no credential resolves for the
    /// slug's provider family, or no factory is registered for it.
    pub fn from_slug(model: &str) -> super::Result<Self> {
        Self::from_slug_with_store(model, default_store().as_ref())
    }

    /// [`Self::from_slug`] against an explicit credential store.
    ///
    /// Why: [`default_store`] reads the machine's real keychain, which makes a
    /// test built on it assert whatever the developer happens to have exported.
    ///
    /// # Errors
    ///
    /// As [`Self::from_slug`].
    pub fn from_slug_with_store(model: &str, store: &dyn KeyStore) -> super::Result<Self> {
        let mut configurator = Configurator::new();
        register_default_factories(&mut configurator);
        #[cfg(feature = "bedrock")]
        trusty_common::inference::register_bedrock_factory(&mut configurator);

        let adapter = configurator.build(model, store)?;
        Ok(Self {
            adapter: Arc::from(adapter),
            model: model.to_string(),
        })
    }

    /// Bind a synthesizer to an already-built adapter.
    ///
    /// Why: a run that already resolved an adapter for the period reviews should
    /// reuse it rather than re-running credential resolution, and tests drive
    /// the real transport with the commons' `ScriptedAdapter`.
    pub fn with_adapter(adapter: Arc<dyn InferenceAdapter>, model: impl Into<String>) -> Self {
        Self {
            adapter,
            model: model.into(),
        }
    }

    /// Run the narrative pass over an already deterministically-synthesised
    /// profile.
    ///
    /// Why: the trajectory, the quality series, and the trend tags are computed
    /// before this call and must survive it — the narrative supplies judgements
    /// that cannot be computed, never the ones that were.
    /// What: sends [`build_synthesis_request`], accumulates usage into
    /// `profile.token_cost`, and applies the answer with
    /// [`apply_synthesis_json`]. On provider failure it writes
    /// [`apply_fallback_narrative`] and RETURNS the error, so a caller can say
    /// the narrative is a fallback rather than presenting it as the model's.
    /// Test: `synthesizer_transport_applies_the_models_narrative`,
    /// `synthesizer_transport_falls_back_and_reports_the_failure`.
    pub async fn synthesize(&self, profile: &mut ContributorProfile) -> Option<InferenceError> {
        let request = build_synthesis_request(profile, &self.model);
        let start = Instant::now();

        let response = match self.adapter.chat(&request).await {
            Ok(r) => r,
            Err(e) => {
                warn!(
                    model = %self.model,
                    error = %e,
                    "synthesizer: inference call failed — using the deterministic fallback narrative"
                );
                apply_fallback_narrative(profile);
                return Some(e);
            }
        };

        let latency_ms = start.elapsed().as_millis() as u64;
        let usage = response.usage();
        accumulate_usage(&mut profile.token_cost, &response, latency_ms);
        debug!(
            model = %response.resolved_model(&self.model),
            input_tokens = usage.prompt_tokens,
            output_tokens = usage.completion_tokens,
            latency_ms,
            "synthesizer: inference call complete"
        );

        apply_synthesis_json(profile, &response.first_text().unwrap_or_default());
        None
    }
}

/// Fold one response's usage into the run's cost summary.
fn accumulate_usage(
    cost: &mut TokenCostSummary,
    response: &trusty_common::inference::ChatResponse,
    latency_ms: u64,
) {
    let usage = response.usage();
    cost.accumulate(
        u64::from(usage.prompt_tokens),
        u64::from(usage.completion_tokens),
        usage.cost_usd.unwrap_or(0.0),
        latency_ms,
    );
}

// ─── Narrative response ───────────────────────────────────────────────────────

/// Wire shape of the narrative response body.
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

/// Apply a narrative response body to the profile.
///
/// Why: the narrative supplies the judgements that cannot be computed, but it
/// must not be able to erase the ones that were — so an unparseable or empty
/// answer falls back rather than leaving the profile blank.
/// What: tries a direct JSON parse, then a fenced ```` ```json ```` block. On
/// success, sets `strengths`, `recurring_weaknesses`, and `narrative`, and
/// overrides `improvement_trajectory` only when the returned value is one of
/// the three known spellings. On failure, calls [`apply_fallback_narrative`].
/// Test: `synthesizer_applies_llm_result`,
/// `synthesizer_applies_direct_json_result`,
/// `synthesizer_unparseable_response_falls_back`,
/// `synthesizer_ignores_unknown_trajectory`.
pub fn apply_synthesis_json(profile: &mut ContributorProfile, body: &str) {
    let body = body.trim();

    let block_opt: Option<SynthesisBlock> = if body.starts_with('{') {
        serde_json::from_str(body).ok()
    } else {
        None
    };

    let block_opt = block_opt.or_else(|| {
        let fence_start = body.rfind("```json")?;
        let after = &body[fence_start + 7..];
        let fence_end = after.find("```")?;
        match serde_json::from_str::<SynthesisBlock>(after[..fence_end].trim()) {
            Ok(b) => Some(b),
            Err(e) => {
                warn!(error = %e, "synthesizer: JSON parse error (fence path)");
                None
            }
        }
    });

    let Some(block) = block_opt else {
        warn!("synthesizer: no parseable JSON in response — applying fallback narrative");
        apply_fallback_narrative(profile);
        return;
    };

    profile.strengths = block.strengths;
    profile.recurring_weaknesses = block.recurring_weaknesses;
    if block.narrative.is_empty() {
        apply_fallback_narrative(profile);
    } else {
        profile.narrative = block.narrative;
    }

    // An unrecognised spelling leaves the deterministic trajectory in place.
    match block.improvement_trajectory.to_lowercase().as_str() {
        "improving" => profile.improvement_trajectory = Trajectory::Improving,
        "declining" => profile.improvement_trajectory = Trajectory::Declining,
        "stable" => profile.improvement_trajectory = Trajectory::Stable,
        _ => {}
    }
}

/// Write a narrative derived only from the deterministic results.
///
/// Why: an empty `narrative` field would read as "nothing to say" rather than
/// "the narrative pass did not run", so the fallback states both the findings
/// and the fact that it is a fallback.
/// What: sets `narrative` to a template naming the contributor, the window, the
/// trajectory, and the recurring-finding count.
/// Test: `synthesizer_fail_safe_narrative`.
pub fn apply_fallback_narrative(profile: &mut ContributorProfile) {
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
         (Narrative generation unavailable — the LLM call failed or returned invalid output.)",
        profile.canonical_name,
        profile.profiled_since,
        profile.profiled_until,
        traj_str,
        n_recurring,
    );
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "synthesizer_tests.rs"]
mod tests;
