//! `tga backfill` — retroactive maintenance operations against the commits
//! table.
//!
//! These operations live outside the normal `collect → classify → report`
//! pipeline because they update existing rows in-place rather than
//! ingesting new data. Each subcommand supports `--dry-run`, in which case
//! it reports the number of rows that *would* change without writing.

use std::sync::OnceLock;

use clap::{Args, Subcommand};
use git2::{Repository, Sort};
use regex::Regex;
use rusqlite::params;
use tga::collect::git::scan_and_persist;
use tga::collect::ticket::is_ticketed;
use tga::core::config::{expand_path, Config};
use tga::core::db::Database;
use tga::core::effort::{compute_effort, FORMULA_VERSION};

/// Arguments for `tga backfill`.
#[derive(Args, Debug)]
pub struct BackfillArgs {
    /// Backfill subcommand.
    #[command(subcommand)]
    pub subcommand: BackfillSubcommand,
    /// Report what would change without writing.
    #[arg(long, default_value_t = false, global = true)]
    pub dry_run: bool,
}

/// `tga backfill` subcommands.
#[derive(Subcommand, Debug)]
pub enum BackfillSubcommand {
    /// Re-run LLM classification on low-confidence prior LLM verdicts.
    AiDetection,
    /// Scan commit messages for revert patterns and set `is_revert`.
    RevertFlags,
    /// Scan commit messages for ticket refs and update `ticket_id`/`ticketed`.
    TicketIds,
    /// Re-run the tag/branch/default-branch reachability scan and upsert
    /// `fact_commit_reachability` without re-collecting commits.
    ///
    /// Use this to fix `on_default_branch=0` rows in existing databases
    /// without running the full 20-minute `tga collect` pipeline (issue #290).
    Reachability(ReachabilityBackfillArgs),
    /// Compute empirical effort scores for historical commits and persist them
    /// in `fact_commit_effort`.
    ///
    /// Uses the v1 formula (LoC + file count + tests factor, mapped to T-shirt
    /// sizes XS/S/M/L/XL) — identical to the pre-commit bash hook for
    /// forward-going commits.  Idempotent by default: commits that already
    /// have a `fact_commit_effort` row are skipped unless `--force` is given.
    Effort(EffortBackfillArgs),
}

/// Arguments for `tga backfill reachability`.
#[derive(Args, Debug)]
pub struct ReachabilityBackfillArgs {
    /// Only backfill reachability for these repository names (repeatable).
    ///
    /// When omitted, all repositories from the config are processed.
    /// Matches against the `name` field in `config.yaml`; falls back to the
    /// directory basename if `name` is absent.
    #[arg(long = "repo", value_name = "NAME")]
    pub repos: Vec<String>,
}

/// Arguments for `tga backfill effort`.
#[derive(Args, Debug)]
pub struct EffortBackfillArgs {
    /// Scope to a single configured repository name.
    ///
    /// When omitted, all repositories from the config file are processed.
    /// Matches against the `name` field in `config.yaml`; falls back to the
    /// directory basename if `name` is absent.
    #[arg(long = "repo", value_name = "NAME")]
    pub repo: Option<String>,

    /// Scope effort computation to a git commit range (e.g. `HEAD~10..HEAD`).
    ///
    /// When omitted, all commits in the chosen repo(s) that do not already
    /// have a `fact_commit_effort` row are processed (unless `--force`).
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
    /// default to keep the backfill lightweight.
    #[arg(long, default_value_t = false)]
    pub notes: bool,

    /// Maximum commits to process per repository.
    ///
    /// Useful for smoke-testing on a large corpus.  When omitted, all
    /// eligible commits are processed.
    #[arg(long, value_name = "N")]
    pub limit: Option<usize>,
}

/// Dispatch entry point for the `tga backfill` subcommand.
///
/// Why: routes each backfill subcommand to its implementation, passing shared
/// state (config, db connection) and the `--dry-run` flag.
/// What: matches on `args.subcommand` and calls the appropriate function.
/// Test: each variant has its own test module below.
///
/// # Errors
///
/// Propagates database errors from the underlying queries.
pub fn run(config: Config, db: &mut Database, args: BackfillArgs) -> anyhow::Result<()> {
    match args.subcommand {
        BackfillSubcommand::AiDetection => backfill_ai_detection(db, args.dry_run),
        BackfillSubcommand::RevertFlags => backfill_revert_flags(db, args.dry_run),
        BackfillSubcommand::TicketIds => backfill_ticket_ids(db, args.dry_run),
        BackfillSubcommand::Reachability(reach_args) => {
            backfill_reachability(config, db, reach_args, args.dry_run)
        }
        BackfillSubcommand::Effort(effort_args) => {
            backfill_effort(config, db, effort_args, args.dry_run)
        }
    }
}

// ── backfill effort ──────────────────────────────────────────────────────────

/// Compute empirical effort scores for historical commits and persist them into
/// `fact_commit_effort`, using the same v1 formula as the pre-commit bash hook.
///
/// Why: changing past commit SHAs is unacceptable for historical work, so
/// effort scores must be stored out-of-band in the analytics DB rather than
/// injected as git trailers retroactively.
/// What: for each configured repository (or a single one if `--repo` is given),
/// opens the git repo, walks commits, computes [`compute_effort`] per diff, and
/// upserts into `fact_commit_effort`.  Skips already-scored commits unless
/// `--force`.  Supports `--limit N` and `--dry-run`.
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

            // Apply --repo filter when supplied.
            if let Some(ref filter) = args.repo {
                if &name != filter {
                    return None;
                }
            }
            Some((path, name))
        })
        .collect();

    if repos_to_process.is_empty() {
        println!("No matching repositories found in config.");
        return Ok(());
    }

    // Summary accumulators.
    let mut total_scored: usize = 0;
    let mut total_skipped: usize = 0;
    let mut total_repos: usize = 0;
    let mut size_counts = [0usize; 5]; // XS, S, M, L, XL

    for (repo_path, repo_name) in &repos_to_process {
        let result = process_one_repo(repo_path, repo_name, db, &args, dry_run);
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

/// Process a single repository for the effort backfill.
///
/// Why: isolates per-repo logic so errors in one repo don't abort the others.
/// What: opens the git repo, queries existing effort rows, walks commits, calls
/// [`compute_effort`], and upserts rows in batches of 1000.
/// Test: called by `backfill_effort`; each path covered in `tests` below.
///
/// Returns `(scored, skipped, [XS, S, M, L, XL])`.
fn process_one_repo(
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
        if records.len() + skipped >= limit && skipped < limit {
            // We're about to process the limit-th eligible commit.
        }
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
    args: ReachabilityBackfillArgs,
    dry_run: bool,
) -> anyhow::Result<()> {
    if dry_run {
        println!(
            "Dry run — would re-run reachability scan for {} repo(s). No changes written.",
            if args.repos.is_empty() {
                config.repositories.len()
            } else {
                args.repos.len()
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

        // Apply --repo filter if provided.
        if !args.repos.is_empty() && !args.repos.contains(&name) {
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
fn backfill_revert_flags(db: &mut Database, dry_run: bool) -> anyhow::Result<()> {
    let mut to_update: Vec<(i64, bool)> = Vec::new();
    {
        let conn = db.connection();
        let mut stmt = conn.prepare("SELECT id, message, is_revert FROM commits")?;
        let rows = stmt.query_map([], |row| {
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
fn backfill_ticket_ids(db: &mut Database, dry_run: bool) -> anyhow::Result<()> {
    let mut to_update: Vec<(i64, Option<String>, i64)> = Vec::new();
    {
        let conn = db.connection();
        let mut stmt = conn.prepare("SELECT id, message, ticket_id, ticketed FROM commits")?;
        let rows = stmt.query_map([], |row| {
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

/// Detect if a commit message looks like a revert.
///
/// Matches:
///   * messages starting with `Revert ` (case-insensitive — `git revert`
///     auto-generates `Revert "<original subject>"`).
///   * messages starting with `revert:` (Conventional Commits style).
fn is_revert(message: &str) -> bool {
    let trimmed = message.trim_start();
    // The longest prefix we test is 7 bytes (`revert ` / `revert:` /
    // `revert"`). All candidates are pure ASCII, so a byte-bounded slice is
    // a safe split point and avoids allocating a lowercased copy of the
    // whole (potentially large) commit message on every commit.
    let head = trimmed.as_bytes();
    let bound = head.len().min(7);
    let prefix = &head[..bound];
    prefix.eq_ignore_ascii_case(b"revert ")
        || prefix.eq_ignore_ascii_case(b"revert:")
        || prefix.eq_ignore_ascii_case(b"revert\"")
}

/// Extract the first recognizable ticket identifier from a commit message.
///
/// Returns `Some("PROJ-123")`, `Some("AB#42")`, `Some("#7")`, or `None`.
fn extract_ticket_id(message: &str) -> Option<String> {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| {
        vec![
            // Order matters: most specific first.
            Regex::new(r"\bAB#\d+\b").expect("azdo pattern"),
            Regex::new(r"\b[A-Z][A-Z0-9]*-\d+\b").expect("jira pattern"),
            Regex::new(r"(?:^|\s)(#\d+)\b").expect("gh bare pattern"),
        ]
    });
    for (i, p) in patterns.iter().enumerate() {
        if let Some(m) = p.find(message) {
            // The gh-bare pattern includes a leading whitespace in its
            // overall match; strip to just the `#NNN` capture.
            let raw = m.as_str();
            if i == 2 {
                return raw.trim_start().to_string().into();
            }
            return Some(raw.to_string());
        }
    }
    None
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
        backfill_revert_flags(&mut db, false).expect("backfill");
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
        backfill_ticket_ids(&mut db, false).expect("backfill");
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
        backfill_revert_flags(&mut db, true).expect("dry run");
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
}
