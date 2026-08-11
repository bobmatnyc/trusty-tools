//! Contributor identity resolution against the tga database.
//!
//! Why: a caller naming a contributor supplies whatever they have — a GitHub
//! login, a display name, an old work email — while every downstream query
//! keys on `authors.canonical_email`. This module owns that translation so no
//! caller has to know the `authors` schema or the resolver's matching tiers.
//! What: [`ContributorSelector`] opens the database, queries `authors`
//! directly, and falls back to
//! [`IdentityResolver`](crate::collect::identity::IdentityResolver) fuzzy
//! matching. An unmatched query returns
//! [`ProfileError::ContributorNotFound`] carrying the `tga aliases list` hint.
//! Test: the `tests` module seeds an in-memory database with known authors and
//! aliases, then asserts exact, name, alias, and not-found resolution.

use tracing::{debug, warn};

use crate::collect::identity::IdentityResolver;
use crate::core::db::Database;

use super::error::{ProfileError, Result};

// ─── Resolved identity ───────────────────────────────────────────────────────

/// A contributor identity resolved against the `authors` table.
///
/// Why: the email keys every later query and the name is what the report
/// header prints, so resolution must return both rather than making the caller
/// look the other one up.
/// What: the two canonical columns of the matched `authors` row.
/// Test: asserted by every positive-path selector test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedIdentity {
    /// Canonical email, as stored in `authors.canonical_email`.
    pub canonical_email: String,
    /// Canonical display name, as stored in `authors.canonical_name`.
    pub canonical_name: String,
}

// ─── ContributorSelector ─────────────────────────────────────────────────────

/// Opens a tga database and resolves contributor identities against it.
///
/// Why: the resolver is expensive to build (it reads every author row), so a
/// profile run builds it once and reuses it for every lookup.
/// What: holds an open [`Database`] plus an [`IdentityResolver`] seeded from
/// the `authors` rows, so partial-name and old-email queries resolve the same
/// way `tga collect` resolves them.
/// Test: see the `tests` module below.
pub struct ContributorSelector {
    db: Database,
    resolver: IdentityResolver,
}

impl ContributorSelector {
    /// Open the tga database at `db_path` and build the resolver from it.
    ///
    /// Why: opening once per run keeps every later `resolve` call in memory.
    /// What: opens the database (the migration runner is idempotent, so
    /// opening an existing database is safe) and seeds the resolver from
    /// `authors`.
    /// Test: `tests::selector_resolves_canonical_email`.
    ///
    /// # Errors
    ///
    /// [`ProfileError::Db`] when the database cannot be opened or read.
    pub fn open(db_path: &std::path::Path) -> Result<Self> {
        let db = Database::open(db_path).map_err(ProfileError::Db)?;
        let resolver = build_resolver_from_db(&db)?;
        Ok(Self { db, resolver })
    }

    /// Open an in-memory database. Test-only.
    ///
    /// Why: the selector tests need to seed a known author set without touching
    /// the filesystem.
    /// What: opens `Database::open_in_memory()` and seeds the resolver from it.
    /// Test: used by every test in this module.
    ///
    /// # Errors
    ///
    /// [`ProfileError::Db`] on SQLite failure.
    #[cfg(test)]
    pub(crate) fn open_in_memory() -> Result<Self> {
        let db = Database::open_in_memory().map_err(ProfileError::Db)?;
        let resolver = build_resolver_from_db(&db)?;
        Ok(Self { db, resolver })
    }

    /// Borrow the underlying database.
    ///
    /// Why: batch assembly and diff sampling query the same database the
    /// selector already has open; handing out a borrow avoids a second
    /// connection and a second migration run.
    /// What: a shared borrow of the internal [`Database`].
    /// Test: used by `tests::selector_resolves_canonical_email` via the seed
    /// helper, and transitively by the batch and diff-sampler tests.
    pub fn database(&self) -> &Database {
        &self.db
    }

    /// Resolve a caller-supplied query to a canonical identity.
    ///
    /// Why: see the module doc — the caller has one of several identifiers and
    /// the pipeline needs exactly one.
    /// What: tries four strategies in order and returns the first hit —
    /// canonical email (case-insensitive), canonical name (case-insensitive),
    /// a substring hit in the `aliases` JSON column, then
    /// [`IdentityResolver`] fuzzy matching.
    /// Test: `tests::selector_resolves_canonical_email`,
    /// `tests::selector_resolves_by_name`, `tests::selector_resolves_via_alias`,
    /// `tests::selector_not_found_returns_error`.
    ///
    /// # Errors
    ///
    /// - [`ProfileError::ContributorNotFound`] when no strategy matches.
    /// - [`ProfileError::Db`] on SQLite failure.
    pub fn resolve(&self, query: &str) -> Result<ResolvedIdentity> {
        let query_lc = query.to_lowercase();
        debug!(query, "ContributorSelector::resolve");

        if let Some(id) = self.lookup_by_email(&query_lc)? {
            return Ok(id);
        }
        if let Some(id) = self.lookup_by_name(query)? {
            return Ok(id);
        }
        if let Some(id) = self.lookup_by_alias_json(query)? {
            return Ok(id);
        }

        // Fuzzy resolution: passing the query as both name and email triggers
        // every matching tier. A returned pair that differs from the input is
        // the resolver's signal that it matched something.
        let (resolved_name, resolved_email) = self.resolver.resolve(query, query);
        if resolved_email != query || resolved_name != query {
            debug!(
                resolved_name,
                resolved_email, "ContributorSelector: fuzzy match"
            );
            return Ok(ResolvedIdentity {
                canonical_email: resolved_email,
                canonical_name: resolved_name,
            });
        }

        warn!(query, "ContributorSelector: no identity found");
        Err(ProfileError::ContributorNotFound {
            query: query.to_string(),
        })
    }

    // ── Private helpers ───────────────────────────────────────────────────

    fn lookup_by_email(&self, email_lc: &str) -> Result<Option<ResolvedIdentity>> {
        self.query_one(
            "SELECT canonical_email, canonical_name \
             FROM authors WHERE LOWER(canonical_email) = ?1 LIMIT 1",
            email_lc,
        )
    }

    fn lookup_by_name(&self, name: &str) -> Result<Option<ResolvedIdentity>> {
        self.query_one(
            "SELECT canonical_email, canonical_name \
             FROM authors WHERE LOWER(canonical_name) = LOWER(?1) LIMIT 1",
            name,
        )
    }

    /// Search the `authors.aliases` JSON column for `query` as a substring.
    ///
    /// Why: GitHub logins and old emails live in the `aliases` JSON array, and
    /// a `LIKE` scan covers the common case without parsing every row's JSON.
    /// What: escapes `%` and `_` in the query, then matches the serialised
    /// array with `LIKE … ESCAPE '\'`.
    /// Test: `tests::selector_resolves_via_alias`.
    fn lookup_by_alias_json(&self, query: &str) -> Result<Option<ResolvedIdentity>> {
        let escaped = query.replace('%', "\\%").replace('_', "\\_");
        let pattern = format!("%{escaped}%");
        self.query_one(
            "SELECT canonical_email, canonical_name \
             FROM authors WHERE aliases LIKE ?1 ESCAPE '\\' LIMIT 1",
            &pattern,
        )
    }

    /// Run a one-parameter, one-row identity query.
    ///
    /// Why: the three lookup strategies differ only in their WHERE clause; one
    /// helper keeps the "no rows is not an error" handling in a single place.
    /// What: maps `QueryReturnedNoRows` to `Ok(None)` and any other rusqlite
    /// error to [`ProfileError::Db`].
    /// Test: exercised by every selector lookup test.
    fn query_one(&self, sql: &str, param: &str) -> Result<Option<ResolvedIdentity>> {
        let conn = self.db.connection();
        let result: rusqlite::Result<(String, String)> =
            conn.query_row(sql, [param], |row| Ok((row.get(0)?, row.get(1)?)));
        match result {
            Ok((canonical_email, canonical_name)) => Ok(Some(ResolvedIdentity {
                canonical_email,
                canonical_name,
            })),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(ProfileError::Db(crate::core::TgaError::from(e))),
        }
    }
}

/// Build an [`IdentityResolver`] seeded from the `authors` table.
///
/// Why: fuzzy matching has to see every known identity, and re-reading the
/// table per query would make resolution O(queries × authors).
/// What: reads `(canonical_name, canonical_email, aliases)` for every author,
/// puts the canonical email first in each alias list so the resolver picks it
/// as canonical, and hands the map to `IdentityResolver::from_alias_map`.
/// Test: exercised by every selector test.
fn build_resolver_from_db(db: &Database) -> Result<IdentityResolver> {
    use std::collections::HashMap;

    let conn = db.connection();
    let mut stmt = conn
        .prepare("SELECT canonical_name, canonical_email, aliases FROM authors")
        .map_err(|e| ProfileError::Db(crate::core::TgaError::from(e)))?;

    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|e| ProfileError::Db(crate::core::TgaError::from(e)))?;

    let mut alias_map: HashMap<String, Vec<String>> = HashMap::new();
    for r in rows {
        let (name, email, aliases_json) =
            r.map_err(|e| ProfileError::Db(crate::core::TgaError::from(e)))?;
        let mut aliases: Vec<String> = serde_json::from_str(&aliases_json).unwrap_or_default();
        if !email.is_empty() {
            aliases.insert(0, email);
        }
        alias_map.insert(name, aliases);
    }

    Ok(IdentityResolver::from_alias_map(&alias_map))
}

/// Resolve a contributor query against the database at `db_path`.
///
/// Why: a one-shot caller (a CLI invocation that resolves and exits) should not
/// have to keep a selector alive.
/// What: opens a selector, resolves, and drops the database on return.
/// Test: `tests::resolve_contributor_opens_and_resolves`.
///
/// # Errors
///
/// As [`ContributorSelector::resolve`], plus [`ProfileError::Db`] on open.
pub fn resolve_contributor(db_path: &std::path::Path, query: &str) -> Result<ResolvedIdentity> {
    let sel = ContributorSelector::open(db_path)?;
    sel.resolve(query)
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    fn seed_author(db: &Database, name: &str, email: &str, aliases_json: &str) {
        db.connection()
            .execute(
                "INSERT INTO authors (canonical_name, canonical_email, aliases) \
                 VALUES (?1, ?2, ?3)",
                params![name, email, aliases_json],
            )
            .expect("insert author");
    }

    fn make_selector_with_authors() -> ContributorSelector {
        let mut sel = ContributorSelector::open_in_memory().expect("open");
        seed_author(sel.database(), "Alice Smith", "alice@example.com", r#"[]"#);
        seed_author(
            sel.database(),
            "Bob Jones",
            "bob@example.com",
            r#"["bob-gh", "bob.jones@old.example.com"]"#,
        );
        // The resolver was built before the seed, so rebuild it.
        sel.resolver = build_resolver_from_db(sel.database()).expect("rebuild resolver");
        sel
    }

    /// Why: an exact canonical email is the cheapest and most common query, and
    /// it must never fall through to fuzzy matching.
    /// What: resolves a known canonical email and asserts both fields.
    /// Test: this test itself.
    #[test]
    fn selector_resolves_canonical_email() {
        let sel = make_selector_with_authors();
        let id = sel.resolve("alice@example.com").expect("resolve");
        assert_eq!(id.canonical_email, "alice@example.com");
        assert_eq!(id.canonical_name, "Alice Smith");
    }

    /// Why: people name colleagues by display name far more often than by
    /// canonical email, and casing is not something a caller should have to get
    /// right.
    /// What: resolves a display name and asserts the canonical pair.
    /// Test: this test itself.
    #[test]
    fn selector_resolves_by_name() {
        let sel = make_selector_with_authors();
        let id = sel.resolve("Bob Jones").expect("resolve by name");
        assert_eq!(id.canonical_email, "bob@example.com");
        assert_eq!(id.canonical_name, "Bob Jones");
    }

    /// Why: a GitHub login is stored only in the `aliases` JSON, so without the
    /// alias scan the most natural identifier a user has would not resolve.
    /// What: resolves `bob-gh`, which appears only inside Bob's alias array.
    /// Test: this test itself.
    #[test]
    fn selector_resolves_via_alias() {
        let sel = make_selector_with_authors();
        let id = sel.resolve("bob-gh").expect("resolve via alias");
        assert_eq!(id.canonical_email, "bob@example.com");
        assert_eq!(id.canonical_name, "Bob Jones");
    }

    /// Why: an unknown contributor is a user error, and the error has to tell
    /// the user how to find the right identifier rather than just failing.
    /// What: resolves an unknown email and asserts both the error variant and
    /// the `tga aliases list` hint in its message.
    /// Test: this test itself.
    #[test]
    fn selector_not_found_returns_error() {
        let sel = make_selector_with_authors();
        let err = sel.resolve("nobody@nowhere.test").expect_err("should fail");
        assert!(
            matches!(err, ProfileError::ContributorNotFound { .. }),
            "expected ContributorNotFound, got: {err:?}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains("tga aliases list"),
            "error message should mention 'tga aliases list': {msg}"
        );
    }

    /// Why: `resolve_contributor` is the one-shot path a CLI takes, and it has
    /// to behave identically to the long-lived selector.
    /// What: writes a seeded database to a temp file, resolves through the free
    /// function, and asserts the canonical pair.
    /// Test: this test itself.
    #[test]
    fn resolve_contributor_opens_and_resolves() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let db_path = tmp.path().join("tga.db");
        {
            let db = Database::open(&db_path).expect("open");
            seed_author(&db, "Carol King", "carol@example.com", r#"["carol-gh"]"#);
        }

        let id = resolve_contributor(&db_path, "carol@example.com").expect("resolve");
        assert_eq!(id.canonical_name, "Carol King");
    }
}
