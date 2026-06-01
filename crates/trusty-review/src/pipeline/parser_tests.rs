//! Tests for the review response parser.
//!
//! Why: extracted from `parser.rs` to keep that file under the 500-line cap
//! while preserving full test coverage.
//! What: exercises the direct JSON parse path (structured output), the
//! fence-based JSON block path (legacy), the verdict keyword scan fallback,
//! and the fail-safe APPROVE path.
//! Test: included as `#[cfg(test)] mod tests` from `parser.rs`.

use super::*;

// ── Direct JSON (structured output) path ─────────────────────────────────

/// Verify that a clean JSON object (no fences) parses correctly.
///
/// Why: this is the primary parse path with forced structured output
/// (Bedrock tool-use / OpenRouter json_schema).  If it fails, every
/// structured-output response falls through to the fence-based path.
/// What: passes a bare JSON object string to `parse_review_response`,
/// asserts correct verdict, summary, and findings.
/// Test: no network.
#[test]
fn parse_direct_json_happy_path() {
    let body = r#"{"verdict":"APPROVE","summary":"Clean change.","findings":[]}"#;
    let result = parse_review_response(body);
    assert!(
        !result.is_fail_safe,
        "direct JSON must not trigger fail-safe"
    );
    assert_eq!(result.verdict, Verdict::Approve);
    assert_eq!(result.summary, "Clean change.");
    assert!(result.findings.is_empty());
}

/// Verify that a direct JSON object with findings parses correctly.
///
/// Why: ensures `try_parse_direct_json` handles non-empty findings arrays
/// from the structured output path.
/// What: passes a bare JSON with one finding, asserts it's parsed correctly.
/// Test: no network.
#[test]
fn parse_direct_json_request_changes_with_findings() {
    let body = serde_json::json!({
        "verdict": "REQUEST_CHANGES",
        "summary": "SQL injection risk.",
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
    })
    .to_string();

    let result = parse_review_response(&body);
    assert!(!result.is_fail_safe, "must not be fail-safe");
    assert_eq!(result.verdict, Verdict::RequestChanges);
    assert_eq!(result.findings.len(), 1);
    assert_eq!(result.findings[0].kind, "SQL injection");
    assert_eq!(result.findings[0].file, "src/login.rs");
    assert_eq!(result.findings[0].line, Some(42));
}

/// Verify that a direct JSON object with a null line field parses correctly.
///
/// Why: the schema allows `line` to be null; serde must handle this.
/// What: passes a bare JSON with a finding where line is null.
/// Test: no network.
#[test]
fn parse_direct_json_finding_with_null_line() {
    let body = r#"{"verdict":"APPROVE","summary":"ok","findings":[{"title":"t","body":"b","severity":"low","confidence":0.5,"file":"src/a.rs","line":null}]}"#;
    let result = parse_review_response(body);
    assert!(!result.is_fail_safe);
    assert_eq!(result.findings.len(), 1);
    assert_eq!(result.findings[0].line, None);
}

// ── Legacy fenced JSON block path ─────────────────────────────────────────

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

// ── Keyword scan fallback ─────────────────────────────────────────────────

#[test]
fn parse_verdict_keyword_fallback_approve_star() {
    let result = parse_review_response(BODY_KEYWORD_ONLY);
    assert!(!result.is_fail_safe);
    assert_eq!(result.verdict, Verdict::ApproveWithReservations);
    assert!(result.findings.is_empty());
}

#[test]
fn parse_verdict_keyword_fallback_block() {
    let result = parse_review_response(BODY_BLOCK_VERDICT);
    assert!(!result.is_fail_safe);
    assert_eq!(result.verdict, Verdict::Block);
}

// ── Fail-safe path ────────────────────────────────────────────────────────

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
    assert_eq!(result.verdict, Verdict::Approve);
    assert!(
        result.is_fail_safe,
        "malformed JSON with no keyword must be fail-safe"
    );
}

#[test]
fn parse_fail_safe_approve_on_unparseable_verdict() {
    let body = r#"```json
{"verdict": "LOOKS_OK", "summary": "fine", "findings": []}
```"#;
    let result = parse_review_response(body);
    assert_eq!(result.verdict, Verdict::Approve);
}

// ── Verdict string normalization ─────────────────────────────────────────

#[test]
fn parse_verdict_string_normalization() {
    assert_eq!(parse_verdict_string("approve"), Some(Verdict::Approve));
    assert_eq!(parse_verdict_string("APPROVE"), Some(Verdict::Approve));
    assert_eq!(
        parse_verdict_string(" REQUEST_CHANGES "),
        Some(Verdict::RequestChanges)
    );
    assert_eq!(parse_verdict_string("block"), Some(Verdict::Block));
    assert_eq!(parse_verdict_string("UNKNOWN"), Some(Verdict::Unknown));
    assert_eq!(parse_verdict_string("unknown"), Some(Verdict::Unknown));
    assert_eq!(parse_verdict_string("N/A"), None);
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

/// Verify the parser extracts UNKNOWN when the model emits it in a JSON block.
///
/// Why: UNKNOWN is the correct grade when the diff is truncated; the parser
/// must pass it through rather than collapsing it to the fail-safe APPROVE.
/// What: passes a direct JSON body with `"verdict":"UNKNOWN"`, asserts the
/// result carries `Verdict::Unknown` and is not fail-safe.
/// Test: no network.
#[test]
fn parse_direct_json_unknown_verdict() {
    let body = r#"{"verdict":"UNKNOWN","summary":"Diff too truncated to assess.","findings":[]}"#;
    let result = parse_review_response(body);
    assert!(
        !result.is_fail_safe,
        "UNKNOWN from model must not trigger fail-safe"
    );
    assert_eq!(
        result.verdict,
        Verdict::Unknown,
        "parser must preserve UNKNOWN from model output"
    );
}

/// Verify the keyword scanner detects UNKNOWN.
///
/// Why: fall-back keyword scan must also pick up UNKNOWN so truncated-diff
/// responses are correctly graded even when forced structured output is not
/// active.
/// What: passes a free-text body ending with "UNKNOWN", asserts the scanner
/// returns `Verdict::Unknown`.
/// Test: no network.
#[test]
fn scan_verdict_keyword_detects_unknown() {
    let body = "The diff is too short to assess. UNKNOWN";
    let verdict = scan_verdict_keyword(body);
    assert_eq!(verdict, Some(Verdict::Unknown));
}

/// Verify APPROVE* round-trips through a direct JSON parse.
///
/// Why: the asterisk in APPROVE* is unusual in JSON enum values; this guards
/// against any serde regression that would corrupt the board grade.
/// What: serialises a direct JSON with `"verdict":"APPROVE*"`, asserts the
/// result carries `Verdict::ApproveWithReservations`.
/// Test: no network.
#[test]
fn parse_direct_json_approve_star() {
    let body = r#"{"verdict":"APPROVE*","summary":"Minor concern noted.","findings":[]}"#;
    let result = parse_review_response(body);
    assert!(!result.is_fail_safe);
    assert_eq!(result.verdict, Verdict::ApproveWithReservations);
}
