//! Database-interaction helpers for the classification pipeline.
//!
//! Extracted from `pipeline.rs` to keep each module under the 500-SLOC cap.

use std::collections::HashMap;

use indicatif::{ProgressBar, ProgressStyle};
use rusqlite::params;
use tracing::{info, warn};

use crate::classify::errors::Result;
use crate::classify::tiers::ClassificationResult;
use crate::core::db::{CheckpointMode, Database};

use super::pipeline::ClassificationStats;

/// A `classifications` row eligible for complexity backfill.
pub(super) struct ComplexityBackfillCandidate {
    /// Primary key of the `classifications` row to update.
    pub(super) classification_id: i64,
    /// SHA of the linked commit (for logging/diagnostics).
    pub(super) commit_sha: String,
    /// Commit message, used as LLM input.
    pub(super) message: String,
}

/// Read classification rows lacking a complexity score.
///
/// Why: the backfill must target only LLM-eligible rows; `exact_rule`
/// verdicts never carried complexity, so they are excluded.
/// What: joins `classifications` to `commits` via `classification_id` and
/// returns rows where `complexity IS NULL` and `method != 'exact_rule'`.
/// Test: covered by the backfill integration test.
pub(super) fn read_complexity_backfill_candidates(
    db: &Database,
) -> Result<Vec<ComplexityBackfillCandidate>> {
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
pub(super) fn read_overrides(
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
pub(super) fn compute_coverage(stats: &mut ClassificationStats) {
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
pub(super) fn persist_repository_status(
    db: &mut Database,
    stats: &ClassificationStats,
) -> Result<()> {
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
pub(super) fn report_coverage(stats: &ClassificationStats, threshold_pct: f64) {
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
pub(super) struct CommitRow {
    pub(super) id: i64,
    pub(super) sha: String,
    pub(super) message: String,
    pub(super) is_merge: bool,
    pub(super) repository: String,
    /// Pre-existing classification row id, if any.
    ///
    /// Populated by [`read_candidate_commits`] for `--force` re-runs so
    /// the write-back path can UPDATE the existing `classifications` row
    /// in place instead of inserting an orphan. `None` for first-time
    /// classification (the default flow).
    pub(super) existing_classification_id: Option<i64>,
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
pub(super) fn is_revert_verdict(category: &str, subcategory: Option<&str>) -> bool {
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
/// verdict, optionally bounded by `--since`/`--until`/`--repos`. The
/// default flow keeps the historical "skip-if-classified" semantics so
/// incremental runs stay cheap.
/// What: dynamically builds a WHERE clause from the supplied filters:
///
///   * `!force`               → `classification_id IS NULL`
///   * `since`                → `timestamp >= ?`
///   * `until`                → `timestamp <= ?`
///   * `repos` non-empty      → `repository IN (?,?,…)`
///
/// Filters compose: all supplied predicates are AND-ed together.
/// Test: covered by `read_candidate_commits_*` unit tests and the
/// `pipeline_force_*` integration tests in this module.
pub(super) fn read_candidate_commits(
    db: &Database,
    force: bool,
    since: Option<&str>,
    until: Option<&str>,
    repos: &[String],
) -> Result<Vec<CommitRow>> {
    use rusqlite::types::Value;

    // Build the WHERE clause dynamically so we can compose all filters.
    let mut predicates: Vec<String> = Vec::new();
    let mut params: Vec<Value> = Vec::new();

    if !force {
        predicates.push("classification_id IS NULL".to_string());
    }
    if let Some(s) = since {
        params.push(Value::Text(s.to_string()));
        predicates.push(format!("timestamp >= ?{}", params.len()));
    }
    if let Some(u) = until {
        params.push(Value::Text(u.to_string()));
        predicates.push(format!("timestamp <= ?{}", params.len()));
    }
    if !repos.is_empty() {
        // Append repo values to params and compute their 1-based placeholder indices.
        let start = params.len() + 1;
        for r in repos {
            params.push(Value::Text(r.clone()));
        }
        let end = params.len();
        let placeholders: Vec<String> = (start..=end).map(|i| format!("?{i}")).collect();
        predicates.push(format!("repository IN ({})", placeholders.join(", ")));
    }

    let where_clause = if predicates.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", predicates.join(" AND "))
    };
    let sql = format!(
        "SELECT id, sha, message, is_merge, repository, classification_id FROM commits{where_clause}"
    );

    let mut stmt = db
        .connection()
        .prepare(&sql)
        .map_err(crate::core::TgaError::from)?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(params.iter()), |row| {
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
pub(super) fn write_results(
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
pub(super) fn write_results_chunk(
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
                    (category, subcategory, ticket_id, confidence, method, complexity, \
                     top_level_category) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
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
                     confidence = ?4, method = ?5, complexity = ?6, \
                     top_level_category = ?7 \
                 WHERE id = ?8",
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
            // Issue #445: derive top_level_category from the verdict's top_level
            // field so the column is populated at classification write time
            // without requiring a separate backfill pass.
            let top_level_str = result.top_level.map(|t| t.as_str_snake());

            let classification_id = if let Some(existing) = commit.existing_classification_id {
                update_existing_classification
                    .execute(params![
                        result.category,
                        result.subcategory,
                        result.ticket_id,
                        result.confidence,
                        result.method.as_str(),
                        result.complexity.map(|v| v as i64),
                        top_level_str,
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
                        top_level_str,
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

pub(super) fn make_progress(len: u64, label: &str) -> ProgressBar {
    let pb = ProgressBar::new(len);
    if let Ok(style) =
        ProgressStyle::with_template("{prefix:.bold} [{bar:40.cyan/blue}] {pos}/{len} ({percent}%)")
    {
        pb.set_style(style.progress_chars("##-"));
    }
    pb.set_prefix(label.to_string());
    pb
}
