//! Cascade orchestrator combining the classification tiers.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rayon::prelude::*;
use rusqlite::Connection;

use crate::classify::errors::Result;
use crate::classify::rules::RuleSet;
use crate::classify::taxonomy::{SubcategoryDef, TaxonomyRegistry};
use crate::classify::tiers::exact::ExactMatcher;
use crate::classify::tiers::fuzzy::FuzzyClassifier;
use crate::classify::tiers::issue_type_tier::IssueTypeTier;
use crate::classify::tiers::jira_project_tier::JiraProjectTier;
use crate::classify::tiers::llm::LlmClassifier;
use crate::classify::tiers::override_tier::OverrideTier;
use crate::classify::tiers::regex_tier::RegexMatcher;
use crate::classify::tiers::ClassificationResult;
use crate::core::models::ClassificationMethod;

/// Runtime configuration for the [`ClassificationEngine`].
///
/// Why: classification tiers (especially LLM) need provider / model /
/// threshold tuning; bundling those knobs into a config struct keeps the
/// engine constructor signature stable as new tiers are added.
/// What: holds LLM toggles (`use_llm`, `llm_model`, `llm_provider`,
/// `openrouter_api_key`) plus a `confidence_threshold` shared by all tiers.
/// Test: every classifier test in `classify::tests` builds one via
/// `Default::default()` or with explicit overrides.
#[derive(Debug, Clone)]
pub struct ClassificationEngineConfig {
    /// Whether to engage the LLM tier when tiers 1–3 fail.
    pub use_llm: bool,
    /// LLM model identifier (provider-specific).
    pub llm_model: String,
    /// LLM provider: `"openrouter"`, `"openai"`, or `"auto"`.
    pub llm_provider: String,
    /// Optional OpenRouter API key. If `None`, the env var
    /// `OPENROUTER_API_KEY` is consulted at engine-build time.
    pub openrouter_api_key: Option<String>,
    /// Minimum confidence required to accept a verdict.
    ///
    /// Verdicts below this threshold are returned as-is (so the caller
    /// can still inspect them), but their `confidence` informs filtering
    /// in downstream reports.
    pub confidence_threshold: f64,
}

impl Default for ClassificationEngineConfig {
    fn default() -> Self {
        Self {
            use_llm: false,
            llm_model: "gpt-4o-mini".to_string(),
            llm_provider: "auto".to_string(),
            openrouter_api_key: None,
            confidence_threshold: 0.7,
        }
    }
}

/// Combined classification cascade.
///
/// Why: a single engine orchestrates the four-tier cascade
/// (override → exact → issue-type → regex → JIRA-project → fuzzy → LLM)
/// so callers don't reimplement the precedence rules.
/// What: holds one classifier per tier plus the shared taxonomy and
/// config. `classify` walks the tiers in order; first non-`None` wins.
/// Test: covered by `classify::tests::engine_classify_batch_does_not_panic`
/// and the cascade-coverage `corpus_uncategorized_below_1_percent` test.
pub struct ClassificationEngine {
    override_tier: Option<OverrideTier>,
    exact: ExactMatcher,
    issue_type: IssueTypeTier,
    regex: RegexMatcher,
    jira_project: JiraProjectTier,
    fuzzy: FuzzyClassifier,
    llm: Option<LlmClassifier>,
    taxonomy: TaxonomyRegistry,
    config: ClassificationEngineConfig,
}

impl ClassificationEngine {
    /// Build a new engine from a [`RuleSet`] and configuration.
    ///
    /// Why: most callers want the default behaviour — built-in taxonomy,
    /// no JIRA mappings, no override tier. This constructor is the shortest
    /// path.
    /// What: delegates to [`Self::with_taxonomy`] with an empty custom
    /// taxonomy vec.
    /// Test: covered by `classify::tests::engine_classify_batch_does_not_panic`.
    ///
    /// The LLM tier is constructed (but only invoked) if `config.use_llm`
    /// is true. The API key is read from the `OPENAI_API_KEY` environment
    /// variable; if unset, the LLM tier silently returns `None`.
    ///
    /// Uses the built-in taxonomy registry only. To extend it with
    /// user-defined subcategories, use [`Self::with_taxonomy`].
    ///
    /// # Errors
    ///
    /// Returns an error if the rules fail to compile (e.g. invalid regex).
    pub fn new(ruleset: RuleSet, config: ClassificationEngineConfig) -> Result<Self> {
        Self::with_taxonomy(ruleset, config, Vec::new())
    }

    /// Build an engine with user-defined subcategory definitions merged into
    /// the built-in taxonomy registry.
    ///
    /// Why: organisations often need custom subcategories
    /// (e.g. `"payments"`, `"auth"`) without forking the binary.
    /// What: delegates to [`Self::with_taxonomy_and_mappings`] with empty
    /// JIRA mappings and no override connection.
    /// Test: covered by `classify::tests::registry_merges_user_defined`.
    ///
    /// # Errors
    ///
    /// Returns an error if the rules fail to compile.
    pub fn with_taxonomy(
        ruleset: RuleSet,
        config: ClassificationEngineConfig,
        custom_taxonomy: Vec<SubcategoryDef>,
    ) -> Result<Self> {
        Self::with_taxonomy_and_mappings(ruleset, config, custom_taxonomy, HashMap::new(), None)
    }

    /// Full builder allowing JIRA project-key mappings and an optional DB
    /// connection for the manual-override tier.
    ///
    /// Why: operators sometimes need to seed both JIRA project keys (so
    /// `PROJ-123` knows it lives under "Project") and an override database
    /// (for human-corrected verdicts). This builder accepts both.
    /// What: delegates to [`Self::with_taxonomy_mappings_and_confidence`]
    /// with `jira_confidence = None` (use the default 0.88).
    /// Test: covered by `jira_project_mapping_*` tests.
    ///
    /// # Errors
    ///
    /// Returns an error if the rules fail to compile.
    pub fn with_taxonomy_and_mappings(
        ruleset: RuleSet,
        config: ClassificationEngineConfig,
        custom_taxonomy: Vec<SubcategoryDef>,
        jira_project_mappings: HashMap<String, String>,
        override_conn: Option<Arc<Mutex<Connection>>>,
    ) -> Result<Self> {
        Self::with_taxonomy_mappings_and_confidence(
            ruleset,
            config,
            custom_taxonomy,
            jira_project_mappings,
            None,
            override_conn,
        )
    }

    /// Full builder with the JIRA project-key mapping confidence override.
    ///
    /// Why: issue #206 — operators need to tune how aggressively the
    /// project-key mapping overrides downstream regex/fuzzy verdicts.
    /// Passing `None` keeps the default
    /// [`crate::classify::tiers::jira_project_tier::DEFAULT_PROJECT_MAPPING_CONFIDENCE`]
    /// (0.88).
    /// What: same as [`Self::with_taxonomy_and_mappings`] but takes an
    /// extra `jira_confidence` parameter.
    /// Test: covered by `jira_project_mapping_*` tests in this module.
    ///
    /// # Errors
    ///
    /// Returns an error if the rules fail to compile.
    pub fn with_taxonomy_mappings_and_confidence(
        ruleset: RuleSet,
        config: ClassificationEngineConfig,
        custom_taxonomy: Vec<SubcategoryDef>,
        jira_project_mappings: HashMap<String, String>,
        jira_confidence: Option<f64>,
        override_conn: Option<Arc<Mutex<Connection>>>,
    ) -> Result<Self> {
        let exact = ExactMatcher::new(&ruleset.rules)?;
        let regex = RegexMatcher::new(&ruleset.rules)?;
        let fuzzy = FuzzyClassifier;
        let llm = if config.use_llm {
            match LlmClassifier::from_provider(
                &config.llm_provider,
                &config.llm_model,
                config.openrouter_api_key.clone(),
            ) {
                Ok(c) => Some(c),
                Err(e) => {
                    return Err(crate::classify::errors::ClassifyError::Config(format!(
                        "LLM provider init failed: {e}"
                    )))
                }
            }
        } else {
            None
        };
        let taxonomy = TaxonomyRegistry::new(custom_taxonomy);
        let issue_type = IssueTypeTier::with_taxonomy(taxonomy.clone());
        let jira_project = JiraProjectTier::with_taxonomy_and_confidence(
            jira_project_mappings,
            taxonomy.clone(),
            jira_confidence.unwrap_or(
                crate::classify::tiers::jira_project_tier::DEFAULT_PROJECT_MAPPING_CONFIDENCE,
            ),
        );
        let override_tier = override_conn.map(|c| OverrideTier::with_taxonomy(c, taxonomy.clone()));
        Ok(Self {
            override_tier,
            exact,
            issue_type,
            regex,
            jira_project,
            fuzzy,
            llm,
            taxonomy,
            config,
        })
    }

    /// Borrow the engine's taxonomy registry.
    ///
    /// Why: report formatters that surface top-level categories need to
    /// resolve subcategory strings; sharing the engine's registry keeps
    /// the resolution consistent across the run.
    /// What: returns a shared reference to the internal
    /// [`TaxonomyRegistry`].
    /// Test: covered indirectly — every callable engine test holds a
    /// taxonomy reference.
    pub fn taxonomy(&self) -> &TaxonomyRegistry {
        &self.taxonomy
    }

    /// Test-only seam: rebuild the LLM tier targeting an explicit endpoint
    /// with a fixed API key.
    ///
    /// Why: integration tests that exercise the LLM tier (e.g. complexity
    /// backfill) need to point the classifier at a `wiremock` server rather
    /// than a real provider. Production code never calls this.
    /// What: replaces `self.llm` with an `LlmClassifier` keyed for `endpoint`.
    /// Test: used by the pipeline complexity-backfill integration tests.
    #[cfg(test)]
    pub(crate) fn with_test_llm_endpoint(mut self, endpoint: &str) -> Self {
        self.llm = Some(
            LlmClassifier::new(&self.config.llm_model, Some("sk-test".to_string()))
                .with_endpoint(endpoint),
        );
        self
    }

    /// Borrow the engine's effective configuration.
    pub fn config(&self) -> &ClassificationEngineConfig {
        &self.config
    }

    /// Run the synchronous tiers (0, 1, 1.5, 2, 3, 3.5) for a single message.
    ///
    /// Returns `None` if no tier matched; callers may then invoke the
    /// async [`ClassificationEngine::classify`] for the LLM fallback.
    ///
    /// `commit_sha` and `repo_path` are optional; when supplied, the
    /// manual-override tier (Tier 0) is consulted first. When `issue_type`
    /// is supplied, the issue-type tier (Tier 1.5) is consulted between
    /// the exact and regex tiers.
    pub fn classify_sync(&self, message: &str, is_merge: bool) -> Option<ClassificationResult> {
        self.classify_sync_with_context(message, is_merge, None, None, None)
    }

    /// Context-aware variant of [`Self::classify_sync`] that supplies
    /// optional commit identity (for Tier 0) and PM-system issue type
    /// (for Tier 1.5).
    pub fn classify_sync_with_context(
        &self,
        message: &str,
        is_merge: bool,
        commit_sha: Option<&str>,
        repo_path: Option<&str>,
        issue_type: Option<&str>,
    ) -> Option<ClassificationResult> {
        // Tier 0: manual override (DB lookup, short-circuits everything).
        if let (Some(tier), Some(sha), Some(repo)) =
            (self.override_tier.as_ref(), commit_sha, repo_path)
        {
            if let Some(r) = tier.lookup(sha, repo) {
                return Some(r);
            }
        }

        // Tier 1: exact keywords
        if let Some(rule) = self.exact.classify(message) {
            return Some(ClassificationResult {
                top_level: self.taxonomy.resolve(&rule.category),
                category: rule.category.clone(),
                subcategory: rule.subcategory.clone(),
                confidence: rule.confidence,
                method: ClassificationMethod::ExactRule,
                ticket_id: RegexMatcher::extract_ticket_id(message),
                complexity: None,
            });
        }

        // Tier 1.5: PM issue-type mapping.
        if let Some(it) = issue_type {
            if let Some(mut r) = self.issue_type.classify(it) {
                r.ticket_id = RegexMatcher::extract_ticket_id(message);
                return Some(r);
            }
        }

        // Tier 1.6: JIRA project-key mapping (if configured).
        //
        // Issue #206 — JIRA project codes (e.g. `TQL-1234`) carry semantic
        // meaning that no amount of message parsing can reproduce: `TQL`
        // is an existing-product bug tracker, `INFRA` is platform work, and
        // so on. Insert this tier *before* the regex tier so the project
        // mapping outranks the generic `[A-Z]+-\d+` `jira-ticket` rule
        // (which routes everything to "feature/ticketed" at confidence
        // 0.7). Tier-0 manual overrides still win because they short-
        // circuit above.
        if !self.jira_project.is_empty() {
            if let Some(r) = self.jira_project.classify(message) {
                return Some(r);
            }
        }

        // Tier 2: regex
        if let Some(rule) = self.regex.classify(message) {
            return Some(ClassificationResult {
                top_level: self.taxonomy.resolve(&rule.category),
                category: rule.category.clone(),
                subcategory: rule.subcategory.clone(),
                confidence: rule.confidence,
                method: ClassificationMethod::RegexRule,
                ticket_id: RegexMatcher::extract_ticket_id(message),
                complexity: None,
            });
        }

        // Tier 3.5: fuzzy heuristics
        if let Some(mut result) = self.fuzzy.classify(message, is_merge) {
            if result.ticket_id.is_none() {
                result.ticket_id = RegexMatcher::extract_ticket_id(message);
            }
            // Re-resolve top_level via the engine's registry in case user
            // overrides changed the parent for the fuzzy verdict's category.
            if let Some(top) = self.taxonomy.resolve(&result.category) {
                result.top_level = Some(top);
            }
            return Some(result);
        }

        None
    }

    /// Run the full four-tier cascade including the optional LLM fallback.
    pub async fn classify(&self, message: &str, is_merge: bool) -> ClassificationResult {
        if let Some(r) = self.classify_sync(message, is_merge) {
            return r;
        }

        if let Some(r) = self.llm_classify_only(message).await {
            return r;
        }

        let mut fallback = ClassificationResult::unclassified();
        fallback.ticket_id = RegexMatcher::extract_ticket_id(message);
        fallback
    }

    /// Invoke the LLM tier directly, bypassing tiers 0–3.5.
    ///
    /// Returns `None` when the LLM tier is not configured, no API key is
    /// reachable, or the underlying request fails. The pipeline-level LLM
    /// fallback uses this to route low-confidence catch-all verdicts to the
    /// LLM without re-running `classify_sync` (which would short-circuit on
    /// the same low-confidence verdict that triggered the fallback).
    ///
    /// Backfills `ticket_id` from the message text — the LLM verdict
    /// itself does not surface ticket IDs, and without this the pipeline's
    /// overwrite-guard would otherwise drop a ticket reference carried by
    /// the original tier-1-3 verdict when the LLM result wins.
    pub async fn llm_classify_only(&self, message: &str) -> Option<ClassificationResult> {
        let llm = self.llm.as_ref()?;
        let mut r = llm.classify(message).await?;
        r.top_level = self.taxonomy.resolve(&r.category);
        if r.ticket_id.is_none() {
            r.ticket_id = RegexMatcher::extract_ticket_id(message);
        }
        Some(r)
    }

    /// `Some(true)` when the LLM tier is enabled and has a reachable API
    /// key, `Some(false)` when it is enabled but unconfigured, `None` when
    /// the tier is disabled entirely. Callers can warn at startup when the
    /// middle case occurs to avoid silent misconfiguration.
    pub fn llm_has_api_key(&self) -> Option<bool> {
        self.llm.as_ref().map(LlmClassifier::has_api_key)
    }

    /// Classify a batch of `(message, is_merge)` pairs in parallel using
    /// Rayon (tiers 1–3 only). Entries where no tier matched are returned
    /// as [`ClassificationResult::unclassified`].
    pub fn classify_batch(&self, messages: &[(&str, bool)]) -> Vec<ClassificationResult> {
        messages
            .par_iter()
            .map(|(msg, is_merge)| {
                self.classify_sync(msg, *is_merge)
                    .unwrap_or_else(ClassificationResult::unclassified)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::rules::default_rules;

    /// Why: issue #206 requires the JIRA project mapping to fire as a
    /// Tier-1.6 tiebreaker — between exact-keyword and regex tiers. If
    /// the ordering regresses, the generic `jira-ticket` regex rule
    /// (confidence 0.7, category "feature/ticketed") would steal the
    /// verdict and operators would never see their mapping fire.
    /// What: build an engine over the default ruleset with one mapping
    /// (`TQL → bug_fix`) and classify a message that matches both the
    /// generic ticket pattern and the mapping. Assert the mapping wins.
    /// Test: pure cascade exercise, no DB.
    #[test]
    fn jira_project_mapping_outranks_generic_ticket_regex() {
        let mut mappings = HashMap::new();
        mappings.insert("TQL".to_string(), "bug_fix".to_string());
        let engine = ClassificationEngine::with_taxonomy_and_mappings(
            default_rules(),
            ClassificationEngineConfig::default(),
            Vec::new(),
            mappings,
            None,
        )
        .expect("engine builds");

        // The catch-all and `jira-ticket` rules would normally classify
        // this as "feature/ticketed" at confidence 0.7. With the mapping,
        // we should get "bug_fix" at the JIRA-tier confidence (0.88).
        let v = engine
            .classify_sync("TQL-1234 fix null pointer", false)
            .expect("verdict");
        assert_eq!(v.category, "bug_fix");
        assert!((v.confidence - 0.88).abs() < 1e-6);
        assert_eq!(v.ticket_id.as_deref(), Some("TQL-1234"));
    }

    /// Why: when the operator configures a per-tier confidence override
    /// (e.g. to crowd out manual overrides less aggressively), the
    /// value must reach the verdict.
    /// What: build an engine with `jira_confidence = Some(0.5)` and
    /// assert the verdict carries that value.
    /// Test: pure constructor + classify exercise.
    #[test]
    fn jira_project_mapping_confidence_threads_through_engine_builder() {
        let mut mappings = HashMap::new();
        mappings.insert("INFRA".to_string(), "platform".to_string());
        let engine = ClassificationEngine::with_taxonomy_mappings_and_confidence(
            default_rules(),
            ClassificationEngineConfig::default(),
            Vec::new(),
            mappings,
            Some(0.5),
            None,
        )
        .expect("engine builds");
        let v = engine
            .classify_sync("INFRA-7 patch", false)
            .expect("verdict");
        assert!((v.confidence - 0.5).abs() < 1e-6);
    }

    /// Why: exact-keyword conventional-commit prefixes (`fix:`, `feat:`)
    /// must still beat the JIRA mapping — they encode developer intent
    /// at much higher confidence than the project key.
    /// What: classify a `fix: TQL-1 ...` message with a TQL mapping
    /// configured; assert the cc-fix rule wins.
    /// Test: pure cascade exercise.
    #[test]
    fn exact_rule_still_beats_jira_project_mapping() {
        let mut mappings = HashMap::new();
        mappings.insert("TQL".to_string(), "platform".to_string());
        let engine = ClassificationEngine::with_taxonomy_and_mappings(
            default_rules(),
            ClassificationEngineConfig::default(),
            Vec::new(),
            mappings,
            None,
        )
        .expect("engine builds");
        let v = engine
            .classify_sync("fix: TQL-1 handle null user", false)
            .expect("verdict");
        assert_eq!(v.category, "bugfix");
        assert_eq!(v.method, ClassificationMethod::ExactRule);
    }
}
