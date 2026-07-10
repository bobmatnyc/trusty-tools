//! Tests for the investigation LLM request/response layer (#2357).
//!
//! Why: the forced schema and grounded prompt are what make verification possible
//! (every finding must cite a provided file and quote it verbatim); parsing must
//! be lenient enough to survive a fenced response but reject garbage.
//! What: asserts the schema shape, that selected file contents reach the prompt,
//! prefix stripping, and the bare/fenced/garbage parse paths.  No live LLM.
//! Test: included as `#[cfg(test)] mod tests` from `analyze.rs`.

use super::*;
use crate::report::investigate::select::{SelectedFile, Selection};

fn selection_with(path: &str, content: &str) -> Selection {
    Selection {
        files: vec![SelectedFile {
            path: path.to_string(),
            content: content.to_string(),
            truncated: false,
            dimensions: vec!["authentication & secrets".to_string()],
        }],
        total_files: 10,
        skipped: 9,
        bytes_sent: content.len(),
        dimensions_covered: vec!["authentication & secrets".to_string()],
        dimensions_absent: vec![],
    }
}

/// Why: forced structured output needs a well-formed schema with the evidence
/// contract front and centre.
/// What: asserts the schema name and the findings item required fields.
/// Test: this test itself.
#[test]
fn schema_shape() {
    let s = investigation_schema();
    assert_eq!(s.name, "repo_investigation");
    let item = &s.schema["properties"]["findings"]["items"];
    let required = item["required"].as_array().unwrap();
    for key in ["title", "severity", "dimension", "file", "evidence_quote"] {
        assert!(
            required.iter().any(|v| v == key),
            "'{key}' must be required"
        );
    }
}

/// Why: the model may only cite files it was given; the contents must be embedded.
/// What: asserts the digest carries the file path and its content, and forces a
/// structured response.
/// Test: this test itself.
#[test]
fn request_embeds_files() {
    let sel = selection_with("src/auth.rs", "let token = read_secret();");
    let req = build_request("Acme", &sel, Some("focus on auth"), "stub/model");
    let digest = &req.messages[0].content;
    assert!(digest.contains("src/auth.rs"));
    assert!(digest.contains("read_secret()"));
    assert!(digest.contains("focus on auth"));
    assert!(req.response_schema.is_some());
}

/// Why: a routing prefix must be stripped so the bare id reaches the provider.
/// What: asserts `bedrock/` is removed from `req.model`.
/// Test: this test itself.
#[test]
fn request_strips_prefix() {
    let sel = selection_with("a.rs", "code");
    let req = build_request("A", &sel, None, "bedrock/us.anthropic.claude-sonnet-4-6");
    assert_eq!(req.model, "us.anthropic.claude-sonnet-4-6");
}

/// Why: a bare JSON object is the primary path.
/// What: parses a valid findings object.
/// Test: this test itself.
#[test]
fn parses_bare_object() {
    let raw = parse_findings(r#"{"findings": [{"title": "T", "severity": "red", "file": "a.rs", "evidence_quote": "x"}]}"#)
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
