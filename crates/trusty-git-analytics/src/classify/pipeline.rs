//! End-to-end classification pipeline: read DB → classify → write back.

use std::collections::HashMap;

use futures::stream::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use rusqlite::params;
use tracing::{info, warn};

use crate::classify::classifier::{ClassificationEngine, ClassificationEngineConfig};
use crate::classify::errors::Result;
use crate::classify::rules::{default_rules, load_rules};
use crate::classify::sources::ExternalSourceResolver;
use crate::classify::tiers::ClassificationResult;
use crate::core::config::Config;
use crate::core::db::{CheckpointMode, Database};
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
/// What: holds the validated [`Config`] plus toggles for `force` re-classify
/// and `since` lower-bound. Built via [`Self::new`] + builder methods.
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
    /// Test: exercised indirectly by the pipeline integration tests.
    ///
    /// # Errors
    ///
    /// Returns an error if rules fail to load/compile or the LLM provider
    /// fails to initialize.
    fn build_engine(&self) -> Result<ClassificationEngine> {
        let ruleset = match self
            .config
            .classification
            .as_ref()
            .and_then(|c| c.rules_file.as_ref())
        {
            Some(path) => {
                let custom = load_rules(path)?;
                if custom.extend_defaults {
                    // Merge: start with defaults, let custom rules override by id
                    let mut merged = default_rules();
                    let custom_ids: std::collections::HashSet<String> =
                        custom.rules.iter().map(|r| r.id.clone()).collect();
                    // Remove any default rules whose id is overridden by custom
                    merged.rules.retain(|r| !custom_ids.contains(&r.id));
                    // Append custom rules (they may have higher priority to win)
                    merged.rules.extend(custom.rules);
                    merged
                } else {
                    custom
                }
            }
            None => default_rules(),
        };

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

        ClassificationEngine::with_taxonomy_mappings_and_confidence(
            ruleset,
            engine_cfg,
            custom_taxonomy,
            jira_mappings,
            jira_confidence,
            // Override-tier DB wiring is deferred: rusqlite::Connection is
            // not Send + Sync, so plumbing the live connection through the
            // Rayon batch would require a redesign. The override tier is
            // still constructible via `with_taxonomy_and_mappings` for
            // single-threaded callers and tests.
            None,
        )
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
        // 1. Build engine.
        let engine = self.build_engine()?;
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
        //    (optionally bounded by `--since`).
        let commits = read_candidate_commits(db, self.force, self.since.as_deref())?;
        let total = commits.len();
        info!(
            total,
            force = self.force,
            since = ?self.since,
            "starting classification"
        );

        if commits.is_empty() {
            return Ok(ClassificationStats::default());
        }

        // 3a. Tier 0 (override) pre-pass — done serially against the live
        //     DB connection. Commits with a hit skip the parallel cascade.
        let overrides = read_overrides(db, &commits)?;

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
            let pb = make_progress(commits.len() as u64, "External sources");
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

            let pb = make_progress(pending.len() as u64, "LLM fallback");
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
        let mut stats = write_results(db, &commits, &results, checkpoint_every)?;
        compute_coverage(&mut stats);
        persist_repository_status(db, &stats)?;
        report_coverage(&stats, self.min_coverage_pct());
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
        let engine = self.build_engine()?;
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
        let candidates = read_complexity_backfill_candidates(db)?;
        let total = candidates.len();
        info!(total, "starting complexity backfill");
        if candidates.is_empty() {
            return Ok(0);
        }

        let pb = make_progress(total as u64, "Complexity backfill");
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

/// A `classifications` row eligible for complexity backfill.
struct ComplexityBackfillCandidate {
    /// Primary key of the `classifications` row to update.
    classification_id: i64,
    /// SHA of the linked commit (for logging/diagnostics).
    commit_sha: String,
    /// Commit message, used as LLM input.
    message: String,
}

/// Read classification rows lacking a complexity score.
///
/// Why: the backfill must target only LLM-eligible rows; `exact_rule`
/// verdicts never carried complexity, so they are excluded.
/// What: joins `classifications` to `commits` via `classification_id` and
/// returns rows where `complexity IS NULL` and `method != 'exact_rule'`.
/// Test: covered by the backfill integration test.
fn read_complexity_backfill_candidates(db: &Database) -> Result<Vec<ComplexityBackfillCandidate>> {
    let mut stmt = db
        .connection()
        .prepare(
            "SELECT cl.id, c.sha, c.message \
             FROM classifications cl \
             JOIN commits c ON c.classification_id = cl.id \
             WHERE cl.complexity IS NULL AND cl.method != 'exact_rule'",
        )
        .map_err(crate::core::TgaError::from)?;
    let rows = stmt
        .query_map([], |row| {
            Ok(ComplexityBackfillCandidate {
                classification_id: row.get(0)?,
                commit_sha: row.get(1)?,
                message: row.get(2)?,
            })
        })
        .map_err(crate::core::TgaError::from)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(crate::core::TgaError::from)?);
    }
    Ok(out)
}

/// Look up manual classification overrides for the given commits.
///
/// Returns a map keyed by commit row id; only commits with a hit are
/// present in the result. Performs one prepared query per commit; for
/// the typical "small N" override list this is cheaper than building an
/// in-memory index.
fn read_overrides(
    db: &Database,
    commits: &[CommitRow],
) -> Result<HashMap<i64, ClassificationResult>> {
    use crate::classify::taxonomy::TaxonomyRegistry;
    use crate::core::models::ClassificationMethod;

    let mut out: HashMap<i64, ClassificationResult> = HashMap::new();
    if commits.is_empty() {
        return Ok(out);
    }
    let taxonomy = TaxonomyRegistry::with_builtins();
    let conn = db.connection();
    let mut stmt = conn
        .prepare(
            "SELECT work_type, change_type FROM classification_overrides \
             WHERE commit_sha = ?1 AND repo_path = ?2",
        )
        .map_err(crate::core::TgaError::from)?;
    for commit in commits {
        let row = stmt.query_row(params![commit.sha, commit.repository], |row| {
            let work_type: String = row.get(0)?;
            let change_type: String = row.get(1)?;
            Ok((work_type, change_type))
        });
        match row {
            Ok((work_type, change_type)) => {
                let top_level = taxonomy
                    .resolve(&change_type)
                    .or_else(|| taxonomy.resolve(&work_type));
                out.insert(
                    commit.id,
                    ClassificationResult {
                        category: work_type,
                        subcategory: Some(change_type),
                        top_level,
                        confidence: 1.0,
                        method: ClassificationMethod::Manual,
                        ticket_id: None,
                        complexity: None,
                    },
                );
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => {}
            Err(e) => return Err(crate::core::TgaError::from(e).into()),
        }
    }
    Ok(out)
}

/// Fill in `coverage_pct` and `coverage_by_repo[_].coverage_pct` from the
/// already-populated totals.
fn compute_coverage(stats: &mut ClassificationStats) {
    stats.coverage_pct = if stats.total_commits == 0 {
        0.0
    } else {
        (stats.classified as f64 / stats.total_commits as f64) * 100.0
    };
    for repo in stats.coverage_by_repo.values_mut() {
        repo.coverage_pct = if repo.total == 0 {
            0.0
        } else {
            (repo.classified as f64 / repo.total as f64) * 100.0
        };
    }
}

/// Upsert one `repository_analysis_status` row per repo seen in `stats`.
fn persist_repository_status(db: &mut Database, stats: &ClassificationStats) -> Result<()> {
    if stats.coverage_by_repo.is_empty() {
        return Ok(());
    }
    let conn = db.connection_mut();
    let tx = conn.transaction().map_err(crate::core::TgaError::from)?;
    {
        let mut upsert = tx
            .prepare(
                "INSERT INTO repository_analysis_status \
                    (repo_name, last_analyzed_at, classification_coverage_pct, \
                     total_commits, classified_commits) \
                 VALUES (?1, datetime('now'), ?2, ?3, ?4) \
                 ON CONFLICT(repo_name) DO UPDATE SET \
                    last_analyzed_at = datetime('now'), \
                    classification_coverage_pct = excluded.classification_coverage_pct, \
                    total_commits = excluded.total_commits, \
                    classified_commits = excluded.classified_commits",
            )
            .map_err(crate::core::TgaError::from)?;
        for (repo, cov) in &stats.coverage_by_repo {
            upsert
                .execute(params![
                    repo,
                    cov.coverage_pct,
                    cov.total as i64,
                    cov.classified as i64,
                ])
                .map_err(crate::core::TgaError::from)?;
        }
    }
    tx.commit().map_err(crate::core::TgaError::from)?;
    Ok(())
}

/// Emit `info!` (always) and `warn!` (when below `threshold_pct`) lines
/// summarizing the coverage achieved by this run.
fn report_coverage(stats: &ClassificationStats, threshold_pct: f64) {
    info!(
        "Classification coverage: {:.1}% ({} / {})",
        stats.coverage_pct, stats.classified, stats.total_commits
    );
    if stats.coverage_pct < threshold_pct && stats.total_commits > 0 {
        warn!(
            coverage_pct = stats.coverage_pct,
            threshold_pct, "classification coverage below configured threshold"
        );
    }
}

/// Minimal projection of a commit row needed for classification.
#[allow(dead_code)]
struct CommitRow {
    id: i64,
    sha: String,
    message: String,
    is_merge: bool,
    repository: String,
    /// Pre-existing classification row id, if any.
    ///
    /// Populated by [`read_candidate_commits`] for `--force` re-runs so
    /// the write-back path can UPDATE the existing `classifications` row
    /// in place instead of inserting an orphan. `None` for first-time
    /// classification (the default flow).
    existing_classification_id: Option<i64>,
}

/// Decide whether a `(category, subcategory)` verdict represents a revert.
///
/// Why: the `commits.is_revert` boolean must mirror the verdict produced by
/// the classification cascade so downstream DORA queries (CFR, MTTR) can
/// join through it without re-running the rules. Issue #210 — before this
/// helper, `is_revert` was never set at classify-time.
/// What: returns `true` when `category` or `subcategory` (case-insensitive)
/// is one of the canonical revert markers: `"revert"`, `"rollback"`.
/// Test: see `is_revert_verdict_*` unit tests below.
fn is_revert_verdict(category: &str, subcategory: Option<&str>) -> bool {
    fn matches(s: &str) -> bool {
        s.eq_ignore_ascii_case("revert") || s.eq_ignore_ascii_case("rollback")
    }
    if matches(category) {
        return true;
    }
    if let Some(sub) = subcategory {
        if matches(sub) {
            return true;
        }
    }
    false
}

/// Read candidates for the classification cascade.
///
/// Why: `--force` (issue #205) re-classifies commits that already have a
/// verdict, optionally bounded by `--since DATE`. The default flow keeps
/// the historical "skip-if-classified" semantics so incremental runs stay
/// cheap.
/// What: builds a SELECT whose WHERE clause toggles based on `force`:
///
///   * `!force`               → `WHERE classification_id IS NULL` (legacy)
///   * `force, since = None`  → no WHERE (every commit)
///   * `force, since = Some`  → `WHERE timestamp >= ?`
///
/// Test: covered by `read_candidate_commits_*` unit tests and the
/// `pipeline_force_*` integration tests in this module.
fn read_candidate_commits(
    db: &Database,
    force: bool,
    since: Option<&str>,
) -> Result<Vec<CommitRow>> {
    use rusqlite::params_from_iter;
    use rusqlite::types::Value;

    let (sql, params): (&str, Vec<Value>) = match (force, since) {
        (false, _) => (
            "SELECT id, sha, message, is_merge, repository, classification_id \
             FROM commits WHERE classification_id IS NULL",
            Vec::new(),
        ),
        (true, None) => (
            "SELECT id, sha, message, is_merge, repository, classification_id \
             FROM commits",
            Vec::new(),
        ),
        (true, Some(date)) => (
            "SELECT id, sha, message, is_merge, repository, classification_id \
             FROM commits WHERE timestamp >= ?1",
            vec![Value::Text(date.to_string())],
        ),
    };

    let mut stmt = db
        .connection()
        .prepare(sql)
        .map_err(crate::core::TgaError::from)?;
    let rows = stmt
        .query_map(params_from_iter(params.iter()), |row| {
            Ok(CommitRow {
                id: row.get(0)?,
                sha: row.get(1)?,
                message: row.get(2)?,
                is_merge: row.get::<_, i64>(3)? != 0,
                repository: row.get(4)?,
                existing_classification_id: row.get(5)?,
            })
        })
        .map_err(crate::core::TgaError::from)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(crate::core::TgaError::from)?);
    }
    Ok(out)
}

/// Write classification results to the database with optional periodic WAL
/// checkpointing (issue #298).
///
/// Why: on large corpora a single long transaction accumulates hundreds of
/// megabytes of WAL data; a crash mid-run loses everything since the last
/// automatic checkpoint. When `checkpoint_every > 0`, the function breaks
/// the write into micro-batches and calls `PRAGMA wal_checkpoint(PASSIVE)`
/// after each batch, bounding the crash-window data-loss to at most
/// `checkpoint_every` rows.
///
/// What: splits `commits/results` into chunks of `checkpoint_every` (or one
/// big chunk when `checkpoint_every == 0`), writes each chunk in a
/// transaction, and calls a PASSIVE checkpoint between chunks.
///
/// Test: covered by `tests::pipeline_checkpoint_every_called_during_long_run`.
fn write_results(
    db: &mut Database,
    commits: &[CommitRow],
    results: &[ClassificationResult],
    checkpoint_every: usize,
) -> Result<ClassificationStats> {
    let mut stats = ClassificationStats {
        total_commits: commits.len(),
        ..Default::default()
    };

    // Determine the effective chunk size. `0` means "one big transaction".
    let chunk_size = if checkpoint_every > 0 {
        checkpoint_every
    } else {
        commits.len().max(1) // at least 1 to avoid divide-by-zero
    };

    let pb = make_progress(commits.len() as u64, "Writing results");

    for (chunk_commits, chunk_results) in commits.chunks(chunk_size).zip(results.chunks(chunk_size))
    {
        write_results_chunk(db, chunk_commits, chunk_results, &mut stats, &pb)?;

        // Issue #298: periodic PASSIVE checkpoint to flush WAL frames and
        // limit crash-window data loss. Only when chunk_size < total
        // (i.e. `checkpoint_every > 0`), so we don't add overhead to the
        // default single-chunk path.
        if checkpoint_every > 0 && chunk_commits.len() == chunk_size {
            if let Err(e) = db.wal_checkpoint(CheckpointMode::Passive) {
                warn!(error = %e, "periodic WAL PASSIVE checkpoint failed (non-fatal)");
            }
        }
    }

    pb.finish_and_clear();

    if stats.classified < stats.total_commits {
        warn!(
            unclassified = stats.total_commits - stats.classified,
            "some commits remained uncategorized"
        );
    }
    Ok(stats)
}

/// Write one chunk of classification results inside a single transaction.
///
/// Why: extracted from `write_results` so the periodic-checkpoint loop stays
/// readable and the transaction boundary is explicit.
/// What: inserts or updates `classifications` rows and updates `commits` for
/// every `(commit, result)` pair in the chunk. Stats are accumulated in place.
/// Test: exercised indirectly by all `write_results` callers.
fn write_results_chunk(
    db: &mut Database,
    commits: &[CommitRow],
    results: &[ClassificationResult],
    stats: &mut ClassificationStats,
    pb: &indicatif::ProgressBar,
) -> Result<()> {
    let conn = db.connection_mut();
    let tx = conn.transaction().map_err(crate::core::TgaError::from)?;
    {
        let mut insert_classification = tx
            .prepare(
                "INSERT INTO classifications \
                    (category, subcategory, ticket_id, confidence, method, complexity) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )
            .map_err(crate::core::TgaError::from)?;
        // Issue #205: when `--force` reclassifies a commit that already
        // has a `classifications` row, UPDATE it in place rather than
        // inserting a fresh row and orphaning the old one. This keeps
        // the table free of dangling rows that no commit points at.
        let mut update_existing_classification = tx
            .prepare(
                "UPDATE classifications \
                 SET category = ?1, subcategory = ?2, ticket_id = ?3, \
                     confidence = ?4, method = ?5, complexity = ?6 \
                 WHERE id = ?7",
            )
            .map_err(crate::core::TgaError::from)?;
        // The UPDATE also sets `is_revert` so the column stays in sync with
        // the verdict category. Without this branch, the column remains 0
        // forever even when classification correctly identified a revert
        // (issue #210). The downstream `backfill revert-flags` path relies on
        // commit-message heuristics rather than classification verdicts, so
        // syncing here is the only place `is_revert` is set automatically.
        let mut update_commit = tx
            .prepare(
                "UPDATE commits SET classification_id = ?1, confidence = ?2, is_revert = ?3 \
                 WHERE id = ?4",
            )
            .map_err(crate::core::TgaError::from)?;

        for (commit, result) in commits.iter().zip(results.iter()) {
            let classification_id = if let Some(existing) = commit.existing_classification_id {
                update_existing_classification
                    .execute(params![
                        result.category,
                        result.subcategory,
                        result.ticket_id,
                        result.confidence,
                        result.method.as_str(),
                        result.complexity.map(|v| v as i64),
                        existing,
                    ])
                    .map_err(crate::core::TgaError::from)?;
                existing
            } else {
                insert_classification
                    .execute(params![
                        result.category,
                        result.subcategory,
                        result.ticket_id,
                        result.confidence,
                        result.method.as_str(),
                        result.complexity.map(|v| v as i64),
                    ])
                    .map_err(crate::core::TgaError::from)?;
                tx.last_insert_rowid()
            };
            // Treat `revert` and `rollback` as the canonical revert
            // categories. Match `subcategory` too because some rules attach
            // the revert signal at the subcategory level (e.g. a merge
            // whose subcategory is "revert"). Comparison is
            // case-insensitive to absorb taxonomy-extension variations.
            let is_revert = is_revert_verdict(&result.category, result.subcategory.as_deref());
            update_commit
                .execute(params![
                    classification_id,
                    result.confidence,
                    if is_revert { 1_i64 } else { 0_i64 },
                    commit.id,
                ])
                .map_err(crate::core::TgaError::from)?;

            // "Classified" means non-null, non-`"uncategorized"` category.
            let is_classified = !result.category.is_empty()
                && result.category != "uncategorized"
                && result.confidence > 0.0;
            if is_classified {
                stats.classified += 1;
            }
            let repo_entry = stats
                .coverage_by_repo
                .entry(commit.repository.clone())
                .or_default();
            repo_entry.total += 1;
            if is_classified {
                repo_entry.classified += 1;
            }
            *stats
                .by_method
                .entry(result.method.as_str().to_string())
                .or_insert(0) += 1;
            *stats
                .by_category
                .entry(result.category.clone())
                .or_insert(0) += 1;
            pb.inc(1);
        }
    }
    tx.commit().map_err(crate::core::TgaError::from)?;
    Ok(())
}

fn make_progress(len: u64, label: &str) -> ProgressBar {
    let pb = ProgressBar::new(len);
    if let Ok(style) =
        ProgressStyle::with_template("{prefix:.bold} [{bar:40.cyan/blue}] {pos}/{len} ({percent}%)")
    {
        pb.set_style(style.progress_chars("##-"));
    }
    pb.set_prefix(label.to_string());
    pb
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::classifier::{ClassificationEngine, ClassificationEngineConfig};
    use crate::classify::rules::default_rules;
    use crate::core::config::Config;
    use rusqlite::params;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Build an engine whose LLM tier points at `endpoint`.
    fn engine_with_mock_llm(endpoint: &str) -> ClassificationEngine {
        let cfg = ClassificationEngineConfig {
            use_llm: true,
            ..ClassificationEngineConfig::default()
        };
        ClassificationEngine::new(default_rules(), cfg)
            .expect("build engine")
            .with_test_llm_endpoint(endpoint)
    }

    /// Stand up a mock chat-completions endpoint returning a fixed verdict.
    async fn mock_llm_server(category: &str, confidence: f64, complexity: u8) -> MockServer {
        let server = MockServer::start().await;
        let body = serde_json::json!({
            "choices": [{
                "message": {
                    "content": format!(
                        "{{\"category\":\"{category}\",\"subcategory\":null,\
                          \"confidence\":{confidence},\"complexity\":{complexity}}}"
                    )
                }
            }]
        });
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;
        server
    }

    /// Insert a commit row with no classification. Returns its row id.
    fn insert_commit(db: &Database, sha: &str, message: &str) -> i64 {
        db.connection()
            .execute(
                "INSERT INTO commits \
                 (sha, author_name, author_email, timestamp, message, repository) \
                 VALUES (?1, 'a', 'a@x', '2024-01-01T00:00:00Z', ?2, 'acme/widgets')",
                params![sha, message],
            )
            .expect("insert commit");
        db.connection().last_insert_rowid()
    }

    /// Why: regression guard for the `--force` retroactive flow (issue
    /// #205). Without `--force`, classified commits are skipped; with
    /// `--force`, they must be re-classified and the existing
    /// `classifications` row updated in place (no orphans). The fixture
    /// pre-seeds a "wrong" verdict that the default ruleset's
    /// conventional-commit tier will overwrite with the correct one.
    /// What: seed one commit with a manually-attached classification
    /// that disagrees with the default rules, run `tga classify --force`,
    /// assert the row's `classifications` was updated (same id, new
    /// category) and that no orphan rows were inserted.
    /// Test: in-memory DB; no LLM.
    #[tokio::test]
    async fn pipeline_force_reclassifies_existing_rows_in_place() {
        let mut db = Database::open_in_memory().expect("db");

        // Pre-seed a (wrong) classification: category "feature" for a
        // message that the cc-fix rule will correctly classify as
        // "bugfix" on a force re-run.
        db.connection()
            .execute(
                "INSERT INTO classifications (category, subcategory, confidence, method) \
                 VALUES ('feature', NULL, 0.5, 'regex_rule')",
                [],
            )
            .expect("insert classification");
        let pre_cls_id = db.connection().last_insert_rowid();
        let commit_id = insert_commit(&db, "sha-fix-1", "fix: handle null user");
        db.connection()
            .execute(
                "UPDATE commits SET classification_id = ?1 WHERE id = ?2",
                params![pre_cls_id, commit_id],
            )
            .expect("link cls");

        // Default flow (no force) should skip — verdict stays as 'feature'.
        let pipeline_no_force = ClassificationPipeline::new(Config::default());
        let engine =
            ClassificationEngine::new(default_rules(), ClassificationEngineConfig::default())
                .expect("engine");
        pipeline_no_force
            .run_with_engine(&mut db, engine)
            .await
            .expect("default run");
        let still_feature: String = db
            .connection()
            .query_row(
                "SELECT cl.category FROM classifications cl \
                 JOIN commits c ON c.classification_id = cl.id WHERE c.sha = 'sha-fix-1'",
                [],
                |row| row.get(0),
            )
            .expect("query 1");
        assert_eq!(
            still_feature, "feature",
            "default flow must NOT re-classify already-classified commits"
        );

        // --force flips the verdict to 'bugfix' via the cc-fix rule.
        let pipeline_forced = ClassificationPipeline::new(Config::default()).with_force(true);
        let engine =
            ClassificationEngine::new(default_rules(), ClassificationEngineConfig::default())
                .expect("engine");
        pipeline_forced
            .run_with_engine(&mut db, engine)
            .await
            .expect("force run");

        // Verdict updated (and joined via the same row id — no orphan).
        let (new_cat, new_cls_id): (String, i64) = db
            .connection()
            .query_row(
                "SELECT cl.category, cl.id FROM classifications cl \
                 JOIN commits c ON c.classification_id = cl.id WHERE c.sha = 'sha-fix-1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("query 2");
        assert_eq!(new_cat, "bugfix");
        assert_eq!(
            new_cls_id, pre_cls_id,
            "force must update in place, not orphan"
        );

        let total_rows: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM classifications", [], |row| row.get(0))
            .expect("count");
        assert_eq!(
            total_rows, 1,
            "force must not duplicate the classifications row"
        );
    }

    /// Why: `--since` bounds the scope of `--force` to a recent window
    /// so operators can rewrite only the last quarter (or any window)
    /// without re-classifying years of history.
    /// What: seed two commits — one in 2023, one in 2025 — pre-classify
    /// both as "feature", then run `tga classify --force --since 2025-01-01`
    /// and assert only the 2025 commit was rewritten.
    /// Test: in-memory DB; commits.timestamp is ISO8601 string.
    #[tokio::test]
    async fn pipeline_force_since_bounds_rewrite_window() {
        let mut db = Database::open_in_memory().expect("db");

        // Seed two pre-classified rows, then re-date their commits.
        for (sha, ts) in [
            ("sha-old", "2023-06-01T00:00:00Z"),
            ("sha-new", "2025-06-01T00:00:00Z"),
        ] {
            db.connection()
                .execute(
                    "INSERT INTO classifications (category, confidence, method) \
                     VALUES ('feature', 0.5, 'regex_rule')",
                    [],
                )
                .expect("insert cls");
            let cls_id = db.connection().last_insert_rowid();
            db.connection()
                .execute(
                    "INSERT INTO commits (sha, author_name, author_email, timestamp, message, repository, classification_id) \
                     VALUES (?1, 'a', 'a@x', ?2, 'fix: handle null user', 'r', ?3)",
                    params![sha, ts, cls_id],
                )
                .expect("insert commit");
        }

        // --force --since 2025-01-01 — only sha-new is in scope.
        let pipeline = ClassificationPipeline::new(Config::default())
            .with_force(true)
            .with_since(Some("2025-01-01".to_string()));
        let engine =
            ClassificationEngine::new(default_rules(), ClassificationEngineConfig::default())
                .expect("engine");
        pipeline
            .run_with_engine(&mut db, engine)
            .await
            .expect("force+since");

        // sha-new now classified as bugfix; sha-old still feature.
        let new_cat: String = db
            .connection()
            .query_row(
                "SELECT cl.category FROM classifications cl \
                 JOIN commits c ON c.classification_id = cl.id WHERE c.sha = 'sha-new'",
                [],
                |row| row.get(0),
            )
            .expect("query new");
        assert_eq!(new_cat, "bugfix");

        let old_cat: String = db
            .connection()
            .query_row(
                "SELECT cl.category FROM classifications cl \
                 JOIN commits c ON c.classification_id = cl.id WHERE c.sha = 'sha-old'",
                [],
                |row| row.get(0),
            )
            .expect("query old");
        assert_eq!(
            old_cat, "feature",
            "--since must exclude commits older than the bound"
        );
    }

    /// Why: `read_candidate_commits` is the single SQL-builder for the
    /// pipeline's candidate set; regressions here would silently change
    /// which commits get re-classified.
    /// What: probes each of the three branches: default (NULL only),
    /// force (all), force+since (windowed).
    /// Test: pure SQL exercise against an in-memory DB.
    #[test]
    fn read_candidate_commits_branches_select_correctly() {
        let db = Database::open_in_memory().expect("db");
        // Two classified commits + one unclassified.
        db.connection()
            .execute(
                "INSERT INTO classifications (category, confidence, method) \
                 VALUES ('feature', 0.5, 'regex_rule')",
                [],
            )
            .expect("cls");
        let cls_id = db.connection().last_insert_rowid();
        db.connection()
            .execute(
                "INSERT INTO commits (sha, author_name, author_email, timestamp, message, repository, classification_id) \
                 VALUES ('old', 'a', 'a@x', '2023-01-01T00:00:00Z', 'm', 'r', ?1)",
                params![cls_id],
            )
            .expect("insert old");
        db.connection()
            .execute(
                "INSERT INTO commits (sha, author_name, author_email, timestamp, message, repository, classification_id) \
                 VALUES ('new', 'a', 'a@x', '2025-06-01T00:00:00Z', 'm', 'r', ?1)",
                params![cls_id],
            )
            .expect("insert new");
        db.connection()
            .execute(
                "INSERT INTO commits (sha, author_name, author_email, timestamp, message, repository) \
                 VALUES ('null', 'a', 'a@x', '2024-01-01T00:00:00Z', 'm', 'r')",
                [],
            )
            .expect("insert unclassified");

        // Default flow: only the unclassified row.
        let v = super::read_candidate_commits(&db, false, None).expect("default");
        let shas: Vec<&str> = v.iter().map(|c| c.sha.as_str()).collect();
        assert_eq!(shas, vec!["null"]);

        // --force: every row.
        let v = super::read_candidate_commits(&db, true, None).expect("force");
        let mut shas: Vec<&str> = v.iter().map(|c| c.sha.as_str()).collect();
        shas.sort();
        assert_eq!(shas, vec!["new", "null", "old"]);

        // --force --since 2025-01-01: only "new" (timestamp >= bound).
        let v = super::read_candidate_commits(&db, true, Some("2025-01-01")).expect("force+since");
        let shas: Vec<&str> = v.iter().map(|c| c.sha.as_str()).collect();
        assert_eq!(shas, vec!["new"]);
    }

    /// Why: regression guard for issue #210. `commits.is_revert` was always
    /// `0` because the classify-time UPDATE never touched it; only the
    /// commit-message-heuristic backfill ever flipped the column. Any commit
    /// whose verdict category is `revert` or `rollback` should now show
    /// `is_revert = 1` after `classify` runs.
    /// What: seed two commits — one with a `Revert "feat: ..."` message that
    /// the default ruleset classifies as `cc-revert`, and one ordinary
    /// `feat: add login` commit — run the synchronous (no-LLM) pipeline,
    /// then assert the revert commit's `is_revert` is `1` and the feature
    /// commit's is `0`.
    /// Test: in-memory DB, default rules, no mocked LLM (use_llm = false).
    #[tokio::test]
    async fn pipeline_sets_is_revert_for_revert_verdicts() {
        let mut db = Database::open_in_memory().expect("db");
        // `cc-revert` rule (priority 115, confidence 0.9) wins on this msg.
        let revert_id = insert_commit(&db, "sha-revert", "Revert \"feat: add login\"");
        // `cc-feat` rule (priority 100, confidence 0.95) wins on this msg.
        let feature_id = insert_commit(&db, "sha-feat", "feat: add login form");

        let config = Config::default();
        let pipeline = ClassificationPipeline::new(config);
        let engine =
            ClassificationEngine::new(default_rules(), ClassificationEngineConfig::default())
                .expect("engine builds");
        pipeline
            .run_with_engine(&mut db, engine)
            .await
            .expect("run pipeline");

        let revert_flag: i64 = db
            .connection()
            .query_row(
                "SELECT is_revert FROM commits WHERE id = ?1",
                params![revert_id],
                |row| row.get(0),
            )
            .expect("query revert");
        assert_eq!(
            revert_flag, 1,
            "revert verdict must set commits.is_revert=1"
        );

        let feat_flag: i64 = db
            .connection()
            .query_row(
                "SELECT is_revert FROM commits WHERE id = ?1",
                params![feature_id],
                |row| row.get(0),
            )
            .expect("query feature");
        assert_eq!(
            feat_flag, 0,
            "non-revert verdict must leave commits.is_revert at 0"
        );
    }

    /// Why: `is_revert_verdict` is the single source of truth for which
    /// classification verdicts flip `commits.is_revert`. A regression here
    /// (e.g. accepting "reverted" or rejecting "rollback") would propagate
    /// silently into DORA reports.
    /// What: exercises every accept/reject path on both `category` and
    /// `subcategory`.
    /// Test: pure-function table.
    #[test]
    fn is_revert_verdict_recognizes_canonical_markers() {
        assert!(super::is_revert_verdict("revert", None));
        assert!(super::is_revert_verdict("Revert", None)); // case-insensitive
        assert!(super::is_revert_verdict("rollback", None));
        assert!(super::is_revert_verdict("ROLLBACK", None));
        assert!(super::is_revert_verdict("merge", Some("revert")));
        assert!(super::is_revert_verdict("merge", Some("rollback")));
        assert!(!super::is_revert_verdict("feature", None));
        assert!(!super::is_revert_verdict("bugfix", Some("hotfix")));
        // "reverted" is not a canonical marker — only the exact words match.
        assert!(!super::is_revert_verdict("reverted", None));
    }

    /// Why: a complexity score from the LLM must reach the `classifications`
    /// table; if the INSERT drops the column the score is silently lost.
    /// What: classify an unclassified commit via a mock LLM that returns
    /// `complexity: 2`, then assert the persisted row has `complexity = 2`.
    /// Test: in-memory DB + wiremock; run the pipeline, query the column.
    #[tokio::test]
    async fn pipeline_writes_complexity_to_db() {
        let server = mock_llm_server("feature", 0.9, 2).await;
        let endpoint = format!("{}/v1/chat/completions", server.uri());

        let mut db = Database::open_in_memory().expect("db");
        // A message long enough (>= 12 chars) to skip the fuzzy "short =
        // chore" heuristic and devoid of rule keywords, so tiers 1–3 all
        // miss and the commit falls through to the LLM tier.
        insert_commit(&db, "sha-a", "zzz qqq vvv www yyy uuu");

        // Route every tier-1..3 verdict through the LLM by setting a
        // fallback threshold above any rule-tier confidence. Without this a
        // low-confidence catch-all rule would pre-empt the LLM tier.
        let classification = crate::core::config::ClassificationConfig {
            use_llm: true,
            llm_fallback_threshold: 1.0,
            ..crate::core::config::ClassificationConfig::default()
        };
        let config = Config {
            classification: Some(classification),
            ..Config::default()
        };

        let pipeline = ClassificationPipeline::new(config);
        let engine = engine_with_mock_llm(&endpoint);
        pipeline
            .run_with_engine(&mut db, engine)
            .await
            .expect("run pipeline");

        let complexity: Option<i64> = db
            .connection()
            .query_row(
                "SELECT cl.complexity FROM classifications cl \
                 JOIN commits c ON c.classification_id = cl.id \
                 WHERE c.sha = 'sha-a'",
                [],
                |row| row.get(0),
            )
            .expect("query complexity");
        assert_eq!(complexity, Some(2));
    }

    /// Why: the backfill must fill NULL complexity rows without disturbing
    /// rows that already have a score.
    /// What: seed one classification with `complexity IS NULL` and one with
    /// `complexity = 3`; run the backfill against a mock LLM that returns
    /// `complexity: 4`; assert the NULL row becomes 4 and the other stays 3.
    /// Test: in-memory DB + wiremock.
    #[tokio::test]
    async fn backfill_complexity_updates_only_null_rows() {
        let server = mock_llm_server("feature", 0.9, 4).await;
        let endpoint = format!("{}/v1/chat/completions", server.uri());

        let mut db = Database::open_in_memory().expect("db");

        // Row 1: classified, complexity NULL, non-exact method → a candidate.
        db.connection()
            .execute(
                "INSERT INTO classifications (category, confidence, method, complexity) \
                 VALUES ('feature', 0.5, 'regex_rule', NULL)",
                [],
            )
            .expect("insert cl 1");
        let cl1 = db.connection().last_insert_rowid();
        let c1 = insert_commit(&db, "sha-null", "needs scoring");
        db.connection()
            .execute(
                "UPDATE commits SET classification_id = ?1 WHERE id = ?2",
                params![cl1, c1],
            )
            .expect("link 1");

        // Row 2: already scored (complexity = 3) → must be left untouched.
        db.connection()
            .execute(
                "INSERT INTO classifications (category, confidence, method, complexity) \
                 VALUES ('bugfix', 0.8, 'regex_rule', 3)",
                [],
            )
            .expect("insert cl 2");
        let cl2 = db.connection().last_insert_rowid();
        let c2 = insert_commit(&db, "sha-scored", "already scored");
        db.connection()
            .execute(
                "UPDATE commits SET classification_id = ?1 WHERE id = ?2",
                params![cl2, c2],
            )
            .expect("link 2");

        let engine = engine_with_mock_llm(&endpoint);
        let updated = ClassificationPipeline::backfill_complexity_with_engine(&mut db, &engine)
            .await
            .expect("backfill");
        assert_eq!(updated, 1, "only the NULL row should be updated");

        let filled: Option<i64> = db
            .connection()
            .query_row(
                "SELECT complexity FROM classifications WHERE id = ?1",
                params![cl1],
                |row| row.get(0),
            )
            .expect("query filled");
        assert_eq!(filled, Some(4), "NULL row backfilled to the LLM score");

        let unchanged: Option<i64> = db
            .connection()
            .query_row(
                "SELECT complexity FROM classifications WHERE id = ?1",
                params![cl2],
                |row| row.get(0),
            )
            .expect("query unchanged");
        assert_eq!(unchanged, Some(3), "already-scored row must be unchanged");
    }
}
