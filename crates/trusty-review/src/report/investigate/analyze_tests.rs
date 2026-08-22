//! Tests for the investigation LLM request/response layer (#2357, wave 3.1).
//!
//! Why: the forced schema and grounded prompt are what make verification possible
//! (every finding must cite a provided file and quote it verbatim); the output
//! must be structurally bounded (`maxItems`/`maxLength`) so a batch cannot
//! truncate, and parsing must be lenient enough to survive a fenced response but
//! reject garbage.
//! What: asserts the schema shape (incl. size bounds), that batch file contents
//! and position reach the prompt, the retry directive, prefix stripping, and
//! the bare/fenced/garbage parse paths.  No live LLM.
//! Test: included as `#[cfg(test)] mod tests` from `analyze.rs`.

use super::*;
use crate::report::investigate::select::SelectedFile;

fn file(path: &str, content: &str) -> SelectedFile {
    SelectedFile {
        path: path.to_string(),
        content: content.to_string(),
        truncated: false,
        dimensions: vec!["authentication & secrets".to_string()],
        selected_by: None,
        hotspot: None,
        declared_for: None,
    }
}

/// Why: forced structured output needs a well-formed schema with the evidence
/// contract AND the size bounds front and centre — those bounds are the
/// structural fix for the output-truncation incident.
/// What: asserts the schema name, required fields, and the `maxItems`/
/// `maxLength` bounds.
/// Test: this test itself.
#[test]
fn schema_shape() {
    let s = investigation_schema(MAX_FINDINGS_PER_BATCH);
    assert_eq!(s.name, "repo_investigation");
    let findings = &s.schema["properties"]["findings"];
    assert_eq!(findings["maxItems"], MAX_FINDINGS_PER_BATCH);
    let item = &findings["items"];
    let required = item["required"].as_array().unwrap();
    for key in ["title", "severity", "dimension", "file", "evidence_quote"] {
        assert!(
            required.iter().any(|v| v == key),
            "'{key}' must be required"
        );
    }
    assert_eq!(
        item["properties"]["evidence_quote"]["maxLength"],
        EVIDENCE_QUOTE_MAX_CHARS
    );
}

/// Why: the retry path asks for a smaller cap so the schema itself shrinks.
/// What: `investigation_schema(RETRY_MAX_FINDINGS)` reflects the tighter cap.
/// Test: this test itself.
#[test]
fn schema_shrinks_on_retry() {
    let s = investigation_schema(RETRY_MAX_FINDINGS);
    assert_eq!(
        s.schema["properties"]["findings"]["maxItems"],
        RETRY_MAX_FINDINGS
    );
}

/// Why: the model may only cite files it was given; the contents must be embedded.
/// What: asserts the digest carries the file path and its content, and forces a
/// structured response capped at the requested `max_findings`.
/// Test: this test itself.
#[test]
fn request_embeds_files() {
    let files = vec![file("src/auth.rs", "let token = read_secret();")];
    let req = build_request(
        "Acme",
        &files,
        1,
        1,
        1,
        Some("focus on auth"),
        "stub/model",
        MAX_FINDINGS_PER_BATCH,
        false,
    );
    let digest = &req.messages[0].content;
    assert!(digest.contains("src/auth.rs"));
    assert!(digest.contains("read_secret()"));
    assert!(digest.contains("focus on auth"));
    let schema = req.response_schema.expect("schema present");
    assert_eq!(
        schema.schema["properties"]["findings"]["maxItems"],
        MAX_FINDINGS_PER_BATCH
    );
}

/// #6146: a file the manifest measured arrives with the measurement stated
/// beside it, BEFORE its content, so the model reads the instruction first.
/// A file with no measurement leaves the digest byte-identical.
#[test]
fn a_hotspot_file_carries_a_focus_line() {
    let mut hot = file("src/pay.rs", "fn settle_invoice() {}");
    hot.hotspot = Some(FunctionHotspot {
        function: Some("settle_invoice".to_string()),
        start_line: 40,
        end_line: 190,
        cyclomatic: 31,
    });
    let plain = file("src/util.rs", "fn noop() {}");

    let digest = |files: &[SelectedFile]| {
        build_request(
            "Acme",
            files,
            1,
            1,
            2,
            None,
            "stub/model",
            MAX_FINDINGS_PER_BATCH,
            false,
        )
        .messages[0]
            .content
            .clone()
    };

    let both = digest(&[hot.clone(), plain.clone()]);
    assert!(
        both.contains(
            "Hotspot: lines 40-190, fn settle_invoice, cyclomatic 31 — prioritize DD analysis of \
             this function."
        ),
        "{both}"
    );
    let focus_at = both.find("Hotspot: lines").expect("the focus line");
    let fence_at = both.find("```").expect("the first fence");
    assert!(
        focus_at < fence_at,
        "the instruction must precede the content: {both}"
    );
    assert_eq!(
        both.matches("Hotspot: lines").count(),
        1,
        "the unmeasured file gains nothing: {both}"
    );
    assert!(
        !digest(&[plain]).contains("Hotspot"),
        "a batch with no measurement is unchanged"
    );
}

/// #6146: a range the writer could not supply produces NO focus line rather
/// than one pointing at lines 0-0. Same for an inverted range.
#[test]
fn an_unusable_hotspot_range_adds_nothing() {
    for (start, end) in [(0, 0), (0, 90), (90, 12)] {
        let mut f = file("src/pay.rs", "code");
        f.hotspot = Some(FunctionHotspot {
            function: Some("settle_invoice".to_string()),
            start_line: start,
            end_line: end,
            cyclomatic: 31,
        });
        let req = build_request(
            "Acme",
            &[f],
            1,
            1,
            1,
            None,
            "stub/model",
            MAX_FINDINGS_PER_BATCH,
            false,
        );
        assert!(
            !req.messages[0].content.contains("Hotspot"),
            "lines {start}-{end} must not reach the prompt"
        );
    }
}

/// Why: a routing prefix must be stripped so the bare id reaches the provider.
/// What: asserts `bedrock/` is removed from `req.model`.
/// Test: this test itself.
#[test]
fn request_strips_prefix() {
    let files = vec![file("a.rs", "code")];
    let req = build_request(
        "A",
        &files,
        1,
        1,
        1,
        None,
        "bedrock/us.anthropic.claude-sonnet-4-6",
        MAX_FINDINGS_PER_BATCH,
        false,
    );
    assert_eq!(req.model, "us.anthropic.claude-sonnet-4-6");
}

/// Why: a multi-batch repository must tell the model its position so it never
/// mistakes one batch for the whole codebase.
/// What: batch 2 of 4 mentions its position and the total tracked file count.
/// Test: this test itself.
#[test]
fn request_reports_batch_position() {
    let files = vec![file("b.rs", "code")];
    let req = build_request(
        "A",
        &files,
        2,
        4,
        175,
        None,
        "stub/model",
        MAX_FINDINGS_PER_BATCH,
        false,
    );
    let digest = &req.messages[0].content;
    assert!(digest.contains("batch 2 of 4"));
    assert!(digest.contains("175 files tracked"));
}

/// Why: a single-batch repository must not carry confusing "batch 1 of 1" noise.
/// What: batch_count = 1 omits the batch-position line.
/// Test: this test itself.
#[test]
fn single_batch_omits_position_line() {
    let files = vec![file("a.rs", "code")];
    let req = build_request(
        "A",
        &files,
        1,
        1,
        1,
        None,
        "stub/model",
        MAX_FINDINGS_PER_BATCH,
        false,
    );
    assert!(!req.messages[0].content.contains("batch 1 of 1"));
}

/// Why: the retry-once path must explicitly ask for a terser, smaller response.
/// What: `retry_concise = true` appends the retry directive with the tighter cap.
/// Test: this test itself.
#[test]
fn retry_directive_appended() {
    let files = vec![file("a.rs", "code")];
    let req = build_request(
        "A",
        &files,
        1,
        1,
        1,
        None,
        "stub/model",
        RETRY_MAX_FINDINGS,
        true,
    );
    let digest = &req.messages[0].content;
    assert!(digest.contains("was truncated"));
    assert!(digest.contains(&RETRY_MAX_FINDINGS.to_string()));
}

/// Why: a bare JSON object is the primary path.
/// What: parses a valid findings object.
/// Test: this test itself.
#[test]
fn parses_bare_object() {
    let raw = parse_findings(
        r#"{"findings": [{"title": "T", "severity": "red", "file": "a.rs", "evidence_quote": "x"}]}"#,
    )
    .expect("parse");
    assert_eq!(raw.findings.len(), 1);
    assert_eq!(raw.findings[0].title, "T");
}

/// Why: a defensive fenced block must still parse.
/// What: parses a ```json fenced response.
/// Test: this test itself.
#[test]
fn parses_fenced_block() {
    let raw = parse_findings("prefix\n```json\n{\"findings\": []}\n```\nsuffix").expect("parse");
    assert!(raw.findings.is_empty());
}

/// Why: unparseable output must fail (→ caller fails closed).
/// What: garbage returns None.
/// Test: this test itself.
#[test]
fn rejects_garbage() {
    assert!(parse_findings("not json at all {{{").is_none());
}

/// An explicit `"line": null` deserializes, and the newly-required prose fields
/// accept an empty string.
///
/// Why: #5675 made every schema property required, so `line` became nullable to
/// express "unknown" — and the system prompt now instructs a GREEN finding to
/// emit exactly that. `#[serde(default)]` fills a MISSING key; it does not by
/// itself accept an explicit `null`, so a plain integer field would reject this
/// payload at runtime and no schema-compliance test would catch it: the schema
/// is correct, the Rust side would be what fails. `RawFinding.line` is
/// `Option<u64>` (`analyze.rs`), matching `LlmFinding.line: Option<u32>` on the
/// review path, so `null` maps to `None`.
/// What: parses one GREEN finding carrying `"line": null` plus an empty string
/// in every field strict mode newly requires, and asserts the round trip.
/// Test: this test itself.
#[test]
fn parses_explicit_null_line_and_empty_required_fields() {
    let raw = parse_findings(
        r#"{"findings": [{
            "title": "Healthy topic",
            "severity": "green",
            "dimension": "Testing",
            "file": "src/lib.rs",
            "line": null,
            "evidence_quote": "",
            "description": "",
            "business_impact": "",
            "remediation": "",
            "cost_effort": ""
        }]}"#,
    )
    .expect("an explicit null line must deserialize, not fail the whole batch");

    let finding = &raw.findings[0];
    assert_eq!(finding.line, None, "null must map to None");
    assert_eq!(finding.title, "Healthy topic");
    assert!(finding.business_impact.is_empty());
    assert!(finding.cost_effort.is_empty());
    assert!(finding.evidence_quote.is_empty());
}
