//! End-to-end classification pipeline: read DB → classify → write back.

use std::collections::HashMap;

use futures::stream::StreamExt;
use rusqlite::params;
use tracing::{info, warn};

use crate::classify::classifier::{ClassificationEngine, ClassificationEngineConfig};
use crate::classify::errors::Result;
use crate::classify::rules::default_rules;
use crate::classify::sources::ExternalSourceResolver;
use crate::classify::tiers::bedrock::DEFAULT_BEDROCK_MODEL;
use crate::classify::tiers::llm::ANTHROPIC_DEFAULT_MODEL;
use crate::classify::tiers::ClassificationResult;
use crate::core::config::{Config, LlmSource};
use crate::core::db::Database;
use crate::core::models::ClassificationMethod;

/// Default minimum coverage threshold (percent) below which the pipeline
/// emits a warning. Used when no config-level override is supplied.
#[allow(dead_code)]
const DEFAULT_MIN_COVERAGE_PCT: f64 = 20.0;

/// Aggregate statistics from a single pipeline run.
///
/// Why: callers (CLI, tests) need a uniform shape describing how many
/// commits were classified and via which tier; coverage breakdowns let
/// reports surface gaps per repository.
/// What: counters per-tier (`by_method`), per-category (`by_category`),
/// and per-repo coverage. Populated by [`ClassificationPipeline::run`].
/// Test: covered by `tests::pipeline_runs_against_in_memory_db` and
/// `pipeline_force_reclassifies_rows`.
#[derive(Debug, Clone, Default)]
pub struct ClassificationStats {
    /// Total commits processed.
    pub total_commits: usize,
    /// Commits that received a non-uncategorized verdict.
    pub classified: usize,
    /// Count of verdicts per tier (`"exact_rule"`, `"regex_rule"`, ...).
    pub by_method: HashMap<String, usize>,
    /// Count of verdicts per category.
    pub by_category: HashMap<String, usize>,
    /// Overall classification coverage as a percentage (0–100).
    ///
    /// Defined as `classified / total_commits * 100`. Zero when
    /// `total_commits == 0`.
    pub coverage_pct: f64,
    /// Per-repository coverage (repo_name → coverage percentage).
    pub coverage_by_repo: HashMap<String, RepoCoverage>,
}

/// Per-repository coverage breakdown.
///
/// Why: a global coverage number hides the case where one repo classifies
/// at 95% and another at 5%; surfacing per-repo coverage lets operators
/// drill in.
/// What: total commits, classified count, and percentage (0–100).
/// Test: covered by classification pipeline integration tests.
#[derive(Debug, Clone, Default)]
pub struct RepoCoverage {
    /// Total commits seen for this repository.
    pub total: usize,
    /// Commits with a non-`"uncategorized"` verdict.
    pub classified: usize,
    /// `classified / total * 100`.
    pub coverage_pct: f64,
}

/// Stage-2 pipeline: classify every unclassified commit currently in the DB.
///
/// Why: classification touches multiple tiers (rules / regex / fuzzy / LLM)
/// and the engine config / taxonomy / JIRA mappings all live in `Config`;
/// concentrating orchestration here keeps the binary's `commands/classify.rs`
/// thin.
/// What: holds the validated [`Config`] plus toggles for `force` re-classify,
/// `since`/`until` date bounds, and `repos` filter. Built via [`Self::new`] +
/// builder methods.
/// Test: covered by `classify::tests::pipeline_runs_against_in_memory_db`.
pub struct ClassificationPipeline {
    config: Config,
    /// When `true`, re-classify commits that already carry a verdict.
    ///
    /// Defaults to `false` (skip-if-classified). See [`Self::with_force`].
    force: bool,
    /// Optional lower bound on `commits.timestamp` (ISO8601: `YYYY-MM-DD`).
    ///
    /// Only consulted when `force == true`; without force the default
    /// "missing-verdict" filter is the only selector. See [`Self::with_since`].
    since: Option<String>,
    /// Optional upper bound on `commits.timestamp` (ISO8601: `YYYY-MM-DD`).
    ///
    /// Scopes re-classification to commits on or before this date.
    /// See [`Self::with_until`].
    until: Option<String>,
    /// Optional repository filter: only classify commits from these repos.
    ///
    /// When non-empty, only commits whose `repository` column matches one of
    /// the listed names are considered. See [`Self::with_repos`].
    repos: Vec<String>,
}

impl ClassificationPipeline {
    /// Construct a new pipeline bound to the given config.
    ///
    /// Why: pipelines start with re-classification disabled by default
    /// (the common "fill in missing verdicts" case); operators opt in to
    /// `force` via the builder.
    /// What: stores the config; sets `force = false`, `since = None`.
    /// Test: covered by `tests::pipeline_constructs_with_default_config`
    /// in `collect::tests` and the pipeline integration tests.
    pub fn new(config: Config) -> Self {
        Self {
            config,
            force: false,
            since: None,
            until: None,
            repos: Vec::new(),
        }
    }

    /// Re-classify commits even if they already have a `classification_id`.
    ///
    /// Why: when the rule set is updated (or a JIRA project mapping is
    /// added), operators need to retroactively apply the new rules to
    /// historical data. Without this, the pipeline skips classified rows
    /// and the new rules never fire on them. Issue #205.
    /// What: flips the read query from "WHERE classification_id IS NULL" to
    /// "any commit", and the write-back replaces the existing
    /// `classifications` row in place (no orphan rows).
    /// Test: see `pipeline_force_reclassifies_rows` in this module.
    pub fn with_force(mut self, force: bool) -> Self {
        self.force = force;
        self
    }

    /// Bound `--force` rewrites to commits whose `timestamp` is on or after
    /// the given ISO8601 date.
    ///
    /// Why: full-corpus rewrites are expensive; the common case for the
    /// retroactive flow is "apply the new rules to the last quarter".
    /// What: stores the string verbatim; the read query appends a
    /// `timestamp >= ?` predicate when set. No-op when `force` is `false`.
    /// Test: covered by the same integration test as `with_force`.
    pub fn with_since(mut self, since: Option<String>) -> Self {
        self.since = since;
        self
    }

    /// Bound classification to commits on or before the given ISO8601 date.
    ///
    /// Why: complements `with_since` to allow a bounded window like
    /// `--since 2026-01-01 --until 2026-03-31` without touching commits
    /// outside that quarter.
    /// What: stores the string; the read query appends a `timestamp <= ?`
    /// predicate when set.
    /// Test: covered by pipeline integration tests that exercise date windows.
    pub fn with_until(mut self, until: Option<String>) -> Self {
        self.until = until;
        self
    }

    /// Restrict classification to commits from specific repositories.
    ///
    /// Why: the `--repos` filter lets operators classify only a slice of the
    /// DB (e.g. one service) without running across the full corpus.
    /// What: when non-empty, adds a `WHERE repository IN (…)` clause to the
    /// candidate commit query.
    /// Test: see `tests::classify_repos_filter_*` in this module.
    pub fn with_repos(mut self, repos: Vec<String>) -> Self {
        self.repos = repos;
        self
    }

    /// Execute the pipeline against `db`.
    ///
    /// Workflow:
    /// 1. Load rules from `config.classification.rules_file`, or fall back
    ///    to [`default_rules`].
    /// 2. Build the [`ClassificationEngine`].
    /// 3. Query all commits with `classification_id IS NULL`.
    /// 4. Classify in parallel (Rayon) using tiers 1–3.
    /// 5. Optionally invoke the async LLM tier for commits still uncategorized.
    /// 6. Write `classifications` rows and update each commit's
    ///    `classification_id` and `confidence`.
    ///
    /// # Errors
    ///
    /// Returns an error if the DB queries, rule loading, or migrations fail.
    /// Build the [`ClassificationEngine`] from this pipeline's config.
    ///
    /// Why: both [`Self::run`] and [`Self::backfill_complexity`] need an
    /// identically-configured engine; extracting this keeps the rule-merge
    /// and config-mapping logic in one place.
    /// What: loads/merges rules, maps `Config` → `ClassificationEngineConfig`,
    /// and constructs the engine (without the DB-backed override tier).
    /// When the top-level `llm:` section is present it takes precedence over
    /// legacy `classification.*` fields; legacy fields emit a deprecation
    /// warning when `llm:` is absent but those fields are used.
    /// Test: exercised indirectly by the pipeline integration tests.
    ///
    /// # Errors
    ///
    /// Returns an error if rules fail to load/compile or the LLM provider
    /// fails to initialize.
    async fn build_engine(&self) -> Result<ClassificationEngine> {
        // Load user-supplied rule files (single or multiple, #445 batch C).
        // When `rules_files` is non-empty, load and merge them in order via
        // `RuleSet::merge`. The last file's `extend_defaults` flag wins.
        // For back-compat the comment above still refers to "rules_file" but the
        // implementation now drives off `rules_files`.
        let ruleset = {
            use crate::classify::rules::load_rules_multi;
            let class_cfg = self.config.classification.as_ref();
            let paths: Vec<&std::path::PathBuf> = class_cfg
                .map(|c| c.rules_files.iter().collect())
                .unwrap_or_default();

            if paths.is_empty() {
                default_rules()
            } else {
                let path_refs: Vec<&std::path::Path> = paths.iter().map(|p| p.as_path()).collect();
                let custom = load_rules_multi(&path_refs)?;
                if custom.extend_defaults {
                    // Merge: start with defaults, let custom rules override by id.
                    let mut merged = default_rules();
                    let custom_ids: std::collections::HashSet<String> =
                        custom.rules.iter().map(|r| r.id.clone()).collect();
                    merged.rules.retain(|r| !custom_ids.contains(&r.id));
                    merged.rules.extend(custom.rules);
                    merged
                } else {
                    custom
                }
            }
        };

        // Determine whether the LLM tier is requested and which source.
        //
        // Precedence (highest first):
        // 1. Top-level `llm:` section (new, preferred): presence of the section
        //    SELF-ENABLES the LLM tier — no `classification.use_llm: true` required.
        //    The intent is: if you wrote `llm:` in config, you mean to use the LLM.
        // 2. Legacy `classification.use_llm: true` — still honored when no `llm:`
        //    section is present.
        //
        // Note: an explicit `use_llm: false` in `classification:` does NOT suppress
        // the `llm:` section; the `llm:` section is always self-enabling. Users who
        // need to temporarily disable the LLM tier while keeping the `llm:` config
        // should remove or comment out the `llm:` block.
        let use_llm = self.config.llm.is_some()
            || self
                .config
                .classification
                .as_ref()
                .map(|c| c.use_llm)
                .unwrap_or(false);

        let engine_cfg = match self.config.classification.as_ref() {
            Some(c) => ClassificationEngineConfig {
                use_llm: c.use_llm,
                llm_model: c.llm_model.clone().unwrap_or_else(|| "gpt-4o-mini".into()),
                llm_provider: c.llm_provider.clone(),
                openrouter_api_key: c.openrouter_api_key.clone(),
                confidence_threshold: c.confidence_threshold,
                weighted_sum: c.weighted_sum.clone(),
            },
            None => ClassificationEngineConfig::default(),
        };

        let custom_taxonomy = self
            .config
            .classification
            .as_ref()
            .map(|c| c.custom_categories.clone())
            .unwrap_or_default();

        let jira_mappings = self
            .config
            .jira
            .as_ref()
            .map(|j| j.jira_project_mappings.clone())
            .unwrap_or_default();

        let jira_confidence = self
            .config
            .jira
            .as_ref()
            .and_then(|j| j.jira_project_mapping_confidence);

        // Build the engine without an injected LLM tier first, then attach
        // the LLM tier (which may require async SDK init for Bedrock) below.
        let engine_cfg_no_llm = ClassificationEngineConfig {
            use_llm: false,
            ..engine_cfg.clone()
        };
        let mut engine = ClassificationEngine::with_taxonomy_mappings_and_confidence(
            ruleset,
            engine_cfg_no_llm,
            custom_taxonomy,
            jira_mappings,
            jira_confidence,
            // Override-tier DB wiring is deferred: rusqlite::Connection is
            // not Send + Sync, so plumbing the live connection through the
            // Rayon batch would require a redesign. The override tier is
            // still constructible via `with_taxonomy_and_mappings` for
            // single-threaded callers and tests.
            None,
        )?;

        // Wire the LLM tier when requested, preferring the `llm:` section.
        if use_llm {
            let llm_classifier = if let Some(llm_cfg) = self.config.llm.as_ref() {
                // New path: `llm:` section present.
                //
                // Model resolution order:
                //  1. Explicit `llm.model` in the `llm:` section.
                //  2. Legacy `classification.llm_model` (migration compat).
                //  3. Source-aware default: bedrock → DEFAULT_BEDROCK_MODEL,
                //     anthropic-api → ANTHROPIC_DEFAULT_MODEL,
                //     openrouter → "gpt-4o-mini".
                //
                // Why: using `gpt-4o-mini` as the universal fallback causes
                // invalid-model errors for `bedrock` and `anthropic-api` sources
                // when `llm.model` is unset. Each provider requires a model id
                // from its own namespace.
                let source_default = match llm_cfg.source {
                    LlmSource::Bedrock => DEFAULT_BEDROCK_MODEL,
                    LlmSource::AnthropicApi => ANTHROPIC_DEFAULT_MODEL,
                    LlmSource::Openrouter => "gpt-4o-mini",
                };
                let model = llm_cfg
                    .model
                    .as_deref()
                    .or(self
                        .config
                        .classification
                        .as_ref()
                        .and_then(|c| c.llm_model.as_deref()))
                    .unwrap_or(source_default);
                crate::classify::tiers::llm::LlmClassifier::from_llm_config(llm_cfg, model)
                    .await
                    .map_err(|e| {
                        crate::classify::errors::ClassifyError::Config(format!(
                            "LLM provider init failed (llm: section): {e}"
                        ))
                    })?
            } else {
                // Legacy path: `classification.llm_provider` etc.
                // Emit a single deprecation warning so existing configs
                // get a clear upgrade path.
                if self
                    .config
                    .classification
                    .as_ref()
                    .map(|c| c.openrouter_api_key.is_some() || c.llm_provider != "auto")
                    .unwrap_or(false)
                {
                    warn!(
                        "classification.openrouter_api_key / classification.llm_provider \
                             are deprecated. Migrate to the top-level `llm:` section: \
                             'llm:\\n  source: openrouter\\n  api_key_env: OPENROUTER_API_KEY'"
                    );
                }
                crate::classify::tiers::llm::LlmClassifier::from_provider_async(
                    &engine_cfg.llm_provider,
                    &engine_cfg.llm_model,
                    engine_cfg.openrouter_api_key.clone(),
                )
                .await
                .map_err(|e| {
                    crate::classify::errors::ClassifyError::Config(format!(
                        "LLM provider init failed: {e}"
                    ))
                })?
            };

            // Fail-loudly guard: when LLM is enabled but no credential resolves,
            // error before writing any DB rows. The error names the specific env
            // var (when an `llm:` section is present) so the user knows exactly
            // what to export. No DB writes occur.
            if !llm_classifier.has_api_key() {
                let var_hint = self
                    .config
                    .llm
                    .as_ref()
                    .map(|l| format!(" ('{}')", l.api_key_env))
                    .unwrap_or_default();
                return Err(crate::classify::errors::ClassifyError::Config(format!(
                    "LLM tier is enabled but no API key or credentials could be resolved. \
                     Ensure the environment variable{var_hint} named by llm.api_key_env \
                     is set and non-empty (for openrouter/anthropic-api), or that valid \
                     AWS credentials are present in the credential chain (for bedrock). \
                     No database writes will occur."
                )));
            }

            engine.attach_llm(llm_classifier);
        }

        Ok(engine)
    }

    /// Build an [`ExternalSourceResolver`] from the pipeline's config, or
    /// return `None` when external sources are disabled or none are configured.
    ///
    /// Why: the resolver is an optional component — teams without JIRA/GitHub
    /// or running in offline CI should not pay any overhead for it. This
    /// method centralises the "should I build a resolver?" decision.
    /// What: returns `None` when `no_external` is `true` OR when the
    /// `sources` list is empty; otherwise constructs a fresh resolver.
    /// Test: exercised by the pipeline integration tests via `run`.
    fn build_resolver(&self) -> Option<ExternalSourceResolver> {
        let no_external = self
            .config
            .classification
            .as_ref()
            .map(|c| c.no_external)
            .unwrap_or(false);
        if no_external {
            return None;
        }
        let sources = self
            .config
            .classification
            .as_ref()
            .map(|c| c.sources.as_slice())
            .unwrap_or(&[]);
        if sources.is_empty() {
            return None;
        }
        Some(ExternalSourceResolver::new(sources))
    }

    /// Execute the pipeline against `db`.
    ///
    /// Workflow:
    /// 1. Build the [`ClassificationEngine`] from config (rules + LLM tier).
    /// 2. Optionally build an [`ExternalSourceResolver`] from `config.sources`.
    /// 3. Query all commits with `classification_id IS NULL`.
    /// 4. Classify in parallel (Rayon) using tiers 0–3.
    /// 5. Optionally invoke the async LLM tier for low-confidence verdicts.
    /// 6. Write `classifications` rows (including `complexity`) and update
    ///    each commit's `classification_id` and `confidence`.
    ///
    /// # Errors
    ///
    /// Returns an error if the DB queries, rule loading, or migrations fail.
    pub async fn run(&self, db: &mut Database) -> Result<ClassificationStats> {
        // 1. Build engine (async to support Bedrock credential init).
        let engine = self.build_engine().await?;
        // 2. Build optional external source resolver.
        let resolver = self.build_resolver();
        self.run_with_engine_and_resolver(db, engine, resolver)
            .await
    }

    /// Run the classification pipeline using a caller-supplied engine.
    ///
    /// Why: tests need to inject an engine wired to a mock LLM endpoint;
    /// [`Self::run`] builds the engine itself and delegates here.
    /// What: identical to [`Self::run`] but skips engine construction.
    /// Test: the complexity-write integration test calls this directly.
    ///
    /// # Errors
    ///
    /// Returns an error if DB queries or write-back fail.
    #[allow(dead_code)]
    pub(crate) async fn run_with_engine(
        &self,
        db: &mut Database,
        engine: ClassificationEngine,
    ) -> Result<ClassificationStats> {
        self.run_with_engine_and_resolver(db, engine, None).await
    }

    /// Run with a caller-supplied engine and optional external resolver.
    ///
    /// Why: tests need to inject both a mock LLM engine and a mock external
    /// resolver independently; this overload allows both injections at once.
    /// What: the innermost execution entry point; all other `run*` variants
    /// delegate here.
    /// Test: used by resolver integration tests.
    ///
    /// # Errors
    ///
    /// Returns an error if DB queries or write-back fail.
    pub(crate) async fn run_with_engine_and_resolver(
        &self,
        db: &mut Database,
        engine: ClassificationEngine,
        resolver: Option<ExternalSourceResolver>,
    ) -> Result<ClassificationStats> {
        // 2. Read candidate commits. The default flow returns only the
        //    rows that lack a verdict; `--force` widens this to every row
        //    (optionally bounded by `--since`/`--until`/`--repos`).
        let commits = super::pipeline_db::read_candidate_commits(
            db,
            self.force,
            self.since.as_deref(),
            self.until.as_deref(),
            &self.repos,
        )?;
        let total = commits.len();
        info!(
            total,
            force = self.force,
            since = ?self.since,
            until = ?self.until,
            repos = ?self.repos,
            "starting classification"
        );

        if commits.is_empty() {
            return Ok(ClassificationStats::default());
        }

        // 3a. Tier 0 (override) pre-pass — done serially against the live
        //     DB connection. Commits with a hit skip the parallel cascade.
        let overrides = super::pipeline_db::read_overrides(db, &commits)?;

        // 3b. Tiers 1–3 in parallel for commits without an override.
        let pairs: Vec<(&str, bool)> = commits
            .iter()
            .map(|c| (c.message.as_str(), c.is_merge))
            .collect();
        let mut results = engine.classify_batch(&pairs);

        // Apply Tier-0 manual overrides (highest precedence).
        for (idx, commit) in commits.iter().enumerate() {
            if let Some(r) = overrides.get(&commit.id) {
                results[idx] = r.clone();
            }
        }

        // Tier 0.5: external sources (JIRA / GitHub Issues).
        //
        // Why: external ticket-type signals are more authoritative than
        // commit-message heuristics but must still defer to manual overrides
        // (Tier 0). We run the resolver serially (network I/O bound) for
        // commits that do not already have a Tier-0 override verdict.
        // The resolver caches results in-memory so the same ticket is only
        // fetched once per run, keeping the HTTP budget proportional to the
        // number of *unique* referenced tickets — not the number of commits.
        if let Some(res) = &resolver {
            let pb = super::pipeline_db::make_progress(commits.len() as u64, "External sources");
            for (idx, commit) in commits.iter().enumerate() {
                // Skip commits already resolved by Tier 0 (manual override).
                if overrides.contains_key(&commit.id) {
                    pb.inc(1);
                    continue;
                }
                if let Some(signal) = res.resolve(&commit.message).await {
                    let top_level = engine.taxonomy().resolve(&signal.category);
                    results[idx] = ClassificationResult {
                        category: signal.category,
                        subcategory: None,
                        top_level,
                        confidence: signal.confidence,
                        method: ClassificationMethod::ExternalSource,
                        ticket_id:
                            crate::classify::tiers::regex_tier::RegexMatcher::extract_ticket_id(
                                &commit.message,
                            ),
                        complexity: None,
                    };
                }
                pb.inc(1);
            }
            pb.finish_and_clear();
        }

        // 4. LLM fallback (async, bounded-concurrency) for entries whose
        //    verdict confidence is at or below `llm_fallback_threshold`. The
        //    default threshold is `0.65` (1.3.0+), which routes low-confidence
        //    deterministic verdicts (fuzzy 0.40/0.60, weighted-sum below 0.65)
        //    through the LLM when `use_llm: true`.
        //
        //    Fan-out is bounded by `llm_fallback_concurrency` via
        //    `buffer_unordered`, which yields ~order-of-magnitude wall-clock
        //    savings on large corpora compared to a serial `for ... .await`.
        //    We collect (commit_idx, new_result) pairs first, then write them
        //    back, so the borrow checker doesn't see mutable refs into
        //    `results` while futures are in flight.
        if engine.config().use_llm {
            // Single startup-time diagnostic when the LLM tier is on but no
            // credential is reachable. Without this, the fallback would emit
            // a warn-per-commit ("did not improve confidence") that obscures
            // the real misconfiguration.
            if matches!(engine.llm_has_api_key(), Some(false)) {
                warn!(
                    "LLM tier enabled but no API key resolved \
                     (OPENAI_API_KEY / OPENROUTER_API_KEY unset); \
                     fallback will short-circuit silently"
                );
            }

            let fallback_threshold = self
                .config
                .classification
                .as_ref()
                .map(|c| c.llm_fallback_threshold)
                .unwrap_or(0.65);
            let concurrency = self
                .config
                .classification
                .as_ref()
                .map(|c| c.llm_fallback_concurrency.max(1))
                .unwrap_or(8);

            // Pre-collect (idx, message, is_merge, original_confidence) for
            // every commit that needs an LLM call. The original verdict is
            // kept by index in `results` and consulted again at write-back.
            let pending: Vec<(usize, String, bool, f64)> = commits
                .iter()
                .enumerate()
                .filter_map(|(idx, commit)| {
                    if results[idx].confidence <= fallback_threshold {
                        Some((
                            idx,
                            commit.message.clone(),
                            commit.is_merge,
                            results[idx].confidence,
                        ))
                    } else {
                        None
                    }
                })
                .collect();

            let pb = super::pipeline_db::make_progress(pending.len() as u64, "LLM fallback");
            let engine_ref = &engine;
            let pb_ref = &pb;
            let new_results: Vec<(usize, ClassificationResult, f64)> =
                futures::stream::iter(pending.into_iter().map(
                    |(idx, message, _is_merge, original_conf)| async move {
                        // Direct LLM dispatch — calling `engine_ref.classify`
                        // here would re-run `classify_sync` first and short-
                        // circuit on the same low-confidence tier-1-3 verdict
                        // that triggered the fallback, so the LLM tier would
                        // never be reached (issue #99).
                        let r = engine_ref
                            .llm_classify_only(&message)
                            .await
                            .unwrap_or_else(ClassificationResult::unclassified);
                        pb_ref.inc(1);
                        (idx, r, original_conf)
                    },
                ))
                .buffer_unordered(concurrency)
                .collect()
                .await;
            pb.finish_and_clear();

            // Overwrite-guard: only adopt the LLM verdict if it strictly
            // improves confidence over the original. Otherwise keep the
            // tier-1..3 verdict so a failed/empty LLM call doesn't regress
            // confidence to 0.0. Errors inside `classify` are already
            // logged at lower layers and surfaced as low-confidence
            // verdicts; we treat them uniformly via this guard.
            for (idx, r, original_conf) in new_results {
                if r.confidence > original_conf {
                    results[idx] = r;
                } else {
                    warn!(
                        commit_idx = idx,
                        original_conf,
                        new_conf = r.confidence,
                        "LLM fallback did not improve confidence; keeping original verdict"
                    );
                }
            }
        }

        // 5. Write back + coverage bookkeeping.
        let checkpoint_every = self
            .config
            .classification
            .as_ref()
            .map(|c| c.checkpoint_every)
            .unwrap_or(0);
        let mut stats =
            super::pipeline_db::write_results(db, &commits, &results, checkpoint_every)?;
        super::pipeline_db::compute_coverage(&mut stats);
        super::pipeline_db::persist_repository_status(db, &stats)?;
        super::pipeline_db::report_coverage(&stats, self.min_coverage_pct());
        info!(
            total = stats.total_commits,
            classified = stats.classified,
            coverage_pct = stats.coverage_pct,
            "classification complete"
        );
        Ok(stats)
    }

    /// Effective minimum-coverage warning threshold for this pipeline.
    fn min_coverage_pct(&self) -> f64 {
        self.config
            .classification
            .as_ref()
            .map(|c| c.min_coverage_pct)
            .unwrap_or(DEFAULT_MIN_COVERAGE_PCT)
    }

    /// Backfill missing `complexity` scores for already-classified commits.
    ///
    /// Why: rows classified before this feature (or by non-LLM tiers) have
    /// `complexity IS NULL`. This fills them in without disturbing the
    /// existing category/confidence/method verdict, so a corpus can gain
    /// complexity scores incrementally.
    /// What: selects `classifications` rows where `complexity IS NULL` and
    /// `method != 'exact_rule'`, asks the LLM for a complexity score per
    /// commit, and writes only the `complexity` column back.
    /// Test: see `tests/` — pre-seed a NULL row and a scored row, run this,
    /// assert the NULL row is filled and the scored row is unchanged.
    ///
    /// # Errors
    ///
    /// Returns an error if engine construction or DB access fails.
    pub async fn backfill_complexity(&self, db: &mut Database) -> Result<usize> {
        let engine = self.build_engine().await?;
        Self::backfill_complexity_with_engine(db, &engine).await
    }

    /// Backfill complexity using a caller-supplied engine.
    ///
    /// Why: tests inject an engine wired to a mock LLM endpoint;
    /// [`Self::backfill_complexity`] builds the engine and delegates here.
    /// What: the engine-agnostic core of the backfill.
    /// Test: the backfill integration test calls this directly.
    ///
    /// # Errors
    ///
    /// Returns an error if DB access fails.
    pub(crate) async fn backfill_complexity_with_engine(
        db: &mut Database,
        engine: &ClassificationEngine,
    ) -> Result<usize> {
        // Collect candidate rows. Rows produced by
        // the `exact_rule` tier are excluded — they were never LLM-eligible.
        let candidates = super::pipeline_db::read_complexity_backfill_candidates(db)?;
        let total = candidates.len();
        info!(total, "starting complexity backfill");
        if candidates.is_empty() {
            return Ok(0);
        }

        let pb = super::pipeline_db::make_progress(total as u64, "Complexity backfill");
        let mut updated = 0_usize;
        {
            let conn = db.connection_mut();
            let tx = conn.transaction().map_err(crate::core::TgaError::from)?;
            {
                let mut update_stmt = tx
                    .prepare("UPDATE classifications SET complexity = ?1 WHERE id = ?2")
                    .map_err(crate::core::TgaError::from)?;
                for cand in &candidates {
                    let verdict = engine.llm_classify_only(&cand.message).await;
                    let complexity = verdict.and_then(|r| r.complexity);
                    match complexity {
                        Some(score) => {
                            update_stmt
                                .execute(params![score as i64, cand.classification_id])
                                .map_err(crate::core::TgaError::from)?;
                            updated += 1;
                            info!(
                                commit_sha = %cand.commit_sha,
                                score,
                                "backfilled complexity"
                            );
                        }
                        None => {
                            warn!(
                                commit_sha = %cand.commit_sha,
                                "LLM returned no complexity score; leaving NULL"
                            );
                        }
                    }
                    pb.inc(1);
                }
            }
            tx.commit().map_err(crate::core::TgaError::from)?;
        }
        pb.finish_and_clear();
        info!(updated, total, "complexity backfill complete");
        Ok(updated)
    }
}
