//! `tga backfill` — retroactive maintenance operations against the commits
//! table.
//!
//! These operations live outside the normal `collect → classify → report`
//! pipeline because they update existing rows in-place rather than
//! ingesting new data. Each subcommand supports `--dry-run`, in which case
//! it reports the number of rows that *would* change without writing.

use std::sync::OnceLock;

use clap::{Args, Subcommand};
use regex::Regex;
use rusqlite::params;
use tga::collect::git::scan_and_persist;
use tga::collect::ticket::is_ticketed;
use tga::core::config::{expand_path, Config};
use tga::core::db::Database;

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
    }
}

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
}
