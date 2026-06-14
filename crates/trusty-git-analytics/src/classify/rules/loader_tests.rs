use super::*;
use std::io::Write;

/// Write a temporary YAML file and load it, returning the parsed RuleSet.
///
/// Why: `load_rules` requires a real file path; this helper avoids
/// duplicating the tempfile boilerplate in every test.
/// What: writes `content` to a `.yaml` temp file and calls `load_rules`.
/// Test: only used as an internal helper — the callers are the real tests.
fn load_yaml(content: &str) -> crate::classify::errors::Result<RuleSet> {
    let mut f = tempfile::NamedTempFile::with_suffix(".yaml").expect("create temp file");
    f.write_all(content.as_bytes()).expect("write yaml");
    load_rules(f.path())
}

/// A rule file using singular `pattern:` key loads successfully and
/// produces the correct `patterns` vec — the core #259 regression test.
///
/// Why: load_rules must handle the full file-parse path (not just the
/// struct-level serde deserialization) for the singular alias to be
/// observable end-to-end.
/// What: writes a two-rule YAML (one with `pattern:`, one with
/// `patterns:`) to a temp file, calls `load_rules`, and asserts that
/// both rules have non-empty `patterns`.
/// Test: passes when the `#[serde(alias = "pattern")]` and
/// `deserialize_with` are in place; fails on the old unaliased field.
#[test]
fn load_rules_singular_pattern_key_accepted() {
    let yaml = r#"
rules:
  - id: singular-test
    category: new_feature
    pattern: "(?i)^feat[:(]"
  - id: plural-test
    category: bugfix
    patterns:
      - "(?i)^fix[:(]"
      - "(?i)^bug[:(]"
"#;
    let set = load_yaml(yaml).expect("load");
    let singular = set.rules.iter().find(|r| r.id == "singular-test").unwrap();
    assert_eq!(
        singular.patterns,
        vec!["(?i)^feat[:(]".to_string()],
        "singular `pattern:` must load as a single-element vec"
    );
    let plural = set.rules.iter().find(|r| r.id == "plural-test").unwrap();
    assert_eq!(plural.patterns.len(), 2);
}

/// A rule with no keywords AND no patterns emits a tracing WARN.
///
/// Why: silent "all uncategorized" results are the hardest bugs to
/// diagnose; the warn gives users a direct pointer to the issue (#259).
/// What: loads a rule file containing one zero-matcher rule and one
/// valid rule, then uses `tracing_test` to assert the warn is emitted
/// exactly for the zero-matcher rule's id.
/// Test: `logs_contain` checks the warn message text; the valid rule
/// must NOT produce a warn.
#[test]
#[tracing_test::traced_test]
fn load_rules_warns_on_empty_matchers() {
    let yaml = r#"
rules:
  - id: no-matchers
    category: custom_category
  - id: has-keyword
    category: bugfix
    keywords:
      - "fix:"
"#;
    let set = load_yaml(yaml).expect("load");
    assert_eq!(set.rules.len(), 2);
    // The warn must mention the rule id of the empty-matcher rule.
    assert!(
        logs_contain("no-matchers"),
        "expected warn log containing the rule id `no-matchers`"
    );
    // The rule with keywords must NOT trigger the warn.
    assert!(
        !logs_contain("has-keyword"),
        "rule `has-keyword` should not warn — it has a keyword"
    );
}

/// End-to-end: load a singular-`pattern:` rule file and classify a
/// commit that should match it, asserting the custom category appears.
///
/// Why: verifies that the fix propagates through the full classification
/// pipeline — load_rules → ClassificationEngine → ClassificationResult.
/// If patterns were still silently dropped, the rule would never fire
/// and the result would fall through to the catch-all.
/// What: builds a ClassificationEngine from a user rules file with
/// `pattern:` (singular), classifies `"feat: add login"`, and asserts
/// `category == "new_feature"` (the custom name, not the built-in
/// `"feature"` from the default ruleset).
/// Test: this is the closest equivalent of the QA repro documented in
/// #259 — standalone rules file, singular pattern, custom category name.
#[test]
fn end_to_end_singular_pattern_rule_fires() {
    use crate::classify::classifier::{ClassificationEngine, ClassificationEngineConfig};

    let yaml = r#"
extend_defaults: false
rules:
  - id: custom-feat
    category: new_feature
    pattern: "(?i)^feat[:(]"
"#;
    let set = load_yaml(yaml).expect("load");
    let engine = ClassificationEngine::new(
        set,
        ClassificationEngineConfig {
            use_llm: false,
            ..Default::default()
        },
    )
    .expect("engine");

    let result = engine
        .classify_sync("feat: add login flow", false)
        .expect("classified");

    assert_eq!(
        result.category, "new_feature",
        "singular `pattern:` rule must fire and produce custom category"
    );
}

/// A rule file with `patterns:` (plural list) also works end-to-end.
///
/// Why: regression guard — the custom deserializer must not break the
/// existing plural form that all prior users had.
/// What: same as `end_to_end_singular_pattern_rule_fires` but uses
/// `patterns:` with two entries.
/// Test: asserts the engine returns the custom category for both patterns.
#[test]
fn end_to_end_plural_patterns_rule_fires() {
    use crate::classify::classifier::{ClassificationEngine, ClassificationEngineConfig};

    let yaml = r#"
extend_defaults: false
rules:
  - id: custom-feat
    category: new_feature
    patterns:
      - "(?i)^feat[:(]"
      - "(?i)^feature[:(]"
"#;
    let set = load_yaml(yaml).expect("load");
    let engine = ClassificationEngine::new(
        set,
        ClassificationEngineConfig {
            use_llm: false,
            ..Default::default()
        },
    )
    .expect("engine");

    for msg in ["feat: add login", "feature: dark mode"] {
        let result = engine.classify_sync(msg, false).expect("classified");
        assert_eq!(
            result.category, "new_feature",
            "plural `patterns:` rule must fire for `{msg}`"
        );
    }
}
