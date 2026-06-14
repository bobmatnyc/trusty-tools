//! Miscellaneous backfill operations: complexity, reachability, top_level, quality.
//!
//! Why: these operations are each small and self-contained, but none fits
//! cleanly into the effort or flags modules. Grouping them here keeps the
//! module tree shallow while avoiding overloading `mod.rs`.

use tga::classify::taxonomy::TaxonomyRegistry;
use tga::classify::ClassificationPipeline;
use tga::collect::git::scan_and_persist;
use tga::core::config::{expand_path, Config};
use tga::core::db::{CheckpointMode, Database};
use tga::report::aggregator::Aggregator;

use super::types::ComplexityBackfillArgs;

/// Fill in missing `complexity` scores for already-classified commits.
///
/// Why: the `complexity` column added in 2.2.0 is only ever written by the LLM
/// tier, and the normal `tga classify` run consults the LLM solely for
/// low-confidence commits. This makes the operation discoverable under
/// `tga backfill` (issue #397, bug 2).
/// What: builds a [`ClassificationPipeline`] from config (forcing `use_llm` on
/// when `--use-llm` is passed), invokes `backfill_complexity`, and checkpoints
/// the WAL on completion. In `--dry-run` it reports the candidate count without
/// calling the LLM or writing.
/// Test: `tests::backfill_complexity_dry_run_reports_candidates_without_writing`
/// (dry-run path) and the library-level pipeline tests (population path).
///
/// # Errors
///
/// Returns an error if pipeline construction, the LLM calls, or DB access fail.
pub(super) async fn backfill_complexity(
    config: Config,
    db: &mut Database,
    args: ComplexityBackfillArgs,
    dry_run: bool,
) -> anyhow::Result<()> {
    if dry_run {
        // Count candidate rows without invoking the LLM or writing anything.
        let candidates: i64 = db
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM classifications \
                 WHERE complexity IS NULL AND method != 'exact_rule'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);
        println!(
            "Dry run — would request complexity scores for {candidates} classification(s) \
             (complexity IS NULL, method != 'exact_rule'). No changes written."
        );
        return Ok(());
    }

    // Force the LLM tier on when requested; complexity scoring is LLM-only.
    let mut cfg = config;
    if args.use_llm {
        let classification = cfg
            .classification
            .get_or_insert_with(tga::core::config::ClassificationConfig::default);
        classification.use_llm = true;
    }

    let pipeline = ClassificationPipeline::new(cfg);
    let updated = pipeline.backfill_complexity(db).await?;
    println!("Backfilled complexity for {updated} commit(s)");

    // Flush the WAL after the backfill so the scores are durable in the main
    // DB file (mirrors the post-classify checkpoint, issue #298).
    if let Err(e) = db.wal_checkpoint(CheckpointMode::Truncate) {
        tracing::warn!(error = %e, "WAL TRUNCATE checkpoint failed after complexity backfill");
    }
    Ok(())
}

/// Re-run the reachability scan and upsert `fact_commit_reachability`.
///
/// Why: existing databases built before issue #290 was fixed have
/// `on_default_branch=0` for every row. Running `tga collect` again costs
/// 20+ minutes on large corpora. This function re-uses the same
/// `scan_and_persist` code path to recompute all five reachability columns
/// in-place via `INSERT … ON CONFLICT … DO UPDATE SET …`.
/// What: iterates configured repositories (filtered by `repos_filter` when
/// provided), opens the local git repo, calls `scan_and_persist`, and prints
/// a per-repo summary + final totals to stdout. When `dry_run=true` no writes
/// occur; instead the function reports what *would* change.
/// Test: compile-time verified; no DB-mutation integration tests for reachability.
///
/// # Errors
///
/// Returns an error if the database connection or git repo open fails. Per-
/// repo scan failures are non-fatal and printed as warnings.
pub(super) fn backfill_reachability(
    config: Config,
    db: &mut Database,
    repos_filter: &[String],
    dry_run: bool,
) -> anyhow::Result<()> {
    if dry_run {
        println!(
            "Dry run — would re-run reachability scan for {} repo(s). No changes written.",
            if repos_filter.is_empty() {
                config.repositories.len()
            } else {
                repos_filter.len()
            }
        );
        return Ok(());
    }

    let reach_cfg = &config.reachability;
    let conn = db.connection();

    let mut total_repos = 0usize;
    let mut total_rows = 0usize;
    let mut total_default_branch = 0usize;
    let mut errors: Vec<String> = Vec::new();

    for repo_cfg in &config.repositories {
        let path = expand_path(&repo_cfg.path);
        let name = repo_cfg
            .name
            .clone()
            .or_else(|| {
                path.file_name()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_string())
            })
            .unwrap_or_else(|| path.display().to_string());

        // Apply --repos filter (global backfill flag).
        if !repos_filter.is_empty() && !repos_filter.contains(&name) {
            continue;
        }

        total_repos += 1;
        tracing::info!(repo = %name, "backfill reachability scan");

        match scan_and_persist(&path, conn, reach_cfg, Some(&name)) {
            Ok(stats) => {
                println!(
                    "  {name}: {} rows upserted \
                     ({} on default branch, {} tagged, {} on release branch)",
                    stats.rows_upserted,
                    stats.default_branch_commits,
                    stats.tagged_commits,
                    stats.release_branch_commits,
                );
                total_rows += stats.rows_upserted;
                total_default_branch += stats.default_branch_commits;
            }
            Err(e) => {
                let msg = format!("  {name}: reachability scan failed: {e}");
                tracing::warn!("{msg}");
                errors.push(msg.clone());
                println!("{msg}");
            }
        }
    }

    println!(
        "\nBackfill complete: {total_repos} repos, {total_rows} rows upserted, \
         {total_default_branch} commits on default branch."
    );
    if !errors.is_empty() {
        println!("{} repo(s) had errors (see warnings above).", errors.len());
    }

    Ok(())
}

/// Fill in `classifications.top_level_category` for existing rows.
///
/// Why: `top_level_category` was added in migration v17. New classifications
/// written by `write_results_chunk` will have it populated automatically;
/// this backfill handles the pre-existing rows where the column is NULL.
/// What: resolves each stored `subcategory` through the built-in
/// [`TaxonomyRegistry`] and sets `top_level_category` to the snake_case
/// string for the resolved variant. Rows with an unrecognized subcategory
/// (or NULL subcategory) are left as NULL. No LLM required.
/// Test: `tests::backfill_top_level_fills_known_subcategories`.
///
/// # Errors
///
/// Propagates database errors from the underlying queries.
pub(super) fn backfill_top_level(db: &mut Database, dry_run: bool) -> anyhow::Result<()> {
    use rusqlite::params;
    let registry = TaxonomyRegistry::with_builtins();

    let mut to_update: Vec<(i64, String)> = Vec::new();
    {
        let conn = db.connection();
        let mut stmt = conn.prepare(
            "SELECT id, subcategory FROM classifications WHERE top_level_category IS NULL",
        )?;
        let rows: Vec<(i64, Option<String>)> = stmt
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?))
            })?
            .collect::<Result<_, _>>()?;

        for (id, subcategory) in rows {
            if let Some(sub) = subcategory {
                if let Some(top) = registry.resolve(&sub) {
                    to_update.push((id, top.as_str_snake().to_string()));
                }
            }
        }
    }

    if dry_run {
        println!(
            "Dry run — would update top_level_category for {} classification(s). \
             No changes written.",
            to_update.len(),
        );
        return Ok(());
    }

    let conn = db.connection_mut();
    let tx = conn.transaction()?;
    {
        let mut up =
            tx.prepare("UPDATE classifications SET top_level_category = ?1 WHERE id = ?2")?;
        for (id, top) in &to_update {
            up.execute(params![top, id])?;
        }
    }
    tx.commit()?;
    println!(
        "Updated top_level_category for {} classification(s).",
        to_update.len()
    );
    Ok(())
}

/// Recompute and persist per-engineer-per-week quality scores for all historical
/// data into `fact_weekly_quality`.
///
/// Why: `fact_weekly_quality` (migration v18) is populated automatically when a
/// `tga report` run calls `Aggregator::persist_weekly_quality`. For databases
/// collected before batch B was deployed, or for databases where quality rows
/// need to be corrected after `tga backfill ticketed` changed the `ticketed`
/// values, this subcommand recomputes the full table from scratch.
/// What: delegates to `Aggregator::build` (which re-aggregates the commits
/// table) and then calls `Aggregator::persist_weekly_quality`. The aggregator
/// shares the same bucketing logic used at report time, so the stored values
/// are guaranteed to match what a fresh `tga report` would produce.
/// Test: `tests::backfill_quality_populates_and_is_idempotent`.
///
/// # Errors
///
/// Propagates aggregator / database errors.
pub(super) fn backfill_quality(db: &mut Database, dry_run: bool) -> anyhow::Result<()> {
    let config = tga::core::config::Config::default();

    if dry_run {
        // Estimate: count distinct (author_email, iso_year, iso_week, repository)
        // tuples that would be written by running the aggregator.
        let candidate: i64 = db
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM ( \
                     SELECT DISTINCT \
                         COALESCE(NULLIF(a.canonical_email, ''), c.author_email) AS ae, \
                         CAST(strftime('%Y', c.timestamp) AS INTEGER) AS yr, \
                         CAST(strftime('%W', c.timestamp) AS INTEGER) AS wk, \
                         c.repository \
                     FROM commits c \
                     LEFT JOIN authors a ON a.id = c.author_id \
                 )",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        println!(
            "Dry run — would write approximately {candidate} quality row(s) \
             to fact_weekly_quality. No changes written."
        );
        return Ok(());
    }

    let data =
        Aggregator::build(db, &config).map_err(|e| anyhow::anyhow!("aggregation failed: {e}"))?;

    let written = Aggregator::persist_weekly_quality(db, &data)
        .map_err(|e| anyhow::anyhow!("quality persist failed: {e}"))?;

    println!("Backfilled fact_weekly_quality: {written} row(s) written (UPSERT semantics).");

    // Flush the WAL so the new rows are durable in the main DB file.
    if let Err(e) = db.wal_checkpoint(tga::core::db::CheckpointMode::Truncate) {
        tracing::warn!(error = %e, "WAL TRUNCATE checkpoint failed after quality backfill");
    }
    Ok(())
}
