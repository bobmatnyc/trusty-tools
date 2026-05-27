//! Rule and rule-set data structures.

use serde::{Deserialize, Deserializer, Serialize};

/// Deserialize the `patterns` / `pattern` field from a YAML rule entry.
///
/// Why: user-authored rules YAMLs naturally write `pattern: "(?i)^feat"` (singular
/// string) while the struct field is `Vec<String>`. Without this deserializer
/// `serde_yaml` rejects the scalar value when the target type is a sequence, and
/// because the field is `#[serde(default)]` the error is silently swallowed,
/// leaving `patterns = []`. That is the root cause of issue #259 — rules with an
/// empty pattern list never fire, causing 100% "uncategorized" results. This
/// function handles three cases so neither existing nor new YAML files break:
///
/// - Absent key (field omitted) → `vec![]`
/// - Single string (`pattern: "foo"`) → `vec!["foo"]`
/// - Sequence (`patterns: ["foo", "bar"]`) → `vec!["foo", "bar"]`
///
/// What: deserializes via an `#[serde(untagged)]` enum that matches all three
/// shapes, then maps each variant to the appropriate `Vec<String>`.
/// Test: covered by `rule_singular_pattern_deserializes`,
/// `rule_plural_patterns_deserializes`, and `rule_missing_patterns_field_gives_empty_vec`
/// in `classify::rules::types::tests`, and by the end-to-end tests in
/// `classify::rules::loader::tests`.
fn deserialize_patterns_field<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrVecOrNull {
        Single(String),
        Many(Vec<String>),
        Null,
    }
    match StringOrVecOrNull::deserialize(deserializer)? {
        StringOrVecOrNull::Single(s) => Ok(vec![s]),
        StringOrVecOrNull::Many(v) => Ok(v),
        StringOrVecOrNull::Null => Ok(vec![]),
    }
}

/// A single classification rule.
///
/// Why: rules are the unit of declarative classification — they decouple
/// the matcher from policy so a user-supplied rule file can extend the
/// behaviour without changing code.
/// What: bundles keywords (Tier 1 exact match), patterns (Tier 2 regex),
/// the produced `category` / `subcategory`, a `priority` for tie-breaking,
/// and a `confidence` (0.0–1.0).
/// Test: covered by `classify::tests::exact_matcher_classifies_*` and
/// `regex_matcher_*` test cases; unknown-field rejection covered by
/// `tests::rule_unknown_field_is_rejected`.
///
/// A rule matches when **any** of its `keywords` is present in the commit
/// message (Tier 1) or **any** of its `patterns` matches (Tier 2). The
/// resulting verdict carries the rule's `category`, `subcategory`, and
/// `confidence`.
///
/// `deny_unknown_fields` closes the class of silent-drop bugs seen in
/// issues #259 (`pattern:` vs `patterns:`) and #286 (`email_env:`). Any
/// YAML key that is not a recognised field name (or alias) causes a loud
/// parse error at load time rather than being silently ignored.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// Unique rule identifier (used in logs and overrides).
    pub id: String,

    /// **Subcategory name** in the two-level taxonomy (e.g. `"feature"`,
    /// `"bugfix"`, `"security"`). Resolved to its top-level parent via
    /// the [`crate::classify::taxonomy::TaxonomyRegistry`]. The field name
    /// `category` is retained for DB-schema compatibility with the Python
    /// predecessor.
    pub category: String,

    /// Optional leaf label, more specific than `category`
    /// (e.g. `"sql-injection"` under `category: "security"`).
    #[serde(default)]
    pub subcategory: Option<String>,

    /// Exact keywords to match against the commit message
    /// (case-insensitive, substring match).
    #[serde(default)]
    pub keywords: Vec<String>,

    /// Regex patterns to match against the commit message.
    ///
    /// Accepts both the singular YAML key `pattern:` (a string or a list of strings)
    /// and the plural key `patterns:` (a list of strings). User-authored rule files
    /// naturally reach for `pattern: "(?i)^feat"` which, without this alias, would be
    /// silently dropped by serde because the target type is `Vec<String>`. The
    /// `deserialize_with` helper coerces a scalar string to a single-element vec.
    ///
    /// **Either form is accepted:**
    ///
    /// ```yaml
    /// pattern: "(?i)^feat[:(]"            # singular string → vec!["(?i)^feat[:(]"]
    /// patterns: ["(?i)^feat", "(?i)^feature"]  # plural list  → vec as-is
    /// ```
    #[serde(
        default,
        alias = "pattern",
        deserialize_with = "deserialize_patterns_field"
    )]
    pub patterns: Vec<String>,

    /// Priority (higher = checked first).
    ///
    /// Defaults to `110` — one step above the highest built-in rule priority
    /// (`cc-revert` at `115` is the sole exception, so custom rules
    /// consistently win over the entire conventional-commit tier at `100`
    /// without needing an explicit priority in every YAML entry).
    /// Before this fix (issue #259) the default was `0`, which meant every
    /// built-in rule silently won over user-supplied rules.
    #[serde(default = "default_rule_priority")]
    pub priority: i32,

    /// Confidence score assigned when this rule matches (0.0–1.0).
    #[serde(default = "default_confidence")]
    pub confidence: f64,
}

fn default_confidence() -> f64 {
    0.85
}

/// Why: user-supplied rules should win over built-in rules by default so
/// `--rules` actually does something without requiring `priority:` on every
/// entry. Built-in conventional-commit rules peak at 100 (`cc-feat`, `cc-fix`);
/// the only exception is `cc-revert` at 115, which is intentional. Defaulting
/// custom rules to 110 places them above the entire cc-* family.
/// What: returns 110 as the default priority for user-supplied rules.
/// Test: see `classify::tests::custom_rule_priority_default_beats_builtin` in
/// `src/classify/mod.rs`.
fn default_rule_priority() -> i32 {
    110
}

/// A collection of rules loaded from a file or built into the binary.
///
/// Why: rule files often want to inherit the built-in default set and
/// only add or override specific entries; `extend_defaults` makes that
/// composition explicit at the file level.
/// What: holds the loaded version tag, the extend-defaults flag, and the
/// vector of [`Rule`]s. Rules are not pre-sorted — use [`Self::by_priority`].
/// Test: covered by `load_rules` round-trip in `classify::tests`;
/// unknown-field rejection covered by `tests::rule_set_unknown_field_is_rejected`.
///
/// `deny_unknown_fields` prevents silent YAML typos from going unnoticed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleSet {
    /// Optional schema version for forward compatibility.
    #[serde(default)]
    pub version: Option<String>,

    /// When `true`, custom rules are merged on top of the built-in default
    /// ruleset. Custom rules that share an `id` with a default rule override
    /// that rule. Set to `true` explicitly to extend rather than replace the
    /// built-in ruleset.
    ///
    /// **Defaults to `false`** (issue #259 fix). A user who supplies a rules
    /// file intends those rules to be the complete ruleset; if they want the
    /// built-ins too, they must opt in with `extend_defaults: true`. Before
    /// this fix the default was `true`, which meant user rules were always
    /// mixed with the built-in 100+ rules, so custom categories never fired
    /// unless the user had `extend_defaults: false` in every file.
    #[serde(default)]
    pub extend_defaults: bool,

    /// All rules in this set. Order is not significant; see [`Rule::priority`].
    pub rules: Vec<Rule>,
}

impl RuleSet {
    /// Return rules sorted by descending priority.
    ///
    /// Why: the cascade evaluates the highest-priority rules first so a
    /// leading `feat(api)!:` beats a stray later `bug` keyword; centralising
    /// the sort here keeps every consumer aligned.
    /// What: returns a `Vec<&Rule>` ordered by descending `priority`;
    /// stable sort preserves declaration order for ties.
    /// Test: covered by `default_rules_is_non_empty` which iterates the
    /// sorted vector.
    pub fn by_priority(&self) -> Vec<&Rule> {
        let mut refs: Vec<&Rule> = self.rules.iter().collect();
        refs.sort_by_key(|r| std::cmp::Reverse(r.priority));
        refs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Singular `pattern:` key with a string value deserializes into a
    /// single-element `patterns` vec.
    ///
    /// Why: this is the primary regression guarded by issue #259. User YAMLs
    /// (and the issue body itself) write `pattern: "..."` (natural English
    /// singular). Before the fix serde silently dropped the unknown key,
    /// giving `patterns = []` — so rules never fired.
    /// What: parses a minimal YAML rule with `pattern:` (singular string) and
    /// asserts that `rule.patterns` has exactly one element matching the input.
    /// Test: this test IS the regression test — watch it fail against the old
    /// `patterns: Vec<String>` field (no alias, no custom deserializer).
    #[test]
    fn rule_singular_pattern_deserializes() {
        let yaml = r#"
id: test-1
category: new_feature
pattern: "(?i)^feat"
"#;
        let rule: Rule = serde_yaml::from_str(yaml).expect("deserialize");
        assert_eq!(
            rule.patterns,
            vec!["(?i)^feat".to_string()],
            "singular `pattern:` must be coerced to a single-element vec"
        );
    }

    /// Plural `patterns:` key with a YAML list deserializes unchanged.
    ///
    /// Why: existing rule files that already use `patterns:` (plural) must
    /// continue to work after adding the singular alias and custom deserializer.
    /// What: parses a minimal YAML rule with `patterns:` as a two-element list
    /// and asserts both elements are preserved in order.
    /// Test: verify the happy path was not broken by the alias addition.
    #[test]
    fn rule_plural_patterns_deserializes() {
        let yaml = r#"
id: test-2
category: new_feature
patterns:
  - "(?i)^feat"
  - "(?i)^feature"
"#;
        let rule: Rule = serde_yaml::from_str(yaml).expect("deserialize");
        assert_eq!(rule.patterns.len(), 2);
        assert_eq!(rule.patterns[0], "(?i)^feat");
        assert_eq!(rule.patterns[1], "(?i)^feature");
    }

    /// A rule with neither `pattern:` nor `patterns:` gives an empty vec
    /// (not a parse error).
    ///
    /// Why: rules that match only on keywords have no patterns field at all;
    /// this must remain valid and produce `patterns = []`.
    /// What: parses a rule with only `keywords:` and asserts `patterns` is
    /// empty and the parse succeeds.
    /// Test: ensures the `#[serde(default)]` fallback still works alongside
    /// the custom deserializer.
    #[test]
    fn rule_missing_patterns_field_gives_empty_vec() {
        let yaml = r#"
id: test-3
category: bugfix
keywords:
  - "fix:"
"#;
        let rule: Rule = serde_yaml::from_str(yaml).expect("deserialize");
        assert!(rule.patterns.is_empty());
        assert_eq!(rule.keywords, vec!["fix:".to_string()]);
    }

    /// A singular `pattern:` value still compiles as a valid regex that
    /// matches the intended commit message, exercising the full round-trip.
    ///
    /// Why: end-to-end check that the coerced string is not mangled — a
    /// deserialization that produces `patterns = [""]` would pass the
    /// structural check above but still silently mismatch everything.
    /// What: deserializes a rule, compiles its single pattern with the
    /// `regex` crate, and asserts it matches an expected commit message.
    /// Test: confirms the regex is intact after string→vec coercion.
    #[test]
    fn rule_singular_pattern_regex_compiles_and_matches() {
        let yaml = r#"
id: test-4
category: new_feature
pattern: "(?i)^feat[:(]"
"#;
        let rule: Rule = serde_yaml::from_str(yaml).expect("deserialize");
        assert_eq!(rule.patterns.len(), 1);
        let re = regex::Regex::new(&rule.patterns[0]).expect("compile");
        assert!(re.is_match("feat: add login flow"));
        assert!(re.is_match("feat(api): new endpoint"));
        assert!(!re.is_match("fix: null deref"));
    }

    /// Why: `deny_unknown_fields` on `Rule` must turn a field typo (e.g.
    /// `method: regex_rule` — user's incidental bug from QA repro #286) into
    /// a loud parse error at load time instead of silently being ignored.
    /// What: attempt to deserialize a rule with `method: regex_rule` and
    /// assert the result is `Err`.
    /// Test: pure deserialization regression guard.
    #[test]
    fn rule_unknown_field_is_rejected() {
        let yaml = r#"
id: test-5
category: bug_fix
keywords: ["bugfix:"]
method: regex_rule
"#;
        let result: Result<Rule, _> = serde_yaml::from_str(yaml);
        assert!(
            result.is_err(),
            "Rule with unknown `method:` field must be rejected at parse time"
        );
    }

    /// Why: `deny_unknown_fields` on `RuleSet` must reject a YAML typo in
    /// the outer envelope (e.g. `extends_defaults:` instead of
    /// `extend_defaults:`).
    /// What: attempt to deserialize a rule set with a misspelled top-level
    /// key and assert the result is `Err`.
    /// Test: pure deserialization regression guard.
    #[test]
    fn rule_set_unknown_field_is_rejected() {
        let yaml = r#"
extends_defaults: false
rules:
  - id: my-rule
    category: bug_fix
    keywords: ["bugfix:"]
"#;
        let result: Result<RuleSet, _> = serde_yaml::from_str(yaml);
        assert!(
            result.is_err(),
            "RuleSet with unknown `extends_defaults:` (typo) must be rejected"
        );
    }
}
