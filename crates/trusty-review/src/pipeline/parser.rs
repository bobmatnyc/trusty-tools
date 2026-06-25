//! Verdict and findings parser for LLM review responses.
//!
//! Why: structured output (via `response_schema` forced output) makes the
//! LLM return a clean JSON object directly, eliminating the fail-safe APPROVE
//! problem.  Free-text parsing is retained as a fallback for transport errors
//! and for callers that do not use forced structured output.
//!
//! What: exposes `parse_review_response` which tries three strategies in order:
//!
//!  1. Direct JSON parse — tries `serde_json::from_str` on the full body.
//!     This succeeds when forced structured output is active (Bedrock tool-use
//!     or OpenRouter json_schema) and the response IS the JSON object.
//!  2. JSON-block extraction — looks for a ```json ... ``` fenced block at the
//!     end of the response and deserialises it (legacy free-text path).
//!  3. Verdict-keyword scan — scans the last 20% of the body for one of the
//!     known board grade tokens (BLOCK, REQUEST_CHANGES, APPROVE*, APPROVE,
//!     UNKNOWN) per spec REV-112.
//!
//! If ALL THREE strategies fail (e.g. a genuine LLM/transport error produced
//! empty or unparseable output), the function returns a fail-safe `ParsedReview`
//! with `verdict = UNKNOWN` and an empty findings list.
//!
//! ## Fail-CLOSED posture (#1241 — supersedes spec REV-130)
//! Spec REV-130 originally specified a fail-OPEN APPROVE here: any parse/LLM
//! failure would silently APPROVE so a pipeline failure never blocked a merge.
//! Ticket #1241 supersedes that decision (ticket > spec precedence): a silent
//! APPROVE on unparseable or truncated model output is a *safety hole* — it
//! posts a green GitHub check for a review that never actually happened.  The
//! fail-safe is now fail-CLOSED: `verdict = UNKNOWN`, which surfaces a clear
//! "could not review" state and never posts a green merge-approval.  See
//! `docs/specs/` REV-130 (marked SUPERSEDED) for the rationale.
//!
//! Test: `parse_direct_json_happy_path`, `parse_json_block_happy_path`,
//! `parse_verdict_keyword_fallback`, `parse_fail_safe_unknown_on_empty_response`,
//! `parse_fail_safe_unknown_on_malformed_json`.

use serde::Deserialize;
use tracing::{debug, warn};

use crate::models::{Effort, Finding, FindingCategory, Verdict};

// ─── Wire types (JSON block deserialization) ──────────────────────────────────

/// Deserialized JSON output block from the LLM reviewer.
///
/// Why: the LLM is instructed to end its response with this JSON block; we
/// deserialise it directly for structured extraction.
/// What: mirrors the output schema in `prompt::reviewer_system_prompt`.
/// Unknown fields are ignored for forward-compatibility.  The `grade` field is
/// new in 0.3.4 (#732); it is optional with `serde(default)` so old responses
/// without it still parse cleanly.
/// Test: `parse_json_block_happy_path`.
#[derive(Debug, Deserialize)]
struct LlmOutputBlock {
    verdict: String,
    #[serde(default)]
    grade: String,
    #[serde(default)]
    #[allow(dead_code)] // Deserialized for schema compliance; not used programmatically.
    grade_justification: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    findings: Vec<LlmFinding>,
}

/// A single finding from the LLM JSON output block.
///
/// Why: the LLM emits findings as structured JSON; we convert them to the
/// internal `Finding` type.
/// What: mirrors the finding schema in the system prompt.  All fields except
/// `title` and `body` are optional and default gracefully.  `category` is new in
/// #1359 (back gate); it is `#[serde(default)]` (→ `Correctness`) so responses
/// from models that do not emit it — and every pre-#1359 fixture — still parse.
/// Test: covered transitively by `parse_json_block_happy_path` and
/// `parse_method_conformance_finding_category` in `parser_tests`.
#[derive(Debug, Deserialize)]
struct LlmFinding {
    title: String,
    body: String,
    #[serde(default)]
    severity: String,
    #[serde(default)]
    confidence: f32,
    #[serde(default)]
    file: String,
    #[serde(default)]
    line: Option<u32>,
    /// Finding axis: `"correctness"` (default) or `"method-conformance"` (#1359).
    #[serde(default)]
    category: FindingCategory,
    /// Brief failure consequence — what goes wrong if unaddressed (#1416).
    ///
    /// `#[serde(default)]` → `""` so models that omit it — and every pre-#1416
    /// fixture — still parse.
    #[serde(default)]
    consequence: String,
    /// Exact replacement code for a committable GitHub `suggestion` block (#1415).
    ///
    /// `#[serde(default)]` → `None` so models that omit it — and every pre-#1415
    /// fixture — still parse.
    #[serde(default)]
    suggested_replacement: Option<String>,
    /// Exact spec/ticket/test-plan source grounding this finding (#1419).
    ///
    /// `#[serde(default)]` → `None` so models that omit it — and every pre-#1419
    /// fixture — still parse.
    #[serde(default)]
    source_citation: Option<String>,
}

// ─── Parsed output ────────────────────────────────────────────────────────────

/// The structured result of parsing a raw LLM review response.
///
/// Why: the pipeline receives a `ParsedReview` and populates a `ReviewResult`
/// from it; keeping the parsed form separate from the final result allows the
/// pipeline to apply confidence-threshold gates before committing the result.
/// What: contains the parsed verdict, grade, summary, and findings list, plus a
/// flag indicating whether the result was produced by the fail-safe path.
/// The `grade` is `None` when the LLM omitted or produced an unparseable grade;
/// the runner falls back to `default_grade_for_verdict` in that case.
/// Test: all parser tests assert `ParsedReview` fields.
#[derive(Debug, Clone)]
pub struct ParsedReview {
    /// Parsed or fail-safe verdict.
    pub verdict: Verdict,
    /// Letter grade from the LLM (A+ through F), or `None` if not provided.
    pub grade: Option<String>,
    /// Pre-floor synthesis letter grade (#1665 item 3) — the grade derived from
    /// the LLM's RAW synthesis verdict BEFORE the two-tier floor was applied.
    /// `None` for non-synthesis reviews or when synthesis is disabled/failed.
    /// When `grade_pre_floor != grade`, the floor changed the verdict; when equal,
    /// no flooring occurred.
    pub grade_pre_floor: Option<String>,
    /// One-line summary extracted from the JSON block, or empty string.
    pub summary: String,
    /// Parsed findings (may be empty).
    pub findings: Vec<Finding>,
    /// True if the parser failed and fell back to the fail-safe UNKNOWN default.
    pub is_fail_safe: bool,
    /// Human-readable reason for the fail-safe, if `is_fail_safe` is true.
    pub fail_safe_reason: Option<String>,
}

impl ParsedReview {
    /// Construct a fail-safe result with verdict UNKNOWN (fail-CLOSED).
    ///
    /// Why: ticket #1241 supersedes spec REV-130's fail-OPEN APPROVE.  A silent
    /// APPROVE on unparseable/truncated model output posts a green GitHub check
    /// for a review that never happened — a safety hole.  Failing CLOSED to
    /// UNKNOWN surfaces a clear "could not review" state that is never treated as
    /// a merge-approval downstream (see `post.rs` / the webhook finalize path).
    /// What: sets `verdict = Unknown`, `findings = []`, `is_fail_safe = true`.
    /// Test: `parse_fail_safe_unknown_on_empty_response`.
    pub fn fail_safe(reason: impl Into<String>) -> Self {
        Self {
            verdict: Verdict::Unknown,
            grade: None,
            grade_pre_floor: None,
            summary: String::new(),
            findings: Vec::new(),
            is_fail_safe: true,
            fail_safe_reason: Some(reason.into()),
        }
    }
}

// ─── Main parser ──────────────────────────────────────────────────────────────

/// Parse a raw LLM review response into a structured `ParsedReview`.
///
/// Why: the pipeline cannot use the raw text directly; structured data is needed
/// to drive the verdict, findings post-processing, and telemetry.
///
/// What: tries three strategies in priority order:
///   1. Direct JSON parse — succeeds when forced structured output (Bedrock
///      tool-use / OpenRouter json_schema) is active; body IS the clean JSON.
///   2. JSON-block extraction — legacy free-text path with fenced JSON block.
///   3. Verdict-keyword scan — last-resort spec REV-112 fallback.
///
/// If ALL THREE fail, returns fail-safe UNKNOWN (fail-CLOSED; genuine error path
/// only — not expected in normal operation with structured output enforced).
/// Ticket #1241 supersedes spec REV-130: the fail-safe is UNKNOWN, not APPROVE.
///
/// Test: `parse_direct_json_happy_path`, `parse_json_block_happy_path`,
/// `parse_verdict_keyword_fallback`, `parse_fail_safe_unknown_on_empty_response`.
pub fn parse_review_response(body: &str) -> ParsedReview {
    if body.trim().is_empty() {
        warn!("LLM returned empty response — applying fail-safe UNKNOWN (fail-closed, #1241)");
        return ParsedReview::fail_safe("empty LLM response");
    }

    // Strategy 1: direct JSON parse (structured output path).
    // When response_schema is used, the provider returns only the JSON object.
    if let Some(parsed) = try_parse_direct_json(body) {
        debug!(verdict = ?parsed.verdict, findings = parsed.findings.len(), "parsed via direct JSON (structured output)");
        return parsed;
    }

    // Strategy 2: JSON block (legacy free-text path).
    if let Some(parsed) = try_parse_json_block(body) {
        debug!(verdict = ?parsed.verdict, findings = parsed.findings.len(), "parsed via JSON block");
        return parsed;
    }

    // Strategy 3: Verdict keyword scan in the last 20% of the body.
    if let Some(verdict) = scan_verdict_keyword(body) {
        warn!(
            ?verdict,
            "JSON parse failed — fell back to verdict keyword scan (spec REV-112)"
        );
        return ParsedReview {
            verdict,
            grade: None,
            grade_pre_floor: None,
            summary: String::new(),
            findings: Vec::new(),
            is_fail_safe: false,
            fail_safe_reason: None,
        };
    }

    // All three strategies failed — genuine error, not a parse failure.
    warn!(
        body_len = body.len(),
        "failed to parse verdict from LLM response — applying fail-safe UNKNOWN \
         (fail-closed; #1241 supersedes spec REV-130)"
    );
    ParsedReview::fail_safe("no parseable verdict in LLM response")
}

// ─── Strategy 1: Direct JSON parse (structured output) ───────────────────────

/// Try to deserialize the entire response body as a `LlmOutputBlock`.
///
/// Why: when forced structured output is active (Bedrock tool-use / OpenRouter
/// json_schema), the provider guarantees `LlmResponse.text` contains only the
/// clean JSON object — no fence, no surrounding prose.  Parsing it directly
/// avoids the fragile fence-stripping logic entirely.
/// What: trims whitespace and calls `serde_json::from_str` on the full body.
/// Returns `None` if the body is not a valid `LlmOutputBlock` JSON object
/// (falls through to the fence-based strategy).
/// Test: `parse_direct_json_happy_path`,
/// `parse_direct_json_request_changes_with_findings`.
fn try_parse_direct_json(body: &str) -> Option<ParsedReview> {
    let trimmed = body.trim();
    // Only attempt if it looks like a JSON object (starts with '{').
    if !trimmed.starts_with('{') {
        return None;
    }
    let block: LlmOutputBlock = serde_json::from_str(trimmed).ok()?;
    // Fail-CLOSED (#1241): an unrecognised verdict token inside otherwise-valid
    // JSON must NOT silently default to APPROVE — surface UNKNOWN instead.
    let verdict = parse_verdict_string(&block.verdict).unwrap_or(Verdict::Unknown);
    let grade = extract_grade_field(&block.grade);
    let findings = block
        .findings
        .into_iter()
        .map(convert_llm_finding)
        .collect();
    Some(ParsedReview {
        verdict,
        grade,
        grade_pre_floor: None,
        summary: block.summary,
        findings,
        is_fail_safe: false,
        fail_safe_reason: None,
    })
}

// ─── Strategy 2: JSON block (legacy free-text) ────────────────────────────────

/// Try to extract and deserialize the trailing ```json ... ``` block.
///
/// Why: the structured output format is the preferred extraction path; it
/// provides the full findings list with confidence scores.
/// What: scans for the last occurrence of ```json ... ``` in the response;
/// if found, deserialises the JSON and converts findings to the internal type.
/// Returns `None` if no valid JSON block is found.
/// Test: `parse_json_block_happy_path`, `parse_json_block_handles_fence_variants`.
fn try_parse_json_block(body: &str) -> Option<ParsedReview> {
    // Find the last ```json fence.
    let fence_start = body.rfind("```json")?;
    let after_fence = &body[fence_start + 7..]; // skip ```json

    // Find the closing fence.
    let fence_end = after_fence.find("```")?;
    let json_text = after_fence[..fence_end].trim();

    let block: LlmOutputBlock = match serde_json::from_str(json_text) {
        Ok(b) => b,
        Err(e) => {
            debug!("JSON block parse error: {e}");
            return None;
        }
    };

    // Fail-CLOSED (#1241): unrecognised verdict token → UNKNOWN, never APPROVE.
    let verdict = parse_verdict_string(&block.verdict).unwrap_or(Verdict::Unknown);
    let grade = extract_grade_field(&block.grade);
    let findings = block
        .findings
        .into_iter()
        .map(convert_llm_finding)
        .collect();

    Some(ParsedReview {
        verdict,
        grade,
        grade_pre_floor: None,
        summary: block.summary,
        findings,
        is_fail_safe: false,
        fail_safe_reason: None,
    })
}

/// Convert an `LlmFinding` wire type to the internal `Finding` type.
///
/// Why: `Finding::new` clamps confidence and normalises effort; the LLM may
/// produce out-of-range values or unknown effort strings.
/// What: maps severity → effort (high/critical → High; medium → Medium; else Low);
/// uses the `title` as the `kind` and `body` as `description`; preserves the
/// finding `category` (#1359 — defaulting to `Correctness` when the model omits
/// it) so the verdict floor can cap a `method-conformance` finding; carries
/// `source_citation` (#1419) when the model provides it.
/// Test: covered transitively by `parse_json_block_happy_path`,
/// `parse_method_conformance_finding_category`, and
/// `parse_finding_carries_source_citation`.
fn convert_llm_finding(f: LlmFinding) -> Finding {
    let effort = match f.severity.to_lowercase().as_str() {
        "high" | "critical" => Effort::High,
        "medium" => Effort::Medium,
        _ => Effort::Low,
    };
    let file = if f.file.is_empty() {
        "unknown".to_string()
    } else {
        f.file
    };
    let category = f.category;
    let line = f.line;
    let mut finding = Finding::new(file, f.title, f.body, String::new(), f.confidence, effort)
        .with_category(category);
    finding.line = line;
    // Carry the failure consequence through for the inline comment (#1416).
    finding.consequence = f.consequence;
    // Carry the committable replacement code through for a GitHub suggestion
    // block (#1415); normalise empty/whitespace-only strings to None.
    finding.suggested_replacement = f.suggested_replacement.filter(|s| !s.trim().is_empty());
    // Carry the spec/ticket source citation through (#1419); normalise
    // empty/whitespace-only strings to None.
    finding.source_citation = f.source_citation.filter(|s| !s.trim().is_empty());
    finding
}

// ─── Strategy 3: Verdict keyword scan ────────────────────────────────────────

/// Scan the last 20% of the body for a verdict keyword (spec REV-112).
///
/// Why: when the LLM ignores the JSON output format, the verdict is often still
/// present as a plain token at or near the end of the response.
/// What: searches the last 20% of `body` (minimum 200 chars) for the verdict
/// tokens in priority order (BLOCK > REQUEST_CHANGES > APPROVE* > APPROVE > UNKNOWN).
/// Returns `None` if no token is found.
/// Test: `parse_verdict_keyword_fallback`, `scan_verdict_keyword_detects_unknown`.
fn scan_verdict_keyword(body: &str) -> Option<Verdict> {
    let scan_start = body.len().saturating_sub((body.len() / 5).max(200));
    let tail = &body[scan_start..];

    // Priority order: most severe first so "BLOCK" beats "APPROVE" if both appear.
    // APPROVE* must be checked before APPROVE so the star variant wins.
    if tail.contains("BLOCK") {
        return Some(Verdict::Block);
    }
    if tail.contains("REQUEST_CHANGES") {
        return Some(Verdict::RequestChanges);
    }
    if tail.contains("APPROVE*") {
        return Some(Verdict::ApproveWithReservations);
    }
    if tail.contains("APPROVE") {
        return Some(Verdict::Approve);
    }
    if tail.contains("UNKNOWN") {
        return Some(Verdict::Unknown);
    }
    None
}

// ─── Grade field extraction ───────────────────────────────────────────────────

/// Extract and validate the grade field from the LLM output block.
///
/// Why: the LLM may omit the grade, emit an empty string, or produce an
/// invalid value.  The pipeline must degrade gracefully — an unparseable grade
/// never panics; it returns `None` and the runner falls back to
/// `default_grade_for_verdict`.
/// What: trims whitespace; if empty → `None`; validates against the 13 known
/// grade strings ("A+", "A", … "F"); invalid strings produce a warning and
/// return `None`.
/// Test: covered transitively by `parse_direct_json_with_grade`.
fn extract_grade_field(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Validate against the 13 canonical grade strings.
    const VALID_GRADES: &[&str] = &[
        "A+", "A", "A-", "B+", "B", "B-", "C+", "C", "C-", "D+", "D", "D-", "F",
    ];
    if VALID_GRADES.contains(&trimmed) {
        Some(trimmed.to_string())
    } else {
        warn!(
            grade = trimmed,
            "LLM returned unrecognised grade — ignoring (will use default)"
        );
        None
    }
}

// ─── Verdict string normalization ─────────────────────────────────────────────

/// Parse a verdict string from the JSON block into a `Verdict`.
///
/// Why: the LLM may emit slightly varied case or include extra whitespace.
/// What: normalises to uppercase and matches against the five board grade
/// tokens; returns `None` for unrecognised strings (caller applies fail-safe).
/// Test: `parse_verdict_string_normalization`.
fn parse_verdict_string(s: &str) -> Option<Verdict> {
    match s.trim().to_uppercase().as_str() {
        "APPROVE" => Some(Verdict::Approve),
        "APPROVE*" => Some(Verdict::ApproveWithReservations),
        "REQUEST_CHANGES" | "REQUEST CHANGES" => Some(Verdict::RequestChanges),
        "BLOCK" => Some(Verdict::Block),
        "UNKNOWN" => Some(Verdict::Unknown),
        _ => None,
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

// ─── Unit tests ─────────────────────────────────────────────────────────────
// Tests extracted to parser_tests.rs to keep this file under the 500-line cap.

#[cfg(test)]
#[path = "parser_tests.rs"]
mod tests;
