//! Tests for CWE weakness-class tagging (#6779).
//!
//! Why: the field is only trustworthy if it is dropped whenever the model's
//! answer is not mechanically readable — a repaired or guessed id would be
//! exactly the fabrication #6779 forbids. These cases pin both halves: what is
//! admitted, and what is silently dropped.
//! What: covers id validation, the class table, the prompt checklist, and the
//! end-to-end serialisation shape through `verify_findings`.
//! Test: included as `#[cfg(test)] mod cwe_tests` from `cwe.rs`.

use super::*;

/// Why: models write `cwe-79` as readily as `CWE-79`; one class must not render
/// as two tags.
#[test]
fn a_well_formed_id_is_admitted_and_upper_cased() {
    assert_eq!(resolve("CWE-79").as_deref(), Some("CWE-79"));
    assert_eq!(resolve("cwe-79").as_deref(), Some("CWE-79"));
    assert_eq!(resolve("  Cwe-1336  ").as_deref(), Some("CWE-1336"));
}

/// Why: #6779's "never guessed" rule. Every one of these is a shape the model
/// has produced for an id field, and every one must be DROPPED rather than
/// repaired into the nearest plausible id.
#[test]
fn a_malformed_id_is_dropped() {
    for raw in [
        "", "   ", "CWE-", "CWE", "cwe89", "SQL-89", "CWE-79a", "CWE-7 9", "unknown", "CW",
    ] {
        assert_eq!(resolve(raw), None, "{raw:?} should not resolve");
    }
}

/// Why: the class-name branch is the fallback for a model that answers the field
/// with a weakness class instead of an id. A sample across the table proves the
/// lookup, the punctuation reduction, and the case folding together.
#[test]
fn the_class_table_round_trips_a_sample() {
    let sample = [
        ("SQL Injection", "CWE-89"),
        ("Cross-Site Scripting (XSS)", "CWE-79"),
        ("path traversal", "CWE-22"),
        ("Hardcoded Credentials", "CWE-798"),
        ("insecure deserialization", "CWE-502"),
        ("Missing Authentication", "CWE-306"),
        ("SSRF", "CWE-918"),
        ("weak cryptography", "CWE-327"),
        ("Improper Input Validation", "CWE-20"),
        ("TOCTOU", "CWE-367"),
        ("resource exhaustion", "CWE-400"),
        ("information exposure through logs", "CWE-532"),
        ("missing error handling", "CWE-390"),
    ];
    for (class, id) in sample {
        assert_eq!(resolve(class).as_deref(), Some(id), "class {class:?}");
    }
}

/// Why: `resolve`'s postcondition says every `Some` matches `^CWE-\d+$`. The id
/// branch checks that itself; this holds the table's own values to it, so a row
/// added with a typo cannot smuggle a malformed tag onto a finding.
#[test]
fn every_table_id_is_well_formed() {
    for (class, id) in WEAKNESS_CLASSES {
        assert_eq!(
            well_formed_id(id).as_deref(),
            Some(*id),
            "table row {class:?} carries a malformed id {id:?}"
        );
        assert_eq!(canonical_key(class).as_str(), *class, "key {class:?}");
    }
}

/// Why: the prompt spends tokens per entry, and the alternate spellings exist
/// for ingestion, not for the model to read back.
#[test]
fn the_checklist_names_each_id_once() {
    let checklist = class_checklist();
    assert!(checklist.contains("sql injection (CWE-89)"), "{checklist}");
    assert!(checklist.contains("ssrf (CWE-918)"), "{checklist}");
    assert_eq!(
        checklist.matches("CWE-798").count(),
        1,
        "one entry per id: {checklist}"
    );
    assert_eq!(
        checklist.matches("CWE-327").count(),
        1,
        "one entry per id: {checklist}"
    );
}

// ── End-to-end through verification and serialisation ────────────────────────

use crate::report::investigate::select::{SelectedFile, Selection};
use crate::report::investigate::verify::verify_findings;

fn selection(path: &str, content: &str) -> Selection {
    Selection {
        files: vec![SelectedFile {
            path: path.to_string(),
            content: content.to_string(),
            truncated: false,
            dimensions: vec![],
            selected_by: None,
            hotspot: None,
            declared_for: None,
        }],
        total_files: 1,
        skipped: 0,
        bytes_sent: content.len(),
        dimensions_covered: vec![],
        dimensions_absent: vec![],
        per_dimension: vec![],
        test_census: Default::default(),
        attributed_files: 0,
        attributed_only: false,
    }
}

/// The model's batch response, parsed the way the live path parses it — so this
/// case exercises the schema field's `#[serde(default)]` too.
fn parse(findings_json: &str) -> Vec<crate::report::investigate::analyze::RawFinding> {
    serde_json::from_str::<crate::report::investigate::analyze::RawInvestigation>(findings_json)
        .expect("investigation parses")
        .findings
}

/// Why: this is #6779's closure condition end to end — a declared weakness class
/// reaches `investigation.json`, and a finding without one carries NO field at
/// all rather than a null or an empty string.
///
/// This is the case that fails on `main`: the serialised finding there has no
/// `cwe_id` key for either input, because nothing in the pipeline carries one.
#[test]
fn a_declared_weakness_class_reaches_the_serialised_finding() {
    let content = "line one\nlet token = \"hunter2\";\nlet q = build(input);\n";
    let sel = selection("auth.rs", content);
    let raw = parse(
        r#"{"findings": [
            {"title": "Hardcoded token", "severity": "red",
             "dimension": "authentication & secrets", "file": "auth.rs",
             "evidence_quote": "let token = \"hunter2\";",
             "description": "d", "business_impact": "b",
             "remediation": "r", "cost_effort": "low",
             "cwe_id": "CWE-798"},
            {"title": "Unclear query construction", "severity": "amber",
             "dimension": "state management", "file": "auth.rs",
             "evidence_quote": "let q = build(input);",
             "description": "d", "business_impact": "b",
             "remediation": "r", "cost_effort": "low"}
        ]}"#,
    );
    let out = verify_findings(raw, &sel);
    assert_eq!(out.verified.len(), 2, "notes: {:?}", out.notes);
    assert_eq!(out.verified[0].cwe_id.as_deref(), Some("CWE-798"));
    assert_eq!(out.verified[1].cwe_id, None);

    let tagged = serde_json::to_string(&out.verified[0]).expect("serialises");
    assert!(tagged.contains(r#""cwe_id":"CWE-798""#), "{tagged}");
    let untagged = serde_json::to_string(&out.verified[1]).expect("serialises");
    assert!(
        !untagged.contains("cwe_id"),
        "a finding with no identifiable weakness class must carry no field: {untagged}"
    );
}

/// Why: a malformed id must not survive ingestion, and it must not reject the
/// finding either — the weakness class is an addition to a finding, never a
/// precondition for it.
#[test]
fn a_malformed_declared_id_is_dropped_and_the_finding_survives() {
    let content = "let token = \"hunter2\";\n";
    let sel = selection("auth.rs", content);
    let raw = parse(
        r#"{"findings": [
            {"title": "Hardcoded token", "severity": "red",
             "dimension": "authentication & secrets", "file": "auth.rs",
             "evidence_quote": "let token = \"hunter2\";",
             "description": "d", "business_impact": "b",
             "remediation": "r", "cost_effort": "low",
             "cwe_id": "SQL-89"}
        ]}"#,
    );
    let out = verify_findings(raw, &sel);
    assert_eq!(out.verified.len(), 1, "notes: {:?}", out.notes);
    assert_eq!(out.verified[0].cwe_id, None);
    let json = serde_json::to_string(&out.verified[0]).expect("serialises");
    assert!(!json.contains("cwe_id"), "{json}");
}

/// Why: a GREEN finding names a strength. A strength has no weakness class, so
/// the tag is blanked on that band exactly as its prose is (#6080).
#[test]
fn a_green_finding_carries_no_weakness_class() {
    let content = "let token = read_secret();\n";
    let sel = selection("auth.rs", content);
    let raw = parse(
        r#"{"findings": [
            {"title": "Secrets read from the environment", "severity": "green",
             "dimension": "authentication & secrets", "file": "auth.rs",
             "evidence_quote": "let token = read_secret();",
             "cwe_id": "CWE-798"}
        ]}"#,
    );
    let out = verify_findings(raw, &sel);
    assert_eq!(out.verified.len(), 1, "notes: {:?}", out.notes);
    assert_eq!(out.verified[0].cwe_id, None);
}

/// Why: the schema is what makes the field reachable at all, and strict mode
/// requires the property to be declared nullable rather than merely omitted
/// (the `line` precedent, #5675).
#[test]
fn the_schema_declares_a_nullable_cwe_id() {
    let schema = crate::report::investigate::analyze::investigation_schema(8);
    let json = serde_json::to_value(&schema).expect("schema serialises");
    let text = json.to_string();
    assert!(text.contains("cwe_id"), "{text}");
    assert!(text.contains("hardcoded credentials (CWE-798)"), "{text}");
}
