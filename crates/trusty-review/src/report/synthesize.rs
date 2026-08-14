//! Required LLM synthesis of report narrative sections (M2, epic #2312 / #2314;
//! made mandatory by #5454).
//!
//! Why: M1 fills the report deterministically and leaves the narrative fields
//! (executive summary, top-risk rationale, RED/AMBER finding prose) as honesty
//! markers.  Synthesis lets an LLM write those — and ONLY those — sections,
//! grounded strictly in the deterministic data.  #5454 made it required: a run
//! that cannot produce verified prose now FAILS instead of quietly shipping a
//! narrative-free report, because a reader cannot tell "the model declined" from
//! "there was nothing to say".  A fabricated figure is still rejected
//! field-by-field by the numeric guardrail — that is a correctness property, not
//! a mode, and dropping one field leaves the deterministic composition (#5374) to
//! fill it.  Greens are excluded structurally (see [`synthesize_prompt`]), never
//! merely by instruction.
//! What: [`Synthesizer`] holds an [`LlmProvider`] and calls it once; [`Synthesis`]
//! is the verified result recorded on the [`ReportModel`] (its JSON twin) and read
//! by the reporter.  [`SynthesisError`] names why a pass produced nothing usable.
//! Test: `synthesize_tests.rs` — happy-path injection, malformed-JSON and
//! provider-error hard failure, numeric-guardrail rejection, greens-never-sent.

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

/// Verified narrative produced by one synthesis pass, recorded on the
/// [`ReportModel`].
///
/// Why: the reporter injects this prose into the narrative placeholders;
/// serialising it onto the model keeps the JSON twin a faithful record of what
/// synthesis produced.  #5454: a `Synthesis` value now exists only when a pass
/// SUCCEEDED — the former `Unavailable` status is gone, so the type can no longer
/// represent "we tried and shipped the report anyway".  A failed pass is a
/// [`SynthesisError`] and no report is written.
/// What: the verified executive summary (absent only when the numeric guardrail
/// rejected it), verified top-risk rows, verified per-finding prose, and
/// human-readable guardrail notes.
/// Test: `synthesize_tests.rs::synthesize_happy_path_injects`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Synthesis {
    /// Verified executive-summary prose, or `None` when the guardrail rejected it.
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

/// Why one synthesis pass produced no verified narrative.
///
/// Why: #5454 made inference required, so each of these was previously a silent
/// degrade to a deterministic-only report.  They are separated because the
/// remedies differ: a credential or model-id mistake is fixed before re-running,
/// while a timeout or rate limit is fixed by re-running the same command against
/// the manifest that is already on disk.
/// What: one variant per failure the pass can hit, after its single built-in
/// concise retry has been spent.
/// Test: `synthesize_tests.rs::{synthesize_provider_error_is_a_hard_error,
/// synthesize_malformed_json_is_a_hard_error,
/// synthesize_still_truncated_after_retry_is_a_hard_error,
/// synthesize_timeout_is_a_hard_error, synthesize_rejects_unverified_figure}`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SynthesisError {
    /// The provider did not answer inside [`DEFAULT_SYNTHESIS_TIMEOUT`].
    #[error("the LLM provider timed out")]
    Timeout,

    /// The provider returned an error (transport, auth, rate limit, model id).
    #[error("the LLM provider failed: {0}")]
    Provider(String),

    /// The response was not the requested JSON object, fenced or bare.
    #[error("the LLM returned a response that is not the requested JSON object")]
    Unparseable,

    /// Truncated at the output-token ceiling, and the concise retry was too.
    #[error(
        "the LLM response was truncated at the output-token ceiling, and the concise retry was truncated as well"
    )]
    Truncated,

    /// The deterministic model could not be serialised to build the guardrail's
    /// numeric allow-set — a bug in this crate, never an operator's problem.
    #[error("internal: the report model could not be serialised for the numeric guardrail: {0}")]
    ModelNotSerialisable(String),

    /// Every field cited a figure absent from the collected data.
    #[error(
        "every synthesized field cited a figure absent from the collected data and was rejected by the numeric guardrail"
    )]
    NoVerifiableContent,
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
    /// The visible status note lines for the rendered report.
    ///
    /// Why: the report states which fields the numeric guardrail rejected, so a
    /// reader can tell a section written by the model from one that fell back to
    /// the deterministic composition (#5374).  #5454 removed the
    /// `unavailable (<reason>)` banner: a report that reaches a reader always had
    /// a successful synthesis pass behind it, so the only news left is the
    /// per-field rejections.
    /// What: a leading `synthesis: available` line, then one line per note.
    /// Test: `synthesize_tests.rs::status_lines_render_banners`.
    pub fn status_lines(&self) -> Vec<String> {
        let mut lines = Vec::with_capacity(1 + self.notes.len());
        lines.push("synthesis: available".to_string());
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

    /// Run synthesis, returning verified prose or the reason there is none.
    ///
    /// Why: the single entry point the CLI awaits; it NEVER fabricates and NEVER
    /// partial-trusts a malformed response.  A live-QA acceptance run found that a
    /// large, real finding count could still hit the output-token ceiling even
    /// with the #2357-follow-up compact digest and bounded schema; a single cheap
    /// retry (mirroring the wave-3 batch investigation's truncation retry) asks for
    /// a shorter response before giving up, rather than discarding the whole
    /// narrative on the first truncation.  #5454 changed only what happens when
    /// that retry is exhausted: the failure propagates instead of degrading the
    /// report to deterministic-only.
    /// What: builds the numeric allow-set from the deterministic model, calls the
    /// provider under a timeout; on `finish_reason = length`/`max_tokens`, retries
    /// ONCE with `retry_concise = true` (a smaller `top_risks` cap + a shorter-
    /// paragraph directive).  A parsed response is passed through the numeric
    /// guardrail field-by-field, dropping any field whose prose cites a figure
    /// absent from the source.
    ///
    /// # Errors
    ///
    /// [`SynthesisError`] for a provider failure, a timeout, an unparseable or
    /// still-truncated response, or a response every field of which the numeric
    /// guardrail rejected.
    ///
    /// Test: `synthesize_tests.rs::{synthesize_happy_path_injects,
    /// synthesize_malformed_json_is_a_hard_error,
    /// synthesize_provider_error_is_a_hard_error, synthesize_rejects_unverified_figure,
    /// synthesize_retry_recovers_from_truncation,
    /// synthesize_still_truncated_after_retry_is_a_hard_error}`.
    pub async fn synthesize(&self, model: &ReportModel) -> Result<Synthesis, SynthesisError> {
        // Ground truth for the guardrail comes from the DETERMINISTIC model only.
        let allowed = match serde_json::to_value(model) {
            Ok(v) => allowed_numbers(&v),
            Err(e) => {
                warn!(error = %e, "synthesis: could not serialise model for guardrail");
                return Err(SynthesisError::ModelNotSerialisable(e.to_string()));
            }
        };

        match self.try_once(model, false).await {
            Attempt::Ok(raw) => apply_guardrail(raw, &allowed),
            Attempt::Truncated => {
                warn!("synthesis: response truncated — retrying once, concise");
                match self.try_once(model, true).await {
                    Attempt::Ok(raw) => apply_guardrail(raw, &allowed),
                    // #5454: the retry is unchanged; only its exhaustion is now fatal.
                    Attempt::Truncated => {
                        warn!("synthesis: still truncated after retry");
                        Err(SynthesisError::Truncated)
                    }
                    Attempt::Failed(e) => Err(e),
                }
            }
            Attempt::Failed(e) => Err(e),
        }
    }

    /// Make one provider call (initial or concise retry) and classify the result.
    async fn try_once(&self, model: &ReportModel, retry_concise: bool) -> Attempt {
        let req = build_synthesis_prompt(model, &self.model, retry_concise);
        let resp = match tokio::time::timeout(self.timeout, self.llm.complete(req)).await {
            Err(_) => {
                warn!("synthesis: provider timed out");
                return Attempt::Failed(SynthesisError::Timeout);
            }
            Ok(Err(e)) => {
                warn!(error = %e, "synthesis: provider error");
                // #5454: the provider's own text reaches an operator's terminal
                // now that this is fatal, so it is scrubbed against the same
                // needle set the report body uses before it is carried anywhere.
                let secrets = crate::report::redact::report_secrets();
                return Attempt::Failed(SynthesisError::Provider(
                    trusty_common::credentials::scrub_secrets(&e.to_string(), &secrets),
                ));
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
                warn!("synthesis: unparseable response");
                return Attempt::Failed(SynthesisError::Unparseable);
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
    Failed(SynthesisError),
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
/// Why: a field whose prose cites a figure not in the source is dropped (never
/// repaired), so the deterministic composition (#5374) fills that placeholder and
/// a visible rejection note is recorded.  Per-FIELD rejection stays a correctness
/// property under #5454's required-inference rule; what changed is that a
/// response with nothing left after the pass is an error rather than a
/// deterministic-only report.
/// What: verifies the executive summary and every risk row / finding against
/// `allowed`; keeps only clean fields.  Findings must carry a RED/AMBER severity
/// (defence-in-depth greens exclusion).  `Ok` iff at least one field survived.
///
/// # Errors
///
/// [`SynthesisError::NoVerifiableContent`] when every field was rejected.
///
/// Test: `synthesize_tests.rs::{synthesize_rejects_unverified_figure,
/// synthesize_happy_path_injects}`.
fn apply_guardrail(
    raw: RawSynthesis,
    allowed: &std::collections::HashSet<String>,
) -> Result<Synthesis, SynthesisError> {
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

    if out.executive_summary.is_none() && out.top_risks.is_empty() && out.findings.is_empty() {
        return Err(SynthesisError::NoVerifiableContent);
    }
    Ok(out)
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
