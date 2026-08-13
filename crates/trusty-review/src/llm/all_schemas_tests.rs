//! OpenAI strict-mode compliance across EVERY response schema this crate sends.
//!
//! Why: #1235 fixed this defect for the two pipeline schemas and added its
//! tests beside them, so the two schemas nobody had looked at —
//! `report_synthesis` and `repo_investigation` — stayed non-compliant and
//! `report_synthesis` failed live against `openai/gpt-5.4-mini-20260317` with
//! `In context=('properties','findings','items'), 'additionalProperties' is
//! required to be supplied and to be false` (#5675). A per-schema test placed
//! next to each builder can only ever prove the schemas someone remembered to
//! write a test for. These tests instead cover the whole set at once, and fail
//! when the set grows without the list growing with it.
//! What: [`all_sent_schemas`] is the single enumeration of every schema the
//! crate can put on the wire. One test walks each recursively; a second reads
//! the crate's own production source to prove the enumeration is complete and
//! that nothing builds a [`ResponseSchema`] around
//! [`ResponseSchema::new`](crate::llm::ResponseSchema::new), which is what
//! applies `enforce_strict_mode`.
//! Test: this file IS the tests.

use std::path::{Path, PathBuf};

use crate::llm::ResponseSchema;
use crate::llm::schema::assert_object_nodes_strict;

/// Every response schema `trusty-review` can send to a provider.
///
/// Why: the enumeration has to live in ONE place, or the next schema is added
/// with a test for itself and no test for the class — which is exactly how
/// #5675 survived #1235's fix.
/// What: calls each builder with representative arguments (the size caps are
/// irrelevant to strict-mode compliance).
/// Test: consumed by both tests in this file;
/// `schema_enumeration_is_complete` fails if a builder is missing from it.
fn all_sent_schemas() -> Vec<(&'static str, ResponseSchema)> {
    vec![
        (
            "review_response_schema",
            crate::pipeline::prompt::review_response_schema(),
        ),
        (
            "verify_response_schema",
            crate::pipeline::verify_prompt::verify_response_schema(),
        ),
        (
            "synthesis_schema",
            crate::report::synthesize_prompt::synthesis_schema(5),
        ),
        (
            "investigation_schema",
            crate::report::investigate::analyze::investigation_schema(8),
        ),
    ]
}

/// Every schema the crate sends satisfies OpenAI strict mode, recursively.
///
/// Why: strict mode is checked at EVERY object node, including the objects
/// under an array's `items` — the node the provider named when it rejected
/// `report_synthesis`. A check of only the top-level object passes on a schema
/// that a strict provider rejects outright.
/// What: walks each enumerated schema with `assert_object_nodes_strict`, which
/// asserts `additionalProperties: false` and `required` == every property key
/// on each object node, descending through `properties`, `items`, and an
/// object-valued `additionalProperties`.
/// Test: this test itself.
#[test]
fn every_sent_schema_is_openai_strict_compliant() {
    for (name, spec) in all_sent_schemas() {
        assert_object_nodes_strict(&spec.schema);
        assert!(
            !spec.name.is_empty(),
            "{name}: schema name must be a non-empty identifier"
        );
    }
}

/// The exact node and property the live provider rejected in #5675.
///
/// Why: `every_sent_schema_is_openai_strict_compliant` would fail on this too,
/// but a generic recursive failure does not say which report this broke. This
/// pins the reported shape so a regression names itself.
/// What: reaches `properties.findings.items` of `report_synthesis` and asserts
/// the two things the provider demanded — `additionalProperties: false`, and
/// every declared property listed in `required` (the original listed 5 of 9).
/// Test: this test itself.
#[test]
fn synthesis_findings_items_is_strict() {
    let spec = crate::report::synthesize_prompt::synthesis_schema(5);
    let items = spec
        .schema
        .pointer("/properties/findings/items")
        .expect("report_synthesis must declare properties.findings.items");

    assert_eq!(
        items.get("additionalProperties"),
        Some(&serde_json::Value::Bool(false)),
        "#5675: findings.items must set additionalProperties:false"
    );

    let props: Vec<&String> = items
        .get("properties")
        .and_then(serde_json::Value::as_object)
        .expect("findings.items must declare properties")
        .keys()
        .collect();
    let required: Vec<&str> = items
        .get("required")
        .and_then(serde_json::Value::as_array)
        .expect("findings.items must declare required")
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect();
    for key in props {
        assert!(
            required.contains(&key.as_str()),
            "#5675: findings.items property {key:?} missing from required"
        );
    }
}

/// Production source may not build a [`ResponseSchema`] by struct literal, and
/// the enumeration above must list every builder that exists.
///
/// Why: the two tests above prove the schemas that are enumerated are
/// compliant. This one is what makes a FIFTH schema compliant by construction
/// rather than by someone remembering: a struct literal skips
/// `ResponseSchema::new` and therefore skips `enforce_strict_mode`, which is
/// precisely how `synthesis_schema` and `investigation_schema` were written.
/// What: scans the crate's production `.rs` files (test files excluded per the
/// repo's classification), asserting no `ResponseSchema {` construction outside
/// `llm/mod.rs` (which defines the type), and that the number of
/// `-> ResponseSchema` builders equals `all_sent_schemas().len()`.
/// Test: this test itself.
#[test]
fn schema_enumeration_is_complete_and_nothing_bypasses_new() {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut builders: Vec<String> = Vec::new();
    let mut literals: Vec<String> = Vec::new();

    for file in production_rs_files(&src) {
        let rel = file
            .strip_prefix(&src)
            .unwrap_or(&file)
            .to_string_lossy()
            .replace('\\', "/");
        // `llm/mod.rs` declares `pub struct ResponseSchema {` and the
        // constructor itself; it is the one place the literal is legitimate.
        let defines_the_type = rel == "llm/mod.rs";
        let text = std::fs::read_to_string(&file).expect("read crate source");

        for line in text.lines() {
            let code = line.trim_start();
            if code.starts_with("//") {
                continue;
            }
            // A builder's own signature ends `-> ResponseSchema {`, which is not
            // a struct literal — classify it first so it is not counted as one.
            let is_signature = code.contains("-> ResponseSchema");
            if is_signature {
                builders.push(format!("{rel}: {}", code.trim()));
            } else if !defines_the_type && code.contains("ResponseSchema {") {
                literals.push(format!("{rel}: {}", code.trim()));
            }
        }
    }

    assert!(
        literals.is_empty(),
        "production code must build schemas via ResponseSchema::new (it applies \
         enforce_strict_mode); struct literals found:\n  {}",
        literals.join("\n  ")
    );

    assert_eq!(
        builders.len(),
        all_sent_schemas().len(),
        "all_sent_schemas() must list every `-> ResponseSchema` builder; found:\n  {}",
        builders.join("\n  ")
    );
}

/// Collect the crate's production `.rs` files under `dir`.
///
/// What: recurses `dir`, keeping `.rs` files that are NOT test files by the
/// repo's rule — basename `tests.rs`, a `_test.rs` / `_tests.rs` suffix, or a
/// `/tests/` path segment.
fn production_rs_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let entries = std::fs::read_dir(dir).expect("read crate src dir");
    for entry in entries {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "tests") {
                continue;
            }
            out.extend(production_rs_files(&path));
            continue;
        }
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if !name.ends_with(".rs")
            || name == "tests.rs"
            || name.ends_with("_test.rs")
            || name.ends_with("_tests.rs")
        {
            continue;
        }
        out.push(path);
    }
    out
}
