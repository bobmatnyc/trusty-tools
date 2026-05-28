//! Tier 1.5: classify by PM-system issue type.
//!
//! Why: When a commit references a JIRA/Linear/ADO ticket, the ticket's
//! `issue_type` field (e.g. `"Bug"`, `"Story"`, `"Task"`) is a high-signal
//! predictor of the commit's change category — often more reliable than
//! parsing the prose message. This tier maps known issue types to canonical
//! subcategory names, slotting in between the exact-rule tier and the
//! regex tier in the cascade.
//!
//! What: A pure-function classifier over an `issue_type` string. Lookups
//! are case-insensitive; unmapped types return `None` so later tiers can
//! still take a swing.
//!
//! Test: Assert that `"Bug"` → `"bugfix"`, `"Story"` → `"feature"`, and
//! `"unknown-type"` → `None`; verify confidence is 0.90 on every hit.

use crate::classify::taxonomy::{TaxonomyRegistry, TopLevelCategory};
use crate::classify::tiers::ClassificationResult;
use crate::core::models::ClassificationMethod;

/// Static mapping from PM-system issue type to canonical subcategory name.
///
/// Mirrors the `ISSUETYPE_CHANGE_TYPE_MAP` constant from the Python
/// predecessor. Keys are matched case-insensitively.
const ISSUE_TYPE_MAP: &[(&str, &str)] = &[
    ("story", "feature"),
    ("bug", "bugfix"),
    ("defect", "bugfix"),
    ("task", "maintenance"),
    ("sub-task", "maintenance"),
    ("epic", "feature"),
    ("improvement", "feature"),
    ("tech debt", "refactor"),
    ("spike", "maintenance"),
    ("test", "test"),
    ("documentation", "documentation"),
];

/// Confidence assigned to every verdict from this tier.
///
/// Slightly below the exact-rule tier (which is typically 0.95+) because
/// issue types are sometimes misclassified in upstream PM tools.
const ISSUE_TYPE_CONFIDENCE: f64 = 0.90;

/// Tier-1.5 issue-type classifier. Stateless.
pub struct IssueTypeTier {
    taxonomy: TaxonomyRegistry,
}

impl Default for IssueTypeTier {
    fn default() -> Self {
        Self::new()
    }
}

impl IssueTypeTier {
    /// Construct a new tier using the built-in taxonomy registry.
    pub fn new() -> Self {
        Self {
            taxonomy: TaxonomyRegistry::with_builtins(),
        }
    }

    /// Construct with a custom taxonomy registry (e.g. one that has
    /// user-defined subcategory overrides merged in).
    pub fn with_taxonomy(taxonomy: TaxonomyRegistry) -> Self {
        Self { taxonomy }
    }

    /// Map a PM-system issue type (e.g. `"Story"`, `"Bug"`) to a
    /// [`ClassificationResult`] with confidence 0.90.
    ///
    /// Returns `None` for empty input or unmapped types.
    pub fn classify(&self, issue_type: &str) -> Option<ClassificationResult> {
        let key = issue_type.trim().to_lowercase();
        if key.is_empty() {
            return None;
        }
        let category = ISSUE_TYPE_MAP
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| (*v).to_string())?;

        let top_level = self
            .taxonomy
            .resolve(&category)
            .unwrap_or(TopLevelCategory::Unknown);

        Some(ClassificationResult {
            category,
            subcategory: None,
            top_level: Some(top_level),
            confidence: ISSUE_TYPE_CONFIDENCE,
            // Why: the verdict is derived from a PM-system issue type field
            // (e.g. JIRA `"Bug"`, Linear `"Story"`) — an external ticket-system
            // signal, not a commit-message heuristic. Using `ExactRule` here
            // caused PM-issue-type-classified commits to report as `exact_rule`
            // in analytics, conflating rule-file keyword matches with PM
            // metadata signals. `ExternalSource` is the correct label — it
            // covers all paths where classification is driven by external
            // ticket-system metadata rather than message-text patterns.
            // Fixed in tga 1.5.3 (issue #319).
            method: ClassificationMethod::ExternalSource,
            ticket_id: None,
            complexity: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bug_maps_to_bugfix() {
        let r = IssueTypeTier::new().classify("Bug").expect("hit");
        assert_eq!(r.category, "bugfix");
        assert!((r.confidence - 0.90).abs() < 1e-9);
    }

    #[test]
    fn story_maps_to_feature() {
        let r = IssueTypeTier::new().classify("Story").expect("hit");
        assert_eq!(r.category, "feature");
    }

    #[test]
    fn case_insensitive_lookup() {
        let r = IssueTypeTier::new().classify("BUG").expect("hit");
        assert_eq!(r.category, "bugfix");
        let r = IssueTypeTier::new().classify("  bug  ").expect("hit");
        assert_eq!(r.category, "bugfix");
    }

    #[test]
    fn unknown_issue_type_returns_none() {
        assert!(IssueTypeTier::new().classify("unknown-type").is_none());
        assert!(IssueTypeTier::new().classify("").is_none());
        assert!(IssueTypeTier::new().classify("   ").is_none());
    }

    #[test]
    fn tech_debt_maps_to_refactor() {
        let r = IssueTypeTier::new().classify("Tech Debt").expect("hit");
        assert_eq!(r.category, "refactor");
    }
}
