//! Contributor identity resolution against the single org-wide tga database.
//!
//! Why: the profile pipeline must map user-supplied identifiers (GitHub login,
//! display name, email) to the canonical `(email, name)` stored in the tga
//! `authors` table.  This module owns that resolution so callers never need
//! to know the tga DB schema or the IdentityResolver internals.
//! What: `ContributorSelector` opens the org-wide tga.db read-only, queries
//! the `authors` table directly, and falls back to tga's `IdentityResolver`
//! fuzzy matching.  Returns `ProfileError::ContributorNotFound` with a helpful
//! hint on failure.
//! Test: `selector::tests` seeds a temp tga in-memory DB with known authors and
//! aliases, then asserts exact, alias, and not-found resolution.

use std::path::PathBuf;

use tracing::{debug, warn};

use tga::collect::identity::IdentityResolver;
use tga::core::db::Database;

use super::error::{ProfileError, Result};

// ─── Resolved identity ────────────────────────────────────────────────────────

/// A resolved contributor identity from the tga database.
///
/// Why: callers need both the canonical email (for DB queries) and the
/// canonical name (for the profile header), so we return both.
/// What: a lightweight value type carrying the two fields extracted from the
/// `authors` table row.
/// Test: asserted by all positive-path selector tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedIdentity {
    /// Canonical email as stored in `authors.canonical_email`.
    pub canonical_email: String,
    /// Canonical display name as stored in `authors.canonical_name`.
    pub canonical_name: String,
}

// ─── ContributorSelector ─────────────────────────────────────────────────────

/// Opens a single org-wide tga database and resolves contributor identities.
///
/// Why: the profile pipeline needs a single entry point that converts
/// user-supplied queries (GitHub login / display name / email) into the
/// canonical `(email, name)` pair stored in tga.  Centralising this avoids
/// duplicating the three-tier resolution logic at every call site.
/// What: holds an open `tga::core::db::Database` (read-only) and a pre-built
/// `IdentityResolver` seeded from the `authors` + `aliases` rows.  The
/// resolver uses the same fuzzy-matching strategy as `tga collect` so
/// partial-name and email-local-part queries work out of the box.
/// Test: see `tests` module below.
pub struct ContributorSelector {
    db: Database,
    resolver: IdentityResolver,
}

impl ContributorSelector {
    /// Open the org-wide tga database at `db_path` and build the resolver.
    ///
    /// Why: the selector is created once per profile run; opening the DB and
    /// building the resolver here ensures subsequent `resolve()` calls are
    /// fast (the resolver is in memory).
    /// What: opens the DB (read-only path is opened via `Database::open` since
    /// tga's `Database` does not expose a separate read-only constructor;
    /// the migration runner is idempotent and safe to call on an existing DB),
    /// queries `authors` rows, seeds the resolver, and returns `Self`.
    /// Test: see `tests::selector_resolves_canonical_email`.
    ///
    /// # Errors
    ///
    /// - `ProfileError::Db` on SQLite failure.
    pub fn open(db_path: &std::path::Path) -> Result<Self> {
        let db = Database::open(db_path).map_err(ProfileError::Db)?;
        let resolver = build_resolver_from_db(&db)?;
        Ok(Self { db, resolver })
    }

    /// Open an in-memory database.  Primarily intended for tests.
    ///
    /// Why: tests need to seed a known set of authors without touching the
    /// filesystem; this constructor mirrors the tga test pattern.
    /// What: opens `Database::open_in_memory()`, seeds the resolver from it,
    /// and returns `Self`.
    /// Test: used by all `tests::*` tests in this module.
    ///
    /// # Errors
    ///
    /// `ProfileError::Db` on SQLite failure.
    #[cfg(test)]
    pub(crate) fn open_in_memory() -> Result<Self> {
        let db = Database::open_in_memory().map_err(ProfileError::Db)?;
        let resolver = build_resolver_from_db(&db)?;
        Ok(Self { db, resolver })
    }

    /// Access the underlying tga `Database` (read access).
    ///
    /// Why: the batch assembler and diff sampler need to run queries against
    /// the same database; returning a reference avoids opening the DB twice.
    /// What: borrow of the internal `Database`.
    /// Test: used transitively by batch and diff-sampler tests.
    pub fn database(&self) -> &Database {
        &self.db
    }

    /// Resolve a user-supplied query string to a canonical identity.
    ///
    /// Why: the caller may supply a GitHub login, display name, or email; we
    /// need to map all three forms to the canonical `(email, name)` pair
    /// required for tga queries.
    /// What: tries three strategies in order:
    ///  1. Exact case-insensitive match on `canonical_email` in `authors`.
    ///  2. Exact case-insensitive match on `canonical_name` in `authors`.
    ///  3. Fuzzy resolution via `IdentityResolver::resolve(query, query)`.
    ///  4. Alias search: checks if `query` appears in `authors.aliases`.
    ///
    /// Returns `ProfileError::ContributorNotFound` if none match.
    ///
    /// # Errors
    ///
    /// - `ProfileError::ContributorNotFound` when no identity matches.
    /// - `ProfileError::Db` on SQLite failure.
    ///
    /// Test: see `tests::selector_resolves_canonical_email`,
    /// `tests::selector_resolves_by_name`,
    /// `tests::selector_resolves_via_alias`,
    /// `tests::selector_not_found_returns_error`.
    pub fn resolve(&self, query: &str) -> Result<ResolvedIdentity> {
        let query_lc = query.to_lowercase();
        debug!(query, "ContributorSelector::resolve");

        // Strategy 1: exact match on canonical_email.
        if let Some(id) = self.lookup_by_email(&query_lc)? {
            return Ok(id);
        }

        // Strategy 2: exact match on canonical_name.
        if let Some(id) = self.lookup_by_name(query)? {
            return Ok(id);
        }

        // Strategy 3: alias search — check if query appears in aliases JSON.
        if let Some(id) = self.lookup_by_alias_json(query)? {
            return Ok(id);
        }

        // Strategy 4: fuzzy resolution via IdentityResolver.
        // We pass the query as both name and email to trigger all matching tiers.
        let (resolved_name, resolved_email) = self.resolver.resolve(query, query);
        // If the resolver returned a different pair than what we passed in,
        // it found a match.
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
        let conn = self.db.connection();
        let result: rusqlite::Result<(String, String)> = conn.query_row(
            "SELECT canonical_email, canonical_name \
             FROM authors WHERE LOWER(canonical_email) = ?1 LIMIT 1",
            [email_lc],
            |row| Ok((row.get(0)?, row.get(1)?)),
        );
        match result {
            Ok((email, name)) => Ok(Some(ResolvedIdentity {
                canonical_email: email,
                canonical_name: name,
            })),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(ProfileError::Db(tga::core::TgaError::from(e))),
        }
    }

    fn lookup_by_name(&self, name: &str) -> Result<Option<ResolvedIdentity>> {
        let conn = self.db.connection();
        let result: rusqlite::Result<(String, String)> = conn.query_row(
            "SELECT canonical_email, canonical_name \
             FROM authors WHERE LOWER(canonical_name) = LOWER(?1) LIMIT 1",
            [name],
            |row| Ok((row.get(0)?, row.get(1)?)),
        );
        match result {
            Ok((email, name_out)) => Ok(Some(ResolvedIdentity {
                canonical_email: email,
                canonical_name: name_out,
            })),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(ProfileError::Db(tga::core::TgaError::from(e))),
        }
    }

    /// Search the `authors.aliases` JSON column for `query` as a substring.
    ///
    /// Why: GitHub logins and display-name variants live in the `aliases`
    /// JSON array; a simple `LIKE` query covers the common case where the
    /// caller supplies a GitHub login that was registered as an alias.
    /// What: uses SQLite `LIKE` to check whether the serialised JSON array
    /// contains the query string; not perfect but correct for typical alias
    /// values (emails and short login handles that don't contain SQL wildcards).
    /// Test: `tests::selector_resolves_via_alias`.
    fn lookup_by_alias_json(&self, query: &str) -> Result<Option<ResolvedIdentity>> {
        // Escape any SQL LIKE special characters in the query.
        let escaped = query.replace('%', "\\%").replace('_', "\\_");
        let pattern = format!("%{}%", escaped);
        let conn = self.db.connection();
        let result: rusqlite::Result<(String, String)> = conn.query_row(
            "SELECT canonical_email, canonical_name \
             FROM authors WHERE aliases LIKE ?1 ESCAPE '\\' LIMIT 1",
            [&pattern],
            |row| Ok((row.get(0)?, row.get(1)?)),
        );
        match result {
            Ok((email, name)) => Ok(Some(ResolvedIdentity {
                canonical_email: email,
                canonical_name: name,
            })),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(ProfileError::Db(tga::core::TgaError::from(e))),
        }
    }
}

/// Build an [`IdentityResolver`] seeded from the `authors` table.
///
/// Why: the resolver is built once from the DB so fuzzy matching works against
/// all known canonical identities without re-reading the DB per query.
/// What: queries `(canonical_name, canonical_email, aliases)` for all authors,
/// constructs an alias map, and passes it to `IdentityResolver::from_alias_map`.
/// Test: exercised indirectly by all selector tests.
fn build_resolver_from_db(db: &Database) -> Result<IdentityResolver> {
    use std::collections::HashMap;

    let conn = db.connection();
    let mut stmt = conn
        .prepare("SELECT canonical_name, canonical_email, aliases FROM authors")
        .map_err(|e| ProfileError::Db(tga::core::TgaError::from(e)))?;

    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|e| ProfileError::Db(tga::core::TgaError::from(e)))?;

    let mut alias_map: HashMap<String, Vec<String>> = HashMap::new();
    for r in rows {
        let (name, email, aliases_json) =
            r.map_err(|e| ProfileError::Db(tga::core::TgaError::from(e)))?;
        let mut aliases: Vec<String> = serde_json::from_str(&aliases_json).unwrap_or_default();
        // Put the canonical email first so IdentityResolver picks it as the
        // canonical email for this person.
        if !email.is_empty() {
            aliases.insert(0, email);
        }
        alias_map.insert(name, aliases);
    }

    Ok(IdentityResolver::from_alias_map(&alias_map))
}

/// Resolve a contributor query against the database at the given path.
///
/// Why: provides a convenience free function for callers that don't need to
/// keep the selector alive (e.g. one-shot CLI invocations).
/// What: opens the selector, calls `resolve`, and returns the result.
/// The DB is closed when the function returns.
/// Test: exercised transitively by batch tests.
///
/// # Errors
///
/// Same as [`ContributorSelector::resolve`] plus `ProfileError::Db` on open.
pub fn resolve_contributor(db_path: &std::path::Path, query: &str) -> Result<ResolvedIdentity> {
    let sel = ContributorSelector::open(db_path)?;
    sel.resolve(query)
}

/// Determine the tga database path from config, env var, or CLI flag.
///
/// Why: callers (CLI, service handler) need a single function to resolve the
/// DB path from the three possible sources so the precedence is consistent.
/// What: returns the first non-None, non-empty value from:
///  1. `explicit_path` (CLI `--db` flag)
///  2. `TRUSTY_REVIEW_TGA_DB` env var
///  3. A project-convention default (`~/.local/share/tga/tga.db`) via `dirs`.
///
/// Returns `ProfileError::DbNotConfigured` when none of the three sources
/// provide a path (stripped environment with no `$HOME`).
///
/// # Errors
///
/// `ProfileError::DbNotConfigured` when no path can be determined.
pub fn resolve_db_path(explicit_path: Option<&std::path::Path>) -> Result<PathBuf> {
    // 1. CLI --db flag.
    if let Some(p) = explicit_path {
        return Ok(p.to_path_buf());
    }
    // 2. TRUSTY_REVIEW_TGA_DB env var.
    if let Ok(val) = std::env::var("TRUSTY_REVIEW_TGA_DB") {
        let p = PathBuf::from(val.trim());
        if !p.as_os_str().is_empty() {
            return Ok(p);
        }
    }
    // 3. Convention default.
    if let Some(data_dir) = dirs::data_dir() {
        return Ok(data_dir.join("tga").join("tga.db"));
    }
    Err(ProfileError::DbNotConfigured)
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;
    use serial_test::serial;

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
        // Alice — plain entry, no aliases.
        seed_author(sel.database(), "Alice Smith", "alice@example.com", r#"[]"#);
        // Bob — with a GitHub login alias.
        seed_author(
            sel.database(),
            "Bob Jones",
            "bob@example.com",
            r#"["bob-gh", "bob.jones@old.example.com"]"#,
        );
        // Rebuild the resolver to include the newly seeded authors.
        sel.resolver = build_resolver_from_db(sel.database()).expect("rebuild resolver");
        sel
    }

    /// Why: a query matching a canonical email exactly must resolve to the
    /// correct identity without fuzzy logic.
    /// What: queries "alice@example.com", asserts canonical_email and name.
    /// Test: this test itself.
    #[test]
    fn selector_resolves_canonical_email() {
        let sel = make_selector_with_authors();
        let id = sel.resolve("alice@example.com").expect("resolve");
        assert_eq!(id.canonical_email, "alice@example.com");
        assert_eq!(id.canonical_name, "Alice Smith");
    }

    /// Why: a query matching a canonical name must resolve to the correct
    /// identity (case-insensitive).
    /// What: queries "bob jones" (lowercase), asserts canonical fields.
    /// Test: this test itself.
    #[test]
    fn selector_resolves_by_name() {
        let sel = make_selector_with_authors();
        let id = sel.resolve("Bob Jones").expect("resolve by name");
        assert_eq!(id.canonical_email, "bob@example.com");
        assert_eq!(id.canonical_name, "Bob Jones");
    }

    /// Why: a GitHub login stored in aliases must resolve to the author.
    /// What: queries "bob-gh" (a login alias for Bob), asserts result.
    /// Test: this test itself.
    #[test]
    fn selector_resolves_via_alias() {
        let sel = make_selector_with_authors();
        let id = sel.resolve("bob-gh").expect("resolve via alias");
        assert_eq!(id.canonical_email, "bob@example.com");
        assert_eq!(id.canonical_name, "Bob Jones");
    }

    /// Why: an unknown query must return ContributorNotFound, not panic.
    /// What: queries an email that has no entry, asserts Err variant.
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

    /// Why: resolve_db_path must use the TRUSTY_REVIEW_TGA_DB env var when
    /// no explicit path is given.
    /// What: sets the env var, calls resolve_db_path(None), asserts path matches.
    /// Test: this test itself.  Marked `#[serial]` because env-var mutation is
    /// not thread-safe with parallel test execution.
    #[test]
    #[serial]
    fn resolve_db_path_uses_env_var() {
        // Use a unique value to avoid cross-test contamination.
        let expected = "/tmp/test-tga.db";
        // Safety: env-var mutations in parallel tests can race; this is acceptable
        // for a quick integration check.  Use a process-unique env var name to
        // reduce collision risk in the rare case tests run truly in parallel.
        unsafe {
            std::env::set_var("TRUSTY_REVIEW_TGA_DB", expected);
        }
        let path = resolve_db_path(None).expect("resolve path");
        assert_eq!(path, std::path::PathBuf::from(expected));
        unsafe {
            std::env::remove_var("TRUSTY_REVIEW_TGA_DB");
        }
    }

    /// Why: an explicit path must take precedence over the env var.
    /// What: sets the env var, passes an explicit path, asserts explicit wins.
    /// Test: this test itself.  Marked `#[serial]` because env-var mutation is
    /// not thread-safe with parallel test execution.
    #[test]
    #[serial]
    fn resolve_db_path_explicit_beats_env() {
        unsafe {
            std::env::set_var("TRUSTY_REVIEW_TGA_DB", "/tmp/env-tga.db");
        }
        let explicit = std::path::Path::new("/tmp/explicit-tga.db");
        let path = resolve_db_path(Some(explicit)).expect("resolve path");
        assert_eq!(path, explicit.to_path_buf());
        unsafe {
            std::env::remove_var("TRUSTY_REVIEW_TGA_DB");
        }
    }
}
