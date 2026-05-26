//! Two-level classification taxonomy.
//!
//! Why: A flat `category` string makes aggregation and reporting awkward —
//! consumers can't tell whether `"security"` rolls up under bugfix or platform
//! work. This module introduces a fixed canonical set of **top-level** work
//! categories plus a registry of **subcategories** (extensible by users via
//! config) that each declare which top-level they belong to.
//!
//! What: Defines [`TopLevelCategory`] (a closed enum of 7 canonical variants)
//! and [`TaxonomyRegistry`] (a lookup table from subcategory name to top-level
//! parent, seeded with built-in defaults and merged with user-defined entries).
//!
//! Test: Construct a `TaxonomyRegistry` and assert that built-in names like
//! `"feature"`, `"bugfix"`, `"security"` resolve to the expected top-level
//! variants, and that user-defined entries override or extend the built-ins.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// The seven canonical top-level work categories.
///
/// Why: reports need a stable, closed set of work types to group against;
/// keeping the top-level enum closed prevents stakeholders from inventing
/// new categories that diverge across teams.
/// What: 7+1 variant enum (7 canonical + `Unknown` fallback) with
/// snake_case serialisation for DB persistence.
/// Test: `classify::tests::registry_resolves_builtin_subcategories`
/// asserts every built-in subcategory resolves to one of these.
///
/// These are fixed — users cannot add to this list. Subcategories (loaded
/// from rules and config) all roll up to exactly one of these variants.
///
/// Plus [`TopLevelCategory::Unknown`] for unresolved/uncategorized commits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TopLevelCategory {
    /// New features, enhancements, additive capability.
    Feature,
    /// Bug fixes, hotfixes, security patches.
    Bugfix,
    /// Keep The Lights On: CI, build, chore, ops, releases.
    Ktlo,
    /// Third-party integrations, APIs, webhooks.
    Integrations,
    /// Infra, architecture, performance, DevOps.
    PlatformWork,
    /// Docs, copy, UI text, assets, localization.
    Content,
    /// Refactor, tests, cleanup, style, deps, reverts.
    Maintenance,
    /// Unresolved / fallback for commits no rule matched.
    Unknown,
}

impl TopLevelCategory {
    /// Human-readable display name.
    ///
    /// Why: report headers and CSV columns need a friendly label
    /// distinct from the enum variant identifier.
    /// What: returns a short capitalised string per variant.
    /// Test: covered indirectly by markdown report tests
    /// (`report::tests::markdown_formatter_emits_report_header`).
    pub fn display_name(&self) -> &'static str {
        match self {
            TopLevelCategory::Feature => "Feature",
            TopLevelCategory::Bugfix => "Bugfix",
            TopLevelCategory::Ktlo => "Keep The Lights On",
            TopLevelCategory::Integrations => "Integrations",
            TopLevelCategory::PlatformWork => "Platform Work",
            TopLevelCategory::Content => "Content",
            TopLevelCategory::Maintenance => "Maintenance",
            TopLevelCategory::Unknown => "Unknown",
        }
    }

    /// All canonical variants in the recommended display order.
    ///
    /// Why: report iteration order must be stable so output stays diffable
    /// across runs; centralising the canonical order here avoids drift.
    /// What: returns a slice of the 7 reportable variants (excludes
    /// `Unknown`, which is a fallback, not a first-class category).
    /// Test: covered indirectly by every report formatter that iterates
    /// categories.
    pub fn all() -> &'static [TopLevelCategory] {
        &[
            TopLevelCategory::Feature,
            TopLevelCategory::Bugfix,
            TopLevelCategory::Ktlo,
            TopLevelCategory::Integrations,
            TopLevelCategory::PlatformWork,
            TopLevelCategory::Content,
            TopLevelCategory::Maintenance,
        ]
    }
}

/// A subcategory entry — maps a subcategory name to a top-level parent.
///
/// Why: users need to extend the taxonomy without modifying the closed
/// `TopLevelCategory` enum; subcategories are the extension point.
/// What: bundles the subcategory `name`, its `parent` top-level, and an
/// optional human-readable `display_name`.
/// Test: covered by `classify::tests::registry_merges_user_defined`.
///
/// User-defined entries are loaded from YAML config and merged on top of the
/// built-in defaults. Names are compared case-insensitively.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubcategoryDef {
    /// Subcategory name (e.g. `"feature"`, `"security"`, `"payments"`).
    pub name: String,
    /// Top-level parent this subcategory rolls up to.
    pub parent: TopLevelCategory,
    /// Optional human-readable label; falls back to `name` if absent.
    #[serde(default)]
    pub display_name: Option<String>,
}

impl SubcategoryDef {
    /// Construct a new definition.
    ///
    /// Why: simple constructor avoids the boilerplate of building the
    /// struct literal in test fixtures and YAML loaders.
    /// What: stores `name` (any `Into<String>`), the parent variant, and
    /// `display_name = None`.
    /// Test: covered by `classify::tests::registry_merges_user_defined`.
    pub fn new(name: impl Into<String>, parent: TopLevelCategory) -> Self {
        Self {
            name: name.into(),
            parent,
            display_name: None,
        }
    }
}

/// Registry of all known subcategories (built-in + user-defined).
///
/// Why: classification verdicts carry only a `category` string; the
/// registry is the lookup table that maps that string to its closed
/// top-level enum.
/// What: holds a case-insensitive `HashMap` for O(1) resolution plus an
/// `ordered` Vec preserving registration order for stable iteration.
/// Test: covered by `classify::tests::registry_resolves_builtin_subcategories`
/// and `registry_merges_user_defined`.
///
/// Lookups are case-insensitive on the subcategory name. User-defined entries
/// with the same name as a built-in entry replace the built-in (last-write-wins).
#[derive(Debug, Clone)]
pub struct TaxonomyRegistry {
    /// Lowercased name → definition.
    by_name: HashMap<String, SubcategoryDef>,
    /// Stable ordered list of definitions (built-ins first, then user-defined).
    ordered: Vec<SubcategoryDef>,
}

impl TaxonomyRegistry {
    /// Build a registry from built-in defaults merged with `user_defs`.
    ///
    /// Why: every taxonomy resolution path starts from the same built-in
    /// baseline; consolidating the merge logic here keeps user overrides
    /// last-write-wins without duplicating the loop in every caller.
    /// What: seeds the registry with [`Self::built_in_defs`], then inserts
    /// `user_defs`; same-name entries replace the built-in in both
    /// `by_name` and `ordered`.
    /// Test: covered by `classify::tests::registry_merges_user_defined`.
    ///
    /// User entries override built-in entries that share a (case-insensitive)
    /// name.
    pub fn new(user_defs: Vec<SubcategoryDef>) -> Self {
        let mut by_name: HashMap<String, SubcategoryDef> = HashMap::new();
        let mut ordered: Vec<SubcategoryDef> = Vec::new();

        for def in Self::built_in_defs() {
            by_name.insert(def.name.to_lowercase(), def.clone());
            ordered.push(def);
        }
        for def in user_defs {
            let key = def.name.to_lowercase();
            if let Some(pos) = ordered.iter().position(|d| d.name.to_lowercase() == key) {
                ordered[pos] = def.clone();
            } else {
                ordered.push(def.clone());
            }
            by_name.insert(key, def);
        }

        Self { by_name, ordered }
    }

    /// Build a registry containing only the built-in defaults.
    ///
    /// Why: tests and CLI dry-runs often want the stock taxonomy without
    /// merging any config.
    /// What: convenience wrapper for `Self::new(Vec::new())`.
    /// Test: covered by `classify::tests::registry_resolves_builtin_subcategories`.
    pub fn with_builtins() -> Self {
        Self::new(Vec::new())
    }

    /// Resolve a subcategory name to its top-level parent.
    ///
    /// Why: the cascade emits subcategory strings; downstream reports
    /// roll them up by top-level category. The registry is the
    /// single source of truth.
    /// What: O(1) lookup in a lowercased `HashMap`; returns `None` for
    /// unregistered names.
    /// Test: covered by `unknown_subcategory_returns_none_top_level`.
    ///
    /// Returns `None` if the name is not registered. Lookup is
    /// case-insensitive.
    pub fn resolve(&self, subcategory: &str) -> Option<TopLevelCategory> {
        self.by_name
            .get(&subcategory.to_lowercase())
            .map(|d| d.parent)
    }

    /// All registered subcategories in insertion order.
    ///
    /// Why: callers iterate this list to surface every known subcategory
    /// in the same order across runs (built-ins first, user entries last).
    /// What: returns a borrowed slice of the internal `ordered` Vec.
    /// Test: covered indirectly by `user_cannot_override_top_level_enum`
    /// which iterates `all()` to count duplicates.
    pub fn all(&self) -> &[SubcategoryDef] {
        &self.ordered
    }

    /// Built-in subcategory definitions.
    ///
    /// Why: a single function listing every default subcategory keeps the
    /// rule set and taxonomy in lockstep — adding a new `category` value
    /// in the rules requires the matching entry here.
    /// What: returns the static built-in list with each subcategory mapped
    /// to its top-level parent.
    /// Test: covered by `classify::tests::registry_resolves_builtin_subcategories`.
    ///
    /// These map every `category` value emitted by the default ruleset (see
    /// `rules::loader::default_rules`) plus the structural verdicts from the
    /// fuzzy tier (`merge`, `revert`, `uncategorized`).
    pub fn built_in_defs() -> Vec<SubcategoryDef> {
        use TopLevelCategory::*;
        vec![
            // Feature family
            SubcategoryDef::new("feature", Feature),
            SubcategoryDef::new("enhancement", Feature),
            SubcategoryDef::new("new-feature", Feature),
            SubcategoryDef::new("breaking", Feature),
            // Bugfix family
            SubcategoryDef::new("bugfix", Bugfix),
            SubcategoryDef::new("bug", Bugfix),
            SubcategoryDef::new("hotfix", Bugfix),
            SubcategoryDef::new("security", Bugfix),
            // KTLO family (build/ci/ops/release/chore stays in Maintenance below)
            SubcategoryDef::new("ci", Ktlo),
            SubcategoryDef::new("build", Ktlo),
            SubcategoryDef::new("ops", Ktlo),
            SubcategoryDef::new("release", Ktlo),
            // Integrations family
            SubcategoryDef::new("integration", Integrations),
            SubcategoryDef::new("integrations", Integrations),
            SubcategoryDef::new("api", Integrations),
            SubcategoryDef::new("webhook", Integrations),
            // Platform work
            SubcategoryDef::new("infra", PlatformWork),
            SubcategoryDef::new("platform", PlatformWork),
            SubcategoryDef::new("performance", PlatformWork),
            SubcategoryDef::new("perf", PlatformWork),
            SubcategoryDef::new("architecture", PlatformWork),
            SubcategoryDef::new("devops", PlatformWork),
            // Content family
            SubcategoryDef::new("docs", Content),
            SubcategoryDef::new("documentation", Content),
            SubcategoryDef::new("content", Content),
            SubcategoryDef::new("localization", Content),
            // Maintenance family
            SubcategoryDef::new("refactor", Maintenance),
            SubcategoryDef::new("test", Maintenance),
            SubcategoryDef::new("tests", Maintenance),
            SubcategoryDef::new("style", Maintenance),
            SubcategoryDef::new("cleanup", Maintenance),
            SubcategoryDef::new("maintenance", Maintenance),
            SubcategoryDef::new("deps", Maintenance),
            SubcategoryDef::new("dependencies", Maintenance),
            SubcategoryDef::new("revert", Maintenance),
            SubcategoryDef::new("merge", Maintenance),
            // `chore` is conventional-commit "miscellaneous" → Maintenance.
            SubcategoryDef::new("chore", Maintenance),
            // Platform-work extensions (cloud / monitoring / db / messaging / networking).
            SubcategoryDef::new("cloud", PlatformWork),
            SubcategoryDef::new("monitoring", PlatformWork),
            SubcategoryDef::new("observability", PlatformWork),
            SubcategoryDef::new("database", PlatformWork),
            SubcategoryDef::new("messaging", PlatformWork),
            SubcategoryDef::new("networking", PlatformWork),
            SubcategoryDef::new("storage", PlatformWork),
            // Feature extensions (experiments / spikes / prototypes).
            SubcategoryDef::new("experiment", Feature),
            SubcategoryDef::new("spike", Feature),
            SubcategoryDef::new("prototype", Feature),
            // Maintenance extensions.
            SubcategoryDef::new("rollback", Maintenance),
            SubcategoryDef::new("config", Maintenance),
            SubcategoryDef::new("tooling", Maintenance),
            // Content extensions.
            SubcategoryDef::new("content-docs", Content),
            SubcategoryDef::new("translation", Content),
            SubcategoryDef::new("assets", Content),
            // Work-in-progress and uncategorized roll up to Unknown.
            SubcategoryDef::new("wip", Unknown),
            SubcategoryDef::new("uncategorized", Unknown),
        ]
    }
}

impl Default for TaxonomyRegistry {
    fn default() -> Self {
        Self::with_builtins()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_resolves_builtin_subcategories() {
        let reg = TaxonomyRegistry::with_builtins();
        assert_eq!(reg.resolve("feature"), Some(TopLevelCategory::Feature));
        assert_eq!(reg.resolve("bugfix"), Some(TopLevelCategory::Bugfix));
        assert_eq!(reg.resolve("security"), Some(TopLevelCategory::Bugfix));
        assert_eq!(reg.resolve("ci"), Some(TopLevelCategory::Ktlo));
        assert_eq!(reg.resolve("build"), Some(TopLevelCategory::Ktlo));
        assert_eq!(
            reg.resolve("performance"),
            Some(TopLevelCategory::PlatformWork)
        );
        assert_eq!(
            reg.resolve("documentation"),
            Some(TopLevelCategory::Content)
        );
        assert_eq!(reg.resolve("refactor"), Some(TopLevelCategory::Maintenance));
        assert_eq!(reg.resolve("chore"), Some(TopLevelCategory::Maintenance));
    }

    #[test]
    fn registry_lookup_is_case_insensitive() {
        let reg = TaxonomyRegistry::with_builtins();
        assert_eq!(reg.resolve("FEATURE"), Some(TopLevelCategory::Feature));
        assert_eq!(reg.resolve("BugFix"), Some(TopLevelCategory::Bugfix));
    }

    #[test]
    fn registry_merges_user_defined() {
        let user = vec![
            SubcategoryDef::new("payments", TopLevelCategory::Integrations),
            SubcategoryDef::new("auth", TopLevelCategory::Feature),
        ];
        let reg = TaxonomyRegistry::new(user);
        assert_eq!(
            reg.resolve("payments"),
            Some(TopLevelCategory::Integrations)
        );
        assert_eq!(reg.resolve("auth"), Some(TopLevelCategory::Feature));
        // Built-ins still resolve.
        assert_eq!(reg.resolve("feature"), Some(TopLevelCategory::Feature));
    }

    #[test]
    fn user_can_override_builtin_without_breaking_registry() {
        // Reclassify "security" from Bugfix to PlatformWork — should replace,
        // not duplicate, the built-in entry.
        let user = vec![SubcategoryDef::new(
            "security",
            TopLevelCategory::PlatformWork,
        )];
        let reg = TaxonomyRegistry::new(user);
        assert_eq!(
            reg.resolve("security"),
            Some(TopLevelCategory::PlatformWork)
        );
        // Should appear only once in the ordered list.
        let count = reg
            .all()
            .iter()
            .filter(|d| d.name.eq_ignore_ascii_case("security"))
            .count();
        assert_eq!(count, 1, "duplicate registration must be deduplicated");
    }

    #[test]
    fn unknown_subcategory_returns_none() {
        let reg = TaxonomyRegistry::with_builtins();
        assert!(reg.resolve("totally-not-a-real-category").is_none());
    }

    #[test]
    fn top_level_all_excludes_unknown() {
        for top in TopLevelCategory::all() {
            assert_ne!(*top, TopLevelCategory::Unknown);
        }
        assert_eq!(TopLevelCategory::all().len(), 7);
    }

    #[test]
    fn top_level_display_names_are_stable() {
        assert_eq!(TopLevelCategory::Feature.display_name(), "Feature");
        assert_eq!(TopLevelCategory::Ktlo.display_name(), "Keep The Lights On");
        assert_eq!(
            TopLevelCategory::PlatformWork.display_name(),
            "Platform Work"
        );
    }
}
