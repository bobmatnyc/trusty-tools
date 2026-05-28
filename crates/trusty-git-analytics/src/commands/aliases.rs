//! `tga aliases` — list and merge developer identities.
//!
//! Backed by the `authors` table (one row per canonical identity) and the
//! `commits.author_id` foreign key. Merging two identities reassigns all
//! commits from the source identity to the destination and deletes the
//! source row, so subsequent runs of any pipeline stage will see a single
//! consolidated identity.

use std::io::{self, BufRead, Write};

use clap::{Args, Subcommand};
use rusqlite::params;
use tga::core::config::Config;
use tga::core::db::Database;

/// Arguments for `tga aliases`.
#[derive(Args, Debug)]
#[command(
    about = "List, merge, or manage developer identity aliases.",
    long_about = "Manage the identity resolution table that maps commit author metadata\n\
(name + email combinations) to a single canonical identity per engineer.\n\n\
Merging two identities reassigns all commits from the source to the destination\n\
and deletes the source row. Subsequent pipeline stages and reports will see only\n\
the consolidated identity.\n\n\
tga automatically resolves identity during collection using fuzzy matching.\n\
Use these subcommands for manual corrections and audits.",
    after_help = "EXAMPLES:\n\
  # List all canonical identities in the database\n\
  tga aliases list\n\n\
  # Merge a work email into the canonical personal-account identity\n\
  tga aliases merge old@contractor.com alice@company.com\n\n\
  # Merge without the confirmation prompt\n\
  tga aliases merge old@example.com alice@example.com --yes\n\n\
TIPS:\n\
  - Run `tga report --author alice@company.com` to verify the merge result.\n\
  - Use the canonical email from `tga aliases list` as the --author value."
)]
pub struct AliasesArgs {
    /// Aliases subcommand.
    #[command(subcommand)]
    pub subcommand: AliasesSubcommand,
}

/// Valid provider identifiers for `tga aliases add-login`.
const VALID_PROVIDERS: &[&str] = &["github", "gitlab", "ado", "bitbucket"];

/// `tga aliases` subcommands.
#[derive(Subcommand, Debug)]
pub enum AliasesSubcommand {
    /// Print every canonical identity known to the database.
    List,
    /// Merge two canonical identities by email.
    Merge {
        /// Source identity email (will be removed after merge).
        src: String,
        /// Destination identity email (commits will be reassigned to this).
        dst: String,
        /// Skip the confirmation prompt.
        #[arg(long, default_value_t = false)]
        yes: bool,
    },
    /// Append a provider login to an author's alias list for PR canonicalization.
    ///
    /// Provider logins (e.g. GitHub usernames) are stored in `authors.aliases`
    /// alongside email aliases so that `tga author <email>` can resolve
    /// pull-request authorship across providers without a schema migration.
    ///
    /// Example:
    ///   tga aliases add-login alice@example.com github alice-dev
    AddLogin {
        /// Canonical email of the author to update.
        email: String,
        /// Provider the login belongs to (github, gitlab, ado, bitbucket).
        provider: String,
        /// Provider login / username to append.
        login: String,
    },
}

/// Dispatch entry point for the `tga aliases` subcommand.
///
/// # Errors
///
/// Returns any database or I/O error raised by the underlying operation.
pub fn run(_config: Config, db: &mut Database, args: AliasesArgs) -> anyhow::Result<()> {
    match args.subcommand {
        AliasesSubcommand::List => list(db),
        AliasesSubcommand::Merge { src, dst, yes } => {
            merge(db, &src, &dst, yes, &mut io::stdin().lock())
        }
        AliasesSubcommand::AddLogin {
            email,
            provider,
            login,
        } => add_login(db, &email, &provider, &login),
    }
}

/// List all canonical identities and any stored aliases for each.
fn list(db: &Database) -> anyhow::Result<()> {
    let conn = db.connection();
    let mut stmt = conn.prepare(
        "SELECT canonical_name, canonical_email, aliases FROM authors \
         ORDER BY canonical_email",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;

    let mut count = 0usize;
    println!(
        "{:<32}  {:<48}  aliases",
        "canonical_name", "canonical_email"
    );
    println!("{}", "-".repeat(96));
    for r in rows {
        let (name, email, aliases) = r?;
        let parsed: Vec<String> = serde_json::from_str(&aliases).unwrap_or_default();
        let alias_str = if parsed.is_empty() {
            "-".to_string()
        } else {
            parsed.join(", ")
        };
        println!("{:<32}  {:<48}  {}", name, email, alias_str);
        count += 1;
    }
    if count == 0 {
        println!("(no authors found — run `tga collect` first)");
    }
    Ok(())
}

/// Merge `src_email` into `dst_email`.
///
/// Both identities must already exist in the `authors` table. After
/// confirmation, all commits pointing at the source author are reassigned
/// to the destination, the source's email is appended to the destination's
/// alias list, and the source row is deleted.
fn merge<R: BufRead>(
    db: &mut Database,
    src_email: &str,
    dst_email: &str,
    skip_confirm: bool,
    reader: &mut R,
) -> anyhow::Result<()> {
    if src_email == dst_email {
        anyhow::bail!("source and destination emails are identical: {src_email}");
    }

    let (src_id, src_name, src_aliases_json) = lookup_author(db, src_email)?
        .ok_or_else(|| anyhow::anyhow!("source identity not found: {src_email}"))?;
    let (dst_id, _dst_name, dst_aliases_json) = lookup_author(db, dst_email)?
        .ok_or_else(|| anyhow::anyhow!("destination identity not found: {dst_email}"))?;

    if !skip_confirm {
        print!(
            "Merge {src_name} <{src_email}> (id={src_id}) into <{dst_email}> (id={dst_id})? [y/N] "
        );
        io::stdout().flush()?;
        let mut line = String::new();
        reader.read_line(&mut line)?;
        if !matches!(line.trim().to_lowercase().as_str(), "y" | "yes") {
            println!("Aborted.");
            return Ok(());
        }
    }

    // Merge alias arrays (best-effort JSON merge).
    let mut src_aliases: Vec<String> = serde_json::from_str(&src_aliases_json).unwrap_or_default();
    let mut dst_aliases: Vec<String> = serde_json::from_str(&dst_aliases_json).unwrap_or_default();
    dst_aliases.append(&mut src_aliases);
    dst_aliases.push(src_email.to_string());
    dst_aliases.sort();
    dst_aliases.dedup();
    let merged_aliases = serde_json::to_string(&dst_aliases)?;

    let conn = db.connection_mut();
    let tx = conn.transaction()?;
    let n_commits = tx.execute(
        "UPDATE commits SET author_id = ?1 WHERE author_id = ?2",
        params![dst_id, src_id],
    )?;
    tx.execute(
        "UPDATE authors SET aliases = ?1 WHERE id = ?2",
        params![merged_aliases, dst_id],
    )?;
    tx.execute("DELETE FROM authors WHERE id = ?1", params![src_id])?;
    tx.commit()?;

    println!(
        "Merged {src_email} → {dst_email} (reassigned {n_commits} commits, source author row deleted)"
    );
    Ok(())
}

/// Append a provider login to `authors.aliases` for the named identity.
///
/// Why: `tga author <email>` resolves PR authorship by looking up provider
/// logins stored in `authors.aliases`; this subcommand is the one-time setup
/// operation that populates those mappings without requiring a schema migration.
/// What: parses the existing `aliases` JSON array, appends `login` (if not
/// already present), writes back. Validates provider against the allow-list.
/// Test: see `add_login_round_trips` and `add_login_deduplicates` below.
fn add_login(db: &mut Database, email: &str, provider: &str, login: &str) -> anyhow::Result<()> {
    if !VALID_PROVIDERS.contains(&provider) {
        anyhow::bail!(
            "invalid provider '{provider}'; must be one of: {}",
            VALID_PROVIDERS.join(", ")
        );
    }
    if login.is_empty() {
        anyhow::bail!("login must not be empty");
    }
    if login.contains('@') {
        anyhow::bail!(
            "login '{login}' looks like an email address; provider logins must not contain '@'"
        );
    }

    let (id, _name, aliases_json) = lookup_author(db, email)?
        .ok_or_else(|| anyhow::anyhow!("identity not found: {email}. Run `tga aliases list`."))?;

    let mut aliases: Vec<String> = serde_json::from_str(&aliases_json).unwrap_or_default();
    if aliases.contains(&login.to_string()) {
        println!("Login '{login}' already present in aliases for {email}. No change.");
        return Ok(());
    }
    aliases.push(login.to_string());
    aliases.sort();
    let updated_json = serde_json::to_string(&aliases)?;

    let conn = db.connection_mut();
    conn.execute(
        "UPDATE authors SET aliases = ?1 WHERE id = ?2",
        params![updated_json, id],
    )?;
    println!("Added login '{login}' ({provider}) to {email}");
    Ok(())
}

/// Fetch `(id, canonical_name, aliases_json)` for the row whose email matches.
fn lookup_author(db: &Database, email: &str) -> anyhow::Result<Option<(i64, String, String)>> {
    let conn = db.connection();
    let mut stmt =
        conn.prepare("SELECT id, canonical_name, aliases FROM authors WHERE canonical_email = ?1")?;
    let mut rows = stmt.query(params![email])?;
    if let Some(row) = rows.next()? {
        Ok(Some((row.get(0)?, row.get(1)?, row.get(2)?)))
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn insert_author(db: &Database, name: &str, email: &str) -> i64 {
        db.connection()
            .execute(
                "INSERT INTO authors (canonical_name, canonical_email, aliases) VALUES (?1, ?2, '[]')",
                params![name, email],
            )
            .expect("insert");
        db.connection().last_insert_rowid()
    }

    fn insert_commit(db: &Database, sha: &str, author_id: i64) {
        db.connection()
            .execute(
                "INSERT INTO commits (sha, author_id, author_name, author_email, timestamp, \
                 message, repository) VALUES (?1, ?2, 'n', 'e', '2024-01-01T00:00:00Z', 'm', 'r')",
                params![sha, author_id],
            )
            .expect("insert commit");
    }

    #[test]
    fn merge_moves_commits_and_deletes_source() {
        let mut db = Database::open_in_memory().expect("open");
        let src = insert_author(&db, "Alice", "old@example.com");
        let dst = insert_author(&db, "Alice", "new@example.com");
        insert_commit(&db, "sha1", src);
        insert_commit(&db, "sha2", src);
        insert_commit(&db, "sha3", dst);

        let mut input: &[u8] = b"y\n";
        merge(
            &mut db,
            "old@example.com",
            "new@example.com",
            false,
            &mut input,
        )
        .expect("merge ok");

        // Source row should be gone.
        let src_exists: i64 = db
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM authors WHERE canonical_email = 'old@example.com'",
                [],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(src_exists, 0);

        // All three commits should now belong to dst.
        let n: i64 = db
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM commits WHERE author_id = ?1",
                params![dst],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(n, 3);
    }

    #[test]
    fn merge_rejects_identical_emails() {
        let mut db = Database::open_in_memory().expect("open");
        insert_author(&db, "A", "a@example.com");
        let mut input: &[u8] = b"y\n";
        let err = merge(&mut db, "a@example.com", "a@example.com", true, &mut input).unwrap_err();
        assert!(err.to_string().contains("identical"));
    }

    #[test]
    fn add_login_round_trips() {
        // Why: verifies that a login is stored and readable back from aliases JSON.
        let mut db = Database::open_in_memory().expect("open");
        insert_author(&db, "Alice", "alice@example.com");
        add_login(&mut db, "alice@example.com", "github", "alice-dev").expect("add login");

        let aliases_json: String = db
            .connection()
            .query_row(
                "SELECT aliases FROM authors WHERE canonical_email = 'alice@example.com'",
                [],
                |r| r.get(0),
            )
            .expect("fetch");
        let aliases: Vec<String> = serde_json::from_str(&aliases_json).unwrap();
        assert!(aliases.contains(&"alice-dev".to_string()));
    }

    #[test]
    fn add_login_deduplicates() {
        // Why: calling add-login twice with the same login must be idempotent.
        let mut db = Database::open_in_memory().expect("open");
        insert_author(&db, "Alice", "alice@example.com");
        add_login(&mut db, "alice@example.com", "github", "alice-dev").expect("first");
        add_login(&mut db, "alice@example.com", "github", "alice-dev").expect("second");

        let aliases_json: String = db
            .connection()
            .query_row(
                "SELECT aliases FROM authors WHERE canonical_email = 'alice@example.com'",
                [],
                |r| r.get(0),
            )
            .expect("fetch");
        let aliases: Vec<String> = serde_json::from_str(&aliases_json).unwrap();
        let count = aliases.iter().filter(|a| a.as_str() == "alice-dev").count();
        assert_eq!(count, 1, "duplicate login must not be stored");
    }

    #[test]
    fn add_login_rejects_invalid_provider() {
        // Why: only known provider identifiers should be accepted.
        let mut db = Database::open_in_memory().expect("open");
        insert_author(&db, "Alice", "alice@example.com");
        let err = add_login(&mut db, "alice@example.com", "slack", "alice-dev").unwrap_err();
        assert!(err.to_string().contains("invalid provider"));
    }

    #[test]
    fn add_login_rejects_email_as_login() {
        // Why: logins must be provider handles, not email addresses; the '@' check
        // guards against accidentally storing an email in the login slot.
        let mut db = Database::open_in_memory().expect("open");
        insert_author(&db, "Alice", "alice@example.com");
        let err =
            add_login(&mut db, "alice@example.com", "github", "alice@example.com").unwrap_err();
        assert!(err.to_string().contains("looks like an email"));
    }

    #[test]
    fn merge_errors_when_source_missing() {
        let mut db = Database::open_in_memory().expect("open");
        insert_author(&db, "B", "b@example.com");
        let mut input: &[u8] = b"y\n";
        let err = merge(
            &mut db,
            "missing@example.com",
            "b@example.com",
            true,
            &mut input,
        )
        .unwrap_err();
        assert!(err.to_string().contains("source identity not found"));
    }
}
