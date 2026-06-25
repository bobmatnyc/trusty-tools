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
//! response.  The two-tier safety floor is re-applied after synthesis (#1665):
//! any non-refuted `Effort::High` finding floors the verdict to at least BLOCK;
//! failing that, a mechanical BLOCK floors the synthesis verdict to at least
//! REQUEST_CHANGES (so synthesis cannot de-escalate BLOCK to APPROVE/APPROVE*).
//! The count-based Medium floor is deliberately omitted — the LLM has already
//! holistically judged those findings.  Any synthesis error causes a graceful
//! fall-back to the original mechanical `ReducedReview` so synthesis can NEVER
//! fail the whole review.
//!
//! Test: `synthesis_tests.rs`.

use std::sync::Arc;

use serde::Deserialize;
use tracing::{debug, info, warn};

use crate::{
    config::mapreduce::MapReduceConfig,
    llm::{ChatMessage, LlmProvider, LlmRequest},
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
/// lets an LLM judge the PR holistically and optionally forgive nits.  A two-tier
/// safety floor (#1665) is re-applied so critical issues cannot be softened past
/// defined lower bounds.
/// What:
///   1. If `!config.synthesis` → returns `reduced` unchanged (legacy path preserved).
///   2. Captures `mechanical_verdict = reduced.verdict` BEFORE any modification.
///   3. Builds a synthesis prompt from PR metadata + deduped finding list.
///   4. Calls the LLM; on any error, logs a warning and returns `reduced` unchanged.
///   5. Parses the JSON response (verdict, grade, summary).
///   6. Applies `apply_synthesis_floor` with the two-tier policy (#1665).
///      Tier 1: any unrefuted `Effort::High` finding → floor to BLOCK.
///      Tier 2: else if `mechanical_verdict == Block` → floor to REQUEST_CHANGES
///      (synthesis may de-escalate BLOCK→REQUEST_CHANGES but NEVER →APPROVE).
///      Tier 3: else no floor; synthesis verdict stands.
///      The count-based Medium floor is deliberately omitted (it is the
///      over-strictness source the synthesis pass is calibrating away).
///   7. Returns a new `ReducedReview` with synthesis verdict/grade/summary plus
///      `grade_pre_floor` for observable telemetry (#1665 item 3).
///
/// Test: `synthesis_tests.rs` — covers `synthesis_softens_minor_nits`,
/// `synthesis_high_severity_still_floors`, `synthesis_false_returns_unchanged`,
/// `synthesis_llm_error_falls_back`, `synthesis_grade_and_summary_flow_through`,
/// `synthesis_block_without_high_finding_floors_to_request_changes`,
/// `synthesis_mechanical_rc_allows_full_softening`,
/// `synthesis_floor_telemetry_differs_when_floored`,
/// `synthesis_floor_telemetry_equal_when_no_floor`.
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

    // Capture the mechanical verdict BEFORE any modification — needed by the
    // two-tier floor (#1665 item 1) and for telemetry logging.
    let mechanical_verdict = reduced.verdict.clone();

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

    // Capture the raw (pre-floor) synthesized verdict so we can compute the
    // pre-floor grade independently from the post-floor grade (#1665 item 3).
    let raw_verdict = synthesis.synthesized_verdict.clone();

    // Apply the two-tier synthesis floor (#1665 item 1):
    //   Tier 1: any unrefuted High finding → floor to BLOCK.
    //   Tier 2: else mechanical_verdict was BLOCK → floor to REQUEST_CHANGES.
    //   Tier 3: else no floor; synthesis verdict stands (may soften freely).
    let floored_verdict = apply_synthesis_floor(
        synthesis.synthesized_verdict,
        &reduced.findings,
        &mechanical_verdict,
    );

    // Pre-floor grade: clamp the LLM grade to the RAW (pre-floor) verdict for
    // observable telemetry (#1665 item 3).  When the floor changes the verdict,
    // pre_floor_grade_str differs from grade_str below; when no floor fires
    // (raw_verdict == floored_verdict), they are identical.
    let pre_floor_grade_str = synthesis
        .grade
        .parse::<Grade>()
        .ok()
        .map(|g| {
            use crate::pipeline::letter_grade::clamp_grade_to_verdict;
            clamp_grade_to_verdict(g, &raw_verdict).to_string()
        })
        .unwrap_or_else(|| grade_for_verdict(&raw_verdict).to_string());

    // Post-floor grade: clamp the LLM grade to the FLOORED verdict so invalid
    // combinations (e.g. "A" on a floored BLOCK) are corrected.
    let grade_str = synthesis
        .grade
        .parse::<Grade>()
        .ok()
        .map(|g| {
            use crate::pipeline::letter_grade::clamp_grade_to_verdict;
            clamp_grade_to_verdict(g, &floored_verdict).to_string()
        })
        .unwrap_or_else(|| grade_for_verdict(&floored_verdict).to_string());

    let summary = synthesis.summary.clone();

    info!(
        mechanical_verdict = %mechanical_verdict,
        synthesis_verdict = %floored_verdict,
        grade = %grade_str,
        "synthesis pass complete"
    );

    ReducedReview {
        verdict: floored_verdict,
        findings: reduced.findings,
        stats: reduced.stats,
        grade: Some(grade_str),
        grade_pre_floor: Some(pre_floor_grade_str),
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

// ─── Two-tier synthesis floor ─────────────────────────────────────────────────

/// Apply the two-tier synthesis floor after the synthesis LLM call (#1665).
///
/// Why: the synthesis LLM has holistically judged the PR; re-applying the full
/// `derive_verdict` (which includes the count-based `≥2 Medium →
/// REQUEST_CHANGES` floor) would undo the calibration the synthesis was designed
/// to provide.  Two non-negotiable floors remain (#1665, decided policy):
///
///   **Tier 1 — High finding → BLOCK:**
///   ANY non-refuted `Effort::High` finding must floor the verdict to at least
///   BLOCK.  Critical findings cannot be forgiven by synthesis — that would be a
///   safety hole.  This matches the unified path's `correctness_floor` semantics
///   (`Effort::High` is the only severity above Medium; there is no separate
///   `Critical` variant).
///
///   **Tier 2 — mechanical BLOCK → at least REQUEST_CHANGES:**
///   If no High finding is present but the mechanical (pre-synthesis) verdict was
///   `Verdict::Block`, synthesis may de-escalate BLOCK → REQUEST_CHANGES but MUST
///   NOT de-escalate BLOCK → APPROVE or APPROVE*.  A per-chunk BLOCK without a
///   High finding is typically driven by a Medium-count heuristic; an LLM
///   holistically judging those nits minor is permitted — but the human reviewer
///   must at minimum be informed via REQUEST_CHANGES, not silently APPROVED.
///
///   **Tier 3 — no floor:** when neither condition above applies, synthesis may
///   soften the verdict freely (e.g. REQUEST_CHANGES → APPROVE is allowed for
///   non-BLOCK mechanical verdicts with no High findings).
///
/// What: given `synthesized` (the LLM's raw output), `findings` (the deduped
/// finding set), and `mechanical_verdict` (the deterministic pre-synthesis
/// verdict captured before synthesis ran):
///   - Returns `BLOCK` when `has_unrefuted_high`.
///   - Returns `max_by_ordinal(synthesized, REQUEST_CHANGES)` when
///     `mechanical_verdict == Block` (and no High finding).
///   - Otherwise returns `synthesized` unchanged.
///
/// Test: `synthesis_high_severity_still_floors`,
/// `synthesis_block_without_high_finding_floors_to_request_changes`,
/// `synthesis_mechanical_rc_allows_full_softening` in synthesis_tests.rs.
pub(crate) fn apply_synthesis_floor(
    synthesized: Verdict,
    findings: &[Finding],
    mechanical_verdict: &Verdict,
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
        // Tier 1: BLOCK is the minimum for a critical finding — take the stricter
        // of the synthesized verdict and BLOCK.
        if synthesized.ordinal() < Verdict::Block.ordinal() {
            debug!(
                synthesis_verdict = %synthesized,
                "synthesis floor tier-1: High-severity finding — upgrading verdict to BLOCK"
            );
            return Verdict::Block;
        }
        return synthesized;
    }

    if *mechanical_verdict == Verdict::Block {
        // Tier 2: mechanical BLOCK without High finding — synthesis may de-escalate
        // BLOCK → REQUEST_CHANGES but NOT further toward APPROVE.
        if synthesized.ordinal() < Verdict::RequestChanges.ordinal() {
            debug!(
                synthesis_verdict = %synthesized,
                "synthesis floor tier-2: mechanical BLOCK (no High finding) — \
                 upgrading synthesis verdict to REQUEST_CHANGES"
            );
            return Verdict::RequestChanges;
        }
    }

    // Tier 3: no floor — return synthesized verdict unchanged.
    synthesized
}

/// Alias kept for backward-compat in existing test call-sites; delegates to
/// `apply_synthesis_floor` with an `Approve` mechanical verdict (tier-2 never
/// fires, only tier-1 applies) so the semantics of the old single-floor helper
/// are preserved exactly.
///
/// Why: avoids a noisy test diff; all call-sites that only exercise the
/// High-severity floor can keep using the name they document.
/// What: calls `apply_synthesis_floor(synthesized, findings, &Verdict::Approve)`.
/// Test: old helper unit tests in synthesis_tests.rs.
#[cfg(test)]
pub(crate) fn apply_high_severity_floor_only(
    synthesized: Verdict,
    findings: &[Finding],
) -> Verdict {
    apply_synthesis_floor(synthesized, findings, &Verdict::Approve)
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
/// Test: `synthesis_response_parses_approve_json`, `synthesis_response_extracts_embedded_json`,
///   `synthesis_embedded_json_ignores_trailing_stray_brace`.
fn parse_synthesis_response(body: &str) -> Option<ParsedSynthesis> {
    let trimmed = body.trim();

    // Strategy 1: the full body is the JSON object (expected with structured output).
    if let Some(s) = try_parse_synthesis_json(trimmed) {
        return Some(s);
    }

    // Strategy 2: brace-depth scan — find the first balanced `{...}` object from the
    // body (prose preamble guard).  Using rfind('}') is unsafe when the model emits
    // trailing stray braces after the JSON object; a depth counter finds the correct
    // closing brace of the FIRST top-level object.
    let embedded = trimmed.find('{').and_then(|start| {
        let tail = &trimmed[start..];
        let mut depth: i32 = 0;
        let mut end_pos: Option<usize> = None;
        for (i, ch) in tail.char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end_pos = Some(start + i);
                        break;
                    }
                }
                _ => {}
            }
        }
        end_pos.and_then(|end| try_parse_synthesis_json(&trimmed[start..=end]))
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
/// unrecognised verdict string — `None` triggers the fall-back path.  `UNKNOWN` is
/// explicitly excluded: allowing it to propagate would confuse downstream grade and
/// poster logic that does not expect `Verdict::Unknown` from a synthesis pass.
/// What: maps the canonical verdict tokens (case-sensitive) to `Verdict`; returns
/// `None` for anything else, including `UNKNOWN` (which triggers the graceful
/// fall-back to the mechanical result).
/// Test: covered transitively by `synthesis_response_parses_approve_json`; FIX A
/// path covered by `synthesis_unknown_verdict_falls_back`.
fn parse_verdict_str(s: &str) -> Option<Verdict> {
    match s.trim() {
        "APPROVE" => Some(Verdict::Approve),
        "APPROVE*" => Some(Verdict::ApproveWithReservations),
        "REQUEST_CHANGES" => Some(Verdict::RequestChanges),
        "BLOCK" => Some(Verdict::Block),
        "UNKNOWN" => {
            warn!(
                verdict = "UNKNOWN",
                "synthesis: model returned UNKNOWN verdict — treating as unrecognised, falling back to mechanical result"
            );
            None
        }
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

#[cfg(test)]
#[path = "synthesis_tests.rs"]
mod tests;
