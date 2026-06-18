//! Tests for the review response parser.
//!
//! Why: extracted from `parser.rs` to keep that file under the 500-line cap
//! while preserving full test coverage.
//! What: exercises the direct JSON parse path (structured output), the
//! fence-based JSON block path (legacy), the verdict keyword scan fallback,
//! and the fail-CLOSED UNKNOWN fail-safe path (#1241).
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

/// Verify a maximally-strict OpenAI-shaped response round-trips cleanly.
///
/// Why: under OpenAI strict mode every property is required, so a conforming
/// response carries ALL top-level fields (`grade`, `grade_justification`,
/// `verdict`, `summary`, `findings`) and every finding carries ALL its fields
/// (`title`, `body`, `severity`, `confidence`, `file`, `line`).  This test
/// proves the `LlmOutputBlock`/`LlmFinding` deserializers parse that fully
/// populated shape — guarding that the schema tightening (which forces the
/// model to emit all fields) does not break the parse contract.
/// What: deserializes a body matching the strict schema exactly and asserts the
/// verdict, grade, and finding fields are extracted.
/// Test: no network.
#[test]
fn parse_direct_json_strict_full_shape() {
    let body = serde_json::json!({
        "grade": "B+",
        "grade_justification": "solid but missing tests",
        "verdict": "REQUEST_CHANGES",
        "summary": "Needs test coverage.",
        "findings": [
            {
                "title": "Missing tests",
                "body": "The new handler has no unit tests.",
                "severity": "medium",
                "confidence": 0.8,
                "file": "src/handler.rs",
                "line": null
            }
        ]
    })
    .to_string();

    let result = parse_review_response(&body);
    assert!(!result.is_fail_safe, "strict-shaped response must parse");
    assert_eq!(result.verdict, Verdict::RequestChanges);
    assert_eq!(result.grade.as_deref(), Some("B+"));
    assert_eq!(result.findings.len(), 1);
    assert_eq!(result.findings[0].file, "src/handler.rs");
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
fn parse_fail_safe_unknown_on_empty_response() {
    // Fail-CLOSED (#1241 supersedes REV-130): empty output → UNKNOWN, not APPROVE.
    let result = parse_review_response("");
    assert!(result.is_fail_safe, "empty response must trigger fail-safe");
    assert_eq!(
        result.verdict,
        Verdict::Unknown,
        "fail-safe must fail CLOSED to UNKNOWN (#1241), never silently APPROVE"
    );
    assert!(result.fail_safe_reason.is_some());
}

#[test]
fn parse_fail_safe_unknown_on_malformed_json() {
    // Fail-CLOSED (#1241): broken JSON with no recoverable keyword → UNKNOWN.
    let body = r#"This is a review response with no verdict.

```json
{ "verdict": "definitely yes", "this_is": broken json
"#;
    let result = parse_review_response(body);
    assert_eq!(
        result.verdict,
        Verdict::Unknown,
        "malformed JSON with no keyword must fail CLOSED to UNKNOWN (#1241)"
    );
    assert!(
        result.is_fail_safe,
        "malformed JSON with no keyword must be fail-safe"
    );
}

#[test]
fn parse_fail_safe_unknown_on_unparseable_verdict() {
    // Fail-CLOSED (#1241): a valid JSON block carrying an UNRECOGNISED verdict
    // token must NOT silently default to APPROVE — it surfaces UNKNOWN.
    let body = r#"```json
{"verdict": "LOOKS_OK", "summary": "fine", "findings": []}
```"#;
    let result = parse_review_response(body);
    assert_eq!(
        result.verdict,
        Verdict::Unknown,
        "unrecognised verdict token must fail CLOSED to UNKNOWN (#1241)"
    );
}

#[test]
fn parse_truncated_json_object_is_unknown() {
    // Fail-CLOSED (#1241): a structured-output response cut off mid-object BEFORE
    // the verdict field is emitted (no closing brace, no fence, no verdict token)
    // is unparseable by all three strategies → UNKNOWN, never silent APPROVE.
    let body = r#"{"summary": "Reviewing the changes to the auth module, I found that the handl"#;
    let result = parse_review_response(body);
    assert_eq!(
        result.verdict,
        Verdict::Unknown,
        "truncated JSON must fail CLOSED to UNKNOWN, never parse-and-APPROVE (#1241)"
    );
    assert!(result.is_fail_safe, "truncated JSON must trigger fail-safe");
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

// ── Method-conformance finding category (#1359) ──────────────────────────

/// A finding emitting `"category":"method-conformance"` parses to the
/// `MethodConformance` category.
///
/// Why: the back gate (#1359) distinguishes conformance findings by category so
/// the verdict floor can cap them at REQUEST_CHANGES.  The parser must preserve
/// the LLM-emitted category.
/// What: parses a direct-JSON finding with the conformance category, asserts the
/// internal `Finding.category`.
/// Test: no network.
#[test]
fn parse_method_conformance_finding_category() {
    let body = r#"{
        "verdict":"REQUEST_CHANGES",
        "summary":"Diff contradicts the ticket method.",
        "findings":[{
            "title":"Uses offset pagination",
            "body":"Ticket specifies cursor-based pagination.",
            "severity":"medium",
            "confidence":0.9,
            "file":"src/page.rs",
            "category":"method-conformance"
        }]
    }"#;
    let result = parse_review_response(body);
    assert!(!result.is_fail_safe);
    assert_eq!(result.findings.len(), 1);
    assert_eq!(
        result.findings[0].category,
        FindingCategory::MethodConformance,
        "the conformance category must survive parsing"
    );
}

/// A finding that OMITS `category` defaults to `Correctness` (back-compat).
///
/// Why: existing fixtures and models that do not emit `category` must keep
/// parsing as correctness findings (the `#[serde(default)]` guarantee, AC). This
/// is also the cross-provider safety net for #1359: `category` is in the schema's
/// `required` array ONLY because OpenAI strict mode demands every property be
/// required (see `review_schema_is_openai_strict_compliant`). Non-strict backends
/// (Bedrock/Anthropic tool-use, Gemini) and older models can omit `category`
/// WITHOUT client-side rejection — both the OpenRouter and Bedrock backends route
/// their structured output through THIS serde path, so an omitted `category` is
/// silently defaulted here rather than rejected by any validation layer.
/// What: parses a finding with no `category` key, asserts the default.
/// Test: no network.
#[test]
fn parse_finding_without_category_defaults_correctness() {
    let body = r#"{
        "verdict":"REQUEST_CHANGES",
        "summary":"Bug.",
        "findings":[{
            "title":"Null deref",
            "body":"Unchecked unwrap.",
            "severity":"high",
            "confidence":0.95,
            "file":"src/x.rs"
        }]
    }"#;
    let result = parse_review_response(body);
    assert_eq!(result.findings.len(), 1);
    assert_eq!(
        result.findings[0].category,
        FindingCategory::Correctness,
        "a finding with no category must default to Correctness (back-compat)"
    );
    // A finding with no `suggested_replacement` defaults to None (#1415 back-compat).
    assert!(
        result.findings[0].suggested_replacement.is_none(),
        "absent suggested_replacement must default to None"
    );
    // A finding with no `consequence` defaults to "" (#1416 back-compat).
    assert!(
        result.findings[0].consequence.is_empty(),
        "absent consequence must default to empty string"
    );
}

/// A finding's `consequence` is carried through the parser (#1416).
///
/// Why: the inline-comment renderer reads `Finding.consequence`; it must survive
/// the JSON → internal conversion so the "_Why it matters:_" line renders.
/// What: parses a finding carrying a `consequence`, asserts it is preserved.
/// Test: this test itself.
#[test]
fn parse_finding_carries_consequence() {
    let body = r#"{
        "verdict":"REQUEST_CHANGES",
        "summary":"Bug.",
        "findings":[{
            "title":"Unwrap",
            "body":"Unchecked unwrap on parse.",
            "severity":"high",
            "confidence":0.9,
            "file":"src/x.rs",
            "line":7,
            "consequence":"panics on malformed input"
        }]
    }"#;
    let result = parse_review_response(body);
    assert_eq!(result.findings.len(), 1);
    assert_eq!(
        result.findings[0].consequence, "panics on malformed input",
        "consequence must be carried through"
    );
}

/// A finding's `suggested_replacement` is carried through the parser (#1415).
///
/// Why: the committable-suggestion renderer reads `Finding.suggested_replacement`;
/// it must survive the JSON → internal conversion, and an empty value must
/// normalise to `None` so the renderer never emits an empty suggestion block.
/// What: parses one finding with concrete replacement code and one with an empty
/// string, asserting the first is `Some` and the second is `None`.
/// Test: this test itself.
#[test]
fn parse_finding_carries_suggested_replacement() {
    let body = r#"{
        "verdict":"REQUEST_CHANGES",
        "summary":"Bug.",
        "findings":[
            {
                "title":"SQLi",
                "body":"Interpolated SQL.",
                "severity":"high",
                "confidence":0.9,
                "file":"src/db.rs",
                "line":42,
                "suggested_replacement":"let sql = bind(\"SELECT ?\", input);"
            },
            {
                "title":"Nit",
                "body":"Naming.",
                "severity":"low",
                "confidence":0.5,
                "file":"src/db.rs",
                "line":43,
                "suggested_replacement":"   "
            }
        ]
    }"#;
    let result = parse_review_response(body);
    assert_eq!(result.findings.len(), 2);
    assert_eq!(
        result.findings[0].suggested_replacement.as_deref(),
        Some("let sql = bind(\"SELECT ?\", input);"),
        "concrete replacement must be carried through"
    );
    assert!(
        result.findings[1].suggested_replacement.is_none(),
        "whitespace-only replacement must normalise to None"
    );
}
