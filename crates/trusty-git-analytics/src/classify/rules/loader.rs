//! Rule file loader and built-in default ruleset.

use std::path::Path;

use crate::classify::errors::{ClassifyError, Result};
use crate::classify::rules::types::{Rule, RuleSet};

/// Load a [`RuleSet`] from a YAML or JSON file.
///
/// Why: deployments often need to layer project-specific rules on top of the
/// built-in ruleset; loading from disk decouples the binary from the rule
/// definitions.
/// What: detects format by extension (`.json` → JSON, anything else → YAML),
/// deserializes into [`RuleSet`], and rejects empty rule lists so config
/// mistakes are surfaced loudly.
/// Test: see `tga::classify::tests` (round-trips serialization).
///
/// # Errors
///
/// - [`ClassifyError::Io`] if the file cannot be read.
/// - [`ClassifyError::Yaml`] / [`ClassifyError::Json`] on parse failure.
pub fn load_rules(path: &Path) -> Result<RuleSet> {
    let text = std::fs::read_to_string(path)?;
    let is_json = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("json"))
        .unwrap_or(false);

    let set: RuleSet = if is_json {
        serde_json::from_str(&text)?
    } else {
        serde_yaml::from_str(&text)?
    };

    if set.rules.is_empty() {
        return Err(ClassifyError::RuleLoad(format!(
            "rule file {} contained no rules",
            path.display()
        )));
    }

    // Warn about rules that will never match — a common symptom of the singular
    // `pattern:` vs plural `patterns:` naming confusion (issue #259). After the
    // fix the deserializer handles both forms, but this guard catches other cases
    // (e.g. a rule with neither keywords nor patterns) and gives a clear diagnostic
    // so users don't spend time debugging silent "all uncategorized" results.
    for rule in &set.rules {
        if rule.keywords.is_empty() && rule.patterns.is_empty() {
            tracing::warn!(
                rule_id = %rule.id,
                category = %rule.category,
                "rule has no keywords or patterns and will never match — \
                 check YAML field names (use `pattern:` for a single regex or \
                 `patterns:` for a list; both are accepted)"
            );
        }
    }

    Ok(set)
}

/// Return the built-in default ruleset.
///
/// Why: a comprehensive baseline ruleset keeps the "uncategorized" rate low
/// without an LLM. The set is assembled from named category helpers so each
/// group can be audited or revised in isolation.
/// What: concatenates rule lists from the per-category builders below and
/// wraps them in a [`RuleSet`] with `extend_defaults = true`.
/// Test: `crate::classify::tests::default_rules_is_non_empty` and the
/// corpus smoke test `corpus_uncategorized_below_1_percent` cover behaviour.
///
/// Covers (in order of inclusion):
///
/// - Conventional commit prefixes (`feat:`, `fix:`, …) — see
///   `default_rules_a`.
/// - Breaking-change marker, merge/revert plumbing, initial/release rules,
///   and dependency rules — see `default_rules_a`.
/// - Code-review, cleanup, infra, and generic-keyword rules — see
///   `default_rules_b`.
/// - Cloud, observability, datastore, messaging, networking — see
///   `default_rules_c`.
/// - Language tooling, PR hygiene, experiment, translation, documentation,
///   content, generic prose, ticket references, and catch-all — see
///   `default_rules_d`.
pub fn default_rules() -> RuleSet {
    use super::default_rules_a::*;
    use super::default_rules_b::*;
    use super::default_rules_c::*;
    use super::default_rules_d::*;

    let mut rules: Vec<Rule> = Vec::new();
    rules.extend(conventional_commit_rules());
    rules.extend(breaking_change_rules());
    rules.extend(merge_plumbing_rules());
    rules.extend(initial_and_release_rules());
    rules.extend(dependency_rules());
    rules.extend(code_review_and_cleanup_rules());
    rules.extend(infra_rules());
    rules.extend(generic_keyword_rules());
    rules.extend(cloud_platform_rules());
    rules.extend(observability_rules());
    rules.extend(datastore_rules());
    rules.extend(messaging_rules());
    rules.extend(networking_rules());
    rules.extend(language_tooling_rules());
    rules.extend(pr_hygiene_rules());
    rules.extend(experiment_and_rollback_rules());
    rules.extend(auto_generated_plumbing_rules());
    rules.extend(translation_rules());
    rules.extend(documentation_meta_rules());
    rules.extend(content_and_assets_rules());
    rules.extend(generic_prose_rules());
    rules.extend(ticket_reference_rules());
    rules.push(catch_all_rule());

    RuleSet {
        version: Some("1.0".into()),
        extend_defaults: true,
        rules,
    }
}
