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
/// Test: consumed by every test in this file;
/// `schema_enumeration_is_complete_and_nothing_bypasses_new` fails if a
/// builder is missing from it.
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
            crate::report::synthesize_prompt::synthesis_schema(5, 10),
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
    let spec = crate::report::synthesize_prompt::synthesis_schema(5, 10);
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

/// Schema builders and raw constructions found in one source file.
struct SchemaSites {
    /// Signatures of functions that return a `ResponseSchema`, `fn new` aside.
    builders: Vec<String>,
    /// Constructions that skip `ResponseSchema::new`, and so skip strict mode.
    constructions: Vec<String>,
}

/// Find every `ResponseSchema` builder and raw construction in one file.
///
/// Why: the first version of this scan matched only the literal strings
/// `-> ResponseSchema` and `ResponseSchema {`, and so was blind to the idiom
/// the codebase actually uses — `fn foo(..) -> Self { Self { .. } }` inside an
/// `impl ResponseSchema` block, which is how `ResponseSchema::new` itself is
/// written. A fifth builder in that style was invisible to both checks, which
/// is a detector that fails OPEN on exactly the case it exists to catch. This
/// repo's rule for a shared line-based detector (CLAUDE.md, on
/// `scripts/lib/sloc_awk.sh`) is that it must fail CLOSED.
/// What: tracks brace depth to know when it is inside an `impl` block naming
/// `ResponseSchema` and inside `fn new`'s body. A builder is any signature
/// returning `ResponseSchema` outright, or returning `Self` inside such an
/// `impl`. A construction is `ResponseSchema {` or — inside such an `impl` —
/// `Self {`, excluding the struct definition, the `impl` header, and the body
/// of `fn new`, which is the one place construction is legitimate. Bodies of
/// `#[cfg(test)] mod … {` are skipped: a mock schema in a test needs no strict
/// pass.
/// Test: `scan_detects_self_returning_builder`,
/// `scan_allows_only_the_constructor_body`.
fn scan_source(rel: &str, text: &str) -> SchemaSites {
    let mut builders = Vec::new();
    let mut constructions = Vec::new();
    let mut depth: i32 = 0;
    let mut impl_start: Option<i32> = None;
    let mut new_start: Option<i32> = None;
    let mut cfg_test_pending = false;
    let mut cfg_test_start: Option<i32> = None;

    for line in text.lines() {
        let code = line.trim_start();
        let opens = code.matches('{').count() as i32;
        let closes = code.matches('}').count() as i32;
        let next_depth = depth + opens - closes;

        // Comments never construct anything and never move depth.
        if code.starts_with("//") {
            continue;
        }

        if let Some(start) = cfg_test_start {
            depth = next_depth;
            if depth < start {
                cfg_test_start = None;
            }
            continue;
        }
        if code.starts_with("#[cfg(test)]") {
            cfg_test_pending = true;
            depth = next_depth;
            continue;
        }
        if code.starts_with("#[") {
            // Another attribute between `#[cfg(test)]` and its item.
            depth = next_depth;
            continue;
        }
        if cfg_test_pending {
            cfg_test_pending = false;
            if code.starts_with("mod ") && opens > 0 {
                cfg_test_start = Some(next_depth);
                depth = next_depth;
                continue;
            }
        }

        let in_impl = impl_start.is_some_and(|d| depth >= d);
        let in_new = new_start.is_some_and(|d| depth >= d);

        let is_impl_header = code.contains("impl ") && code.contains("ResponseSchema");
        let is_struct_def = code.contains("struct ResponseSchema");
        let is_new_fn = code.contains("fn new");
        let is_builder =
            code.contains("-> ResponseSchema") || (in_impl && code.contains("-> Self"));
        let is_construction = !is_impl_header
            && !is_struct_def
            && !is_builder
            && (code.contains("ResponseSchema {") || (in_impl && code.contains("Self {")));

        if is_builder && !is_new_fn {
            builders.push(format!("{rel}: {}", code.trim()));
        }
        if is_construction && !in_new {
            constructions.push(format!("{rel}: {}", code.trim()));
        }

        if is_impl_header && opens > 0 {
            impl_start = Some(next_depth);
        }
        if in_impl && is_new_fn && opens > 0 {
            new_start = Some(next_depth);
        }

        depth = next_depth;
        if impl_start.is_some_and(|d| depth < d) {
            impl_start = None;
        }
        if new_start.is_some_and(|d| depth < d) {
            new_start = None;
        }
    }

    SchemaSites {
        builders,
        constructions,
    }
}

/// Production source may not build a [`ResponseSchema`] outside
/// `ResponseSchema::new`, and the enumeration above must list every builder.
///
/// Why: the two tests above prove the schemas that ARE enumerated are
/// compliant. This one is what makes a FIFTH schema compliant by construction
/// rather than by someone remembering: any construction that skips
/// `ResponseSchema::new` skips `enforce_strict_mode`, which is precisely how
/// `synthesis_schema` and `investigation_schema` were written.
/// What: runs [`scan_source`] over the crate's production `.rs` files (test
/// files excluded per the repo's classification) and requires no construction
/// outside `fn new`, and a builder count equal to `all_sent_schemas().len()`.
/// Test: this test itself; [`scan_source`]'s own behaviour is pinned by
/// `scan_detects_self_returning_builder` and
/// `scan_allows_only_the_constructor_body`.
#[test]
fn schema_enumeration_is_complete_and_nothing_bypasses_new() {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut builders: Vec<String> = Vec::new();
    let mut constructions: Vec<String> = Vec::new();

    for file in production_rs_files(&src) {
        let rel = file
            .strip_prefix(&src)
            .unwrap_or(&file)
            .to_string_lossy()
            .replace('\\', "/");
        let text = std::fs::read_to_string(&file).expect("read crate source");
        let sites = scan_source(&rel, &text);
        builders.extend(sites.builders);
        constructions.extend(sites.constructions);
    }

    assert!(
        constructions.is_empty(),
        "production code must build schemas via ResponseSchema::new (it applies \
         enforce_strict_mode); constructions found:\n  {}",
        constructions.join("\n  ")
    );

    assert_eq!(
        builders.len(),
        all_sent_schemas().len(),
        "all_sent_schemas() must list every ResponseSchema builder; found:\n  {}",
        builders.join("\n  ")
    );
}

/// The idiom the previous scan was blind to is detected.
///
/// Why: a fifth builder written the way `ResponseSchema::new` is written —
/// `-> Self` returning `Self { .. }` inside `impl ResponseSchema` — matched
/// neither literal the old scan looked for, so it bypassed `enforce_strict_mode`
/// with every test green. This pins the fix without leaving a non-compliant
/// builder in the tree.
/// What: scans a synthetic source carrying both `fn new` and a sneak builder in
/// that idiom, and asserts only the sneak builder is reported.
/// Test: this test itself.
#[test]
fn scan_detects_self_returning_builder() {
    let sites = scan_source(
        "llm/mod.rs",
        r#"
pub struct ResponseSchema {
    pub name: String,
}
impl ResponseSchema {
    pub fn new(name: String, mut schema: Value) -> Self {
        enforce_strict_mode(&mut schema);
        Self { name, schema }
    }

    pub fn sneaky(cap: usize) -> Self {
        Self { name: "x".to_string(), schema: json!({}) }
    }
}
"#,
    );

    assert_eq!(
        sites.builders.len(),
        1,
        "the `-> Self` builder must be counted (fn new excepted): {:?}",
        sites.builders
    );
    assert!(
        sites.builders[0].contains("sneaky"),
        "wrong builder reported: {:?}",
        sites.builders
    );
    assert_eq!(
        sites.constructions.len(),
        1,
        "`Self {{ .. }}` outside fn new must be reported: {:?}",
        sites.constructions
    );
    assert!(
        sites.constructions[0].contains("\"x\""),
        "wrong construction reported: {:?}",
        sites.constructions
    );
}

/// The struct definition, the `impl` header, `fn new`'s body, and test modules
/// are not reported.
///
/// Why: the previous scan exempted ALL of `llm/mod.rs` from the construction
/// check, so a raw literal anywhere else in that file evaded detection too. The
/// exemption is now the constructor body, not the file — this pins that the
/// narrowing did not turn the legitimate sites into false positives.
/// What: scans a synthetic source holding each legitimate shape plus a
/// `#[cfg(test)]` module that builds a mock schema, and asserts nothing is
/// reported.
/// Test: this test itself.
#[test]
fn scan_allows_only_the_constructor_body() {
    let sites = scan_source(
        "llm/mod.rs",
        r#"
pub struct ResponseSchema {
    pub name: String,
}
impl ResponseSchema {
    pub fn new(name: String, mut schema: Value) -> Self {
        enforce_strict_mode(&mut schema);
        Self { name, schema }
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn mock() {
        let s = ResponseSchema { name: "m".to_string(), schema: json!({}) };
    }
}
"#,
    );

    assert!(
        sites.constructions.is_empty(),
        "legitimate sites must not be reported: {:?}",
        sites.constructions
    );
    assert!(
        sites.builders.is_empty(),
        "fn new is the constructor, not a schema builder: {:?}",
        sites.builders
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
