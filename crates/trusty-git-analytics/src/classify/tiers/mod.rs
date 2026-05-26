//! Implementations of the four classification tiers.
//!
//! The cascade runs them in order:
//! 1. [`exact`] — fast multi-keyword matching via Aho-Corasick.
//! 2. [`regex_tier`] — regex pattern matching.
//! 3. [`fuzzy`] — heuristics (merge/revert detection, etc.).
//! 4. [`llm`] — optional async LLM fallback.

pub mod bedrock;
pub mod exact;
pub mod fuzzy;
pub mod issue_type_tier;
pub mod jira_project_tier;
pub mod llm;
pub mod override_tier;
pub mod regex_tier;

use serde::{Deserialize, Serialize};

use crate::classify::taxonomy::TopLevelCategory;
use crate::core::models::ClassificationMethod;

/// Output of any tier: a category verdict plus provenance.
///
/// Why: every classifier tier needs to return the same shape so the cascade
/// orchestrator can compare tiers' verdicts and decide which to accept.
/// What: bundles the verdict (`category`, `subcategory`, `top_level`),
/// confidence, source tier (`method`), extracted ticket id, and an
/// optional LLM-only complexity score.
/// Test: covered by `unclassified_defaults_complexity_to_none` and every
/// classifier test that asserts the verdict shape.
///
/// The hierarchy is:
/// - `top_level` — one of the canonical [`TopLevelCategory`] variants
///   (resolved from `category` via the [`crate::classify::taxonomy::TaxonomyRegistry`]).
/// - `category` — the **subcategory name** (e.g. `"feature"`, `"security"`).
///   Kept as a free-form string for backward compatibility with the DB schema.
/// - `subcategory` — an even-more-specific leaf label (e.g. `"sql-injection"`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClassificationResult {
    /// Subcategory name (e.g. `"feature"`, `"bugfix"`, `"security"`).
    ///
    /// Despite the field name, this is the **subcategory** in the two-level
    /// taxonomy — the registered `TopLevelCategory` parent is reported in
    /// `top_level`. The field name is preserved for DB-schema compatibility.
    pub category: String,
    /// Optional leaf label (e.g. `"sql-injection"`, `"cleanup"`).
    pub subcategory: Option<String>,
    /// Resolved top-level category (`None` if `category` is unregistered).
    #[serde(default)]
    pub top_level: Option<TopLevelCategory>,
    /// Confidence in this verdict (0.0–1.0).
    pub confidence: f64,
    /// Which tier produced this verdict.
    pub method: ClassificationMethod,
    /// Optional extracted ticket id (e.g. `"PROJ-123"`).
    pub ticket_id: Option<String>,
    /// Optional commit complexity score on a 1–5 scale.
    ///
    /// `None` means the commit was not scored — only the LLM tier produces
    /// a complexity score; rule/regex/fuzzy tiers always leave this `None`.
    /// The scale is: 1 = trivial, 2 = simple, 3 = moderate, 4 = complex,
    /// 5 = highly complex.
    #[serde(default)]
    pub complexity: Option<u8>,
}

impl ClassificationResult {
    /// Construct an "unclassified" result used as a default when no tier matches.
    ///
    /// Why: callers prefer a deterministic verdict over a panic / Option
    /// when the cascade fails; this is the canonical safe default.
    /// What: returns `category = "uncategorized"`, `top_level = Unknown`,
    /// `confidence = 0.0`, `method = FuzzyMatch`, and `complexity = None`.
    /// Test: covered by `unclassified_defaults_complexity_to_none`.
    ///
    /// `complexity` defaults to `None` — unclassified commits are never
    /// complexity-scored.
    pub fn unclassified() -> Self {
        Self {
            category: "uncategorized".to_string(),
            subcategory: None,
            top_level: Some(TopLevelCategory::Unknown),
            confidence: 0.0,
            method: ClassificationMethod::FuzzyMatch,
            ticket_id: None,
            complexity: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: non-LLM tiers must never fabricate a complexity score; the
    /// "unclassified" default is the canonical example.
    /// What: asserts `ClassificationResult::unclassified()` leaves
    /// `complexity` as `None`.
    /// Test: construct the default and assert the field.
    #[test]
    fn unclassified_defaults_complexity_to_none() {
        let r = ClassificationResult::unclassified();
        assert_eq!(r.complexity, None);
    }
}
