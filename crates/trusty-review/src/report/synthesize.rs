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
//! fill it.  Greens are excluded structurally (see [`synthesize_prompt`](crate::report::synthesize_prompt)), never
//! merely by instruction.
//! What: [`Synthesizer`] holds an [`LlmProvider`] and calls it once; [`Synthesis`]
//! is the verified result recorded on the [`ReportModel`] (its JSON twin) and read
//! by the reporter.  [`SynthesisError`] names why a pass produced nothing usable.
//! Test: `synthesize_tests.rs` — happy-path injection, malformed-JSON and
//! provider-error hard failure, numeric-guardrail rejection, greens-never-sent.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::llm::LlmProvider;
use crate::report::model::ReportModel;
use crate::report::synthesize_grounding::Grounding;

// #6082 lap 8: the guardrail layer moved into `synthesize_guardrail` to keep
// this file under the production SLOC cap. Re-exported so the public path
// callers already use stays exactly where it was.
pub use super::synthesize_guardrail::ground_investigation_prose;
use crate::report::synthesize_guard::allowed_numbers_with;
use crate::report::synthesize_prompt::{
    SYNTHESIS_DEFAULT_MAX_TOKENS, SYNTHESIS_TIER_LADDER, SynthesisTier, build_synthesis_prompt,
};

/// Default wall-clock ceiling for one synthesis pass before failing closed.
const DEFAULT_SYNTHESIS_TIMEOUT: Duration = Duration::from_secs(120);

/// Filename an unparseable-response capture is written under, inside the
/// configured [`Synthesizer::with_raw_capture_dir`] directory.
///
/// Why: #6009 — a live 4/4 repro against `anthropic/claude-opus-4.8` cost real
/// OpenRouter spend to diagnose because the only evidence was a WARN log line;
/// the response itself was gone by the time anyone looked. Persisting it next
/// to the report output makes every future occurrence diagnosable for free.
/// What: a fixed name (overwritten on each new failure — this is a diagnostic
/// aid, not an audit trail) so operators know where to look without parsing a
/// timestamp out of a WARN line.
const UNPARSEABLE_CAPTURE_FILENAME: &str = "synthesis-unparseable-response.txt";

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
    /// Verified Code Quality & Architecture narrative, or `None` when rejected or
    /// never emitted (#6004). Same guardrail path as `executive_summary`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_quality_summary: Option<String>,
    /// Verified Security Posture narrative, or `None` when rejected or never
    /// emitted (#6004). Same guardrail path as `executive_summary`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub security_summary: Option<String>,
    /// Verified Authorship & Key-Person Risk narrative, or `None` when
    /// rejected or never emitted (#5453, #6004). Same guardrail path as
    /// `executive_summary`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorship_summary: Option<String>,
    /// Verified top-risk rows (rejected rows are dropped).
    #[serde(default)]
    pub top_risks: Vec<RiskRow>,
    /// Verified per-finding elaboration prose (rejected findings are dropped).
    #[serde(default)]
    pub findings: Vec<FindingProse>,
    /// Reader-facing guardrail/routing notes recorded during injection.
    #[serde(default)]
    pub notes: Vec<StatusNote>,
}

/// One Synthesis Status disclosure, and the finding it is about.
///
/// Why: every line of that block is written for a diligence reader, so it must
/// name the numbered finding it concerns rather than a pipeline label —
/// and the §5.2 number is not knowable when the note is recorded, because the
/// reporter has not numbered the rows yet (#6082 lap 8). Carrying the title
/// separately from the prose lets the reporter resolve the number at render.
/// What: `text` is a complete reader sentence on its own; `subject` is the
/// finding title the reporter looks up, absent for report-level prose.
/// Test: `reporter_tests.rs::a_status_note_cites_its_finding_number`, and
/// `synthesize_tests.rs::a_status_note_renders_with_and_without_a_citation` for
/// the bare arm.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct StatusNote {
    /// The disclosure, as the reader sees it.
    pub text: String,
    /// The finding this disclosure is about, when it is about one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// True when this note discloses that a guard WITHHELD content (#6189).
    ///
    /// Why: a routing note and a withholding read alike in the Synthesis Status
    /// list, but only the second is something a reader who supplied
    /// `instructions.md` needs pointing at — an instruction can ask for a claim
    /// the guards then refuse, and the refusal was visible only in that list.
    /// The Analyst Instructions section counts these to raise the notice.
    /// Test: `reporter_tests::the_instructions_section_counts_withheld_claims`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub withheld: bool,
}

impl StatusNote {
    /// A disclosure about no particular finding.
    pub fn plain(text: impl Into<String>) -> Self {
        StatusNote {
            text: text.into(),
            subject: None,
            withheld: false,
        }
    }

    /// A disclosure about `subject`, which the reporter numbers if it renders.
    pub fn about(subject: &str, text: impl Into<String>) -> Self {
        StatusNote {
            text: text.into(),
            subject: Some(subject.trim().to_string()),
            withheld: false,
        }
    }

    /// Mark this note as disclosing a withholding (#6189).
    pub fn withheld(mut self) -> Self {
        self.withheld = true;
        self
    }

    /// Render one status line, prefixed with `cite` when the reporter resolved
    /// the subject to a numbered finding.
    pub fn render(&self, cite: Option<&str>) -> String {
        match cite {
            Some(c) => format!("{c}: {}", self.text),
            None => self.text.clone(),
        }
    }
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
///
/// Why: three consecutive live calls against `anthropic/claude-opus-4.8`
/// (#6009) returned three different `top_risks` field-name shapes —
/// canonical, `risk`/`applications` (shape 2), and
/// `risk`/`cost_effort_framing`/`affected_applications` (shape 3). Field-name
/// recovery now happens once, upstream, in
/// [`crate::report::synthesize_normalize`] — a whitelist synonym table that
/// runs before `serde_json::from_value` — rather than as a per-shape
/// `#[serde(alias)]` here, which can never converge on a model that renames
/// fields differently on every call.
/// What: `severity`/`cost` still default to `""` when the provider omits them
/// (observed in shapes 2 and 3); the reporter renders an empty value as the
/// honesty marker (`not stated in source data`), never a fabricated band or
/// figure — see `reporter.rs::inject_synthesis_summary`.
/// Test: `synthesize_tests.rs::{parse_raw_recovers_shape2_field_name_drift,
/// parse_raw_recovers_shape3_field_name_drift}`,
/// `reporter_tests.rs::reporter_renders_defaulted_top_risk_severity_honestly`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskRow {
    /// Risk description.
    pub description: String,
    /// Severity band (`RED`/`AMBER`), or `""` when the provider omitted it
    /// (#6009 shapes 2/3) — never fabricated.
    #[serde(default)]
    pub severity: String,
    /// Qualitative cost/effort framing, or `""` when the provider omitted it
    /// (#6009 shapes 2/3) — never fabricated.
    #[serde(default)]
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
    /// The one-line trace verdict marker for this finding, or empty (#6166 leg
    /// 2).
    ///
    /// Never LLM-authored: synthesis has no way to produce it, and the field is
    /// `#[serde(default)]` so a synthesis response that omits it (every one of
    /// them) deserialises unchanged. It is carried here because the investigation
    /// prose is the row that wins for a verified finding, and the marker has to
    /// travel with it to reach the rendered evidence block.
    #[serde(default)]
    pub trace_verdict: String,
    /// The CWE id for this finding's weakness class, or `None` (#6779).
    ///
    /// Never LLM-authored HERE: synthesis has no schema field for it, so every
    /// synthesis response deserialises with `None`. It travels on this row
    /// because the investigation's prose is the row that wins for a verified
    /// finding, and the tag has to reach the rendered title with it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwe_id: Option<String>,
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
    raw_capture_dir: Option<PathBuf>,
    max_tokens: u32,
}

impl Synthesizer {
    /// Create a synthesizer from an injected provider and model id.
    ///
    /// Why: the CLI builds the provider from the reviewer role config; tests
    /// inject stubs.
    /// What: stores the provider + model with the default timeout, the default
    /// output budget, and no raw capture directory (opt-in via
    /// [`Self::with_raw_capture_dir`]).
    /// Test: exercised by every synthesize test.
    pub fn new(llm: Arc<dyn LlmProvider>, model: impl Into<String>) -> Self {
        Self {
            llm,
            model: model.into(),
            timeout: DEFAULT_SYNTHESIS_TIMEOUT,
            raw_capture_dir: None,
            max_tokens: SYNTHESIS_DEFAULT_MAX_TOKENS,
        }
    }

    /// Set the output-token ceiling synthesis asks for on its first attempt.
    ///
    /// Why: #6093 — `[models.reviewer].max_tokens` never reached this path.
    /// `Synthesizer::new(provider, role.model.clone())` took the model id and
    /// nothing else, and the request carried a hardcoded 3072 regardless of
    /// what the operator configured, so raising the configured ceiling did
    /// not change the request that truncated.
    /// What: stores the ceiling used as the base of the retry ladder's budget
    /// (see [`SynthesisTier::budget`], which clamps it into a supported range
    /// and escalates it on each retry rung).
    /// Test: `synthesize_tests.rs::configured_max_tokens_reaches_the_request`.
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    /// Override the fail-closed timeout (used by tests).
    #[cfg(test)]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Opt in to persisting the raw provider response on an unparseable-
    /// response failure (#6009).
    ///
    /// Why: the only prior evidence of an unparseable response was a WARN log
    /// line with no body — a live 4/4 repro against `anthropic/claude-opus-4.8`
    /// had to spend a fresh OpenRouter call just to see what the model actually
    /// sent. The CLI wires this to the report's own output directory so the
    /// evidence lands next to the report a failed run was trying to produce.
    /// What: when set, [`Self::synthesize`] writes the scrubbed response text
    /// to `<dir>/`[`UNPARSEABLE_CAPTURE_FILENAME`] the moment `parse_raw`
    /// rejects it — before the guardrail or any retry — so the capture reflects
    /// exactly what the provider sent. The directory is created if missing; a
    /// write failure is logged and never escalated, since a diagnostic aid must
    /// not itself turn a reportable error into a different one.
    /// Test: `capture_dir_persists_raw_response_on_parse_failure`.
    pub fn with_raw_capture_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.raw_capture_dir = Some(dir.into());
        self
    }

    /// Run synthesis, returning verified prose or the reason there is none.
    ///
    /// Why: the single entry point the CLI awaits; it NEVER fabricates and NEVER
    /// partial-trusts a malformed response.  A live-QA acceptance run found that a
    /// large, real finding count could still hit the output-token ceiling even
    /// with the #2357-follow-up compact digest and bounded schema.  #6093 is what
    /// that retry could not fix: it shrank `top_risks` from five rows to three
    /// while leaving ten per-finding elaborations — the dominant output cost —
    /// and the request's hardcoded 3072-token ceiling both untouched, so a
    /// 45-finding investigation truncated on both calls and exited with no
    /// report.  The retry is now a three-rung ladder in which every rung raises
    /// the budget AND shrinks the ask, ending in a narrative-only rung that
    /// requests no elaboration prose at all.  #5454 still decides what happens
    /// when the ladder is spent: the failure propagates rather than degrading
    /// the report to deterministic-only.
    /// What: builds the numeric allow-set from the deterministic model and the
    /// figures that model's own report prints (#6137), then
    /// walks [`SYNTHESIS_TIER_LADDER`], calling the provider under a timeout at
    /// each rung; a `finish_reason` of `length`/`max_tokens` advances to the
    /// next rung, and exhausting the ladder is [`SynthesisError::Truncated`].
    /// A parsed response is passed through the numeric guardrail field-by-field,
    /// dropping any field whose prose cites a figure absent from the source.
    ///
    /// Dropping elaboration on the last rung drops no finding: every RED/AMBER
    /// finding still renders from the deterministic composition (#5374), and the
    /// investigation's verified prose is merged over synthesis prose afterwards
    /// regardless (`cli_report::merge_investigation_prose`).
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
    /// synthesize_ladder_recovers_a_large_finding_set,
    /// synthesize_still_truncated_after_retry_is_a_hard_error}`.
    pub async fn synthesize(&self, model: &ReportModel) -> Result<Synthesis, SynthesisError> {
        // Ground truth for the guardrail comes from the DETERMINISTIC model
        // only — plus, since #6137, the figures the deterministic report
        // computes at render time and prints in its own sections.
        let printed = crate::report::figures::printed_figures(model);
        let allowed = match serde_json::to_value(model) {
            Ok(v) => allowed_numbers_with(&v, &printed),
            Err(e) => {
                warn!(error = %e, "synthesis: could not serialise model for guardrail");
                return Err(SynthesisError::ModelNotSerialisable(e.to_string()));
            }
        };

        // #6082 lap 4: the claim-level ground truth, from the same deterministic
        // model. Built once, alongside the numeric allow-set it complements.
        let grounding = Grounding::from_model(model);

        for (i, tier) in SYNTHESIS_TIER_LADDER.iter().enumerate() {
            match self.try_once(model, *tier).await {
                Attempt::Ok(raw, norm_notes) => {
                    let seed = norm_notes.into_iter().map(StatusNote::plain).collect();
                    return super::synthesize_guardrail::apply_guardrail(
                        raw, &allowed, seed, &grounding,
                    );
                }
                Attempt::Failed(e) => return Err(e),
                Attempt::Truncated => match SYNTHESIS_TIER_LADDER.get(i + 1) {
                    Some(next) => warn!(
                        next_tier = ?next,
                        next_budget = next.budget(self.max_tokens),
                        next_elaborations = next.elaboration_cap(),
                        "synthesis: response truncated — retrying with a larger budget and a smaller ask"
                    ),
                    None => warn!("synthesis: still truncated after the narrative-only attempt"),
                },
            }
        }
        // #5454: the ladder is spent; the failure is fatal, never a degraded report.
        Err(SynthesisError::Truncated)
    }

    /// Make one provider call at one rung of the ladder and classify the result.
    async fn try_once(&self, model: &ReportModel, tier: SynthesisTier) -> Attempt {
        let req = build_synthesis_prompt(model, &self.model, tier, self.max_tokens);
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

        let (raw, norm_notes) = match parse_raw(&resp.text) {
            Some(r) => r,
            None => {
                warn!("synthesis: unparseable response");
                self.capture_unparseable(&resp.text);
                return Attempt::Failed(SynthesisError::Unparseable);
            }
        };

        debug!(
            input_tokens = resp.input_tokens,
            output_tokens = resp.output_tokens,
            "synthesis: provider call complete"
        );
        Attempt::Ok(raw, norm_notes)
    }

    /// Persist a scrubbed copy of an unparseable response, when a capture
    /// directory was configured (#6009).
    ///
    /// Why: see [`Self::with_raw_capture_dir`]. A no-op when unset, which keeps
    /// every existing test (none of which configure a capture dir) unaffected.
    /// What: scrubs `text` against [`crate::report::redact::report_secrets`]
    /// (the same needle set the `Provider` error path already uses) and writes
    /// it to `<dir>/`[`UNPARSEABLE_CAPTURE_FILENAME`], creating the directory if
    /// needed. Any I/O failure is logged at `warn` and swallowed — the
    /// diagnostic write must never mask or replace the real
    /// [`SynthesisError::Unparseable`] the caller already returns.
    /// Test: `capture_dir_persists_raw_response_on_parse_failure`.
    fn capture_unparseable(&self, text: &str) {
        let Some(dir) = &self.raw_capture_dir else {
            return;
        };
        let secrets = crate::report::redact::report_secrets();
        let scrubbed = trusty_common::credentials::scrub_secrets(text, &secrets);
        if let Err(e) = std::fs::create_dir_all(dir) {
            warn!(error = %e, dir = %dir.display(), "synthesis: could not create raw-capture directory");
            return;
        }
        let path = dir.join(UNPARSEABLE_CAPTURE_FILENAME);
        match std::fs::write(&path, &scrubbed) {
            Ok(()) => {
                warn!(path = %path.display(), "synthesis: unparseable response captured for diagnosis")
            }
            Err(e) => {
                warn!(error = %e, path = %path.display(), "synthesis: could not write raw-capture file")
            }
        }
    }
}

/// The outcome of one provider call attempt (pre-guardrail).
enum Attempt {
    /// Parsed cleanly; carries the raw (pre-guardrail) synthesis plus any
    /// normalization notes recorded while resolving field-name drift (#6009
    /// shape 3 — see [`crate::report::synthesize_normalize`]).
    Ok(RawSynthesis, Vec<String>),
    /// The response was truncated at the output-token ceiling.
    Truncated,
    /// A provider/timeout/parse error — not a truncation.
    Failed(SynthesisError),
}

// ─── Raw (pre-guardrail) parse types ────────────────────────────────────────

/// The raw JSON contract the provider is asked to emit (pre-verification).
#[derive(Debug, Deserialize)]
pub(super) struct RawSynthesis {
    #[serde(default)]
    pub(super) executive_summary: String,
    /// #6004: same schema/call/guardrail as `executive_summary`.
    #[serde(default)]
    pub(super) code_quality_summary: String,
    /// #6004: same schema/call/guardrail as `executive_summary`.
    #[serde(default)]
    pub(super) security_summary: String,
    /// #5453/#6004: same schema/call/guardrail as `executive_summary`.
    #[serde(default)]
    pub(super) authorship_summary: String,
    #[serde(default)]
    pub(super) top_risks: Vec<RiskRow>,
    #[serde(default)]
    pub(super) findings: Vec<FindingProse>,
}

/// Parse the provider response into [`RawSynthesis`] plus any normalization
/// notes recorded while resolving field-name drift.
///
/// Why: forced structured output returns a bare JSON object, but we still accept
/// a fenced block (defensive, matching the profile synthesizer) so a provider
/// that ignores the schema does not silently fail.  #6009: three consecutive
/// live calls against `anthropic/claude-opus-4.8` produced three different
/// shapes — pure markdown prose (shape 1), valid JSON with `top_risks` fields
/// renamed to `risk`/`applications` (shape 2), and valid JSON renamed to
/// `risk`/`cost_effort_framing`/`affected_applications` (shape 3).
/// `response_format: json_schema` is best-effort for Anthropic-family models —
/// there is no native strict-JSON mode, so OpenRouter forwards but cannot
/// enforce the shape — and per-shape `#[serde(alias)]` additions can never
/// converge on a model that renames fields differently on every call.
/// What: tries a direct object parse, then a ```json fenced block — both
/// routed through [`parse_json_object`], which normalizes known field-name
/// synonyms (shapes 2/3) via [`crate::report::synthesize_normalize`] BEFORE
/// typed deserialization — then a markdown-heading fallback (shape 1) that
/// recovers ONLY `executive_summary` (never `top_risks`/`findings` —
/// reconstructing structured rows out of prose would mean inventing fields
/// the model never actually populated, which is exactly what this parser
/// must not do).  `None` when none of the three yields anything, so a
/// response that is not even this fallback shape is still rejected as
/// [`SynthesisError::Unparseable`].
/// Test: `synthesize_tests.rs::{synthesize_happy_path_injects,
/// synthesize_malformed_json_is_a_hard_error,
/// synthesize_recovers_executive_summary_from_markdown_fallback,
/// synthesize_markdown_fallback_rejects_response_with_no_heading,
/// parse_raw_recovers_shape2_field_name_drift,
/// parse_raw_recovers_shape3_field_name_drift,
/// synthesize_rejects_a_wholly_unrecognized_shape}`.
fn parse_raw(text: &str) -> Option<(RawSynthesis, Vec<String>)> {
    let body = text.trim();
    if body.starts_with('{')
        && let Some(result) = parse_json_object(body)
    {
        return Some(result);
    }
    if let Some(result) = parse_fenced_json(body) {
        return Some(result);
    }
    parse_markdown_fallback(body).map(|r| (r, Vec::new()))
}

/// Parse one JSON object into [`RawSynthesis`], normalizing known field-name
/// synonyms first.
///
/// Why: split out so both the direct-object and fenced-block tiers share the
/// exact same normalize-then-deserialize path — a synonym recognised at one
/// tier is recognised at the other.
/// What: parses `body` as a [`serde_json::Value`], runs
/// [`crate::report::synthesize_normalize::normalize_raw_synthesis`], then
/// deserializes the normalized value into [`RawSynthesis`]. `None` when
/// `body` is not valid JSON or the normalized shape still does not match —
/// a wholly unrecognised shape (every key dropped by the whitelist) still
/// deserializes to an empty [`RawSynthesis`], which the numeric guardrail
/// then rejects as [`SynthesisError::NoVerifiableContent`] rather than this
/// function silently accepting it as content.
/// Test: `synthesize_tests.rs::parse_raw_recovers_shape3_field_name_drift`.
fn parse_json_object(body: &str) -> Option<(RawSynthesis, Vec<String>)> {
    let mut value: serde_json::Value = serde_json::from_str(body).ok()?;
    let mut notes = Vec::new();
    super::synthesize_normalize::normalize_raw_synthesis(&mut value, &mut notes);
    let raw: RawSynthesis = serde_json::from_value(value).ok()?;
    Some((raw, notes))
}

/// Parse a ```` ```json ````-fenced block, if one is present.
///
/// Why: split out of [`parse_raw`] so each fallback tier is independently
/// readable and testable.
/// What: finds the LAST `\`\`\`json` marker (defensive against a fence
/// appearing earlier in reasoning/preamble text), then the next closing fence
/// after it; `None` when either marker is absent or the enclosed text does not
/// decode via [`parse_json_object`].
/// Test: covered transitively via `parse_raw`'s own tests.
fn parse_fenced_json(body: &str) -> Option<(RawSynthesis, Vec<String>)> {
    let fence_start = body.rfind("```json")?;
    let after = &body[fence_start + 7..];
    let fence_end = after.find("```")?;
    parse_json_object(after[..fence_end].trim())
}

/// Recover an `executive_summary` from a markdown-headed free-text response
/// (#6009) when the provider ignored the forced JSON schema entirely.
///
/// Why: see [`parse_raw`]'s doc comment. `top_risks` and `findings` are always
/// left empty here — the deterministic composition (#5374) fills those
/// placeholders, and the numeric guardrail still verifies whatever text is
/// recovered before it reaches a reader.
/// What: locates a line that is ONLY a markdown heading (`#`+ prefix) whose
/// title matches "executive summary" case-insensitively, and takes every line
/// up to the next heading (or end of input) as the summary body.  `None` when
/// no such heading exists or its body is blank, so an unrelated free-text
/// response (an error message, a refusal, a genuinely different shape) is
/// still rejected rather than silently accepted.
/// Test: `synthesize_recovers_executive_summary_from_markdown_fallback`,
/// `synthesize_markdown_fallback_rejects_response_with_no_heading`.
fn parse_markdown_fallback(text: &str) -> Option<RawSynthesis> {
    let exec = heading_section(text, "executive summary")?;
    if exec.is_empty() {
        return None;
    }
    Some(RawSynthesis {
        executive_summary: exec,
        // #6004/#5453 fields are never recoverable from this free-text
        // fallback — reconstructing them from prose would mean inventing
        // content the model never structured; they stay empty and the
        // guardrail leaves them absent from `Synthesis`, same honesty-marker
        // path as an outright-rejected field.
        code_quality_summary: String::new(),
        security_summary: String::new(),
        authorship_summary: String::new(),
        top_risks: Vec::new(),
        findings: Vec::new(),
    })
}

/// Return the trimmed line, minus a leading run of `#`, iff the line is
/// nothing but a markdown heading marker.
fn heading_title(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    let title = trimmed.trim_start_matches('#');
    // Require at least one '#' actually present, and a space (or end of
    // line) after it — "#5454" in prose must never be misread as a heading.
    if title.len() == trimmed.len() {
        return None;
    }
    let title = title.trim();
    (!title.is_empty()).then_some(title)
}

/// Extract the body text between a heading whose title matches `wanted`
/// (case-insensitively) and the next heading line, or end of input.
fn heading_section(text: &str, wanted: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines
        .iter()
        .position(|l| heading_title(l).is_some_and(|t| t.eq_ignore_ascii_case(wanted)))?;
    let body: Vec<&str> = lines[start + 1..]
        .iter()
        .take_while(|l| heading_title(l).is_none())
        .copied()
        .collect();
    let joined = body.join("\n").trim().to_string();
    (!joined.is_empty()).then_some(joined)
}

#[cfg(test)]
#[path = "synthesize_tests.rs"]
mod tests;
