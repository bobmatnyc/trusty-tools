//! LLM synthesis pass for the map-reduce reduce stage (Phase 6, #1663).
//!
//! Why: after the mechanical reduce stage aggregates per-chunk findings with a
//! deterministic worst-chunk-wins verdict, the overall verdict is often harsher
//! than a single unified reviewer would produce.  The primary driver is the
//! count-based `≥2 Medium → REQUEST_CHANGES` floor firing on minor per-file nits
//! that, holistically, would not concern a reviewer reading the whole PR.  A
//! calibration LLM pass judges the PR as a coherent whole, discounting isolated
//! nits, and its verdict is then safety-floored against High-severity findings so
//! critical issues can never be softened.
//!
//! What: `synthesize_review` takes the mechanically-reduced `ReducedReview`,
//! makes ONE additional LLM call asking the model to judge the PR holistically
//! (given the deduped finding list and per-chunk verdicts), and returns a new
//! `ReducedReview` whose `verdict`, `grade`, and `summary` come from the synthesis
//! response.  The High-severity safety floor is re-applied after synthesis, but the
//! count-based Medium floor is deliberately omitted — the LLM has already holistically
//! judged those findings.  Any synthesis error causes a graceful fall-back to the
//! original mechanical `ReducedReview` so synthesis can NEVER fail the whole review.
//!
//! Test: `synthesis_tests.rs`.

use std::sync::Arc;

use serde::Deserialize;
use tracing::{debug, info, warn};

use crate::{
    config::mapreduce::MapReduceConfig,
    llm::{ChatMessage, LlmError, LlmProvider, LlmRequest},
    models::{Effort, Finding, Verdict},
    pipeline::{letter_grade::Grade, mapreduce::map::MapContext},
};

use super::outcome::ReducedReview;

// ─── Synthesis LLM request constants ─────────────────────────────────────────

/// Temperature for the synthesis LLM call — determinism is preferred.
///
/// Why: synthesis must not introduce randomness; a low temperature keeps the
/// holistic judgment stable across runs with identical inputs.
/// What: 0.2 — slightly lower than the per-file reviewer temperature (0.3) so
/// the synthesis model leans toward its most confident assessment.
/// Test: covered transitively by synthesis integration tests.
const SYNTHESIS_TEMPERATURE: f32 = 0.2;

/// Max tokens for the synthesis response — only verdict, grade, summary needed.
///
/// Why: the synthesis call only emits a small JSON object (no finding list — those
/// come from the mechanical reduce); a low ceiling keeps costs down and prevents
/// the model from wandering into prose.
/// What: 512 tokens is comfortably above the expected ~50-token JSON output.
/// Test: covered transitively.
const SYNTHESIS_MAX_TOKENS: u32 = 512;

// ─── Wire type for synthesis response ─────────────────────────────────────────

/// Deserialized synthesis response from the LLM.
///
/// Why: only three fields are needed — the mechanical reduce already produced the
/// authoritative finding list; synthesis only contributes a calibrated verdict,
/// a holistic grade, and a prose summary.
/// What: serde-deserialized from the synthesis LLM response JSON.
/// Test: `synthesis_response_parses_approve_json` in synthesis_tests.rs.
#[derive(Debug, Deserialize)]
struct SynthesisResponse {
    /// Calibrated holistic verdict: one of APPROVE, APPROVE*, REQUEST_CHANGES, BLOCK.
    verdict: String,
    /// Holistic letter grade (A+ through F).
    #[serde(default)]
    grade: String,
    /// Prose summary of the PR quality as a whole.
    #[serde(default)]
    summary: String,
}

// ─── Public synthesis entry point ─────────────────────────────────────────────

/// Run the optional LLM synthesis pass after mechanical reduce.
///
/// Why: the mechanical reduce path applies a count-based `≥2 Medium →
/// REQUEST_CHANGES` floor that fires on minor isolated nits and over-strictens the
/// verdict compared to a single reviewer reading the whole PR.  The synthesis pass
/// lets an LLM judge the PR holistically and optionally forgive nits.  The
/// High-severity safety floor is re-applied so critical issues cannot be softened.
/// What:
///   1. If `!config.synthesis` → returns `reduced` unchanged (legacy path preserved).
///   2. Builds a synthesis prompt from PR metadata + deduped finding list.
///   3. Calls the LLM; on any error, logs a warning and returns `reduced` unchanged.
///   4. Parses the JSON response (verdict, grade, summary).
///   5. Applies `apply_high_severity_floor_only` (High-severity floor ONLY —
///      deliberately omitting the count-based Medium floor, which is the over-strictness
///      source).
///   6. Returns a new `ReducedReview` with the synthesis verdict/grade/summary.
///
/// Test: `synthesis_tests.rs` — `synthesis_softens_minor_nits`,
///   `synthesis_high_severity_still_floors`, `synthesis_false_returns_unchanged`,
///   `synthesis_llm_error_falls_back`, `synthesis_grade_and_summary_flow_through`.
pub async fn synthesize_review(
    reduced: ReducedReview,
    llm: &Arc<dyn LlmProvider>,
    ctx: &MapContext<'_>,
    config: &MapReduceConfig,
) -> ReducedReview {
    // Gate: synthesis disabled → return unchanged (legacy behaviour preserved).
    if !config.synthesis {
        debug!("synthesis disabled via config.synthesis=false — returning mechanical reduce");
        return reduced;
    }

    // Build the synthesis prompt from PR metadata and the deduped finding list.
    let prompt = build_synthesis_prompt(ctx, &reduced);

    let req = LlmRequest {
        model: ctx.reviewer_model.to_string(),
        system: SYNTHESIS_SYSTEM_PROMPT.to_string(),
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: prompt,
        }],
        temperature: SYNTHESIS_TEMPERATURE,
        max_tokens: SYNTHESIS_MAX_TOKENS,
        response_schema: None,
    };

    // Call the LLM; on any error fall back gracefully to the mechanical result.
    let resp = match llm.complete(req).await {
        Ok(r) => r,
        Err(e) => {
            warn!(
                error = %e,
                "synthesis LLM call failed — falling back to mechanical reduce (fail-safe)"
            );
            return reduced;
        }
    };

    // Parse the synthesis response.
    let synthesis = match parse_synthesis_response(&resp.text) {
        Some(s) => s,
        None => {
            warn!(
                body_len = resp.text.len(),
                "synthesis response could not be parsed — falling back to mechanical reduce"
            );
            return reduced;
        }
    };

    // Apply the High-severity safety floor (omitting the count-based Medium floor).
    let floored_verdict =
        apply_high_severity_floor_only(synthesis.synthesized_verdict, &reduced.findings);

    // Parse the synthesized grade string; fall back to the floor-derived default.
    let grade_str = if synthesis.grade.is_empty() {
        grade_for_verdict(&floored_verdict).to_string()
    } else {
        synthesis.grade.clone()
    };

    let summary = synthesis.summary.clone();

    info!(
        mechanical_verdict = %reduced.verdict,
        synthesis_verdict = %floored_verdict,
        grade = %grade_str,
        "synthesis pass complete"
    );

    ReducedReview {
        verdict: floored_verdict,
        findings: reduced.findings,
        stats: reduced.stats,
        grade: Some(grade_str),
        summary,
    }
}

// ─── Parsed synthesis output ──────────────────────────────────────────────────

/// Internal parsed output of a synthesis response.
///
/// Why: captures the raw string fields from the JSON so they can be processed
/// (verdict string → enum, grade kept as-is for the caller) before being folded
/// into the updated `ReducedReview`.
/// What: holds the parsed `Verdict` and the raw `grade`/`summary` strings.
/// Test: covered by `synthesis_response_parses_approve_json`.
struct ParsedSynthesis {
    /// Parsed verdict (Unknown used as a sentinel for "unrecognised").
    synthesized_verdict: Verdict,
    /// Raw grade string (e.g. "B+"), may be empty.
    grade: String,
    /// Prose summary, may be empty.
    summary: String,
}

// ─── High-severity-only floor ─────────────────────────────────────────────────

/// Apply ONLY the High-severity floor after synthesis (closes #1663).
///
/// Why: the synthesis LLM has holistically judged the PR; re-applying the full
/// `derive_verdict` (which includes the count-based `≥2 Medium →
/// REQUEST_CHANGES` floor) would undo the calibration the synthesis was designed
/// to provide.  The one non-negotiable floor is: ANY non-refuted High-severity
/// finding must still floor the verdict to at least BLOCK.  Critical findings
/// cannot be forgiven by synthesis — that would be a safety hole.
/// What: if any finding in `findings` is `Effort::High` AND not refuted →
/// returns the stricter of (`synthesized_verdict`, BLOCK).  Otherwise returns
/// `synthesized_verdict` unchanged.  This deliberately does NOT apply the
/// count-based `≥2 Medium → REQUEST_CHANGES` floor (that floor is the
/// over-strictness source the synthesis is calibrating away).
/// Test: `synthesis_high_severity_still_floors` in synthesis_tests.rs.
pub(crate) fn apply_high_severity_floor_only(
    synthesized: Verdict,
    findings: &[Finding],
) -> Verdict {
    use crate::models::VerifyOutcome;

    // A refuted finding is disproven — never counts toward any floor.
    let has_unrefuted_high = findings.iter().any(|f| {
        f.effort == Effort::High
            && !matches!(
                f.verified,
                Some(VerifyOutcome::Refuted)
                    | Some(VerifyOutcome::ErrorRefuted { .. })
                    | Some(VerifyOutcome::TruncationRefuted)
            )
    });

    if has_unrefuted_high {
        // BLOCK is the minimum for a critical finding — take the stricter of
        // the synthesized verdict and BLOCK.
        if synthesized.ordinal() < Verdict::Block.ordinal() {
            debug!(
                synthesis_verdict = %synthesized,
                "High-severity safety floor: synthesis verdict upgraded to BLOCK"
            );
            return Verdict::Block;
        }
    }
    synthesized
}

// ─── Prompt builders ──────────────────────────────────────────────────────────

/// Build the synthesis user-message prompt.
///
/// Why: the synthesis prompt must give the LLM everything it needs to make a
/// holistic judgment: PR metadata, the deduped finding list (file, line, kind,
/// severity, confidence, description, consequence), and the per-chunk verdicts
/// already produced.
/// What: produces a string with PR title/description, the finding list (rendered
/// as a numbered table), and explicit instructions to judge holistically and
/// forgive isolated minor nits that don't matter to the overall change.
/// Test: covered transitively by synthesis integration tests.
fn build_synthesis_prompt(ctx: &MapContext<'_>, reduced: &ReducedReview) -> String {
    let pr_title = &ctx.pr_meta.title;
    let pr_body = &ctx.pr_meta.body;
    let mechanical_verdict = &reduced.verdict;

    // Build the finding table.
    let finding_lines: Vec<String> = reduced
        .findings
        .iter()
        .enumerate()
        .map(|(i, f)| {
            format!(
                "{}. [{}] {} (file: {}, line: {}, confidence: {:.0}%, effort: {:?}) — {}{}",
                i + 1,
                f.kind,
                f.description,
                f.file,
                f.line
                    .map(|l| l.to_string())
                    .unwrap_or_else(|| "n/a".to_string()),
                f.confidence * 100.0,
                f.effort,
                f.suggestion,
                if f.consequence.is_empty() {
                    String::new()
                } else {
                    format!(" | consequence: {}", f.consequence)
                },
            )
        })
        .collect();

    let findings_section = if finding_lines.is_empty() {
        "No findings surfaced.".to_string()
    } else {
        finding_lines.join("\n")
    };

    format!(
        "## PR under review\n\
         **Title:** {pr_title}\n\
         **Description:** {pr_body}\n\n\
         ## Mechanical aggregate verdict (from per-file reviews)\n\
         {mechanical_verdict}\n\n\
         ## Deduped findings (from per-file reviews)\n\
         {findings_section}\n\n\
         ## Your task\n\
         The findings above were gathered in per-file isolation. As a holistic \
         reviewer reading the ENTIRE PR as a coherent whole:\n\
         1. Decide a calibrated verdict for the PR overall.\n\
         2. You MAY forgive minor/isolated nits (Low or Medium effort) that do not \
         materially affect the overall change quality — especially if they are \
         concentrated in unrelated files or are stylistic.\n\
         3. You MUST NOT forgive High-effort (critical/high severity) findings — those \
         are enforced regardless of your verdict.\n\
         4. Assign a letter grade (A+ through F) and write a one-line prose summary \
         of your holistic judgment.\n\n\
         Respond with ONLY the JSON object below (no prose before or after):\n\
         {{\"verdict\":\"APPROVE|APPROVE*|REQUEST_CHANGES|BLOCK\",\
         \"grade\":\"A+|A|A-|B+|B|B-|C+|C|C-|D+|D|D-|F\",\
         \"summary\":\"<one-line summary>\"}}"
    )
}

/// System prompt for the synthesis LLM call.
///
/// Why: a compact system prompt focuses the synthesis model on its one job —
/// holistic calibration — without the full reviewer policy.
/// What: instructs the model to act as a calibration reviewer, respond ONLY with
/// the small JSON object, and not emit findings (the mechanical reduce already did).
/// Test: covered transitively.
const SYNTHESIS_SYSTEM_PROMPT: &str = "\
You are a calibration reviewer. You receive a list of findings produced by \
per-file code reviewers and a mechanical aggregate verdict. Your SOLE job is to \
decide whether the overall PR deserves a holistic, calibrated verdict that may be \
LESS strict than the mechanical aggregate, because per-file isolation sometimes \
surfaces minor nits that are irrelevant when viewing the PR as a whole.\n\
\n\
Rules:\n\
- Respond ONLY with a JSON object: {\"verdict\": ..., \"grade\": ..., \"summary\": ...}.\n\
- Do NOT add prose, explanations, or any other content outside the JSON object.\n\
- Do NOT add findings — the finding list is already determined.\n\
- Verdict MUST be exactly one of: APPROVE, APPROVE*, REQUEST_CHANGES, BLOCK.\n\
- Grade MUST be exactly one of: A+, A, A-, B+, B, B-, C+, C, C-, D+, D, D-, F.\n\
- Summary MUST be a single sentence (no newlines).\n\
- High-effort (critical/high severity) findings will be enforced regardless of \
your verdict, so do not waste reasoning on them — focus on whether the Low/Medium \
findings justify a stricter verdict than APPROVE.";

// ─── Response parser ──────────────────────────────────────────────────────────

/// Parse the synthesis LLM response into a `ParsedSynthesis`.
///
/// Why: the synthesis response is a small JSON object; reusing the full parser
/// (`parse_review_response`) would pull in unneeded verdict-keyword fallbacks.
/// A targeted parser here is simpler and more legible.
/// What: tries to deserialise the full response body as a `SynthesisResponse`;
/// on failure, tries to find the first `{...}` JSON object in the body (in case
/// the model emitted a small prose preamble).  Returns `None` on parse failure
/// so the caller can fall back gracefully.
/// Test: `synthesis_response_parses_approve_json`, `synthesis_response_extracts_embedded_json`.
fn parse_synthesis_response(body: &str) -> Option<ParsedSynthesis> {
    let trimmed = body.trim();

    // Strategy 1: the full body is the JSON object (expected with structured output).
    if let Some(s) = try_parse_synthesis_json(trimmed) {
        return Some(s);
    }

    // Strategy 2: extract the first `{...}` object from the body (prose preamble guard).
    let embedded = trimmed
        .find('{')
        .and_then(|start| trimmed.rfind('}').map(|end| (start, end)))
        .and_then(|(start, end)| {
            if end > start {
                try_parse_synthesis_json(&trimmed[start..=end])
            } else {
                None
            }
        });
    if embedded.is_some() {
        return embedded;
    }

    None
}

/// Try to deserialise `s` as a `SynthesisResponse`.
///
/// Why: extracted so both strategies in `parse_synthesis_response` can reuse it.
/// What: calls `serde_json::from_str` and maps to `ParsedSynthesis` on success.
/// Test: covered transitively by `parse_synthesis_response` tests.
fn try_parse_synthesis_json(s: &str) -> Option<ParsedSynthesis> {
    let raw: SynthesisResponse = serde_json::from_str(s).ok()?;
    let synthesized_verdict = parse_verdict_str(&raw.verdict)?;
    Some(ParsedSynthesis {
        synthesized_verdict,
        grade: raw.grade,
        summary: raw.summary,
    })
}

/// Parse a verdict string to a `Verdict`, returning `None` for unrecognised tokens.
///
/// Why: synthesis must not silently default to APPROVE when the model returns an
/// unrecognised verdict string — `None` triggers the fall-back path.
/// What: maps the canonical verdict tokens (case-sensitive) to `Verdict`; returns
/// `None` for anything else.
/// Test: covered transitively by `synthesis_response_parses_approve_json`.
fn parse_verdict_str(s: &str) -> Option<Verdict> {
    match s.trim() {
        "APPROVE" => Some(Verdict::Approve),
        "APPROVE*" => Some(Verdict::ApproveWithReservations),
        "REQUEST_CHANGES" => Some(Verdict::RequestChanges),
        "BLOCK" => Some(Verdict::Block),
        "UNKNOWN" => Some(Verdict::Unknown),
        other => {
            warn!(verdict = other, "synthesis: unrecognised verdict token");
            None
        }
    }
}

// ─── Grade helper ─────────────────────────────────────────────────────────────

/// Return a default grade string for a verdict when synthesis omits the grade.
///
/// Why: synthesis may return a verdict without a grade; the runner needs a grade
/// to populate `result.grade`.  This maps the verdict to a reasonable default.
/// What: APPROVE → "B", APPROVE* → "B-", REQUEST_CHANGES → "D+", BLOCK → "F",
/// UNKNOWN → "F".
/// Test: covered transitively by `synthesis_grade_and_summary_flow_through`.
fn grade_for_verdict(v: &Verdict) -> Grade {
    use crate::pipeline::letter_grade::default_grade_for_verdict;
    default_grade_for_verdict(v)
}

// ─── Synthesis error type ─────────────────────────────────────────────────────

/// Synthesis-stage error for tracing (never propagated — all paths fall back).
///
/// Why: synthesis errors must be observable but never fatal; using a typed enum
/// keeps the `warn!` calls structured and avoids string formatting at call sites.
/// What: two variants — LLM transport error and JSON parse error.  Both cause
/// fall-back to the mechanical `ReducedReview`.
/// Test: `synthesis_llm_error_falls_back`, `synthesis_parse_error_falls_back`.
#[allow(dead_code)]
enum SynthesisError {
    /// LLM call returned an error.
    Llm(LlmError),
    /// Response could not be parsed as a valid synthesis JSON.
    Parse(String),
}

#[cfg(test)]
#[path = "synthesis_tests.rs"]
mod tests;
