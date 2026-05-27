//! Rule and rule-set data structures.

use serde::{Deserialize, Serialize};

/// A single classification rule.
///
/// Why: rules are the unit of declarative classification — they decouple
/// the matcher from policy so a user-supplied rule file can extend the
/// behaviour without changing code.
/// What: bundles keywords (Tier 1 exact match), patterns (Tier 2 regex),
/// the produced `category` / `subcategory`, a `priority` for tie-breaking,
/// and a `confidence` (0.0–1.0).
/// Test: covered by `classify::tests::exact_matcher_classifies_*` and
/// `regex_matcher_*` test cases.
///
/// A rule matches when **any** of its `keywords` is present in the commit
/// message (Tier 1) or **any** of its `patterns` matches (Tier 2). The
/// resulting verdict carries the rule's `category`, `subcategory`, and
/// `confidence`.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    #[serde(default)]
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
/// Test: covered by `load_rules` round-trip in `classify::tests`.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
