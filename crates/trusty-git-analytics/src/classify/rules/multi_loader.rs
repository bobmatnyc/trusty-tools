//! Multi-file rule loader and repo-category fallback (#445 batch C).
//!
//! ## Supplemental rule files (`rules_files`)
//!
//! [`ClassificationConfig::rules_files`] accepts either a single path
//! (backward-compat alias `rules_file`) or a list of paths. Files are loaded
//! **in order**: each file's [`RuleSet`] is merged into the accumulator via
//! [`RuleSet::merge`]. Later files win on conflicting rule IDs.
//!
//! This means operators can keep a base rules file and overlay project-specific
//! overrides without touching the shared file:
//!
//! ```yaml
//! classification:
//!   rules_files:
//!     - ~/shared/tga-base-rules.yaml
//!     - ./project-overrides.yaml   # wins over base on duplicate IDs
//! ```
//!
//! ## Repo-category fallback (`repo_categories`)
//!
//! [`ClassificationConfig::repo_categories`] is a map from repo name (or a
//! simple `*`-glob) to a default subcategory name. When the classification
//! cascade produces an `"uncategorized"` / `"catch-all"` verdict OR a verdict
//! below the configured `confidence_threshold`, and the commit's repository
//! matches one of the configured entries, the subcategory is replaced with the
//! configured default and the top-level category is resolved through the
//! taxonomy registry.
//!
//! **Precedence** (lowest to highest — later tiers win):
//! 1. Tier 0 manual overrides (highest)
//! 2. Exact-keyword / conventional-commit prefix rules (Tier 1)
//! 3. Regex tier (Tier 2)
//! 4. Weighted-sum tier (Tier 2.5)
//! 5. Fuzzy tier (Tier 3)
//! 6. Issue-type / JIRA-project / external-source tiers (Tier 1.5, 1.6, 1.7)
//! 7. LLM fallback (Tier 4)
//! 8. **`repo_categories` fallback** ← fires ONLY for uncategorized or
//!    confidence < threshold after the full cascade (Tier 5 / last resort)
//! 9. Catch-all rule result (uncategorized/maintenance, confidence 0.3)
//!
//! A confidently-classified commit is NEVER overridden by `repo_categories`.

use std::collections::HashMap;
use std::path::Path;

use crate::classify::errors::{ClassifyError, Result};
use crate::classify::rules::loader::load_rules;
use crate::classify::rules::types::RuleSet;
use crate::classify::tiers::ClassificationResult;

/// Load and merge multiple rule files in order.
///
/// Why: `rules_files` lets operators compose a base ruleset with
/// project-specific overrides without editing the shared file.
/// What: iterates `paths`, loads each via [`load_rules`], and merges via
/// [`RuleSet::merge`] (later files extend/override earlier). Returns the
/// merged [`RuleSet`]. Fails fast if any file cannot be loaded or parsed.
/// Test: `tests::multiple_files_merge_in_order` and
/// `tests::single_string_back_compat`.
///
/// # Errors
///
/// Returns [`ClassifyError::Io`] or parse errors from [`load_rules`] for any
/// file that cannot be loaded.
pub fn load_rules_multi(paths: &[&Path]) -> Result<RuleSet> {
    if paths.is_empty() {
        return Err(ClassifyError::RuleLoad(
            "rules_files is empty — at least one file is required".to_string(),
        ));
    }

    let mut merged: Option<RuleSet> = None;
    for path in paths {
        let set = load_rules(path)?;
        merged = Some(match merged {
            None => set,
            Some(acc) => acc.merge(set),
        });
    }

    // SAFETY: paths is non-empty, so the loop ran at least once.
    Ok(merged.expect("at least one file was loaded"))
}

/// Check whether a repo name matches a `repo_categories` key.
///
/// Why: repo_categories supports literal names and simple glob patterns (a
/// single `*` wildcard, no path separators).
/// What: if the key contains `*` it is treated as a glob where `*` matches
/// any sequence of characters (case-sensitive). Otherwise exact match.
/// Test: `tests::repo_glob_matching`.
pub fn repo_matches(repo_name: &str, pattern: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix('*') {
        if prefix.is_empty() {
            return true; // bare `*` matches everything
        }
        return repo_name.starts_with(prefix);
    }
    if let Some(suffix) = pattern.strip_prefix('*') {
        if suffix.is_empty() {
            return true;
        }
        return repo_name.ends_with(suffix);
    }
    // Midpoint wildcard: split into prefix + suffix.
    if let Some(pos) = pattern.find('*') {
        let prefix = &pattern[..pos];
        let suffix = &pattern[pos + 1..];
        return repo_name.starts_with(prefix) && repo_name.ends_with(suffix);
    }
    repo_name == pattern
}

/// Apply the `repo_categories` fallback tier to a classification result.
///
/// Why: reduces the 'uncategorized' rate for known repos without LLM cost.
/// What: when `result` is uncategorized OR its confidence is below
/// `confidence_threshold`, looks up the commit's `repo_name` in
/// `repo_categories` (literal then glob), and if matched, replaces the
/// subcategory with the configured default and resolves `top_level` via the
/// taxonomy (if a `top_level_for_subcategory` resolver is provided).
///
/// A confidently-classified non-uncategorized commit is **not** modified.
///
/// ## Precedence note
///
/// This is the last-resort fallback (Tier 5) — it fires only after every
/// other tier has already had a chance. The caller is responsible for ensuring
/// this function is invoked at the correct point in the cascade (after LLM,
/// before returning the final result).
///
/// Test: `tests::repo_category_applies_to_uncategorized`,
/// `tests::repo_category_skips_confident_result`, and
/// `tests::repo_category_glob_match`.
pub fn apply_repo_category_fallback(
    result: &mut ClassificationResult,
    repo_name: &str,
    repo_categories: &HashMap<String, String>,
    confidence_threshold: f64,
) {
    // Only fire when the result is "uncategorized" or low-confidence.
    let is_uncategorized = result.subcategory.as_deref() == Some("uncategorized")
        || result.category == "uncategorized"
        || result.category.is_empty();
    let is_low_confidence = result.confidence < confidence_threshold;

    if !is_uncategorized && !is_low_confidence {
        return; // Confident result — do not override.
    }

    // Find the first matching repo_categories entry (literal wins over glob).
    let matched_subcategory = repo_categories
        .get(repo_name)
        .or_else(|| {
            repo_categories
                .iter()
                .find(|(k, _)| k.contains('*') && repo_matches(repo_name, k))
                .map(|(_, v)| v)
        })
        .cloned();

    if let Some(subcategory) = matched_subcategory {
        tracing::debug!(
            repo = repo_name,
            old_category = %result.category,
            old_subcategory = ?result.subcategory,
            old_confidence = result.confidence,
            new_subcategory = %subcategory,
            "repo_categories fallback applied"
        );
        result.subcategory = Some(subcategory.clone());
        // Leave category as-is (the taxonomy resolver in the pipeline
        // will update top_level_category; we set category to subcategory
        // as a best-effort until taxonomy is consulted).
        result.category = subcategory;
        result.method = crate::core::models::ClassificationMethod::RepoCategoryFallback;
        // Confidence stays at the original value — the caller knows this
        // was a fallback (method == RepoCategoryFallback).
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_yaml(content: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::with_suffix(".yaml").expect("create temp file");
        f.write_all(content.as_bytes()).expect("write yaml");
        f
    }

    /// Why: multiple rule files must merge in order so that rules from later
    /// files extend and override earlier ones.
    /// What: file A has rule "rule-a"; file B has rule "rule-a" with higher
    /// confidence and a new rule "rule-b". After merge, rule-a from B wins
    /// and rule-b is present.
    /// Test: this test itself.
    #[test]
    fn multiple_files_merge_in_order() {
        let yaml_a = r#"
rules:
  - id: rule-a
    category: feature
    keywords: ["feat:"]
    confidence: 0.8
  - id: rule-c
    category: chore
    keywords: ["chore:"]
"#;
        let yaml_b = r#"
rules:
  - id: rule-a
    category: feature
    keywords: ["feat:", "feature:"]
    confidence: 0.95
  - id: rule-b
    category: bugfix
    keywords: ["fix:"]
"#;
        let file_a = write_yaml(yaml_a);
        let file_b = write_yaml(yaml_b);

        let merged = load_rules_multi(&[file_a.path(), file_b.path()]).expect("load");
        // rule-a from B (confidence 0.95) must win over A (0.8).
        let rule_a = merged
            .rules
            .iter()
            .find(|r| r.id == "rule-a")
            .expect("rule-a");
        assert!(
            (rule_a.confidence - 0.95).abs() < 1e-9,
            "later file must override earlier: got confidence {}",
            rule_a.confidence
        );
        // Both rule-b and rule-c must survive.
        assert!(
            merged.rules.iter().any(|r| r.id == "rule-b"),
            "rule-b from file B must be present"
        );
        assert!(
            merged.rules.iter().any(|r| r.id == "rule-c"),
            "rule-c from file A must be present"
        );
    }

    /// Why: a single-file call to load_rules_multi must behave identically to
    /// load_rules for backward compatibility.
    /// What: single path, assert rule count == expected.
    /// Test: this test itself.
    #[test]
    fn single_file_load_works() {
        let yaml = r#"
rules:
  - id: cc-feat
    category: feature
    keywords: ["feat:"]
"#;
        let f = write_yaml(yaml);
        let set = load_rules_multi(&[f.path()]).expect("single-file load");
        assert_eq!(set.rules.len(), 1);
    }

    /// Why: glob patterns in repo_categories must match expected names.
    /// What: test `infra-*` glob matches `infra-api` but not `platform-api`.
    /// Test: this test itself.
    #[test]
    fn repo_glob_matching() {
        assert!(repo_matches("infra-api", "infra-*"));
        assert!(repo_matches("infra-web", "infra-*"));
        assert!(!repo_matches("platform-api", "infra-*"));
        assert!(repo_matches("anything", "*"));
        assert!(repo_matches("api-service", "*service"));
        assert!(!repo_matches("api-tools", "*service"));
        assert!(repo_matches("exactname", "exactname"));
        assert!(!repo_matches("other", "exactname"));
    }

    fn make_low_confidence_result() -> ClassificationResult {
        ClassificationResult {
            category: "maintenance".to_string(),
            subcategory: Some("uncategorized".to_string()),
            top_level: None,
            confidence: 0.3,
            method: crate::core::models::ClassificationMethod::CatchAll,
            ticket_id: None,
            complexity: None,
        }
    }

    fn make_confident_result() -> ClassificationResult {
        ClassificationResult {
            category: "feature".to_string(),
            subcategory: Some("api".to_string()),
            top_level: None,
            confidence: 0.95,
            method: crate::core::models::ClassificationMethod::ExactRule,
            ticket_id: None,
            complexity: None,
        }
    }

    /// Why: repo_categories must apply only to uncategorized/low-confidence results.
    /// What: uncategorized commit from "infra-api" with category "infra-work"
    /// configured → result updated to infra-work.
    /// Test: this test itself.
    #[test]
    fn repo_category_applies_to_uncategorized() {
        let mut result = make_low_confidence_result();
        let mut repo_categories = HashMap::new();
        repo_categories.insert(
            "infra-api".to_string(),
            "platform_infrastructure".to_string(),
        );

        apply_repo_category_fallback(&mut result, "infra-api", &repo_categories, 0.7);

        assert_eq!(result.category, "platform_infrastructure");
        assert_eq!(
            result.subcategory,
            Some("platform_infrastructure".to_string())
        );
        assert!(matches!(
            result.method,
            crate::core::models::ClassificationMethod::RepoCategoryFallback
        ));
    }

    /// Why: a confidently-classified commit must NOT be overridden by
    /// repo_categories — the fallback is last-resort only.
    /// What: high-confidence feature commit from "infra-api" must remain feature.
    /// Test: this test itself.
    #[test]
    fn repo_category_skips_confident_result() {
        let mut result = make_confident_result();
        let mut repo_categories = HashMap::new();
        repo_categories.insert(
            "infra-api".to_string(),
            "platform_infrastructure".to_string(),
        );

        apply_repo_category_fallback(&mut result, "infra-api", &repo_categories, 0.7);

        // Unchanged — confident result must not be overridden.
        assert_eq!(result.category, "feature");
        assert!(matches!(
            result.method,
            crate::core::models::ClassificationMethod::ExactRule
        ));
    }

    /// Why: glob entries in repo_categories must match by pattern.
    /// What: repo_categories has `"infra-*"` → "platform_infrastructure";
    /// repo `"infra-payments"` must match and get the fallback.
    /// Test: this test itself.
    #[test]
    fn repo_category_glob_match() {
        let mut result = make_low_confidence_result();
        let mut repo_categories = HashMap::new();
        repo_categories.insert("infra-*".to_string(), "platform_infrastructure".to_string());

        apply_repo_category_fallback(&mut result, "infra-payments", &repo_categories, 0.7);

        assert_eq!(result.category, "platform_infrastructure");
    }

    /// Why: when no repo_categories entry matches the repo, the result must
    /// be left unchanged.
    /// What: uncategorized commit from "unknown-repo" with no matching config.
    /// Test: this test itself.
    #[test]
    fn repo_category_no_match_leaves_result_unchanged() {
        let mut result = make_low_confidence_result();
        let repo_categories = HashMap::new();

        apply_repo_category_fallback(&mut result, "unknown-repo", &repo_categories, 0.7);

        assert_eq!(result.category, "maintenance");
        assert_eq!(result.subcategory, Some("uncategorized".to_string()));
    }
}
