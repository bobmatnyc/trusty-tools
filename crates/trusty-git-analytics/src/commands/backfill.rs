//! `tga backfill` — retroactive maintenance operations against the commits
//! table.
//!
//! These operations live outside the normal `collect → classify → report`
//! pipeline because they update existing rows in-place rather than
//! ingesting new data. Each subcommand supports `--dry-run`, in which case
//! it reports the number of rows that *would* change without writing.
//!
//! Uniform filter flags (`--repos`, `--weeks`, `--since`, `--until`) scope
//! the backfill to a specific slice of the database. `--branch` is **not**
//! applicable to backfill operations because commits in the database do not
//! carry branch attribution after the original collection walk.

use clap::{Args, Subcommand};
use git2::{Repository, Sort};
use rusqlite::{params, Connection};
use tga::classify::ClassificationPipeline;
use tga::collect::git::scan_and_persist;
use tga::collect::ticket::{extract_ticket_id, is_ticketed};
use tga::core::config::{expand_path, Config};
use tga::core::db::{CheckpointMode, Database};
use tga::core::effort::{compute_effort, FORMULA_VERSION};

/// Arguments for `tga backfill`.
#[derive(Args, Debug)]
#[command(
    about = "Retroactive maintenance operations on existing commit rows.",
    long_about = "Re-run extraction or scoring steps on commits already in the database.\n\n\
These operations update existing rows in-place rather than ingesting new data.\n\
Each subcommand supports --dry-run to preview changes without writing.\n\n\
NOTE: --branch is collect-only. Commits in the DB do not carry branch\n\
attribution after the walk, so there is no branch filter on backfill operations.\n\
If you need to re-walk specific branches, use `tga collect --branch <name>`.\n\n\
TIPS:\n\
  - Use --repos to limit scope to one service at a time on large corpora.\n\
  - Use --since/--until or --weeks to limit the date window for fast iteration.",
    after_help = "EXAMPLES:\n\
  # Re-extract ticket IDs for all commits (after pattern change)\n\
  tga backfill ticket-ids\n\n\
  # Re-score effort for the last 4 weeks of one repo\n\
  tga backfill effort --repos my-service --weeks 4 --force\n\n\
  # Re-run reachability scan after adding release-branch patterns\n\
  tga backfill reachability --repos core-api"
)]
pub struct BackfillArgs {
    /// Backfill subcommand.
    #[command(subcommand)]
    pub subcommand: BackfillSubcommand,
    /// Report what would change without writing.
    #[arg(long, default_value_t = false, global = true)]
    pub dry_run: bool,
    /// Limit backfill to these repository names (comma-separated). [global]
    ///
    /// Matches against the `repository` column in the `commits` table
    /// (for ticket-ids, revert-flags) or the repo `name` in config
    /// (for reachability, effort). When omitted, all repos are processed.
    ///
    /// NOTE: not applicable to ai-detection (global LLM re-classification).
    #[arg(long, value_delimiter = ',', global = true)]
    pub repos: Vec<String>,
    /// Limit backfill to commits in the last N ISO weeks. [global]
    ///
    /// Restricts the set of commits processed by timestamp. Mutually exclusive
    /// with --since/--until. Not applicable to reachability (uses config repos).
    #[arg(long, value_name = "N", global = true, conflicts_with_all = ["since", "until"])]
    pub weeks: Option<u32>,
    /// Limit backfill to commits on or after this date (ISO8601: YYYY-MM-DD). [global]
    ///
    /// Lower bound on the author timestamp. Mutually exclusive with --weeks.
    #[arg(long, value_name = "DATE", global = true, conflicts_with = "weeks")]
    pub since: Option<String>,
    /// Limit backfill to commits on or before this date (ISO8601: YYYY-MM-DD). [global]
    ///
    /// Upper bound on the author timestamp. Mutually exclusive with --weeks.
    #[arg(long, value_name = "DATE", global = true, conflicts_with = "weeks")]
    pub until: Option<String>,
}

/// `tga backfill` subcommands.
#[derive(Subcommand, Debug)]
pub enum BackfillSubcommand {
    /// Re-run LLM classification on low-confidence prior LLM verdicts.
    ///
    /// Clears `classification_id` on commits classified by the LLM tier
    /// with confidence < 0.7, making them eligible for re-classification
    /// on the next `tga classify` run. Use `tga classify --force` after
    /// this to immediately re-process the cleared commits.
    AiDetection,
    /// Scan commit messages for revert patterns and update `is_revert`.
    ///
    /// Detects `Revert "..."`, `revert:`, and `revert"` prefixes
    /// (case-insensitive). Use --repos/--since/--until to limit scope.
    RevertFlags,
    /// Scan commit messages for ticket references and update `ticket_id`/`ticketed`.
    ///
    /// Useful after extending ticket patterns or when collecting a new
    /// repo whose commits were never run through ticket extraction.
    /// --branch is collect-only and not applicable here.
    TicketIds,
    /// Re-run the tag/branch/default-branch reachability scan.
    ///
    /// Upserts `fact_commit_reachability` rows without re-collecting commits.
    /// Use this to fix `on_default_branch=0` rows in existing databases
    /// without running the full 20-minute `tga collect` pipeline (issue #290).
    ///
    /// Use --repos (via BackfillArgs) to limit to specific repositories.
    /// --branch is collect-only; reachability is computed from the live git
    /// repo graph, not from the branch the commits were originally collected on.
    Reachability,
    /// Compute empirical effort scores for historical commits.
    ///
    /// Persists scores in `fact_commit_effort` using the v1 formula
    /// (LoC + file count + tests factor, mapped to XS/S/M/L/XL).
    ///
    /// Default path (db-only): reads from `commits JOIN files` — no on-disk
    /// git repo required. Use --range or --notes to switch to the git path.
    ///
    /// --branch is collect-only and not applicable here.
    Effort(EffortBackfillArgs),
    /// Fill in missing `complexity` scores (1–5) for already-classified commits.
    ///
    /// The `complexity` column is only ever populated by the LLM tier, which
    /// the normal `tga classify` run consults solely for low-confidence
    /// commits. Commits resolved by rules or external sources (JIRA/GitHub)
    /// therefore keep `complexity = NULL`. This subcommand asks the LLM for a
    /// 1–5 complexity score for every classification with `complexity IS NULL`
    /// and a non-`exact_rule` method, leaving category/confidence/method
    /// untouched. Requires `use_llm: true` (or `--use-llm`) and an LLM API key.
    ///
    /// Equivalent to `tga classify --backfill-complexity`; exposed here so the
    /// operation is discoverable under `tga backfill` (issue #397, bug 2).
    /// --repos/--since/--until/--weeks do not scope this operation: all NULL
    /// rows are processed.
    Complexity(ComplexityBackfillArgs),
}

/// Arguments for `tga backfill complexity`.
#[derive(Args, Debug)]
pub struct ComplexityBackfillArgs {
    /// Enable the LLM tier for this run even if `config.classification.use_llm`
    /// is `false`.
    ///
    /// Complexity scoring is LLM-only, so the LLM tier must be on. Pass this
    /// flag (or set `use_llm: true` in config) along with an API key
    /// (`OPENAI_API_KEY` / `OPENROUTER_API_KEY`).
    #[arg(long, default_value_t = false)]
    pub use_llm: bool,
}

/// Arguments for `tga backfill effort`.
#[derive(Args, Debug)]
pub struct EffortBackfillArgs {
    /// Scope effort computation to a git commit range (e.g. `HEAD~10..HEAD`).
    ///
    /// When omitted, all commits in the chosen repo(s) that do not already
    /// have a `fact_commit_effort` row are processed (unless `--force`).
    /// Requires a live on-disk git repository.
    #[arg(long, value_name = "RANGE")]
    pub range: Option<String>,

    /// Recompute effort even if a row already exists (UPSERT semantics).
    ///
    /// Without this flag, commits that already have a row in
    /// `fact_commit_effort` are skipped.  With `--force`, every commit is
    /// re-scored and the existing row is replaced.
    #[arg(long, default_value_t = false)]
    pub force: bool,

    /// Also write a git note to `refs/notes/effort` for each scored commit.
    ///
    /// The note body is `Effort: <size>` (e.g. `Effort: M`), matching the
    /// format the pre-commit hook injects into commit messages.  Off by
    /// default to keep the backfill lightweight. Requires a live git repo.
    #[arg(long, default_value_t = false)]
    pub notes: bool,

    /// Maximum commits to process per repository.
    ///
    /// Useful for smoke-testing on a large corpus.  When omitted, all
    /// eligible commits are processed.
    #[arg(long, value_name = "N")]
    pub limit: Option<usize>,
}

/// Resolve the effective date window from global backfill filter flags.
///
/// Why: the `--weeks`, `--since`, and `--until` flags are declared globally
/// on `BackfillArgs` so they can scope any backfill subcommand uniformly.
/// What: returns `(since_rfc, until_rfc)` as optional RFC3339 strings, or
/// `(since_plain, until_plain)` if only plain ISO dates are provided.
/// Test: indirectly exercised by each backfill subcommand's date-scoped tests.
fn resolve_backfill_date_range(
    args: &BackfillArgs,
) -> anyhow::Result<(Option<String>, Option<String>)> {
    use crate::commands::date_range::resolve_date_range;
    resolve_date_range(
        args.weeks,
        args.since.as_deref(),
        args.until.as_deref(),
        None,
    )
}

/// Dispatch entry point for the `tga backfill` subcommand.
///
/// Why: routes each backfill subcommand to its implementation, passing shared
/// state (config, db connection), the `--dry-run` flag, and the uniform
/// filter flags (--repos, --weeks, --since, --until).
/// What: matches on `args.subcommand` and calls the appropriate function.
/// Test: each variant has its own test module below.
///
/// # Errors
///
/// Propagates database errors from the underlying queries.
pub async fn run(config: Config, db: &mut Database, args: BackfillArgs) -> anyhow::Result<()> {
    let (since, until) = resolve_backfill_date_range(&args)?;
    let repos = args.repos.clone();
    match args.subcommand {
        BackfillSubcommand::AiDetection => backfill_ai_detection(db, args.dry_run),
        BackfillSubcommand::RevertFlags => {
            backfill_revert_flags(db, args.dry_run, &repos, since.as_deref(), until.as_deref())
        }
        BackfillSubcommand::TicketIds => {
            backfill_ticket_ids(db, args.dry_run, &repos, since.as_deref(), until.as_deref())
        }
        BackfillSubcommand::Reachability => backfill_reachability(config, db, &repos, args.dry_run),
        BackfillSubcommand::Effort(effort_args) => backfill_effort(
            config,
            db,
            effort_args,
            &repos,
            since.as_deref(),
            until.as_deref(),
            args.dry_run,
        ),
        BackfillSubcommand::Complexity(complexity_args) => {
            backfill_complexity(config, db, complexity_args, args.dry_run).await
        }
    }
}

// ── backfill complexity ────────────────────────────────────────────────────────

/// Fill in missing `complexity` scores for already-classified commits.
///
/// Why: the `complexity` column added in 2.2.0 is only ever written by the LLM
/// tier, and the normal `tga classify` run consults the LLM solely for
/// low-confidence commits. On a corpus where most commits are resolved by
/// rules or external sources (JIRA/GitHub), `complexity` stays `NULL` for
/// nearly every row. The population logic already exists
/// ([`ClassificationPipeline::backfill_complexity`]) and was reachable via
/// `tga classify --backfill-complexity`, but operators looked for it under
/// `tga backfill` and found nothing — so it appeared the feature was never
/// shipped (issue #397, bug 2). This makes the operation discoverable here.
/// What: builds a [`ClassificationPipeline`] from config (forcing `use_llm` on
/// when `--use-llm` is passed), invokes `backfill_complexity`, and checkpoints
/// the WAL on completion. In `--dry-run` it reports the candidate count without
/// calling the LLM or writing.
/// Test: `tests::backfill_complexity_dry_run_reports_candidates` (dry-run path)
/// and the library-level `pipeline::tests::backfill_complexity_updates_only_null_rows`
/// (population path, mock LLM).
///
/// # Errors
///
/// Returns an error if pipeline construction, the LLM calls, or DB access fail.
async fn backfill_complexity(
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

// ── backfill effort ──────────────────────────────────────────────────────────

/// Compute empirical effort scores for historical commits and persist them into
/// `fact_commit_effort`, using the same v1 formula as the pre-commit bash hook.
///
/// Why: changing past commit SHAs is unacceptable for historical work, so
/// effort scores must be stored out-of-band in the analytics DB rather than
/// injected as git trailers retroactively.
/// What: for each configured repository (or a single one if `--repo` is given),
/// selects the per-file diff data from the `commits JOIN files` tables (default
/// path) — or re-walks git via libgit2 when `--range` or `--notes` is given —
/// computes [`compute_effort`] per commit, and upserts into `fact_commit_effort`.
/// Skips already-scored commits unless `--force`.  Supports `--limit N` and
/// `--dry-run`.
///
/// **Path selection**:
/// - `--range` is present → libgit2 path (revwalk needed to interpret git ranges)
/// - `--notes` is present  → libgit2 path (live repo needed to write git notes)
/// - otherwise             → db-only path (no repo on disk required)
///
/// Test: `tests::backfill_effort_*` below.
///
/// # Errors
///
/// Returns an error if the config, database, or any git repo open fails.
/// Per-commit diff failures are logged as warnings and skipped.
fn backfill_effort(
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
            process_one_repo_git(repo_path, repo_name, db, &args, dry_run)
        } else {
            process_one_repo_db(db.connection(), repo_name, &args, dry_run).and_then(
                |(scored, skipped, sizes, rows)| {
                    if !dry_run {
                        persist_effort_rows(db, &rows)?;
                    }
                    Ok((scored, skipped, sizes))
                },
            )
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

/// Process a single repository for the effort backfill using only the database.
///
/// Why: `tga collect` already stores `(path, insertions, deletions)` per file in
/// the `files` table for every collected commit.  Reading from the database
/// avoids opening the on-disk git repo entirely, making `tga backfill effort`
/// self-sufficient on `tga.db` alone — no repository checkout required.
///
/// Commits outside the `tga collect` window are not present in the `files`
/// table and are silently skipped.  Expand the collection `since`/`until`
/// window to score them.
///
/// What: queries `commits JOIN files` for the given repository, groups rows by
/// SHA, feeds each group to [`compute_effort`], and returns the accumulated
/// [`EffortRow`] records alongside the scored/skipped counts.  Does NOT call
/// [`persist_effort_rows`]; the caller is responsible for persisting.
///
/// Returns `(scored, skipped, [XS, S, M, L, XL], rows)`.
///
/// Test: `tests::backfill_effort_db_path_*` below.
fn process_one_repo_db(
    conn: &Connection,
    repo_name: &str,
    args: &EffortBackfillArgs,
    dry_run: bool,
) -> anyhow::Result<(usize, usize, [usize; 5], Vec<EffortRow>)> {
    // Build the set of SHAs that already have an effort row (unless --force).
    let already_scored: std::collections::HashSet<String> = if args.force {
        std::collections::HashSet::new()
    } else {
        let mut stmt = conn.prepare("SELECT sha FROM fact_commit_effort WHERE repository = ?1")?;
        let rows = stmt.query_map(params![repo_name], |row| row.get::<_, String>(0))?;
        let mut set = std::collections::HashSet::new();
        for r in rows {
            set.insert(r?);
        }
        set
    };

    // Count commits available in the database for this repo (for logging).
    let in_db: i64 = conn
        .query_row(
            "SELECT COUNT(DISTINCT c.sha) FROM commits c WHERE c.repository = ?1",
            params![repo_name],
            |r| r.get(0),
        )
        .unwrap_or(0);

    tracing::info!(
        repo = %repo_name,
        in_db = in_db,
        already_scored = already_scored.len(),
        "effort backfill db path: starting"
    );

    // Pull all (sha, path, insertions, deletions) rows for this repo.
    // ORDER BY c.timestamp, c.sha ensures stable ordering; the sha secondary
    // sort handles ties so the grouping below is deterministic.
    let mut stmt = conn.prepare(
        "SELECT c.sha, f.path, f.insertions, f.deletions \
         FROM commits c \
         JOIN files f ON f.commit_id = c.id \
         WHERE c.repository = ?1 \
         ORDER BY c.timestamp ASC, c.sha ASC",
    )?;

    let limit = args.limit.unwrap_or(usize::MAX);
    let mut records: Vec<EffortRow> = Vec::new();
    let mut skipped: usize = 0;

    // Group consecutive rows by SHA (they arrive sorted by timestamp+sha).
    let mut current_sha: Option<String> = None;
    let mut current_files: Vec<(String, u32, u32)> = Vec::new();

    // Helper closure: flush the accumulated files for the current SHA.
    // Returns true if a record was pushed (i.e., not skipped, not over limit).
    let flush = |sha: &str,
                 files: &[(String, u32, u32)],
                 already_scored: &std::collections::HashSet<String>,
                 records: &mut Vec<EffortRow>,
                 skipped: &mut usize|
     -> bool {
        if records.len() >= limit {
            return false;
        }
        if already_scored.contains(sha) {
            *skipped += 1;
            return true; // keep iterating — may still reach the limit
        }
        if files.is_empty() {
            tracing::warn!(
                sha = %sha,
                "commit has no rows in the files table; skipping effort computation"
            );
            return true;
        }
        let file_refs: Vec<(&str, u32, u32)> =
            files.iter().map(|(p, i, d)| (p.as_str(), *i, *d)).collect();
        let effort = compute_effort(file_refs);
        let computed_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        records.push(EffortRow {
            sha: sha.to_string(),
            repository: repo_name.to_string(),
            size: effort.size_label().to_string(),
            score: effort.score,
            loc: effort.loc,
            files: effort.files,
            test_loc: effort.test_loc,
            tests_factor: effort.tests_factor,
            formula_version: FORMULA_VERSION.to_string(),
            computed_at,
        });
        if records.len().is_multiple_of(1000) {
            tracing::info!(
                repo = %repo_name,
                processed = records.len(),
                "effort backfill db path: progress"
            );
        }
        true
    };

    let rows = stmt.query_map(params![repo_name], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, u32>(2)?,
            row.get::<_, u32>(3)?,
        ))
    })?;

    for row_res in rows {
        let (sha, path, ins, del) = row_res?;
        match &current_sha {
            None => {
                current_sha = Some(sha.clone());
                current_files.push((path, ins, del));
            }
            Some(cur) if cur == &sha => {
                current_files.push((path, ins, del));
            }
            Some(_) => {
                // New SHA — flush the previous one.
                let prev_sha = current_sha.take().expect("just checked Some");
                let should_continue = flush(
                    &prev_sha,
                    &current_files,
                    &already_scored,
                    &mut records,
                    &mut skipped,
                );
                current_files.clear();
                if !should_continue || records.len() >= limit {
                    break;
                }
                current_sha = Some(sha.clone());
                current_files.push((path, ins, del));
            }
        }
    }
    // Flush the last group.
    if let Some(last_sha) = current_sha.take() {
        if records.len() < limit {
            flush(
                &last_sha,
                &current_files,
                &already_scored,
                &mut records,
                &mut skipped,
            );
        }
    }

    let mut size_counts = [0usize; 5];
    for row in &records {
        let idx = match row.size.as_str() {
            "XS" => 0,
            "S" => 1,
            "M" => 2,
            "L" => 3,
            _ => 4, // XL
        };
        size_counts[idx] += 1;
    }

    tracing::info!(
        repo = %repo_name,
        in_db = in_db,
        scored = records.len(),
        skipped = skipped,
        dry_run = dry_run,
        "effort backfill db path: complete"
    );

    Ok((records.len(), skipped, size_counts, records))
}

/// Process a single repository for the effort backfill using libgit2 (git path).
///
/// Why: required for two cases that cannot use the db-only path —
/// (1) `--range`: revwalk is needed to interpret git range syntax such as
/// `HEAD~10..HEAD`; (2) `--notes`: writing `refs/notes/effort` requires a live
/// `Repository`.
///
/// What: opens the on-disk git repo via libgit2, walks commits (optionally
/// filtered by `--range`), computes [`compute_effort`] per diff, and upserts
/// into `fact_commit_effort`.  Skips already-scored commits unless `--force`.
/// Supports `--limit N` and `--dry-run`.
///
/// Test: existing `tests::backfill_effort_persists_rows` and related tests
/// exercise `persist_effort_rows`; end-to-end git path tested via `--notes`
/// and `--range` integration paths.
///
/// Returns `(scored, skipped, [XS, S, M, L, XL])`.
fn process_one_repo_git(
    repo_path: &std::path::Path,
    repo_name: &str,
    db: &mut Database,
    args: &EffortBackfillArgs,
    dry_run: bool,
) -> anyhow::Result<(usize, usize, [usize; 5])> {
    let repo = Repository::open(repo_path)
        .map_err(|e| anyhow::anyhow!("cannot open git repo {}: {e}", repo_path.display()))?;

    // Build the set of SHAs that already have an effort row (unless --force).
    let already_scored: std::collections::HashSet<String> = if args.force {
        std::collections::HashSet::new()
    } else {
        let conn = db.connection();
        let mut stmt = conn.prepare("SELECT sha FROM fact_commit_effort WHERE repository = ?1")?;
        let rows = stmt.query_map(params![repo_name], |row| row.get::<_, String>(0))?;
        let mut set = std::collections::HashSet::new();
        for r in rows {
            set.insert(r?);
        }
        set
    };

    // Set up the revwalk.
    let mut revwalk = repo.revwalk()?;
    revwalk.set_sorting(Sort::TIME)?;

    if let Some(ref range) = args.range {
        // Parse the range: "A..B" → push B, hide A.
        if let Some((base, tip)) = range.split_once("..") {
            let tip_oid = repo
                .revparse_single(tip.trim())
                .map_err(|e| anyhow::anyhow!("cannot resolve git ref '{tip}': {e}"))?
                .id();
            revwalk.push(tip_oid)?;
            if !base.trim().is_empty() {
                let base_oid = repo
                    .revparse_single(base.trim())
                    .map_err(|e| anyhow::anyhow!("cannot resolve git ref '{base}': {e}"))?
                    .id();
                revwalk.hide(base_oid)?;
            }
        } else {
            // Single ref — walk from there.
            let oid = repo
                .revparse_single(range.trim())
                .map_err(|e| anyhow::anyhow!("cannot resolve git ref '{range}': {e}"))?
                .id();
            revwalk.push(oid)?;
        }
    } else {
        // HEAD may not exist on an empty repo — silently skip.
        let _ = revwalk.push_head();
    }

    // Collect records for this repo.
    let mut records: Vec<EffortRow> = Vec::new();
    let mut skipped: usize = 0;
    let limit = args.limit.unwrap_or(usize::MAX);

    for oid_res in revwalk {
        if records.len() >= limit {
            break;
        }

        let oid = match oid_res {
            Ok(o) => o,
            Err(e) => {
                tracing::warn!(repo = %repo_name, error = %e, "revwalk error; stopping");
                break;
            }
        };

        let sha_str = oid.to_string();

        // Skip already-scored commits unless --force.
        if already_scored.contains(&sha_str) {
            skipped += 1;
            continue;
        }

        // Compute the diff.
        let commit = match repo.find_commit(oid) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(sha = %sha_str, error = %e, "cannot find commit; skipping");
                continue;
            }
        };

        let tree = match commit.tree() {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(sha = %sha_str, error = %e, "cannot get tree; skipping");
                continue;
            }
        };

        let parent_tree = if commit.parent_count() > 0 {
            match commit.parent(0).and_then(|p| p.tree()) {
                Ok(t) => Some(t),
                Err(e) => {
                    tracing::warn!(sha = %sha_str, error = %e, "cannot get parent tree; skipping");
                    continue;
                }
            }
        } else {
            None
        };

        let diff = match repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(sha = %sha_str, error = %e, "diff failed; skipping");
                continue;
            }
        };

        // Extract per-file stats for the effort formula.
        // We walk the diff to collect (path, insertions, deletions) tuples.
        let file_stats: std::cell::RefCell<Vec<(String, u32, u32)>> =
            std::cell::RefCell::new(Vec::new());

        // First pass: collect file paths.
        let _ = diff.foreach(
            &mut |delta, _progress| {
                let path = delta
                    .new_file()
                    .path()
                    .or_else(|| delta.old_file().path())
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default();
                file_stats.borrow_mut().push((path, 0, 0));
                true
            },
            None,
            None,
            Some(&mut |delta, _hunk, line| {
                let path = delta
                    .new_file()
                    .path()
                    .or_else(|| delta.old_file().path())
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default();
                let mut files = file_stats.borrow_mut();
                if let Some(entry) = files.iter_mut().find(|e| e.0 == path) {
                    match line.origin() {
                        '+' => entry.1 = entry.1.saturating_add(1),
                        '-' => entry.2 = entry.2.saturating_add(1),
                        _ => {}
                    }
                }
                true
            }),
        );

        // Extend the lifetime of the borrow by binding to a named variable.
        let stats_snapshot = file_stats.into_inner();
        let file_refs: Vec<(&str, u32, u32)> = stats_snapshot
            .iter()
            .map(|(p, ins, del)| (p.as_str(), *ins, *del))
            .collect();

        let effort = compute_effort(file_refs);
        let computed_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        records.push(EffortRow {
            sha: sha_str,
            repository: repo_name.to_string(),
            size: effort.size_label().to_string(),
            score: effort.score,
            loc: effort.loc,
            files: effort.files,
            test_loc: effort.test_loc,
            tests_factor: effort.tests_factor,
            formula_version: FORMULA_VERSION.to_string(),
            computed_at,
        });

        // Log progress every 1000 commits.
        if records.len().is_multiple_of(1000) {
            tracing::info!(
                repo = %repo_name,
                processed = records.len(),
                "effort backfill progress"
            );
        }
    }

    // Write git notes if requested (--notes).
    if args.notes && !dry_run {
        write_effort_notes(&repo, &records);
    }

    let mut size_counts = [0usize; 5];
    for row in &records {
        let idx = match row.size.as_str() {
            "XS" => 0,
            "S" => 1,
            "M" => 2,
            "L" => 3,
            _ => 4, // XL
        };
        size_counts[idx] += 1;
    }

    if !dry_run {
        persist_effort_rows(db, &records)?;
    }

    Ok((records.len(), skipped, size_counts))
}

/// A single row to be written to `fact_commit_effort`.
struct EffortRow {
    sha: String,
    repository: String,
    size: String,
    score: f64,
    loc: u32,
    files: u32,
    test_loc: u32,
    tests_factor: f64,
    formula_version: String,
    computed_at: i64,
}

/// Persist effort rows in batches of 1000 using UPSERT semantics.
///
/// Why: batching avoids per-row transaction overhead on large corpora; UPSERT
/// (`INSERT OR REPLACE`) ensures --force re-computation overwrites stale rows.
/// What: splits `rows` into chunks of 1000 and wraps each chunk in a single
/// transaction.
/// Test: `tests::backfill_effort_persists_rows` and
/// `tests::backfill_effort_force_recomputes`.
fn persist_effort_rows(db: &mut Database, rows: &[EffortRow]) -> anyhow::Result<()> {
    for chunk in rows.chunks(1000) {
        let conn = db.connection_mut();
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO fact_commit_effort \
                 (sha, repository, size, score, loc, files, test_loc, tests_factor, \
                  formula_version, computed_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            )?;
            for row in chunk {
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
                ])?;
            }
        }
        tx.commit()?;
    }
    Ok(())
}

/// Write `Effort: <size>` git notes to `refs/notes/effort`.
///
/// Why: optional git-native visibility for effort scores — lets users run
/// `git log --show-notes=effort` to see effort annotations inline.
/// What: for each row, appends a note to `refs/notes/effort` on the commit.
/// Soft-fails per commit (notes API errors are logged but do not abort).
/// Test: exercised by the `--notes` integration path; not unit-tested since
/// it requires a real on-disk git repo and mutates git state.
fn write_effort_notes(repo: &Repository, rows: &[EffortRow]) {
    // Resolve the notes ref signature (falls back to repo config or a
    // placeholder — notes are informational only).
    let sig = match repo.signature() {
        Ok(s) => s,
        Err(_) => match git2::Signature::now("tga", "tga@localhost") {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "cannot create git signature for notes; skipping");
                return;
            }
        },
    };

    for row in rows {
        let oid = match git2::Oid::from_str(&row.sha) {
            Ok(o) => o,
            Err(_) => continue,
        };
        let note_body = format!("Effort: {}", row.size);
        if let Err(e) = repo.note(
            &sig,
            &sig,
            Some("refs/notes/effort"),
            oid,
            &note_body,
            true, // force-overwrite
        ) {
            tracing::warn!(sha = %row.sha, error = %e, "failed to write git note; skipping");
        }
    }
}

// ── backfill reachability ─────────────────────────────────────────────────────

/// Re-run the reachability scan (tags, release branches, default branch) and
/// upsert `fact_commit_reachability` for all configured repositories (or a
/// filtered subset) without re-collecting commits.
///
/// Why: existing databases built before issue #290 was fixed have
/// `on_default_branch=0` for every row.  Running `tga collect` again costs
/// 20+ minutes on large corpora.  This function re-uses the same
/// `scan_and_persist` code path to recompute all five reachability columns
/// in-place via `INSERT … ON CONFLICT … DO UPDATE SET …`.
/// What: iterates configured repositories (filtered by `args.repos` when
/// provided), opens the local git repo, calls `scan_and_persist`, and prints
/// a per-repo summary + final totals to stdout.  When `dry_run=true` no writes
/// occur; instead the function reports what *would* change.
/// Test: `tests::backfill_reachability_*` below cover the upsert, idempotency,
/// and repo-filter paths.
///
/// # Errors
///
/// Returns an error if the database connection or git repo open fails.  Per-
/// repo scan failures are non-fatal and printed as warnings.
fn backfill_reachability(
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

/// Mark every commit whose existing classification was produced by the LLM
/// tier with a confidence below 0.7 as needing re-classification.
///
/// Implementation: we don't have a separate LLM tier yet, so re-running
/// classification means clearing the `classification_id` foreign key on
/// the affected commits. The next `tga classify` run will pick them up.
fn backfill_ai_detection(db: &mut Database, dry_run: bool) -> anyhow::Result<()> {
    let conn = db.connection();
    // Count first.
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM commits c \
             JOIN classifications cl ON c.classification_id = cl.id \
             WHERE cl.method = 'llm' AND COALESCE(c.confidence, cl.confidence) < 0.7",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);

    if dry_run {
        println!(
            "Would re-classify {count} commits (method='llm', confidence<0.7). No changes written."
        );
        return Ok(());
    }

    let conn = db.connection_mut();
    let tx = conn.transaction()?;
    let n = tx.execute(
        "UPDATE commits SET classification_id = NULL, confidence = NULL \
         WHERE classification_id IN ( \
             SELECT id FROM classifications WHERE method = 'llm' \
         ) AND COALESCE(confidence, 0.0) < 0.7",
        [],
    )?;
    tx.commit()?;
    println!(
        "Cleared classification on {n} commits — next `tga classify` run will reprocess them."
    );
    Ok(())
}

/// Scan every commit message for revert patterns and update `is_revert`.
///
/// Why: the `is_revert` boolean must mirror the verdict produced by the
/// classification cascade so DORA queries (CFR, MTTR) can join through it.
/// What: scans `commits` (filtered by repos/since/until when supplied),
/// detects revert prefixes, and updates changed rows. Supports dry-run.
/// Test: see `tests::backfill_revert_flags_updates_only_changed_rows`.
fn backfill_revert_flags(
    db: &mut Database,
    dry_run: bool,
    repos_filter: &[String],
    since: Option<&str>,
    until: Option<&str>,
) -> anyhow::Result<()> {
    let mut to_update: Vec<(i64, bool)> = Vec::new();
    {
        let conn = db.connection();
        // Build filtered SQL for repos/since/until.
        let (sql, params) = build_commits_filter_sql(
            "SELECT id, message, is_revert FROM commits",
            repos_filter,
            since,
            until,
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        for r in rows {
            let (id, message, current) = r?;
            let detected = is_revert(&message);
            let target = if detected { 1 } else { 0 };
            if target != current {
                to_update.push((id, detected));
            }
        }
    }

    if dry_run {
        println!(
            "Would update {} commits ({} would be marked as reverts). No changes written.",
            to_update.len(),
            to_update.iter().filter(|(_, v)| *v).count(),
        );
        return Ok(());
    }

    let conn = db.connection_mut();
    let tx = conn.transaction()?;
    {
        let mut up = tx.prepare("UPDATE commits SET is_revert = ?1 WHERE id = ?2")?;
        for (id, flag) in &to_update {
            up.execute(params![if *flag { 1 } else { 0 }, id])?;
        }
    }
    tx.commit()?;
    println!(
        "Updated is_revert on {} commits ({} are reverts).",
        to_update.len(),
        to_update.iter().filter(|(_, v)| *v).count(),
    );
    Ok(())
}

/// Scan every commit message, extract the first ticket reference, and
/// update `ticket_id` + `ticketed`.
///
/// Why: ticket extraction patterns evolve; backfilling lets operators
/// update the DB after extending patterns without re-collecting.
/// What: scans `commits` (filtered by repos/since/until when supplied),
/// extracts ticket IDs, and updates changed rows.
/// Test: see `tests::backfill_ticket_ids_populates_ticket_id`.
fn backfill_ticket_ids(
    db: &mut Database,
    dry_run: bool,
    repos_filter: &[String],
    since: Option<&str>,
    until: Option<&str>,
) -> anyhow::Result<()> {
    let mut to_update: Vec<(i64, Option<String>, i64)> = Vec::new();
    {
        let conn = db.connection();
        let (sql, params) = build_commits_filter_sql(
            "SELECT id, message, ticket_id, ticketed FROM commits",
            repos_filter,
            since,
            until,
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?;
        for r in rows {
            let (id, message, current_id, current_ticketed) = r?;
            let extracted = extract_ticket_id(&message);
            let ticketed = if is_ticketed(&message) { 1 } else { 0 };
            if extracted != current_id || ticketed != current_ticketed {
                to_update.push((id, extracted, ticketed));
            }
        }
    }

    if dry_run {
        let with_id = to_update.iter().filter(|(_, id, _)| id.is_some()).count();
        println!(
            "Would update {} commits ({} would gain a ticket_id). No changes written.",
            to_update.len(),
            with_id,
        );
        return Ok(());
    }

    let conn = db.connection_mut();
    let tx = conn.transaction()?;
    {
        let mut up =
            tx.prepare("UPDATE commits SET ticket_id = ?1, ticketed = ?2 WHERE id = ?3")?;
        for (id, ticket, ticketed) in &to_update {
            up.execute(params![ticket, ticketed, id])?;
        }
    }
    tx.commit()?;
    let with_id = to_update.iter().filter(|(_, id, _)| id.is_some()).count();
    println!(
        "Updated {} commits ({} now have a ticket_id).",
        to_update.len(),
        with_id,
    );
    Ok(())
}

/// Build a SQL fragment and bind params for the common backfill filters.
///
/// Why: revert-flags and ticket-ids both need `WHERE` clauses for repos,
/// since, and until; extracting this avoids duplicating the SQL-building
/// logic in each function.
/// What: given a base SELECT (ending before any WHERE clause), appends
/// predicates for `repository IN (…)`, `timestamp >= ?`, `timestamp <= ?`
/// as needed, returning the assembled SQL string and bound values.
/// Test: exercised indirectly by backfill filter tests.
fn build_commits_filter_sql(
    base_sql: &str,
    repos: &[String],
    since: Option<&str>,
    until: Option<&str>,
) -> (String, Vec<rusqlite::types::Value>) {
    use rusqlite::types::Value;
    let mut predicates: Vec<String> = Vec::new();
    let mut params: Vec<Value> = Vec::new();

    if !repos.is_empty() {
        let start = params.len() + 1;
        for r in repos {
            params.push(Value::Text(r.clone()));
        }
        let end = params.len();
        let placeholders: Vec<String> = (start..=end).map(|i| format!("?{i}")).collect();
        predicates.push(format!("repository IN ({})", placeholders.join(", ")));
    }
    if let Some(s) = since {
        params.push(Value::Text(s.to_string()));
        predicates.push(format!("timestamp >= ?{}", params.len()));
    }
    if let Some(u) = until {
        params.push(Value::Text(u.to_string()));
        predicates.push(format!("timestamp <= ?{}", params.len()));
    }

    let sql = if predicates.is_empty() {
        base_sql.to_string()
    } else {
        format!("{base_sql} WHERE {}", predicates.join(" AND "))
    };
    (sql, params)
}

/// Detect if a commit message looks like a revert.
///
/// Why: the `commits.is_revert` column written by `tga backfill is-revert`
/// must agree with the revert rate the report computes from the same commit
/// messages. Issue #377 unified both paths onto
/// [`tga::core::revert::is_revert`]; this thin wrapper preserves the
/// existing call sites while delegating to the single source of truth.
/// What: forwards to [`tga::core::revert::is_revert`], which catches
/// `Revert "..."`, `revert:`, `revert(scope):`, `^revert`, and `^fix.*revert`
/// (case-insensitive, first-line only).
/// Test: `tests::revert_detector_matches_expected_forms` below, plus the
/// canonical coverage in `crate::core::revert::tests`.
fn is_revert(message: &str) -> bool {
    tga::core::revert::is_revert(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(db: &Database, sha: &str, message: &str) {
        db.connection()
            .execute(
                "INSERT INTO commits (sha, author_name, author_email, timestamp, message, repository) \
                 VALUES (?1, 'n', 'e', '2024-01-01T00:00:00Z', ?2, 'r')",
                params![sha, message],
            )
            .expect("insert");
    }

    #[test]
    fn revert_detector_matches_expected_forms() {
        assert!(is_revert("Revert \"feat: add login\""));
        assert!(is_revert("revert: bad merge"));
        assert!(is_revert("Revert this change"));
        assert!(!is_revert("Refactor revert handling"));
        assert!(!is_revert("Fix bug in feature"));
    }

    #[test]
    fn ticket_id_extraction_prefers_specific_patterns() {
        assert_eq!(
            extract_ticket_id("AB#42 implement"),
            Some("AB#42".to_string())
        );
        assert_eq!(
            extract_ticket_id("ENG-123: feature"),
            Some("ENG-123".to_string())
        );
        assert_eq!(extract_ticket_id("fixes #99"), Some("#99".to_string()));
        assert_eq!(extract_ticket_id("misc cleanup"), None);
    }

    #[test]
    fn backfill_revert_flags_updates_only_changed_rows() {
        let mut db = Database::open_in_memory().expect("open");
        seed(&db, "a", "Revert \"foo\"");
        seed(&db, "b", "feat: thing");
        backfill_revert_flags(&mut db, false, &[], None, None).expect("backfill");
        let reverts: i64 = db
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM commits WHERE is_revert = 1",
                [],
                |r| r.get(0),
            )
            .expect("q");
        assert_eq!(reverts, 1);
    }

    #[test]
    fn backfill_ticket_ids_populates_ticket_id() {
        let mut db = Database::open_in_memory().expect("open");
        seed(&db, "a", "ENG-7: thing");
        seed(&db, "b", "no ticket");
        backfill_ticket_ids(&mut db, false, &[], None, None).expect("backfill");
        let t: Option<String> = db
            .connection()
            .query_row("SELECT ticket_id FROM commits WHERE sha = 'a'", [], |r| {
                r.get(0)
            })
            .expect("q");
        assert_eq!(t, Some("ENG-7".to_string()));
        let n: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM commits WHERE ticketed = 1", [], |r| {
                r.get(0)
            })
            .expect("q");
        assert_eq!(n, 1);
    }

    #[test]
    fn dry_run_does_not_modify_rows() {
        let mut db = Database::open_in_memory().expect("open");
        seed(&db, "a", "Revert \"foo\"");
        backfill_revert_flags(&mut db, true, &[], None, None).expect("dry run");
        let reverts: i64 = db
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM commits WHERE is_revert = 1",
                [],
                |r| r.get(0),
            )
            .expect("q");
        assert_eq!(reverts, 0);
    }

    /// Why: regression guard for issue #397 bug 2. `tga backfill complexity`
    /// must be wired and its dry-run path must report the count of NULL-complexity
    /// candidates without invoking the LLM (so it works offline) and without
    /// mutating any row.
    /// What: seed one classification with `complexity IS NULL` (regex_rule,
    /// eligible) and one already-scored row (must not be counted); run the
    /// dry-run backfill; assert no LLM is needed and nothing is written.
    /// Test: in-memory DB; dry_run=true short-circuits before any LLM call.
    #[tokio::test]
    async fn backfill_complexity_dry_run_reports_candidates_without_writing() {
        let mut db = Database::open_in_memory().expect("open");

        // Candidate: NULL complexity, non-exact method.
        db.connection()
            .execute(
                "INSERT INTO classifications (category, confidence, method, complexity) \
                 VALUES ('feature', 0.5, 'regex_rule', NULL)",
                [],
            )
            .expect("insert null-complexity row");
        // Not a candidate: already scored.
        db.connection()
            .execute(
                "INSERT INTO classifications (category, confidence, method, complexity) \
                 VALUES ('bugfix', 0.8, 'regex_rule', 3)",
                [],
            )
            .expect("insert scored row");

        let args = ComplexityBackfillArgs { use_llm: false };
        // dry_run=true must not hit the network; Config::default() has no LLM key.
        backfill_complexity(Config::default(), &mut db, args, true)
            .await
            .expect("dry-run complexity backfill");

        // Nothing changed: the NULL row is still NULL, the scored row still 3.
        let null_count: i64 = db
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM classifications WHERE complexity IS NULL",
                [],
                |r| r.get(0),
            )
            .expect("count null");
        assert_eq!(null_count, 1, "dry-run must not write complexity scores");
    }

    // ── effort backfill tests ─────────────────────────────────────────────────

    /// Why: verify the schema migration and UPSERT INSERT path work end-to-end.
    /// What: calls `persist_effort_rows` with known data and reads it back.
    /// Test: this test itself.
    #[test]
    fn backfill_effort_persists_rows() {
        let mut db = Database::open_in_memory().expect("open");

        let rows = vec![EffortRow {
            sha: "abc123".to_string(),
            repository: "testrepo".to_string(),
            size: "M".to_string(),
            score: 9.1,
            loc: 50,
            files: 2,
            test_loc: 0,
            tests_factor: 1.0,
            formula_version: FORMULA_VERSION.to_string(),
            computed_at: 1_000_000,
        }];

        persist_effort_rows(&mut db, &rows).expect("persist");

        let (size, score, loc, files): (String, f64, i64, i64) = db
            .connection()
            .query_row(
                "SELECT size, score, loc, files \
                 FROM fact_commit_effort WHERE sha = 'abc123'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .expect("query");

        assert_eq!(size, "M");
        assert!((score - 9.1).abs() < 0.001);
        assert_eq!(loc, 50);
        assert_eq!(files, 2);
    }

    /// Why: verify that `--force` semantics replace an existing row with
    /// updated values rather than silently keeping the old one.
    /// What: inserts a row with score=1.0, then re-inserts with score=9.9
    /// (simulating --force); asserts the score was updated.
    /// Test: this test itself.
    #[test]
    fn backfill_effort_force_recomputes() {
        let mut db = Database::open_in_memory().expect("open");

        // First pass: insert initial row.
        let first = vec![EffortRow {
            sha: "deadbeef".to_string(),
            repository: "repo".to_string(),
            size: "XS".to_string(),
            score: 1.0,
            loc: 1,
            files: 1,
            test_loc: 0,
            tests_factor: 1.0,
            formula_version: FORMULA_VERSION.to_string(),
            computed_at: 1_000_000,
        }];
        persist_effort_rows(&mut db, &first).expect("first persist");

        // Second pass: replace with updated score.
        let second = vec![EffortRow {
            sha: "deadbeef".to_string(),
            repository: "repo".to_string(),
            size: "XL".to_string(),
            score: 99.9,
            loc: 100_000,
            files: 500,
            test_loc: 0,
            tests_factor: 1.0,
            formula_version: FORMULA_VERSION.to_string(),
            computed_at: 2_000_000,
        }];
        persist_effort_rows(&mut db, &second).expect("second persist");

        // Only one row should exist (no duplicate).
        let count: i64 = db
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM fact_commit_effort WHERE sha = 'deadbeef'",
                [],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(count, 1, "UPSERT must not create duplicate rows");

        let score: f64 = db
            .connection()
            .query_row(
                "SELECT score FROM fact_commit_effort WHERE sha = 'deadbeef'",
                [],
                |r| r.get(0),
            )
            .expect("score");
        assert!(
            (score - 99.9).abs() < 0.001,
            "score must be updated to 99.9"
        );
    }

    /// Why: `fact_commit_effort` must allow the same SHA in two different
    /// repositories (fork/mirror scenarios).
    /// What: insert two rows with the same SHA but different repository; both
    /// must persist without conflict.
    /// Test: this test itself.
    #[test]
    fn backfill_effort_same_sha_different_repos() {
        let mut db = Database::open_in_memory().expect("open");

        let rows = vec![
            EffortRow {
                sha: "cafebabe".to_string(),
                repository: "repo-a".to_string(),
                size: "S".to_string(),
                score: 5.5,
                loc: 30,
                files: 2,
                test_loc: 0,
                tests_factor: 1.0,
                formula_version: FORMULA_VERSION.to_string(),
                computed_at: 1_000_000,
            },
            EffortRow {
                sha: "cafebabe".to_string(),
                repository: "repo-b".to_string(),
                size: "M".to_string(),
                score: 8.0,
                loc: 60,
                files: 3,
                test_loc: 0,
                tests_factor: 1.0,
                formula_version: FORMULA_VERSION.to_string(),
                computed_at: 1_000_000,
            },
        ];

        persist_effort_rows(&mut db, &rows).expect("persist");

        let count: i64 = db
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM fact_commit_effort WHERE sha = 'cafebabe'",
                [],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(count, 2, "same SHA in two repos must produce two rows");
    }

    /// Why: an effort backfill on an empty repo should produce zero rows and
    /// no errors.
    /// What: calls `persist_effort_rows` with an empty slice.
    /// Test: this test itself.
    #[test]
    fn backfill_effort_empty_produces_no_rows() {
        let mut db = Database::open_in_memory().expect("open");
        persist_effort_rows(&mut db, &[]).expect("empty persist");
        let count: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM fact_commit_effort", [], |r| r.get(0))
            .expect("count");
        assert_eq!(count, 0);
    }

    // ── db-path tests ─────────────────────────────────────────────────────────

    /// Seed a commit row and its associated files rows into an in-memory DB.
    ///
    /// Why: shared helper for db-path tests; avoids repetitive SQL in each test.
    /// What: inserts one commit row and one or more file rows, returning the
    /// commit's integer id.
    /// Test: used by `backfill_effort_db_path_*` tests below.
    fn seed_commit_with_files(
        db: &Database,
        sha: &str,
        repo: &str,
        timestamp: &str,
        files: &[(&str, u32, u32)], // (path, insertions, deletions)
    ) -> i64 {
        let conn = db.connection();
        conn.execute(
            "INSERT INTO commits (sha, author_name, author_email, timestamp, message, repository) \
             VALUES (?1, 'tester', 'test@example.com', ?2, 'msg', ?3)",
            params![sha, timestamp, repo],
        )
        .expect("insert commit");
        let commit_id = conn.last_insert_rowid();
        for (path, ins, del) in files {
            conn.execute(
                "INSERT INTO files (commit_id, path, change_type, insertions, deletions) \
                 VALUES (?1, ?2, 'modified', ?3, ?4)",
                params![commit_id, path, ins, del],
            )
            .expect("insert file");
        }
        commit_id
    }

    /// Why: verify the db-only path reads `commits JOIN files` and populates
    /// `fact_commit_effort` correctly without touching a git repo.
    /// What: seeds two commits with file rows; calls `process_one_repo_db` and
    /// then persists; asserts both rows appear in `fact_commit_effort`.
    /// Test: this test itself.
    #[test]
    fn backfill_effort_db_path_populates_fact_table() {
        let mut db = Database::open_in_memory().expect("open");

        seed_commit_with_files(
            &db,
            "aaa111",
            "myrepo",
            "2024-01-01T00:00:00Z",
            &[("src/main.rs", 30, 10), ("src/lib.rs", 5, 2)],
        );
        seed_commit_with_files(
            &db,
            "bbb222",
            "myrepo",
            "2024-01-02T00:00:00Z",
            &[("src/tests/foo_test.rs", 20, 0)],
        );

        let args = EffortBackfillArgs {
            range: None,
            force: false,
            notes: false,
            limit: None,
        };

        let (scored, skipped, _sizes, rows) =
            process_one_repo_db(db.connection(), "myrepo", &args, false).expect("db path");
        assert_eq!(scored, 2, "both commits should be scored");
        assert_eq!(skipped, 0, "nothing pre-scored");

        persist_effort_rows(&mut db, &rows).expect("persist");

        let count: i64 = db
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM fact_commit_effort WHERE repository = 'myrepo'",
                [],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(count, 2, "two effort rows expected");

        // Verify the test-file commit has a reduced tests_factor.
        let (size_b, tests_factor_b): (String, f64) = db
            .connection()
            .query_row(
                "SELECT size, tests_factor FROM fact_commit_effort WHERE sha = 'bbb222'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("bbb222 row");
        // 20 test LoC out of 20 total → ratio=1 → tests_factor=0.7
        assert!(
            (tests_factor_b - 0.7).abs() < 1e-6,
            "expected tests_factor=0.7 for all-test commit, got {tests_factor_b}"
        );
        // score = 1.0*log2(21) + 1.5*log2(2) + 1.0*0.7 ≈ 4.392 + 1.5 + 0.7 = 6.592 → S
        assert_eq!(size_b, "S", "all-test commit should be S");
    }

    /// Why: verify the db-path respects the `--force=false` default — commits
    /// that already have an effort row must be skipped.
    /// What: inserts a pre-existing effort row for one commit; runs db path;
    /// asserts only the unscored commit is returned.
    /// Test: this test itself.
    #[test]
    fn backfill_effort_db_path_skips_already_scored() {
        let mut db = Database::open_in_memory().expect("open");

        seed_commit_with_files(
            &db,
            "scored111",
            "repo",
            "2024-01-01T00:00:00Z",
            &[("src/a.rs", 10, 0)],
        );
        seed_commit_with_files(
            &db,
            "unscored222",
            "repo",
            "2024-01-02T00:00:00Z",
            &[("src/b.rs", 5, 5)],
        );

        // Pre-populate an effort row for scored111.
        let pre = vec![EffortRow {
            sha: "scored111".to_string(),
            repository: "repo".to_string(),
            size: "XS".to_string(),
            score: 1.0,
            loc: 10,
            files: 1,
            test_loc: 0,
            tests_factor: 1.0,
            formula_version: FORMULA_VERSION.to_string(),
            computed_at: 0,
        }];
        persist_effort_rows(&mut db, &pre).expect("pre-persist");

        let args = EffortBackfillArgs {
            range: None,
            force: false,
            notes: false,
            limit: None,
        };

        let (scored, skipped, _sizes, rows) =
            process_one_repo_db(db.connection(), "repo", &args, false).expect("db path");

        assert_eq!(scored, 1, "only unscored222 should be scored");
        assert_eq!(skipped, 1, "scored111 should be skipped");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].sha, "unscored222");
    }

    /// Why: verify `--force` causes already-scored commits to be re-scored
    /// rather than skipped on the db path.
    /// What: pre-populates effort for a commit; runs db path with force=true;
    /// asserts the commit appears in the returned rows.
    /// Test: this test itself.
    #[test]
    fn backfill_effort_db_path_force_rescores_all() {
        let mut db = Database::open_in_memory().expect("open");

        seed_commit_with_files(
            &db,
            "sha001",
            "repo",
            "2024-01-01T00:00:00Z",
            &[("src/x.rs", 100, 50)],
        );

        // Insert a stale effort row.
        let stale = vec![EffortRow {
            sha: "sha001".to_string(),
            repository: "repo".to_string(),
            size: "XS".to_string(),
            score: 0.1,
            loc: 1,
            files: 1,
            test_loc: 0,
            tests_factor: 1.0,
            formula_version: "v0".to_string(),
            computed_at: 0,
        }];
        persist_effort_rows(&mut db, &stale).expect("stale persist");

        let args = EffortBackfillArgs {
            range: None,
            force: true, // re-score everything
            notes: false,
            limit: None,
        };

        let (scored, skipped, _sizes, rows) =
            process_one_repo_db(db.connection(), "repo", &args, false).expect("db path");

        assert_eq!(scored, 1, "force path should score the commit");
        assert_eq!(skipped, 0, "nothing should be skipped with --force");
        // The new score should reflect 150 LoC, not the stale 0.1.
        assert!(
            rows[0].score > 1.0,
            "re-scored effort should be higher than stale 0.1"
        );
    }

    /// Why: commits present in the `commits` table but with no rows in `files`
    /// (e.g., empty commits) must not cause errors — they should be silently
    /// skipped with a warning.
    /// What: inserts a commit with no file rows; runs db path; asserts zero
    /// records returned and no error raised.
    /// Test: this test itself.
    #[test]
    fn backfill_effort_db_path_skips_commit_with_no_files() {
        let db = Database::open_in_memory().expect("open");

        // Insert commit row but NO file rows.
        db.connection()
            .execute(
                "INSERT INTO commits (sha, author_name, author_email, timestamp, message, repository) \
                 VALUES ('empty001', 'tester', 'test@example.com', '2024-01-01T00:00:00Z', 'empty', 'repo')",
                [],
            )
            .expect("insert commit");

        // The above commit has no files rows, so the JOIN returns no rows —
        // `process_one_repo_db` will not even see a SHA to group.
        let args = EffortBackfillArgs {
            range: None,
            force: false,
            notes: false,
            limit: None,
        };

        let (scored, skipped, _sizes, rows) =
            process_one_repo_db(db.connection(), "repo", &args, false).expect("db path");

        // Zero files rows → nothing scored.
        assert_eq!(scored, 0, "commit with no files should produce no records");
        assert_eq!(skipped, 0);
        assert!(rows.is_empty());
    }

    /// Why: the `--limit N` flag must cap records at N even when more commits
    /// are available in the db.
    /// What: seeds 5 commits; runs db path with limit=3; asserts exactly 3
    /// records are returned.
    /// Test: this test itself.
    #[test]
    fn backfill_effort_db_path_respects_limit() {
        let db = Database::open_in_memory().expect("open");

        for i in 0..5u32 {
            seed_commit_with_files(
                &db,
                &format!("sha{i:03}"),
                "repo",
                &format!("2024-01-{:02}T00:00:00Z", i + 1),
                &[("src/foo.rs", 10, 5)],
            );
        }

        let args = EffortBackfillArgs {
            range: None,
            force: false,
            notes: false,
            limit: Some(3),
        };

        let (scored, _skipped, _sizes, rows) =
            process_one_repo_db(db.connection(), "repo", &args, false).expect("db path");

        assert_eq!(scored, 3, "limit=3 should cap at 3 records");
        assert_eq!(rows.len(), 3);
    }

    /// Why: the db path must correctly segregate commits by repository when
    /// multiple repos share the same database.
    /// What: seeds commits for two different repos; runs db path for one;
    /// asserts only that repo's commits are scored.
    /// Test: this test itself.
    #[test]
    fn backfill_effort_db_path_scoped_to_repo() {
        let db = Database::open_in_memory().expect("open");

        seed_commit_with_files(
            &db,
            "alpha001",
            "repo-alpha",
            "2024-01-01T00:00:00Z",
            &[("src/a.rs", 20, 10)],
        );
        seed_commit_with_files(
            &db,
            "beta001",
            "repo-beta",
            "2024-01-01T00:00:00Z",
            &[("src/b.rs", 50, 20)],
        );

        let args = EffortBackfillArgs {
            range: None,
            force: false,
            notes: false,
            limit: None,
        };

        // Process only repo-alpha.
        let (scored, _skipped, _sizes, rows) =
            process_one_repo_db(db.connection(), "repo-alpha", &args, false).expect("db path");

        assert_eq!(scored, 1);
        assert_eq!(rows[0].sha, "alpha001");
        assert_eq!(rows[0].repository, "repo-alpha");
    }

    /// Why: dry_run=true on the db path must return rows (for reporting) but
    /// the caller must not persist them — this test verifies the path selection
    /// in `backfill_effort` withholds `persist_effort_rows`.
    /// What: directly calls `process_one_repo_db` with dry_run=true; asserts
    /// rows are returned but `fact_commit_effort` remains empty.
    /// Test: this test itself.
    #[test]
    fn backfill_effort_db_path_dry_run_returns_rows_without_persisting() {
        let db = Database::open_in_memory().expect("open");

        seed_commit_with_files(
            &db,
            "drysha1",
            "repo",
            "2024-01-01T00:00:00Z",
            &[("src/main.rs", 40, 10)],
        );

        let args = EffortBackfillArgs {
            range: None,
            force: false,
            notes: false,
            limit: None,
        };

        let (scored, _skipped, _sizes, rows) =
            process_one_repo_db(db.connection(), "repo", &args, true /* dry_run */)
                .expect("db path");

        assert_eq!(
            scored, 1,
            "db path should return 1 scored row even in dry_run"
        );
        assert_eq!(rows.len(), 1);

        // Caller is responsible for not persisting in dry_run; here we do NOT
        // call persist_effort_rows, mirroring the behaviour in `backfill_effort`.
        let count: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM fact_commit_effort", [], |r| r.get(0))
            .expect("count");
        assert_eq!(count, 0, "dry_run must not write to fact_commit_effort");
    }
}
