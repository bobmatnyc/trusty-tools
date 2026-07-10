//! Opt-in LLM synthesis of report narrative sections (M2, epic #2312 / #2314).
//!
//! Why: M1 fills the report deterministically and leaves the narrative fields
//! (executive summary, top-risk rationale, RED/AMBER finding prose) as honesty
//! markers.  M2 lets an LLM write those — and ONLY those — sections, grounded
//! strictly in the deterministic data.  The posture mirrors the pipeline's
//! `Verdict::Unknown` convention: any provider error, timeout, truncation, or
//! unparseable response fails CLOSED to the deterministic M1 output, and a
//! fabricated figure is rejected field-by-field by a numeric guardrail.  Greens
//! are excluded structurally (see [`synthesize_prompt`]), never merely by
//! instruction.
//! What: [`Synthesizer`] holds an [`LlmProvider`] and calls it once; [`Synthesis`]
//! is the injected result recorded on the [`ReportModel`] (its JSON twin) and read
//! by the reporter.  [`SynthesisStatus`] carries the fail-closed reason.
//! Test: `synthesize_tests.rs` — happy-path injection, malformed-JSON and
//! provider-error fail-closed, numeric-guardrail rejection, greens-never-sent.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::llm::LlmProvider;
use crate::report::model::ReportModel;
use crate::report::synthesize_guard::{allowed_numbers, verify_prose};
use crate::report::synthesize_prompt::build_synthesis_prompt;

/// Default wall-clock ceiling for one synthesis pass before failing closed.
const DEFAULT_SYNTHESIS_TIMEOUT: Duration = Duration::from_secs(120);

/// The literal prefix every guardrail-rejection note carries, so the reporter and
/// tests can assert on it.
const REJECTED_NOTE: &str = "synthesis: rejected (unverified figure)";

// ─── Injected result types ──────────────────────────────────────────────────

/// Outcome of a synthesis attempt, recorded on the [`ReportModel`].
///
/// Why: the reporter injects surviving prose into the narrative placeholders and
/// renders a visible status note; serialising this onto the model keeps the JSON
/// twin a faithful record of what synthesis did (and did not) produce.
/// What: the overall [`SynthesisStatus`], the verified executive summary (absent
/// when unavailable/rejected), verified top-risk rows, verified per-finding
/// prose, and human-readable guardrail notes.
/// Test: `synthesize_tests.rs::synthesize_happy_path_injects`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Synthesis {
    /// Overall status of the synthesis attempt.
    pub status: SynthesisStatus,
    /// Verified executive-summary prose, or `None` when unavailable/rejected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executive_summary: Option<String>,
    /// Verified top-risk rows (rejected rows are dropped).
    #[serde(default)]
    pub top_risks: Vec<RiskRow>,
    /// Verified per-finding elaboration prose (rejected findings are dropped).
    #[serde(default)]
    pub findings: Vec<FindingProse>,
    /// Human-readable guardrail/routing notes recorded during injection.
    #[serde(default)]
    pub notes: Vec<String>,
}

/// Overall status of a synthesis attempt.
///
/// Why: the fail-closed contract needs a single value that says whether the
/// narrative is trustworthy and, if not, why — surfaced verbatim in the report.
/// What: `Available` when at least the executive summary or one row/finding
/// survived; `Unavailable(reason)` for provider/parse/timeout failures.
/// Test: `synthesize_tests.rs::synthesize_provider_error_fails_closed`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", content = "reason", rename_all = "snake_case")]
pub enum SynthesisStatus {
    /// Synthesis produced usable, verified prose.
    Available,
    /// Synthesis failed closed; the deterministic output stands.  Carries reason.
    Unavailable(String),
}

impl Default for SynthesisStatus {
    fn default() -> Self {
        SynthesisStatus::Unavailable("not attempted".to_string())
    }
}

/// One synthesized top-risk table row (rationale for the Top Risks table).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskRow {
    /// Risk description.
    pub description: String,
    /// Severity band (`RED`/`AMBER`).
    pub severity: String,
    /// Qualitative cost/effort framing.
    pub cost: String,
    /// Affected application(s).
    pub apps: String,
}

/// Elaboration prose for one RED/AMBER finding, routed by `app_slug`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FindingProse {
    /// Application slug this finding belongs to (matches a repository slug).
    pub app_slug: String,
    /// Finding title.
    pub title: String,
    /// Severity band (`RED`/`AMBER`).
    pub severity: String,
    /// One-line finding description.
    pub description: String,
    /// Supporting evidence.
    #[serde(default)]
    pub evidence: String,
    /// Affected component/path.
    #[serde(default)]
    pub component: String,
    /// Business-impact framing.
    #[serde(default)]
    pub business_impact: String,
    /// Remediation framing.
    pub remediation: String,
    /// Remediation cost/effort framing.
    #[serde(default)]
    pub cost_effort: String,
    /// True when `evidence` is a verbatim quote mechanically verified against the
    /// actual file content (wave-3 investigation, #2357) — such evidence is
    /// `measured` provenance and the reporter tags it ⁽ᵐ⁾.  False (the default,
    /// and always so for M2 synthesis) means the evidence is LLM-claimed and is
    /// tagged ⁽ⁱ⁾ inferred like the rest of the prose.
    #[serde(default)]
    pub evidence_measured: bool,
}

impl Synthesis {
    /// A fail-closed result carrying a reason and no prose.
    ///
    /// Why: every failure path returns the same shape so the reporter treats
    /// them uniformly (deterministic output + a visible status note).
    /// What: sets `status = Unavailable(reason)` and leaves all prose empty.
    /// Test: `synthesize_tests.rs::synthesize_provider_error_fails_closed`.
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Synthesis {
            status: SynthesisStatus::Unavailable(reason.into()),
            ..Default::default()
        }
    }

    /// True when synthesis produced usable prose.
    ///
    /// Why: the reporter injects prose only when the attempt is `Available`.
    /// What: matches `SynthesisStatus::Available`.
    /// Test: exercised by `reporter_tests.rs` synthesis cases.
    pub fn is_available(&self) -> bool {
        matches!(self.status, SynthesisStatus::Available)
    }

    /// The visible status note lines for the rendered report.
    ///
    /// Why: the report must surface the synthesis outcome — an `available`
    /// banner, an `unavailable (<reason>)` fail-closed banner, or the
    /// per-field guardrail rejections — so a reader never mistakes deterministic
    /// fallback for synthesized analysis.
    /// What: first line is the overall status; subsequent lines are the notes.
    /// Test: `reporter_tests.rs::reporter_appends_unavailable_note`.
    pub fn status_lines(&self) -> Vec<String> {
        let mut lines = Vec::with_capacity(1 + self.notes.len());
        match &self.status {
            SynthesisStatus::Available => lines.push("synthesis: available".to_string()),
            SynthesisStatus::Unavailable(reason) => {
                lines.push(format!("synthesis: unavailable ({reason})"));
            }
        }
        lines.extend(self.notes.iter().cloned());
        lines
    }
}

// ─── Synthesizer ────────────────────────────────────────────────────────────

/// Drives one LLM synthesis pass over a deterministic [`ReportModel`].
///
/// Why: dependency injection of the provider lets tests supply stub providers
/// (fake/malformed/error) without any network access, exactly as the pipeline
/// and profile synthesizer do.
/// What: holds the provider, the resolved model id, and the fail-closed timeout.
/// Test: all `synthesize_tests.rs` cases construct one with a stub provider.
pub struct Synthesizer {
    llm: Arc<dyn LlmProvider>,
    model: String,
    timeout: Duration,
}

impl Synthesizer {
    /// Create a synthesizer from an injected provider and model id.
    ///
    /// Why: the CLI builds the provider from the reviewer role config; tests
    /// inject stubs.
    /// What: stores the provider + model with the default timeout.
    /// Test: exercised by every synthesize test.
    pub fn new(llm: Arc<dyn LlmProvider>, model: impl Into<String>) -> Self {
        Self {
            llm,
            model: model.into(),
            timeout: DEFAULT_SYNTHESIS_TIMEOUT,
        }
    }

    /// Override the fail-closed timeout (used by tests).
    #[cfg(test)]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Run synthesis, returning a fail-closed [`Synthesis`].
    ///
    /// Why: the single entry point the CLI awaits; it NEVER fabricates and NEVER
    /// partial-trusts a malformed response — on any failure it returns an
    /// `Unavailable` result and the deterministic output stands.  A live-QA
    /// acceptance run found that a large, real finding count could still hit the
    /// output-token ceiling even with the #2357-follow-up compact digest and
    /// bounded schema; a single cheap retry (mirroring the wave-3 batch
    /// investigation's truncation retry) asks for a shorter response before
    /// failing closed, rather than discarding the whole narrative on the first
    /// truncation.
    /// What: builds the numeric allow-set from the deterministic model, calls the
    /// provider under a timeout; on `finish_reason = length`/`max_tokens`, retries
    /// ONCE with `retry_concise = true` (a smaller `top_risks` cap + a shorter-
    /// paragraph directive); a still-truncated retry (or any other failure) fails
    /// closed.  A parsed response is passed through the numeric guardrail
    /// field-by-field, dropping any field whose prose cites a figure absent from
    /// the source.
    /// Test: `synthesize_tests.rs::{synthesize_happy_path_injects,
    /// synthesize_malformed_json_fails_closed, synthesize_provider_error_fails_closed,
    /// synthesize_rejects_unverified_figure, synthesize_retry_recovers_from_truncation,
    /// synthesize_still_truncated_after_retry_fails_closed}`.
    pub async fn synthesize(&self, model: &ReportModel) -> Synthesis {
        // Ground truth for the guardrail comes from the DETERMINISTIC model only.
        let allowed = match serde_json::to_value(model) {
            Ok(v) => allowed_numbers(&v),
            Err(e) => {
                warn!(error = %e, "synthesis: could not serialise model for guardrail");
                return Synthesis::unavailable("internal: model not serialisable");
            }
        };

        match self.try_once(model, false).await {
            Attempt::Ok(raw) => apply_guardrail(raw, &allowed),
            Attempt::Truncated => {
                warn!("synthesis: response truncated — retrying once, concise");
                match self.try_once(model, true).await {
                    Attempt::Ok(raw) => apply_guardrail(raw, &allowed),
                    Attempt::Truncated => {
                        warn!("synthesis: still truncated after retry — failing closed");
                        Synthesis::unavailable("truncated response")
                    }
                    Attempt::Failed(reason) => Synthesis::unavailable(reason),
                }
            }
            Attempt::Failed(reason) => Synthesis::unavailable(reason),
        }
    }

    /// Make one provider call (initial or concise retry) and classify the result.
    async fn try_once(&self, model: &ReportModel, retry_concise: bool) -> Attempt {
        let req = build_synthesis_prompt(model, &self.model, retry_concise);
        let resp = match tokio::time::timeout(self.timeout, self.llm.complete(req)).await {
            Err(_) => {
                warn!("synthesis: provider timed out — failing closed");
                return Attempt::Failed("provider timeout".to_string());
            }
            Ok(Err(e)) => {
                warn!(error = %e, "synthesis: provider error — failing closed");
                return Attempt::Failed(format!("provider error: {e}"));
            }
            Ok(Ok(r)) => r,
        };

        // A truncated response is an incomplete response — never partial-trust it.
        if matches!(
            resp.finish_reason.as_deref(),
            Some("length") | Some("max_tokens")
        ) {
            return Attempt::Truncated;
        }

        let raw = match parse_raw(&resp.text) {
            Some(r) => r,
            None => {
                warn!("synthesis: unparseable response — failing closed");
                return Attempt::Failed("unparseable response".to_string());
            }
        };

        debug!(
            input_tokens = resp.input_tokens,
            output_tokens = resp.output_tokens,
            "synthesis: provider call complete"
        );
        Attempt::Ok(raw)
    }
}

/// The outcome of one provider call attempt (pre-guardrail).
enum Attempt {
    /// Parsed cleanly; carries the raw (pre-guardrail) synthesis.
    Ok(RawSynthesis),
    /// The response was truncated at the output-token ceiling.
    Truncated,
    /// A provider/timeout/parse error — not a truncation.
    Failed(String),
}

// ─── Raw (pre-guardrail) parse types ────────────────────────────────────────

/// The raw JSON contract the provider is asked to emit (pre-verification).
#[derive(Debug, Deserialize)]
struct RawSynthesis {
    #[serde(default)]
    executive_summary: String,
    #[serde(default)]
    top_risks: Vec<RiskRow>,
    #[serde(default)]
    findings: Vec<FindingProse>,
}

/// Parse the provider response into [`RawSynthesis`].
///
/// Why: forced structured output returns a bare JSON object, but we still accept
/// a fenced block (defensive, matching the profile synthesizer) so a provider
/// that ignores the schema does not silently fail.
/// What: tries a direct object parse, then a ```json fenced block; `None` when
/// neither yields a decodable object.
/// Test: `synthesize_tests.rs::{synthesize_happy_path_injects,
/// synthesize_malformed_json_fails_closed}`.
fn parse_raw(text: &str) -> Option<RawSynthesis> {
    let body = text.trim();
    if body.starts_with('{')
        && let Ok(r) = serde_json::from_str::<RawSynthesis>(body)
    {
        return Some(r);
    }
    let fence_start = body.rfind("```json")?;
    let after = &body[fence_start + 7..];
    let fence_end = after.find("```")?;
    serde_json::from_str::<RawSynthesis>(after[..fence_end].trim()).ok()
}

/// Apply the numeric guardrail to the raw synthesis, field by field.
///
/// Why: this is the fail-closed core — a field whose prose cites a figure not in
/// the source is dropped (never repaired), so the deterministic honesty marker
/// stands and a visible rejection note is recorded.
/// What: verifies the executive summary and every risk row / finding against
/// `allowed`; keeps only clean fields.  Findings must carry a RED/AMBER severity
/// (defence-in-depth greens exclusion).  The result is `Available` iff at least
/// one field survived, else `Unavailable("no verifiable content")`.
/// Test: `synthesize_tests.rs::{synthesize_rejects_unverified_figure,
/// synthesize_happy_path_injects}`.
fn apply_guardrail(raw: RawSynthesis, allowed: &std::collections::HashSet<String>) -> Synthesis {
    let mut out = Synthesis::default();

    // Executive summary.
    let exec = raw.executive_summary.trim();
    if !exec.is_empty() {
        match verify_prose(exec, allowed) {
            Ok(()) => out.executive_summary = Some(exec.to_string()),
            Err(tok) => out
                .notes
                .push(format!("{REJECTED_NOTE} in executive summary: {tok}")),
        }
    }

    // Top-risk rows.
    for (i, r) in raw.top_risks.into_iter().enumerate() {
        match verify_all(&[&r.description, &r.severity, &r.cost, &r.apps], allowed) {
            Ok(()) => out.top_risks.push(r),
            Err(tok) => out
                .notes
                .push(format!("{REJECTED_NOTE} in top-risk row {}: {tok}", i + 1)),
        }
    }

    // RED/AMBER finding elaborations.
    for f in raw.findings {
        let sev = f.severity.to_uppercase();
        if sev != "RED" && sev != "AMBER" {
            out.notes.push(format!(
                "synthesis: dropped finding '{}' (non-RED/AMBER severity '{}')",
                f.title, f.severity
            ));
            continue;
        }
        match verify_all(
            &[
                &f.description,
                &f.evidence,
                &f.component,
                &f.business_impact,
                &f.remediation,
                &f.cost_effort,
            ],
            allowed,
        ) {
            Ok(()) => out.findings.push(f),
            Err(tok) => out
                .notes
                .push(format!("{REJECTED_NOTE} in finding '{}': {tok}", f.title)),
        }
    }

    if out.executive_summary.is_some() || !out.top_risks.is_empty() || !out.findings.is_empty() {
        out.status = SynthesisStatus::Available;
    } else {
        out.status = SynthesisStatus::Unavailable("no verifiable content".to_string());
    }
    out
}

/// Verify every field of a group; returns the first offending token on failure.
fn verify_all(fields: &[&str], allowed: &std::collections::HashSet<String>) -> Result<(), String> {
    for field in fields {
        verify_prose(field, allowed)?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "synthesize_tests.rs"]
mod tests;
