//! The verdict pass: judge each traced finding against its own trace (#6166,
//! leg 2).
//!
//! Why: leg 1 anchored a finding to the symbol its `file:line` resolves to and
//! stopped there — the reader still had to decide whether the traced code
//! actually supports what the finding claims. This pass asks the verifier-role
//! model that question once per traced finding and records the answer, so a
//! finding the trace contradicts is downgraded and labelled rather than left
//! standing at full severity.
//!
//! What: [`run_verdicts`] makes one bounded provider call per assembled trace
//! and returns a [`VerdictSet`]; [`apply_verdicts`] folds that set onto the
//! findings — CONFIRMED annotates, CLEARED downgrades one band and appends the
//! verifier's one-line reason, UNVERIFIABLE changes nothing at all.
//!
//! Fail-closed: a transport error, a timeout, a truncated response, a refusal,
//! an unparseable body, or a verdict token this crate does not know all land on
//! [`Verdict::Unverifiable`] carrying the reason. A candidate that never got a
//! trace is UNVERIFIABLE too, with leg 1's `no trace:` reason. The pass can
//! therefore only ever leave a finding as it was or lower it — it has no branch
//! that raises a severity or removes a finding, which is what keeps a failing
//! verdict pass from making the report look better than the evidence.
//!
//! Test: `verdict_tests.rs` drives one stub per error arm.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::llm::{ChatMessage, LlmProvider, LlmRequest, ResponseSchema, strip_provider_prefix};
use crate::report::metrics::Severity;

use super::trace::{FindingTrace, TraceSet};
use super::verify::VerifiedFinding;

/// Wall-clock ceiling for one verdict call.
///
/// One finding and its trace is a far smaller request than an investigation
/// batch, so this is a third of `batch::BATCH_TIMEOUT` — long enough for a cold
/// provider, short enough that a hung call cannot stall the whole traced set.
const VERDICT_TIMEOUT: Duration = Duration::from_secs(60);

/// Temperature for the verdict call — zero, because this is an adjudication and
/// not a generation.
const VERDICT_TEMPERATURE: f32 = 0.0;

/// Output-token ceiling for one verdict: an enum token and one sentence.
const VERDICT_MAX_TOKENS: u32 = 512;

/// Maximum characters of the verifier's `reason` that reach the page.
pub const REASON_MAX_CHARS: usize = 300;

/// The verifier-role client the verdict pass calls.
///
/// Why: `run_investigation` already holds the reviewer-role provider, and the
/// verdict pass must run on the verifier role the manifest declares — a
/// different model, built through the same [`crate::llm::build_provider`] path.
/// Pairing the provider with its id keeps the two from being passed separately
/// and mismatched.
/// What: the built provider and the model id as configured (the provider
/// routing prefix is stripped at request-build time).
pub struct Verifier {
    /// The built verifier-role provider.
    pub provider: Arc<dyn LlmProvider>,
    /// The verifier-role model id, prefix included.
    pub model: String,
}

/// What the verifier concluded about one finding, given its trace.
///
/// Why: three outcomes, and only three — a verdict that "sort of" supports the
/// finding is [`Verdict::Unverifiable`], because a due-diligence reader needs
/// the uncertain case counted rather than rounded toward either side.
/// What: `Confirmed` when the traced code supports the finding, `Cleared` when
/// it contradicts it, `Unverifiable` for everything else including every
/// failure.
/// Test: `verdict_tests::{a_provider_error_is_unverifiable_and_names_itself,
/// an_unknown_verdict_token_is_unverifiable}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// The trace supports the finding.
    Confirmed,
    /// The trace contradicts the finding.
    Cleared,
    /// The trace does not settle it, or the pass could not ask.
    Unverifiable,
}

impl Verdict {
    /// Parse the model's verdict token; anything unrecognised is `None` so the
    /// caller fails it closed with a reason naming what was received.
    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "confirmed" => Some(Verdict::Confirmed),
            "cleared" => Some(Verdict::Cleared),
            "unverifiable" => Some(Verdict::Unverifiable),
            _ => None,
        }
    }
}

/// One finding's verdict, keyed the way findings are keyed everywhere else.
///
/// Why: `(file, title)` is the identity `batch::merge_dedupe` and
/// `merge_investigation_prose` already use, so a verdict folds back onto its
/// finding without a second notion of identity.
/// What: the finding's file and title, the verdict, and the one-line reason —
/// the verifier's own for a decided verdict, or the failure for a fail-closed
/// one.
#[derive(Debug, Clone, Serialize)]
pub struct FindingVerdict {
    /// The finding's title.
    pub title: String,
    /// The finding's repository-relative file.
    pub file: String,
    /// The verdict.
    pub verdict: Verdict,
    /// One line: the verifier's reason, or why the pass failed closed.
    pub reason: String,
}

/// Every verdict for one repository, with the counts the report states.
///
/// Why: the coverage line and the manifest gaps line both quote these counts,
/// so they are computed once here rather than recounted at each render site —
/// two surfaces disagreeing about the same set is the defect #6080 fixed for
/// the finding counts, and this pass must not reintroduce it.
/// What: one [`FindingVerdict`] per traced candidate (refused traces included),
/// the three counts, and the model that judged them.
/// Test: `verdict_tests::the_counts_match_the_verdicts_and_one_call_is_made_per_trace`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct VerdictSet {
    /// One entry per candidate the trace pass considered.
    pub verdicts: Vec<FindingVerdict>,
    /// Count of [`Verdict::Confirmed`].
    pub confirmed: usize,
    /// Count of [`Verdict::Cleared`].
    pub cleared: usize,
    /// Count of [`Verdict::Unverifiable`], no-trace candidates included.
    pub unverifiable: usize,
    /// Total candidates judged (equals `verdicts.len()`).
    pub traced: usize,
    /// The verifier model id that judged them, or empty when none was built.
    pub model: String,
}

impl VerdictSet {
    /// Build the set from its verdicts, deriving every count.
    fn from_verdicts(verdicts: Vec<FindingVerdict>, model: String) -> Self {
        let count = |v: Verdict| verdicts.iter().filter(|f| f.verdict == v).count();
        Self {
            confirmed: count(Verdict::Confirmed),
            cleared: count(Verdict::Cleared),
            unverifiable: count(Verdict::Unverifiable),
            traced: verdicts.len(),
            verdicts,
            model,
        }
    }

    /// The one-line summary both the coverage section and the gaps line quote.
    pub fn summary_line(&self) -> String {
        format!(
            "{} confirmed, {} cleared, {} unverifiable of {} traced",
            self.confirmed, self.cleared, self.unverifiable, self.traced,
        )
    }
}

/// The verifier's answer, exactly as the forced schema shapes it.
#[derive(Debug, Clone, Deserialize)]
struct RawVerdict {
    #[serde(default)]
    verdict: String,
    #[serde(default)]
    reason: String,
}

/// The forced JSON schema for one verdict.
///
/// Why: an enum-constrained token removes the "did it say 'CONFIRMED.' or
/// 'I confirm'" classification problem entirely — a body that does not conform
/// fails to parse and is counted as unverifiable rather than guessed at.
/// What: `verdict` is one of the three tokens; `reason` is one bounded
/// sentence.
/// Test: `verdict_tests::the_schema_constrains_the_verdict_token`.
pub fn verdict_schema() -> ResponseSchema {
    ResponseSchema::new(
        "trace_verdict",
        serde_json::json!({
            "type": "object",
            "properties": {
                "verdict": {
                    "type": "string",
                    "enum": ["confirmed", "cleared", "unverifiable"],
                    "description": "confirmed = the traced code supports the finding; cleared = the traced code contradicts it; unverifiable = the trace does not settle it."
                },
                "reason": {
                    "type": "string",
                    "maxLength": REASON_MAX_CHARS,
                    "description": "ONE sentence naming the specific line or construct that decided the verdict. Never restate the finding."
                }
            }
        }),
    )
}

/// The verdict system prompt.
fn system_prompt() -> &'static str {
    r#"You are adjudicating one due-diligence finding against a symbol-graph trace of the code it cites. You are given the finding as written and the declaration plus in-file usage sites of the symbol its citation resolves to.

Return exactly one verdict:
- `confirmed` — the traced code supports the finding as stated.
- `cleared` — the traced code CONTRADICTS the finding: the guard, bound, check, or handling the finding says is missing is visibly present at the traced site.
- `unverifiable` — the trace does not carry enough code to decide either way.

## Absolute rules
- Judge ONLY the code shown below. Never assume a caller, a guard, a validation layer, or a test that is not in the trace.
- Absence of evidence is `unverifiable`, never `cleared`. `cleared` requires the trace to show the opposite of what the finding claims.
- A thin trace is `unverifiable`. Deciding either way on insufficient code is the exact failure this pass exists to prevent, and an honest `unverifiable` costs the report nothing.
- `reason` is ONE sentence naming the specific line, identifier, or construct that decided it. Do not restate the finding, and do not hedge across two verdicts."#
}

/// Build the verdict request for one traced finding.
///
/// Why: the finding and its trace are the whole input — no repository context is
/// re-sent, which is what keeps this pass a bounded per-finding call rather than
/// a second investigation.
/// What: system rules plus a digest carrying the finding (title, severity,
/// dimension, citation, verified evidence quote) and the trace (anchor symbol,
/// declaration signature, usage snippets, and any usage-read failure).
/// Test: `verdict_tests::{the_request_carries_the_finding_and_its_trace,
/// the_request_strips_the_provider_prefix}`.
pub fn build_request(f: &VerifiedFinding, t: &FindingTrace, llm_model: &str) -> LlmRequest {
    LlmRequest {
        model: strip_provider_prefix(llm_model).to_string(),
        system: system_prompt().to_string(),
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: build_digest(f, t),
        }],
        temperature: VERDICT_TEMPERATURE,
        max_tokens: VERDICT_MAX_TOKENS,
        response_schema: Some(verdict_schema()),
    }
}

/// The user digest: the finding, then its trace.
fn build_digest(f: &VerifiedFinding, t: &FindingTrace) -> String {
    let mut msg = String::with_capacity(2048);
    msg.push_str("# Finding under review\n\n");
    msg.push_str(&format!("- title: {}\n", f.title));
    msg.push_str(&format!("- severity: {}\n", severity_token(f.severity)));
    msg.push_str(&format!("- dimension: {}\n", f.dimension));
    msg.push_str(&format!(
        "- cited at: {}{}\n",
        f.file,
        f.line.map(|l| format!(":{l}")).unwrap_or_default()
    ));
    if !f.description.is_empty() {
        msg.push_str(&format!("- description: {}\n", f.description));
    }
    msg.push_str("\n## Verified evidence quote (verbatim from the cited file)\n\n");
    msg.push_str(&format!("```\n{}\n```\n\n", f.evidence_quote));

    msg.push_str("# Trace of the symbol that citation resolves to\n\n");
    match &t.anchor {
        Some(a) => {
            msg.push_str(&format!("- symbol: {}\n", a.symbol));
            msg.push_str(&format!("- declared at: {}:{}\n", a.file, a.line));
            if !a.signature.is_empty() {
                msg.push_str(&format!("- declaration: `{}`\n", a.signature));
            }
        }
        None => msg.push_str("- no anchor was assembled for this finding\n"),
    }
    if t.usages.is_empty() {
        let why = if t.usages_status.is_empty() {
            "none were found in the cited file"
        } else {
            &t.usages_status
        };
        msg.push_str(&format!("\n## Usage sites\n\nNo usage sites ({why}).\n\n"));
    } else {
        msg.push_str(&format!("\n## Usage sites ({})\n\n", t.usages.len()));
        for u in &t.usages {
            msg.push_str(&format!("### {}:{}\n\n", u.file, u.line));
            msg.push_str(&format!("```\n{}\n```\n\n", u.snippet));
        }
    }
    msg.push_str(
        "Return the verdict for this finding. Choose `unverifiable` unless the code above \
         genuinely settles it.\n",
    );
    msg
}

/// The severity token the digest names, matching the investigation prompt's own
/// spelling.
fn severity_token(s: Severity) -> &'static str {
    match s {
        Severity::Red => "red",
        Severity::Amber => "amber",
        Severity::Green => "green",
    }
}

/// Parse the verifier response into a raw verdict, tolerating a fenced body the
/// way [`super::analyze::parse_findings`] does.
fn parse_verdict(text: &str) -> Option<RawVerdict> {
    let body = text.trim();
    if body.starts_with('{')
        && let Ok(v) = serde_json::from_str::<RawVerdict>(body)
    {
        return Some(v);
    }
    let fence_start = body.rfind("```json")?;
    let after = &body[fence_start + 7..];
    let fence_end = after.find("```")?;
    serde_json::from_str::<RawVerdict>(after[..fence_end].trim()).ok()
}

/// Run the verdict pass over one repository's traced findings.
///
/// Why: this is the whole leg-2 outcome — every candidate the trace pass
/// considered gets a recorded verdict, so the counts the report states cover
/// the traced set exactly rather than only its successes.
/// What: one bounded provider call per trace that HAS an anchor; a trace leg 1
/// refused is recorded UNVERIFIABLE carrying that refusal reason without
/// spending a call. `None` for `verifier` records every candidate UNVERIFIABLE
/// with the reason the caller supplies.
/// Test: `verdict_tests::{a_confirmed_finding_carries_its_trace_context,
/// a_no_trace_candidate_never_calls_the_model,
/// a_truncated_response_is_unverifiable,
/// a_refusal_or_unparseable_body_is_unverifiable,
/// a_hung_verifier_times_out_into_unverifiable}`.
pub async fn run_verdicts(
    verifier: Option<&Verifier>,
    traces: &TraceSet,
    findings: &[VerifiedFinding],
    no_verifier_reason: &str,
) -> VerdictSet {
    let model = verifier.map(|v| v.model.clone()).unwrap_or_default();
    let mut out = Vec::with_capacity(traces.traces.len());
    for t in &traces.traces {
        // Leg 1 already refused this candidate; there is nothing to judge, and
        // the refusal is the honest reason.
        if let Some(reason) = &t.no_trace {
            out.push(unverifiable(t, reason.clone()));
            continue;
        }
        let Some(v) = verifier else {
            out.push(unverifiable(t, no_verifier_reason.to_string()));
            continue;
        };
        let Some(f) = findings
            .iter()
            .find(|f| f.file == t.file && f.title == t.title)
        else {
            // Unreachable through `run_investigation` (the traces are built from
            // these same findings), but a caller that pairs a stale trace set
            // with a fresh finding list gets a counted gap, not a panic.
            out.push(unverifiable(
                t,
                "no verdict: the traced finding is no longer in the finding set".to_string(),
            ));
            continue;
        };
        out.push(judge_one(v, f, t).await);
    }
    VerdictSet::from_verdicts(out, model)
}

/// One provider call, with every failure classified as UNVERIFIABLE.
async fn judge_one(v: &Verifier, f: &VerifiedFinding, t: &FindingTrace) -> FindingVerdict {
    let req = build_request(f, t, &v.model);
    let resp = match tokio::time::timeout(VERDICT_TIMEOUT, v.provider.complete(req)).await {
        Err(_) => return unverifiable(t, "no verdict: verifier timeout".to_string()),
        Ok(Err(e)) => return unverifiable(t, format!("no verdict: verifier error: {e}")),
        Ok(Ok(r)) => r,
    };
    if matches!(
        resp.finish_reason.as_deref(),
        Some("length") | Some("max_tokens")
    ) {
        return unverifiable(t, "no verdict: verifier response truncated".to_string());
    }
    let Some(raw) = parse_verdict(&resp.text) else {
        // A refusal ("I can't help with that") is exactly this shape: a 200 with
        // a body that is not the schema.
        return unverifiable(
            t,
            "no verdict: verifier response was not a verdict object".to_string(),
        );
    };
    let Some(verdict) = Verdict::parse(&raw.verdict) else {
        return unverifiable(
            t,
            format!(
                "no verdict: verifier answered an unknown verdict '{}'",
                truncate_reason(&raw.verdict)
            ),
        );
    };
    let reason = truncate_reason(&raw.reason);
    FindingVerdict {
        title: t.title.clone(),
        file: t.file.clone(),
        verdict,
        reason: if reason.is_empty() {
            "the verifier gave no reason".to_string()
        } else {
            reason
        },
    }
}

/// A fail-closed verdict for one trace record.
fn unverifiable(t: &FindingTrace, reason: String) -> FindingVerdict {
    FindingVerdict {
        title: t.title.clone(),
        file: t.file.clone(),
        verdict: Verdict::Unverifiable,
        reason,
    }
}

/// Collapse a reason to one line within [`REASON_MAX_CHARS`].
///
/// Why: the schema bounds it, but a provider that ignores `maxLength` must not
/// be able to splice a multi-line body into a single-line report bullet — and
/// the live run showed the second half of that: Haiku returned a 340-character
/// reason and a plain character cut ended the rendered bullet "…visible in the
/// tr", which reads as corrupted text rather than as an abbreviation.
/// What: collapses all whitespace to single spaces, then — only when the result
/// is over the cap — cuts at the last word boundary inside it and appends an
/// ellipsis. A reason already within the cap is returned untouched, so the
/// common case gains no marker.
/// Test: `verdict_tests::{a_multi_line_reason_is_collapsed_and_bounded,
/// an_over_long_reason_is_cut_at_a_word_boundary,
/// a_reason_within_the_cap_is_untouched}`.
fn truncate_reason(s: &str) -> String {
    let one_line = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= REASON_MAX_CHARS {
        return one_line;
    }
    // Leave room for the ellipsis so the whole bullet still fits the cap.
    let cut: String = one_line.chars().take(REASON_MAX_CHARS - 1).collect();
    let head = match cut.rsplit_once(' ') {
        Some((head, _)) if !head.is_empty() => head,
        // A single word longer than the cap has no boundary to cut at.
        _ => cut.trim_end(),
    };
    format!("{}…", head.trim_end())
}

/// Fold a verdict set onto its findings.
///
/// Why: the verdict is only worth making if it reaches the page, and the two
/// ways it does are the per-finding marker and the severity a CLEARED finding
/// drops to. Doing both here — in one pass over the findings, before anything
/// downstream reads them — is what keeps the rendered band, the JSON twin, and
/// the coverage counts describing the same set.
/// What: CONFIRMED writes the marker and leaves severity alone; CLEARED writes
/// the marker and drops the severity one band (RED→AMBER, AMBER→GREEN, GREEN
/// unchanged — nothing below it); UNVERIFIABLE writes nothing, so an unverified
/// finding renders exactly as it did before this pass existed. No arm removes a
/// finding, edits its evidence quote, or moves its citation.
/// Test: `verdict_tests::{a_cleared_finding_drops_one_band,
/// an_unverifiable_finding_is_left_exactly_as_it_was,
/// clearing_never_touches_the_evidence_or_the_citation}`.
pub fn apply_verdicts(findings: &mut [VerifiedFinding], set: &VerdictSet, traces: &TraceSet) {
    for v in &set.verdicts {
        let Some(f) = findings
            .iter_mut()
            .find(|f| f.file == v.file && f.title == v.title)
        else {
            continue;
        };
        match v.verdict {
            Verdict::Unverifiable => {}
            Verdict::Confirmed => {
                let anchor = traces
                    .traces
                    .iter()
                    .find(|t| t.file == v.file && t.title == v.title)
                    .map(anchor_context)
                    .unwrap_or_default();
                f.trace_verdict = format!("verified-by-trace{anchor} — {}", v.reason);
            }
            Verdict::Cleared => {
                f.trace_verdict = format!("cleared-by-trace: {}", v.reason);
                f.severity = downgrade(f.severity);
            }
        }
    }
}

/// The trace context a CONFIRMED finding carries into its rendered evidence.
fn anchor_context(t: &FindingTrace) -> String {
    let Some(a) = &t.anchor else {
        return String::new();
    };
    format!(
        " (`{}` declared at {}:{}, {} usage site(s) in the cited file)",
        a.symbol,
        a.file,
        a.line,
        t.usages.len(),
    )
}

/// One severity band down; GREEN is the floor.
///
/// A cleared finding is never deleted — it lands one band lower carrying the
/// reason it was cleared, so the decision stays on the page for the owner to
/// disagree with.
fn downgrade(s: Severity) -> Severity {
    match s {
        Severity::Red => Severity::Amber,
        Severity::Amber | Severity::Green => Severity::Green,
    }
}

#[cfg(test)]
#[path = "verdict_tests.rs"]
mod tests;
