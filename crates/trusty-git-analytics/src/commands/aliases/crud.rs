//! CRUD subcommands for `tga aliases` (issue #348).
//!
//! Covers: `add`, `remove-alias`, `show`, `unmerge`, `rename`. Together with
//! the `list` / `merge` / `add-login` / `suggest` commands defined in
//! `mod.rs` and `suggest.rs`, these provide complete lifecycle control over
//! the identity table without dropping into raw SQL.

use rusqlite::params;
use tga::core::db::Database;

use super::lookup_author;

/// Create a new canonical identity from scratch.
///
/// Why: operators occasionally need to seed an identity that has not yet
/// appeared in any commit (e.g. registering a contractor in advance, or
/// pre-populating the table from a directory export). Going through the
/// regular `tga collect` upsert path requires a real commit.
/// What: inserts a row in `authors` with the given `email` as canonical and
/// `name` (or the email local-part if `None`) as display name; appends any
/// provided alias emails to the JSON `aliases` column.
/// Test: see `tests::add_creates_identity_with_aliases`.
pub(super) fn add(
    db: &mut Database,
    email: &str,
    name: Option<&str>,
    aliases: &[String],
) -> anyhow::Result<()> {
    if email.is_empty() {
        anyhow::bail!("canonical email must not be empty");
    }
    if !email.contains('@') {
        anyhow::bail!("canonical email '{email}' must contain '@'");
    }
    if let Some(existing) = lookup_author(db, email)? {
        anyhow::bail!(
            "identity already exists: {email} (id={}). Use `tga aliases show {email}` to inspect, \
             or `tga aliases rename {email} --name <new>` to update the display name.",
            existing.0
        );
    }
    let display_name = name
        .map(str::to_string)
        .unwrap_or_else(|| email.split('@').next().unwrap_or(email).to_string());

    // De-duplicate aliases at insertion time so the round-trip is idempotent.
    let mut sorted: Vec<String> = aliases.to_vec();
    sorted.sort();
    sorted.dedup();
    let aliases_json = serde_json::to_string(&sorted)?;

    let conn = db.connection_mut();
    conn.execute(
        "INSERT INTO authors (canonical_name, canonical_email, aliases) VALUES (?1, ?2, ?3)",
        params![display_name, email, aliases_json],
    )?;
    let id = conn.last_insert_rowid();
    println!("Added identity {display_name} <{email}> (id={id})");
    if !sorted.is_empty() {
        println!("  aliases: {}", sorted.join(", "));
    }
    Ok(())
}

/// Remove a single alias from an identity without deleting the canonical row.
///
/// Why: previously the only way to "undo" registering an alias was to merge
/// the two identities back apart, which destroys commit history continuity.
/// This is the focused, non-destructive inverse of `aliases add --alias <e>`
/// or the implicit alias append done during `aliases merge`.
/// What: loads the destination identity by `canonical`, removes `alias`
/// (case-insensitive match) from the JSON array, and writes it back. Errors
/// when either side is missing or when the alias is not present on the row.
/// Test: see `tests::remove_alias_drops_entry_only`.
pub(super) fn remove_alias(db: &mut Database, alias: &str, canonical: &str) -> anyhow::Result<()> {
    let (id, _name, aliases_json) = lookup_author(db, canonical)?
        .ok_or_else(|| anyhow::anyhow!("canonical identity not found: {canonical}"))?;
    let mut aliases: Vec<String> = serde_json::from_str(&aliases_json).unwrap_or_default();
    let before = aliases.len();
    let alias_lc = alias.to_lowercase();
    aliases.retain(|a| a.to_lowercase() != alias_lc);
    if aliases.len() == before {
        anyhow::bail!("alias '{alias}' is not present on identity {canonical}");
    }
    let updated = serde_json::to_string(&aliases)?;
    db.connection_mut().execute(
        "UPDATE authors SET aliases = ?1 WHERE id = ?2",
        params![updated, id],
    )?;
    println!("Removed alias '{alias}' from {canonical} (id={id})");
    Ok(())
}

/// Show full profile for one canonical identity.
///
/// Why: `tga aliases list` truncates aliases to one line per row; operators
/// auditing a single identity want a richer view with commit stats and the
/// full alias list.
/// What: prints canonical email, display name, alias list (one per line),
/// commit count, first/last commit timestamps, and PR count when the
/// `pull_requests` table exists.
/// Test: see `tests::show_emits_expected_fields`.
pub(super) fn show(db: &Database, email: &str) -> anyhow::Result<()> {
    let (id, name, aliases_json) =
        lookup_author(db, email)?.ok_or_else(|| anyhow::anyhow!("identity not found: {email}"))?;
    let aliases: Vec<String> = serde_json::from_str(&aliases_json).unwrap_or_default();

    let conn = db.connection();
    let (commit_count, first_commit, last_commit): (i64, Option<String>, Option<String>) = conn
        .query_row(
            "SELECT COUNT(*), MIN(timestamp), MAX(timestamp) \
             FROM commits WHERE author_id = ?1",
            params![id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .unwrap_or((0, None, None));

    // PR count is best-effort: the table may not exist on a fresh DB. We
    // detect via sqlite_master rather than relying on a migration version
    // because this command should work on any schema that contains `authors`.
    let pr_count: Option<i64> = if conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='pull_requests' LIMIT 1",
            [],
            |row| row.get::<_, i64>(0),
        )
        .is_ok()
    {
        conn.query_row(
            "SELECT COUNT(*) FROM pull_requests WHERE author_id = ?1",
            params![id],
            |row| row.get::<_, i64>(0),
        )
        .ok()
    } else {
        None
    };

    println!("Identity profile");
    println!("  id:              {id}");
    println!("  canonical_name:  {name}");
    println!("  canonical_email: {email}");
    if aliases.is_empty() {
        println!("  aliases:         -");
    } else {
        println!("  aliases:");
        for a in &aliases {
            println!("    - {a}");
        }
    }
    println!("  commits:         {commit_count}");
    if let Some(ts) = first_commit {
        println!("  first_commit:    {ts}");
    }
    if let Some(ts) = last_commit {
        println!("  last_commit:     {ts}");
    }
    if let Some(n) = pr_count {
        println!("  pull_requests:   {n}");
    }
    Ok(())
}

/// Undo a prior merge — detach an alias back to its own identity row.
///
/// Why: `aliases merge` is destructive — the source row is deleted and its
/// commits reassigned. Recovering the original identity (so commits authored
/// by the alias can be re-segmented) previously required hand-written SQL.
/// What: locates the canonical row whose `aliases` array contains `alias`,
/// removes the entry, and inserts a fresh row keyed on `alias` (display name
/// copied from the canonical row so the operator can `rename` afterwards).
/// Commits are NOT reassigned automatically — the alias becomes a fresh,
/// empty identity row that future commits will route to via the normal
/// resolver path.
/// Test: see `tests::unmerge_detaches_alias_into_its_own_row`.
pub(super) fn unmerge(db: &mut Database, alias: &str) -> anyhow::Result<()> {
    let conn = db.connection();
    // Find the canonical row whose aliases JSON array contains this alias.
    let mut stmt = conn.prepare(
        "SELECT id, canonical_name, canonical_email, aliases FROM authors \
         WHERE aliases LIKE ?1",
    )?;
    let pattern = format!("%\"{}\"%", alias);
    let mut rows = stmt.query(params![pattern])?;
    let mut found: Option<(i64, String, String, Vec<String>)> = None;
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let name: String = row.get(1)?;
        let canon: String = row.get(2)?;
        let aliases_json: String = row.get(3)?;
        let aliases: Vec<String> = serde_json::from_str(&aliases_json).unwrap_or_default();
        // LIKE may produce false positives if an alias is a substring of
        // another; require an exact (case-insensitive) match in the parsed
        // array before committing to a target row.
        let alias_lc = alias.to_lowercase();
        if aliases.iter().any(|a| a.to_lowercase() == alias_lc) {
            found = Some((id, name, canon, aliases));
            break;
        }
    }
    drop(rows);
    drop(stmt);

    let (id, name, canon, mut aliases) =
        found.ok_or_else(|| anyhow::anyhow!("alias '{alias}' not found on any identity"))?;
    if canon.eq_ignore_ascii_case(alias) {
        anyhow::bail!(
            "alias '{alias}' equals the canonical email of its own identity; nothing to unmerge"
        );
    }

    // Drop the alias from the source row's array.
    let alias_lc = alias.to_lowercase();
    aliases.retain(|a| a.to_lowercase() != alias_lc);
    let updated = serde_json::to_string(&aliases)?;

    // Insert the alias as its own identity. If a row already exists at this
    // email, fail rather than silently overwriting (the operator should
    // resolve the collision manually).
    if lookup_author(db, alias)?.is_some() {
        anyhow::bail!(
            "an identity already exists at {alias}; manual resolution required \
             (consider `tga aliases merge {alias} {canon}` to re-collapse)"
        );
    }

    let conn = db.connection_mut();
    let tx = conn.transaction()?;
    tx.execute(
        "UPDATE authors SET aliases = ?1 WHERE id = ?2",
        params![updated, id],
    )?;
    tx.execute(
        "INSERT INTO authors (canonical_name, canonical_email, aliases) VALUES (?1, ?2, '[]')",
        params![name, alias],
    )?;
    let new_id = tx.last_insert_rowid();
    tx.commit()?;

    println!(
        "Detached alias '{alias}' from {canon} (id={id}) into new identity id={new_id}. \
         Commits remain attached to {canon} — reassign manually if needed."
    );
    Ok(())
}

/// Update the display name of an identity without touching email/aliases.
///
/// Why: display names drift over time (people get married, change preferred
/// presentation, etc.) and the existing path required raw SQL.
/// What: updates `canonical_name` for the row keyed on `email`.
/// Test: see `tests::rename_updates_canonical_name`.
pub(super) fn rename(db: &mut Database, email: &str, new_name: &str) -> anyhow::Result<()> {
    if new_name.trim().is_empty() {
        anyhow::bail!("new name must not be empty");
    }
    let (id, _old, _aliases) =
        lookup_author(db, email)?.ok_or_else(|| anyhow::anyhow!("identity not found: {email}"))?;
    db.connection_mut().execute(
        "UPDATE authors SET canonical_name = ?1 WHERE id = ?2",
        params![new_name, id],
    )?;
    println!("Renamed {email} to '{new_name}' (id={id})");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::aliases::tests::{insert_author, insert_commit};

    #[test]
    fn add_creates_identity_with_aliases() {
        let mut db = Database::open_in_memory().expect("open");
        let alias_emails = vec!["bob.work@example.com".to_string()];
        add(&mut db, "bob@example.com", Some("Bob"), &alias_emails).expect("add");
        let (id, name, aliases_json) = lookup_author(&db, "bob@example.com")
            .expect("lookup")
            .expect("present");
        assert!(id > 0);
        assert_eq!(name, "Bob");
        let aliases: Vec<String> = serde_json::from_str(&aliases_json).unwrap();
        assert_eq!(aliases, vec!["bob.work@example.com".to_string()]);
    }

    #[test]
    fn add_rejects_duplicate_canonical() {
        let mut db = Database::open_in_memory().expect("open");
        add(&mut db, "x@example.com", None, &[]).expect("first");
        let err = add(&mut db, "x@example.com", None, &[]).unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn add_rejects_invalid_email() {
        let mut db = Database::open_in_memory().expect("open");
        let err = add(&mut db, "no-at-symbol", None, &[]).unwrap_err();
        assert!(err.to_string().contains("must contain '@'"));
    }

    #[test]
    fn add_defaults_name_to_local_part() {
        let mut db = Database::open_in_memory().expect("open");
        add(&mut db, "alice@x.com", None, &[]).expect("add");
        let (_, name, _) = lookup_author(&db, "alice@x.com")
            .expect("lookup")
            .expect("present");
        assert_eq!(name, "alice");
    }

    #[test]
    fn remove_alias_drops_entry_only() {
        let mut db = Database::open_in_memory().expect("open");
        let id = insert_author(&db, "Bob", "bob@example.com");
        db.connection_mut()
            .execute(
                "UPDATE authors SET aliases = ?1 WHERE id = ?2",
                params![r#"["bob.alt@example.com","bob-cli"]"#, id],
            )
            .expect("seed");
        remove_alias(&mut db, "bob.alt@example.com", "bob@example.com").expect("remove");
        let (_, _, aliases_json) = lookup_author(&db, "bob@example.com")
            .expect("lookup")
            .expect("present");
        let aliases: Vec<String> = serde_json::from_str(&aliases_json).unwrap();
        assert_eq!(aliases, vec!["bob-cli".to_string()]);
    }

    #[test]
    fn remove_alias_errors_when_missing() {
        let mut db = Database::open_in_memory().expect("open");
        insert_author(&db, "Bob", "bob@example.com");
        let err = remove_alias(&mut db, "nope@example.com", "bob@example.com").unwrap_err();
        assert!(err.to_string().contains("not present"));
    }

    #[test]
    fn show_emits_expected_fields() {
        // We can't easily capture stdout in a sync test without extra deps;
        // confirm the SELECTs run without error against a populated row.
        let db = Database::open_in_memory().expect("open");
        let id = insert_author(&db, "Bob", "bob@example.com");
        insert_commit(&db, "sha1", id);
        insert_commit(&db, "sha2", id);
        show(&db, "bob@example.com").expect("show ok");
    }

    #[test]
    fn show_errors_on_missing_identity() {
        let db = Database::open_in_memory().expect("open");
        let err = show(&db, "missing@example.com").unwrap_err();
        assert!(err.to_string().contains("identity not found"));
    }

    #[test]
    fn unmerge_detaches_alias_into_its_own_row() {
        let mut db = Database::open_in_memory().expect("open");
        let id = insert_author(&db, "Bob", "bob@example.com");
        db.connection_mut()
            .execute(
                "UPDATE authors SET aliases = ?1 WHERE id = ?2",
                params![r#"["old@contractor.com","other"]"#, id],
            )
            .expect("seed");
        unmerge(&mut db, "old@contractor.com").expect("unmerge");

        // Original row no longer has the alias.
        let (_, _, aliases_json) = lookup_author(&db, "bob@example.com")
            .expect("lookup")
            .expect("present");
        let aliases: Vec<String> = serde_json::from_str(&aliases_json).unwrap();
        assert_eq!(aliases, vec!["other".to_string()]);

        // A new identity row exists at the alias email.
        let (_, name, _) = lookup_author(&db, "old@contractor.com")
            .expect("lookup")
            .expect("present");
        assert_eq!(name, "Bob"); // copied from source canonical_name.
    }

    #[test]
    fn unmerge_errors_when_collision() {
        let mut db = Database::open_in_memory().expect("open");
        let id = insert_author(&db, "Bob", "bob@example.com");
        insert_author(&db, "Other Bob", "old@contractor.com");
        db.connection_mut()
            .execute(
                "UPDATE authors SET aliases = ?1 WHERE id = ?2",
                params![r#"["old@contractor.com"]"#, id],
            )
            .expect("seed");
        let err = unmerge(&mut db, "old@contractor.com").unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn unmerge_errors_when_alias_absent() {
        let mut db = Database::open_in_memory().expect("open");
        insert_author(&db, "Bob", "bob@example.com");
        let err = unmerge(&mut db, "absent@nowhere.test").unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn rename_updates_canonical_name() {
        let mut db = Database::open_in_memory().expect("open");
        insert_author(&db, "Old Name", "x@example.com");
        rename(&mut db, "x@example.com", "New Name").expect("rename");
        let (_, name, _) = lookup_author(&db, "x@example.com")
            .expect("lookup")
            .expect("present");
        assert_eq!(name, "New Name");
    }

    #[test]
    fn rename_rejects_empty_name() {
        let mut db = Database::open_in_memory().expect("open");
        insert_author(&db, "Old", "x@example.com");
        let err = rename(&mut db, "x@example.com", "   ").unwrap_err();
        assert!(err.to_string().contains("must not be empty"));
    }
}
