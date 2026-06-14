//! Effort backfill operations for `tga backfill effort` and `tga backfill effort-tshirt`.
//!
//! Why: effort scoring is the largest and most complex backfill operation,
//! spanning two processing paths (db-only and libgit2), batch persistence, and
//! git note writing. This module orchestrates both paths; the per-path
//! implementations live in `effort_db.rs` and `effort_git.rs`.

use rusqlite::params;
use tga::core::config::{expand_path, Config};
use tga::core::db::Database;
use tga::core::effort::effort_tshirt_from_size;

use super::types::{EffortBackfillArgs, EffortRow};

/// Compute empirical effort scores for historical commits and persist them into
/// `fact_commit_effort`, using the same v1 formula as the pre-commit bash hook.
///
/// Why: changing past commit SHAs is unacceptable for historical work, so
/// effort scores must be stored out-of-band in the analytics DB rather than
/// injected as git trailers retroactively.
/// What: for each configured repository (or a single one if `--repo` is given),
/// selects the per-file diff data from the `commits JOIN files` tables (default
/// path) — or re-walks git via libgit2 when `--range` or `--notes` is given —
/// computes effort per commit, and upserts into `fact_commit_effort`.
/// Skips already-scored commits unless `--force`. Supports `--limit N` and
/// `--dry-run`.
///
/// **Path selection**:
/// - `--range` is present → libgit2 path (revwalk needed to interpret git ranges)
/// - `--notes` is present  → libgit2 path (live repo needed to write git notes)
/// - otherwise             → db-only path (no repo on disk required)
///
/// Test: `tests::backfill_effort_*` in `tests.rs`.
///
/// # Errors
///
/// Returns an error if the config, database, or any git repo open fails.
/// Per-commit diff failures are logged as warnings and skipped.
pub(super) fn backfill_effort(
    config: Config,
    db: &mut Database,
    args: EffortBackfillArgs,
    repos_filter: &[String],
    since: Option<&str>,
    until: Option<&str>,
    dry_run: bool,
) -> anyhow::Result<()> {
    // Collect the (path, display-name) pairs we will process.
    let repos_to_process: Vec<(std::path::PathBuf, String)> = config
        .repositories
        .iter()
        .filter_map(|repo_cfg| {
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
                return None;
            }
            Some((path, name))
        })
        .collect();

    // Log the effective date window when supplied.
    if since.is_some() || until.is_some() {
        tracing::info!(
            since = ?since,
            until = ?until,
            "effort backfill: applying date window filter (--since/--until/--weeks)"
        );
        tracing::warn!(
            "effort backfill: --since/--until/--weeks filters affect the log output only;\n\
             the db-only path queries all commits for each repo via `commits JOIN files`.\n\
             For precise date-scoped effort scoring use --range on the git path."
        );
    }

    if repos_to_process.is_empty() {
        println!("No matching repositories found in config.");
        return Ok(());
    }

    // Decide which processing path for all repos.
    // --range and --notes both require a live git repository via libgit2.
    let use_git_path = args.range.is_some() || args.notes;
    let _ = since; // date window noted in warning above; effort db path queries all timestamps
    let _ = until;

    // Summary accumulators.
    let mut total_scored: usize = 0;
    let mut total_skipped: usize = 0;
    let mut total_repos: usize = 0;
    let mut size_counts = [0usize; 5]; // XS, S, M, L, XL

    for (repo_path, repo_name) in &repos_to_process {
        let result = if use_git_path {
            super::effort_git::process_one_repo_git(repo_path, repo_name, db, &args, dry_run)
                .and_then(|(scored, skipped, sizes, rows)| {
                    if !dry_run {
                        persist_effort_rows(db, &rows)?;
                    }
                    Ok((scored, skipped, sizes))
                })
        } else {
            super::effort_db::process_one_repo_db(db.connection(), repo_name, &args, dry_run)
                .and_then(|(scored, skipped, sizes, rows)| {
                    if !dry_run {
                        persist_effort_rows(db, &rows)?;
                    }
                    Ok((scored, skipped, sizes))
                })
        };
        match result {
            Ok((scored, skipped, sizes)) => {
                total_repos += 1;
                total_scored += scored;
                total_skipped += skipped;
                for i in 0..5 {
                    size_counts[i] += sizes[i];
                }
                let verb = if dry_run { "would score" } else { "scored" };
                println!(
                    "  {repo_name}: {verb} {scored} commits, skipped {skipped} already-scored"
                );
            }
            Err(e) => {
                tracing::warn!(repo = %repo_name, error = %e, "backfill effort failed for repo");
                println!("  {repo_name}: error — {e}");
            }
        }
    }

    let verb = if dry_run { "Would score" } else { "Scored" };
    println!(
        "\nBackfill complete: {total_repos} repos, {verb} {total_scored} commits \
         ({} skipped already-scored).",
        total_skipped,
    );
    println!(
        "  Size distribution: XS={} S={} M={} L={} XL={}",
        size_counts[0], size_counts[1], size_counts[2], size_counts[3], size_counts[4],
    );

    Ok(())
}

/// Persist effort rows in batches of 1000 using UPSERT semantics.
///
/// Why: batching avoids per-row transaction overhead on large corpora; UPSERT
/// (`INSERT OR REPLACE`) ensures --force re-computation overwrites stale rows.
/// What: splits `rows` into chunks of 1000 and wraps each chunk in a single
/// transaction. Each row's `effort_tshirt` is recomputed using
/// `tshirt_for_score_incremental` which bins against stored corpus percentile
/// thresholds when available, falling back to the static size-label mapping
/// when no thresholds have been stored yet (i.e., before the first
/// `tga backfill effort-tshirt` run).
/// Test: `tests::backfill_effort_persists_rows` and
/// `tests::backfill_effort_force_recomputes`.
pub(super) fn persist_effort_rows(db: &mut Database, rows: &[EffortRow]) -> anyhow::Result<()> {
    // Load stored percentile thresholds once per batch call (not per row).
    let thresholds = tga::core::effort_percentile::load_thresholds(db.connection()).unwrap_or(None);

    for chunk in rows.chunks(1000) {
        let conn = db.connection_mut();
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO fact_commit_effort \
                 (sha, repository, size, score, loc, files, test_loc, tests_factor, \
                  formula_version, computed_at, effort_tshirt) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            )?;
            for row in chunk {
                // Use stored percentile thresholds when available for consistent
                // incremental binning; fall back to static mapping otherwise.
                let tshirt = match &thresholds {
                    Some(t) => t.band_for_score(row.score),
                    None => effort_tshirt_from_size(&row.size),
                };
                stmt.execute(params![
                    row.sha,
                    row.repository,
                    row.size,
                    row.score,
                    row.loc as i64,
                    row.files as i64,
                    row.test_loc as i64,
                    row.tests_factor,
                    row.formula_version,
                    row.computed_at,
                    tshirt,
                ])?;
            }
        }
        tx.commit()?;
    }
    Ok(())
}

/// Recompute `fact_commit_effort.effort_tshirt` using corpus-percentile binning.
///
/// Why: batch A added `effort_tshirt` as a static size→integer map (XS=1…XL=5).
/// Batch C replaces this with corpus-percentile binning so the integer encodes
/// relative standing within the actual score distribution rather than an
/// absolute threshold.
///
/// What: delegates to [`tga::core::effort_percentile::rebin_all`], which
/// computes p20/p40/p60/p80 breakpoints, persists thresholds, and updates
/// every row's `effort_tshirt` to the corresponding quintile (1–5).
///
/// Tiny-corpus fallback: when fewer than 5 rows exist, percentile breakpoints
/// cannot be computed reliably. The function falls back to the static
/// size-label → integer mapping (XS=1…XL=5) and logs a WARN.
///
/// Test: `tests::backfill_effort_tshirt_uses_percentile_binning` and
/// `tests::backfill_effort_tshirt_tiny_corpus_fallback`.
///
/// # Errors
///
/// Propagates database errors from the underlying queries or transaction.
pub(super) fn backfill_effort_tshirt(db: &mut Database, dry_run: bool) -> anyhow::Result<()> {
    if dry_run {
        // Count total rows that will be rebinned.
        let count: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM fact_commit_effort", [], |r| r.get(0))
            .unwrap_or(0);
        println!(
            "Dry run — would rebin effort_tshirt (percentile) for {count} row(s) \
             and persist corpus thresholds to effort_percentile_thresholds. \
             No changes written."
        );
        return Ok(());
    }

    let (rows_updated, thresholds) =
        tga::core::effort_percentile::rebin_all(db.connection_mut())
            .map_err(|e| anyhow::anyhow!("percentile rebin failed: {e}"))?;

    match thresholds {
        Some(ref t) => {
            println!(
                "Rebinned effort_tshirt (percentile) for {rows_updated} row(s). \
                 Corpus thresholds persisted: p20={:.3} p40={:.3} p60={:.3} p80={:.3} \
                 (sample_count={}).",
                t.p20, t.p40, t.p60, t.p80, t.sample_count,
            );
        }
        None => {
            println!(
                "Rebinned effort_tshirt for {rows_updated} row(s) using \
                 static size-label mapping (corpus too small for percentile binning; \
                 run again after collecting more commits)."
            );
        }
    }
    Ok(())
}
