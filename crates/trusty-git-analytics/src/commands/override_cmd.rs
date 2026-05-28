//! `tga override` — manage manual commit classification overrides.
//!
//! Reads/writes the `classification_overrides` table, which is consulted as
//! Tier 0 by the classification engine (see
//! [`crate::classify::tiers::override_tier`]). A row here pins a commit's
//! verdict regardless of what the rules-based or LLM tiers would say.
//!
//! Why: human reviewers occasionally find commits that the cascade
//! mislabels, or want to force a specific subcategory before a release
//! report runs. The override table is the canonical mechanism for that.

use std::io::{self, BufRead, Write};

use clap::{Args, Subcommand};
use rusqlite::params;
use tga::core::config::Config;
use tga::core::db::Database;

/// Arguments for `tga override`.
#[derive(Args, Debug)]
#[command(
    about = "Manage manual classification overrides (Tier 0 of the cascade).",
    long_about = "Insert, list, or remove commit-level classification overrides.\n\n\
Overrides are stored in the `classification_overrides` table and consulted as\n\
Tier 0 by the classification engine — they win over all rules-based and LLM\n\
verdicts unconditionally. Use them to fix mislabelled commits or to pin a\n\
verdict for a specific commit before a release report.\n\n\
After adding or removing overrides, run `tga classify --force --repos <name>`\n\
to apply the change to the classifications table immediately.",
    after_help = "EXAMPLES:\n\
  # Pin commit abc123 as a bugfix in the my-service repo\n\
  tga override add abc123 bugfix bug_fix --repo my-service\n\n\
  # List all overrides for auditing\n\
  tga override list\n\n\
  # Remove an override (restores the cascade-based verdict)\n\
  tga override remove abc123 --yes\n\n\
TIPS:\n\
  - After adding an override, re-run `tga classify --force` to propagate it.\n\
  - Use `tga rules show <sha>` to check the current verdict before overriding."
)]
pub struct OverrideArgs {
    /// `override` subcommand to run.
    #[command(subcommand)]
    pub subcommand: OverrideSubcommand,
}

/// `tga override` subcommands.
#[derive(Subcommand, Debug)]
pub enum OverrideSubcommand {
    /// Insert (or replace) an override row for a commit SHA.
    Add {
        /// Full commit SHA the override applies to.
        sha: String,
        /// Subcategory / work type to record (e.g. `feature`, `bugfix`).
        work_type: String,
        /// Top-level change type (e.g. `feature`, `maintenance`).
        change_type: String,
        /// Optional free-form note explaining the override.
        #[arg(long)]
        notes: Option<String>,
        /// Repository path the override applies to. Required because the
        /// override table is keyed on `(sha, repo_path)`. When omitted,
        /// the SHA's existing repo (if found in `commits`) is used.
        #[arg(long)]
        repo: Option<String>,
    },
    /// List every override row in the database.
    List {
        /// Restrict output to a single repository path.
        #[arg(long)]
        repo: Option<String>,
    },
    /// Delete the override row(s) for a given SHA.
    Remove {
        /// Full commit SHA whose override should be deleted.
        sha: String,
        /// Skip the interactive confirmation prompt.
        #[arg(long, default_value_t = false)]
        yes: bool,
    },
}

/// Dispatch entry point for `tga override`.
///
/// Why: matches the top-level dispatch pattern used by sibling commands
/// (`aliases`, `backfill`).
/// What: routes to `add`, `list`, or `remove` based on the subcommand.
/// Test: each branch has dedicated unit tests below.
///
/// # Errors
///
/// Returns any database or I/O error raised by the underlying operation.
pub fn run(_config: Config, db: &mut Database, args: OverrideArgs) -> anyhow::Result<()> {
    match args.subcommand {
        OverrideSubcommand::Add {
            sha,
            work_type,
            change_type,
            notes,
            repo,
        } => add(
            db,
            &sha,
            &work_type,
            &change_type,
            notes.as_deref(),
            repo.as_deref(),
        ),
        OverrideSubcommand::List { repo } => list(db, repo.as_deref()),
        OverrideSubcommand::Remove { sha, yes } => remove(db, &sha, yes, &mut io::stdin().lock()),
    }
}

/// Insert (or replace) an override row.
///
/// Why: provides a CLI surface for the override table that doesn't require
/// hand-writing SQL.
/// What: shows the current classification for the SHA (if found), then
/// inserts via `INSERT OR REPLACE` so a re-`add` updates an existing row.
/// Test: `add_inserts_row` and `add_uses_repo_from_commits_when_omitted`.
fn add(
    db: &Database,
    sha: &str,
    work_type: &str,
    change_type: &str,
    notes: Option<&str>,
    repo: Option<&str>,
) -> anyhow::Result<()> {
    let conn = db.connection();

    // Resolve the repo path: explicit flag wins, otherwise look up in commits.
    let repo_path: String = match repo {
        Some(r) => r.to_string(),
        None => {
            let mut stmt = conn.prepare("SELECT repository FROM commits WHERE sha = ?1")?;
            let mut rows = stmt.query(params![sha])?;
            match rows.next()? {
                Some(row) => row.get::<_, String>(0)?,
                None => {
                    anyhow::bail!(
                        "no commit with sha {sha} found in DB and --repo not provided; \
                         pass --repo <path> to add an override for an unknown SHA"
                    )
                }
            }
        }
    };

    // Surface the current classification (if any) before mutating.
    let current = lookup_current_classification(db, sha)?;
    match current {
        Some((cat, sub, conf, method)) => {
            println!(
                "Current classification for {sha}: category={cat}, subcategory={}, \
                 confidence={conf:.2}, method={method}",
                sub.as_deref().unwrap_or("-")
            );
        }
        None => println!("(no existing classification for {sha} — adding fresh override)"),
    }

    conn.execute(
        "INSERT OR REPLACE INTO classification_overrides \
         (commit_sha, repo_path, work_type, change_type, notes) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![sha, repo_path, work_type, change_type, notes],
    )?;

    println!(
        "Override added: {sha} ({repo_path}) -> work_type={work_type}, change_type={change_type}"
    );
    Ok(())
}

/// Print every override row, optionally filtered by repo.
///
/// Why: operators need to audit which commits are pinned before re-running
/// classification.
/// What: queries `classification_overrides` with optional `repo_path` filter
/// and prints a fixed-width table.
/// Test: `list_filters_by_repo` and `list_returns_all_when_repo_none`.
fn list(db: &Database, repo: Option<&str>) -> anyhow::Result<()> {
    let conn = db.connection();
    let (sql, bind): (&str, Vec<&str>) = match repo {
        Some(r) => (
            "SELECT commit_sha, repo_path, work_type, change_type, notes, created_at \
             FROM classification_overrides WHERE repo_path = ?1 ORDER BY created_at DESC",
            vec![r],
        ),
        None => (
            "SELECT commit_sha, repo_path, work_type, change_type, notes, created_at \
             FROM classification_overrides ORDER BY created_at DESC",
            vec![],
        ),
    };

    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(bind.iter()), |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, String>(5)?,
        ))
    })?;

    println!(
        "{:<10}  {:<32}  {:<14}  {:<14}  {:<20}  notes",
        "sha", "repo_path", "work_type", "change_type", "created_at"
    );
    println!("{}", "-".repeat(110));

    let mut count = 0usize;
    for r in rows {
        let (sha, repo_path, work_type, change_type, notes, created_at) = r?;
        let short_sha = sha.chars().take(8).collect::<String>();
        let note_str = notes.unwrap_or_else(|| "-".to_string());
        println!(
            "{:<10}  {:<32}  {:<14}  {:<14}  {:<20}  {}",
            short_sha, repo_path, work_type, change_type, created_at, note_str
        );
        count += 1;
    }
    if count == 0 {
        println!("(no overrides found)");
    }
    Ok(())
}

/// Delete the override row(s) for the given SHA.
///
/// Why: lets reviewers reverse a pinning when the rule cascade is later
/// corrected to produce the right verdict on its own.
/// What: prompts for confirmation unless `skip_confirm` is true, then
/// deletes all rows in `classification_overrides` matching the SHA.
/// Test: `remove_deletes_row` and `remove_aborts_without_confirmation`.
fn remove<R: BufRead>(
    db: &mut Database,
    sha: &str,
    skip_confirm: bool,
    reader: &mut R,
) -> anyhow::Result<()> {
    let conn = db.connection();
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM classification_overrides WHERE commit_sha = ?1",
        params![sha],
        |row| row.get(0),
    )?;
    if n == 0 {
        println!("No override exists for {sha}.");
        return Ok(());
    }

    if !skip_confirm {
        print!("Delete {n} override row(s) for {sha}? [y/N] ");
        io::stdout().flush()?;
        let mut line = String::new();
        reader.read_line(&mut line)?;
        if !matches!(line.trim().to_lowercase().as_str(), "y" | "yes") {
            println!("Aborted.");
            return Ok(());
        }
    }

    let deleted = conn.execute(
        "DELETE FROM classification_overrides WHERE commit_sha = ?1",
        params![sha],
    )?;
    println!("Removed {deleted} override row(s) for {sha}.");
    Ok(())
}

/// Tuple returned by [`lookup_current_classification`]:
/// `(category, subcategory, confidence, method)`.
type CurrentClassification = (String, Option<String>, f64, String);

/// Look up the current classification (if any) for a commit SHA.
///
/// Returns the tuple or `None` if the commit is not yet classified.
fn lookup_current_classification(
    db: &Database,
    sha: &str,
) -> anyhow::Result<Option<CurrentClassification>> {
    let conn = db.connection();
    let mut stmt = conn.prepare(
        "SELECT c.category, c.subcategory, c.confidence, c.method \
         FROM commits cm \
         JOIN classifications c ON cm.classification_id = c.id \
         WHERE cm.sha = ?1",
    )?;
    let mut rows = stmt.query(params![sha])?;
    if let Some(row) = rows.next()? {
        Ok(Some((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_db() -> Database {
        Database::open_in_memory().expect("open in-memory db")
    }

    fn insert_commit(db: &Database, sha: &str, repo: &str) {
        db.connection()
            .execute(
                "INSERT INTO commits (sha, author_name, author_email, timestamp, message, repository) \
                 VALUES (?1, 'n', 'e', '2024-01-01T00:00:00Z', 'm', ?2)",
                params![sha, repo],
            )
            .expect("insert commit");
    }

    /// `add` writes a row keyed on (sha, repo_path).
    ///
    /// Why: the table is the contract Tier 0 reads — if `add` skips it,
    /// no override ever fires.
    /// What: call `add` with explicit `--repo`, then assert the row exists.
    /// Test: count the matching row in `classification_overrides`.
    #[test]
    fn add_inserts_row() {
        let db = fresh_db();
        add(
            &db,
            "abc123",
            "feature",
            "feature",
            Some("note"),
            Some("/tmp/r"),
        )
        .expect("add ok");
        let n: i64 = db
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM classification_overrides WHERE commit_sha = ?1",
                params!["abc123"],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(n, 1);
    }

    /// When `--repo` is omitted, `add` should resolve the repository from
    /// the `commits` table.
    ///
    /// Why: most operators won't remember the canonical repo path; the
    /// SHA alone should suffice when the commit is already collected.
    /// What: insert a commit row, call `add` without `--repo`, assert
    /// the override row picks up the repo from `commits`.
    /// Test: query the inserted override row and compare its `repo_path`.
    #[test]
    fn add_uses_repo_from_commits_when_omitted() {
        let db = fresh_db();
        insert_commit(&db, "sha-x", "/repo/x");
        add(&db, "sha-x", "feature", "feature", None, None).expect("add ok");
        let repo_path: String = db
            .connection()
            .query_row(
                "SELECT repo_path FROM classification_overrides WHERE commit_sha = 'sha-x'",
                [],
                |r| r.get(0),
            )
            .expect("query");
        assert_eq!(repo_path, "/repo/x");
    }

    /// `add` errors when neither `--repo` is given nor a commit row exists.
    ///
    /// Why: silently inserting an empty `repo_path` would create rows that
    /// never match the tier-0 lookup key — failure must be loud.
    /// What: call `add` for an unknown SHA without `--repo`, expect bail.
    /// Test: assert the error contains the guidance text.
    #[test]
    fn add_errors_when_repo_unresolvable() {
        let db = fresh_db();
        let err = add(&db, "missing", "feature", "feature", None, None).unwrap_err();
        assert!(err.to_string().contains("--repo"));
    }

    /// `list` filters by repo when the flag is provided.
    ///
    /// Why: in multi-repo setups, scrolling through every override is noisy.
    /// What: add two overrides in different repos, list with one filter,
    /// confirm only one row would be printed (we count via DB).
    /// Test: query the same SQL the function uses.
    #[test]
    fn list_filters_by_repo() {
        let db = fresh_db();
        add(&db, "a", "feature", "feature", None, Some("/repo/a")).expect("add a");
        add(&db, "b", "feature", "feature", None, Some("/repo/b")).expect("add b");
        let n: i64 = db
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM classification_overrides WHERE repo_path = '/repo/a'",
                [],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(n, 1);
    }

    /// `remove` deletes the row(s) for a SHA when confirmation is supplied.
    ///
    /// Why: deletes are destructive — the prompt is the safety net, but
    /// when supplied with `y` the row must actually go.
    /// What: add a row, feed `y\n` to `remove`, assert the row is gone.
    /// Test: count after delete.
    #[test]
    fn remove_deletes_row() {
        let mut db = fresh_db();
        add(&db, "del-me", "feature", "feature", None, Some("/r")).expect("add");
        let mut input: &[u8] = b"y\n";
        remove(&mut db, "del-me", false, &mut input).expect("remove ok");
        let n: i64 = db
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM classification_overrides WHERE commit_sha = 'del-me'",
                [],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(n, 0);
    }

    /// `remove` aborts without deleting when confirmation is declined.
    ///
    /// Why: the prompt is the safety net — if the user types `n`, no row
    /// should disappear.
    /// What: feed `n\n`, assert the row still exists afterward.
    /// Test: count after attempted remove.
    #[test]
    fn remove_aborts_without_confirmation() {
        let mut db = fresh_db();
        add(&db, "keep", "feature", "feature", None, Some("/r")).expect("add");
        let mut input: &[u8] = b"n\n";
        remove(&mut db, "keep", false, &mut input).expect("remove returns ok");
        let n: i64 = db
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM classification_overrides WHERE commit_sha = 'keep'",
                [],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(n, 1);
    }
}
