//! Verdict and findings parser for LLM review responses.
//!
//! Why: the LLM response is free-form text; parsing it correctly is the most
//! brittle step in the pipeline.  A dedicated module keeps the parsing logic
//! isolated and testable independently of the pipeline runner.
//!
//! What: exposes `parse_review_response` which tries two strategies in order:
//!
//!  1. JSON-block extraction — looks for a ```json ... ``` fenced block at the
//!     end of the response and deserialises it.
//!  2. Verdict-keyword scan — scans the last 20% of the body for one of the
//!     known verdict tokens (APPROVE, REQUEST_CHANGES, BLOCK) per spec REV-112.
//!
//! If both strategies fail, the function returns a fail-safe `ParsedReview`
//! with `verdict = APPROVE` and an empty findings list (spec REV-130).
//!
//! Test: `parse_json_block_happy_path`, `parse_verdict_keyword_fallback`,
//! `parse_fail_safe_approve_on_empty_response`,
//! `parse_fail_safe_approve_on_malformed_json`.

use serde::Deserialize;
use tracing::{debug, warn};

use crate::models::{Effort, Finding, Verdict};

// ─── Wire types (JSON block deserialization) ──────────────────────────────────

/// Deserialized JSON output block from the LLM reviewer.
///
/// Why: the LLM is instructed to end its response with this JSON block; we
/// deserialise it directly for structured extraction.
/// What: mirrors the output schema in `prompt::reviewer_system_prompt`.
/// Unknown fields are ignored for forward-compatibility.
/// Test: `parse_json_block_happy_path`.
#[derive(Debug, Deserialize)]
struct LlmOutputBlock {
    verdict: String,
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
/// `title` and `body` are optional and default gracefully.
/// Test: covered transitively by `parse_json_block_happy_path`.
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
}

// ─── Parsed output ────────────────────────────────────────────────────────────

/// The structured result of parsing a raw LLM review response.
///
/// Why: the pipeline receives a `ParsedReview` and populates a `ReviewResult`
/// from it; keeping the parsed form separate from the final result allows the
/// pipeline to apply confidence-threshold gates before committing the result.
/// What: contains the parsed verdict, summary, and findings list, plus a flag
/// indicating whether the result was produced by the fail-safe path.
/// Test: all parser tests assert `ParsedReview` fields.
#[derive(Debug, Clone)]
pub struct ParsedReview {
    /// Parsed or fail-safe verdict.
    pub verdict: Verdict,
    /// One-line summary extracted from the JSON block, or empty string.
    pub summary: String,
    /// Parsed findings (may be empty).
    pub findings: Vec<Finding>,
    /// True if the parser failed and fell back to the fail-safe APPROVE default.
    pub is_fail_safe: bool,
    /// Human-readable reason for the fail-safe, if `is_fail_safe` is true.
    pub fail_safe_reason: Option<String>,
}

impl ParsedReview {
    /// Construct a fail-safe result with verdict APPROVE.
    ///
    /// Why: spec REV-130 requires the pipeline to APPROVE on any parse or LLM
    /// failure; a pipeline failure must never block a merge.
    /// What: sets `verdict = Approve`, `findings = []`, `is_fail_safe = true`.
    /// Test: `parse_fail_safe_approve_on_empty_response`.
    pub fn fail_safe(reason: impl Into<String>) -> Self {
        Self {
            verdict: Verdict::Approve,
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
/// What: tries JSON-block extraction first; falls back to verdict-keyword scan
/// (spec REV-112); if both fail, returns the fail-safe APPROVE default.
/// Test: `parse_json_block_happy_path`, `parse_verdict_keyword_fallback`,
/// `parse_fail_safe_approve_on_empty_response`.
pub fn parse_review_response(body: &str) -> ParsedReview {
    if body.trim().is_empty() {
        warn!("LLM returned empty response — applying fail-safe APPROVE");
        return ParsedReview::fail_safe("empty LLM response");
    }

    // Strategy 1: JSON block.
    if let Some(parsed) = try_parse_json_block(body) {
        debug!(verdict = ?parsed.verdict, findings = parsed.findings.len(), "parsed via JSON block");
        return parsed;
    }

    // Strategy 2: Verdict keyword scan in the last 20% of the body.
    if let Some(verdict) = scan_verdict_keyword(body) {
        warn!(
            ?verdict,
            "JSON block parse failed — fell back to verdict keyword scan (spec REV-112)"
        );
        return ParsedReview {
            verdict,
            summary: String::new(),
            findings: Vec::new(),
            is_fail_safe: false,
            fail_safe_reason: None,
        };
    }

    // Both strategies failed.
    warn!(
        body_len = body.len(),
        "failed to parse verdict from LLM response — applying fail-safe APPROVE (spec REV-130)"
    );
    ParsedReview::fail_safe("no parseable verdict in LLM response")
}

// ─── Strategy 1: JSON block ───────────────────────────────────────────────────

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

    let verdict = parse_verdict_string(&block.verdict).unwrap_or(Verdict::Approve);
    let findings = block
        .findings
        .into_iter()
        .map(convert_llm_finding)
        .collect();

    Some(ParsedReview {
        verdict,
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
/// uses the `title` as the `kind` and `body` as `description`.
/// Test: covered transitively by `parse_json_block_happy_path`.
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
    let mut finding = Finding::new(file, f.title, f.body, String::new(), f.confidence, effort);
    finding.line = f.line;
    finding
}

// ─── Strategy 2: Verdict keyword scan ────────────────────────────────────────

/// Scan the last 20% of the body for a verdict keyword (spec REV-112).
///
/// Why: when the LLM ignores the JSON output format, the verdict is often still
/// present as a plain token at or near the end of the response.
/// What: searches the last 20% of `body` (minimum 200 chars) for the verdict
/// tokens in priority order (BLOCK > REQUEST_CHANGES > APPROVE* > APPROVE).
/// Returns `None` if no token is found.
/// Test: `parse_verdict_keyword_fallback`.
fn scan_verdict_keyword(body: &str) -> Option<Verdict> {
    let scan_start = body.len().saturating_sub((body.len() / 5).max(200));
    let tail = &body[scan_start..];

    // Priority order: most severe first so "BLOCK" beats "APPROVE" if both appear.
    if tail.contains("BLOCK") {
        return Some(Verdict::Block);
    }
    if tail.contains("REQUEST_CHANGES") {
        return Some(Verdict::RequestChanges);
    }
    if tail.contains("APPROVE*") {
        return Some(Verdict::ApproveStar);
    }
    if tail.contains("APPROVE") {
        return Some(Verdict::Approve);
    }
    if tail.contains("N/A") {
        return Some(Verdict::NotApplicable);
    }
    None
}

// ─── Verdict string normalization ─────────────────────────────────────────────

/// Parse a verdict string from the JSON block into a `Verdict`.
///
/// Why: the LLM may emit slightly varied case or include extra whitespace.
/// What: normalises to uppercase and matches against the known tokens; returns
/// `None` for unrecognised strings (caller applies fail-safe).
/// Test: `parse_verdict_string_normalization`.
fn parse_verdict_string(s: &str) -> Option<Verdict> {
    match s.trim().to_uppercase().as_str() {
        "APPROVE" => Some(Verdict::Approve),
        "APPROVE*" => Some(Verdict::ApproveStar),
        "REQUEST_CHANGES" | "REQUEST CHANGES" => Some(Verdict::RequestChanges),
        "BLOCK" => Some(Verdict::Block),
        "N/A" | "NA" => Some(Verdict::NotApplicable),
        _ => None,
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const BODY_WITH_JSON_APPROVE: &str = r#"
This PR looks good overall. The authentication logic is straightforward.

```json
{
  "verdict": "APPROVE",
  "summary": "Clean authentication refactor with no issues.",
  "findings": []
}
```
"#;

    const BODY_WITH_JSON_REQUEST_CHANGES: &str = r#"
I found a security issue in this PR.

```json
{
  "verdict": "REQUEST_CHANGES",
  "summary": "SQL injection risk in login handler.",
  "findings": [
    {
      "title": "SQL injection",
      "body": "Line 42 uses string interpolation in a SQL query.",
      "severity": "critical",
      "confidence": 0.95,
      "file": "src/login.rs",
      "line": 42
    }
  ]
}
```
"#;

    const BODY_KEYWORD_ONLY: &str = r#"
After reviewing this PR, I believe the changes look reasonable.
There are some minor style issues but nothing blocking.

The verdict is APPROVE*.
"#;

    const BODY_BLOCK_VERDICT: &str = r#"
This PR introduces a critical auth bypass.

BLOCK — this must not merge.
"#;

    #[test]
    fn parse_json_block_happy_path_approve() {
        let result = parse_review_response(BODY_WITH_JSON_APPROVE);
        assert!(
            !result.is_fail_safe,
            "should not be fail-safe: {:?}",
            result.fail_safe_reason
        );
        assert_eq!(result.verdict, Verdict::Approve);
        assert_eq!(
            result.summary,
            "Clean authentication refactor with no issues."
        );
        assert!(result.findings.is_empty());
    }

    #[test]
    fn parse_json_block_happy_path_request_changes() {
        let result = parse_review_response(BODY_WITH_JSON_REQUEST_CHANGES);
        assert!(!result.is_fail_safe);
        assert_eq!(result.verdict, Verdict::RequestChanges);
        assert_eq!(result.findings.len(), 1);
        let f = &result.findings[0];
        assert_eq!(f.kind, "SQL injection");
        assert_eq!(f.file, "src/login.rs");
        assert_eq!(f.line, Some(42));
        assert!((f.confidence - 0.95_f32).abs() < 1e-5);
    }

    #[test]
    fn parse_verdict_keyword_fallback_approve_star() {
        let result = parse_review_response(BODY_KEYWORD_ONLY);
        assert!(!result.is_fail_safe);
        assert_eq!(result.verdict, Verdict::ApproveStar);
        assert!(result.findings.is_empty());
    }

    #[test]
    fn parse_verdict_keyword_fallback_block() {
        let result = parse_review_response(BODY_BLOCK_VERDICT);
        assert!(!result.is_fail_safe);
        assert_eq!(result.verdict, Verdict::Block);
    }

    #[test]
    fn parse_fail_safe_approve_on_empty_response() {
        let result = parse_review_response("");
        assert!(result.is_fail_safe, "empty response must trigger fail-safe");
        assert_eq!(
            result.verdict,
            Verdict::Approve,
            "fail-safe must default to APPROVE"
        );
        assert!(result.fail_safe_reason.is_some());
    }

    #[test]
    fn parse_fail_safe_approve_on_malformed_json() {
        let body = r#"This is a review response with no verdict.

```json
{ "verdict": "definitely yes", "this_is": broken json
"#;
        let result = parse_review_response(body);
        // No valid JSON block, no keyword → fail-safe APPROVE.
        assert_eq!(result.verdict, Verdict::Approve);
        // (may or may not be fail-safe depending on keyword scan — APPROVE is
        //  not present in the body, so it should be fail-safe)
        assert!(
            result.is_fail_safe,
            "malformed JSON with no keyword must be fail-safe"
        );
    }

    #[test]
    fn parse_fail_safe_approve_on_unparseable_verdict() {
        // JSON block present but verdict string is unrecognized; we fall back to APPROVE.
        let body = r#"```json
{"verdict": "LOOKS_OK", "summary": "fine", "findings": []}
```"#;
        let result = parse_review_response(body);
        // `parse_verdict_string` returns None → defaults to Approve.
        assert_eq!(result.verdict, Verdict::Approve);
    }

    #[test]
    fn parse_verdict_string_normalization() {
        assert_eq!(parse_verdict_string("approve"), Some(Verdict::Approve));
        assert_eq!(parse_verdict_string("APPROVE"), Some(Verdict::Approve));
        assert_eq!(
            parse_verdict_string(" REQUEST_CHANGES "),
            Some(Verdict::RequestChanges)
        );
        assert_eq!(parse_verdict_string("block"), Some(Verdict::Block));
        assert_eq!(parse_verdict_string("n/a"), Some(Verdict::NotApplicable));
        assert_eq!(parse_verdict_string("UNKNOWN"), None);
    }

    #[test]
    fn parse_json_block_handles_fence_variants() {
        // Verify the parser finds the last ```json block, not a middle one.
        let body = r#"
First example:
```json
{"verdict": "BLOCK", "summary": "not the last one", "findings": []}
```

Second example:
```json
{"verdict": "APPROVE", "summary": "this is the last one", "findings": []}
```
"#;
        let result = parse_review_response(body);
        assert_eq!(result.verdict, Verdict::Approve);
        assert_eq!(result.summary, "this is the last one");
    }

    #[test]
    fn parse_findings_confidence_clamped() {
        let body = r#"```json
{
  "verdict": "REQUEST_CHANGES",
  "summary": "test",
  "findings": [
    {"title": "t", "body": "b", "severity": "low", "confidence": 2.5, "file": "a.rs"}
  ]
}
```"#;
        let result = parse_review_response(body);
        assert_eq!(result.findings.len(), 1);
        assert!(
            result.findings[0].confidence <= 1.0,
            "confidence must be clamped: {}",
            result.findings[0].confidence
        );
    }

    #[test]
    fn parse_finding_missing_file_defaults_to_unknown() {
        let body = r#"```json
{
  "verdict": "APPROVE",
  "summary": "ok",
  "findings": [{"title": "t", "body": "b"}]
}
```"#;
        let result = parse_review_response(body);
        assert_eq!(result.findings[0].file, "unknown");
    }

    #[test]
    fn scan_verdict_keyword_priority_block_beats_approve() {
        // Body contains both BLOCK and APPROVE — BLOCK wins.
        let body = "This APPROVE-worthy PR unfortunately has a BLOCK issue.";
        let verdict = scan_verdict_keyword(body);
        assert_eq!(verdict, Some(Verdict::Block));
    }
}
