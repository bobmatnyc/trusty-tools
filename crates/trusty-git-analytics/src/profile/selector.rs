//! Contributor identity resolution against the org-wide tga database.
//!
//! Why: a caller naming a contributor has a GitHub login, a display name, or an
//! email — but every downstream query keys on `authors.canonical_email`. This
//! module owns that translation so no other stage has to know the schema or the
//! fuzzy-matching rules.
//! What: [`ContributorSelector`] opens the database, seeds an
//! [`IdentityResolver`] from the `authors` rows, and tries email, name, alias,
//! then fuzzy matching in that order. [`resolve_db_path`] finds the database
//! itself.
//! Test: `selector_resolves_canonical_email`, `selector_resolves_by_name`,
//! `selector_resolves_via_alias`, `selector_not_found_returns_error`,
//! `resolve_db_path_uses_env_var`, `resolve_db_path_explicit_beats_env`.

use std::path::{Path, PathBuf};

use tracing::{debug, warn};

use crate::collect::identity::IdentityResolver;
use crate::core::config::expand_path;
use crate::core::db::Database;

use super::error::{ProfileError, Result};

/// Environment variable naming the org-wide tga database.
///
/// Why: profiling reads one database covering every repository, which is not
/// necessarily the per-config database a `tga collect` run writes, so it needs
/// its own override that a caller can set once for a shell session.
/// What: `"TGA_DB"`. A leading `~` in the value is expanded.
/// Test: `resolve_db_path_uses_env_var`.
pub const ENV_TGA_DB: &str = "TGA_DB";

// ─── Resolved identity ────────────────────────────────────────────────────────

/// A contributor identity resolved against the `authors` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedIdentity {
    /// Canonical email, as stored in `authors.canonical_email`.
    pub canonical_email: String,
    /// Canonical display name, as stored in `authors.canonical_name`.
    pub canonical_name: String,
}

// ─── ContributorSelector ──────────────────────────────────────────────────────

/// An open tga database plus a resolver seeded from its `authors` rows.
///
/// Why: resolution happens once per profile run but the database stays open for
/// the batch and diff-sampling stages, so holding both together avoids opening
/// the file twice and rebuilding the resolver per query.
/// What: owns a [`Database`] and an [`IdentityResolver`]; `resolve` runs the
/// four-strategy lookup, `database` lends the handle to later stages.
/// Test: see the module-level `Test:` pointers.
pub struct ContributorSelector {
    db: Database,
    resolver: IdentityResolver,
}

impl ContributorSelector {
    /// Open the database at `db_path` and build the resolver from it.
    ///
    /// # Errors
    ///
    /// [`ProfileError::Db`] when the database cannot be opened or queried.
    ///
    /// Test: `selector_resolves_canonical_email` (via `open_in_memory`).
    pub fn open(db_path: &Path) -> Result<Self> {
        let db = Database::open(db_path)?;
        let resolver = build_resolver_from_db(&db)?;
        Ok(Self { db, resolver })
    }

    /// Open an in-memory database. Test-only.
    #[cfg(test)]
    pub(crate) fn open_in_memory() -> Result<Self> {
        let db = Database::open_in_memory()?;
        let resolver = build_resolver_from_db(&db)?;
        Ok(Self { db, resolver })
    }

    /// Borrow the open database so later stages can reuse it.
    pub fn database(&self) -> &Database {
        &self.db
    }

    /// Resolve a caller-supplied query to a canonical identity.
    ///
    /// Why: the caller may hold a GitHub login, a display name, or any of a
    /// person's historical emails, and all three have to reach the same
    /// canonical pair or the profile silently covers a fraction of their work.
    /// What: tries, in order, an exact case-insensitive `canonical_email` match,
    /// an exact case-insensitive `canonical_name` match, a substring match
    /// against the `aliases` JSON column, and finally [`IdentityResolver`]'s
    /// fuzzy matching. The resolver returns its input unchanged when it finds
    /// nothing, which is how a miss is detected.
    ///
    /// # Errors
    ///
    /// - [`ProfileError::ContributorNotFound`] when nothing matches.
    /// - [`ProfileError::Db`] on a SQLite failure.
    ///
    /// Test: `selector_resolves_canonical_email`, `selector_resolves_by_name`,
    /// `selector_resolves_via_alias`, `selector_not_found_returns_error`.
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

        // Passing the query as both name and email triggers every resolver tier.
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
        self.query_identity(
            "SELECT canonical_email, canonical_name \
             FROM authors WHERE LOWER(canonical_email) = ?1 LIMIT 1",
            email_lc,
        )
    }

    fn lookup_by_name(&self, name: &str) -> Result<Option<ResolvedIdentity>> {
        self.query_identity(
            "SELECT canonical_email, canonical_name \
             FROM authors WHERE LOWER(canonical_name) = LOWER(?1) LIMIT 1",
            name,
        )
    }

    /// Search the `authors.aliases` JSON column for `query` as a substring.
    ///
    /// Why: GitHub logins and old email addresses live in that JSON array, and a
    /// `LIKE` scan covers the common case without parsing every row's JSON.
    /// What: escapes `%` and `_` so an alias containing a wildcard cannot widen
    /// the match, then runs a `LIKE … ESCAPE '\'` query.
    /// Test: `selector_resolves_via_alias`.
    fn lookup_by_alias_json(&self, query: &str) -> Result<Option<ResolvedIdentity>> {
        let escaped = query.replace('%', "\\%").replace('_', "\\_");
        let pattern = format!("%{escaped}%");
        self.query_identity(
            "SELECT canonical_email, canonical_name \
             FROM authors WHERE aliases LIKE ?1 ESCAPE '\\' LIMIT 1",
            &pattern,
        )
    }

    /// Run a single-parameter identity query, mapping "no rows" to `Ok(None)`.
    fn query_identity(&self, sql: &str, param: &str) -> Result<Option<ResolvedIdentity>> {
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

/// Build an [`IdentityResolver`] seeded from every `authors` row.
///
/// Why: fuzzy matching has to see all known identities at once, and re-reading
/// the table per query would make resolution O(queries × authors).
/// What: reads `(canonical_name, canonical_email, aliases)`, puts the canonical
/// email first in each alias list so the resolver picks it as canonical, and
/// hands the map to `IdentityResolver::from_alias_map`. A row whose `aliases`
/// column is not valid JSON contributes an empty alias list rather than
/// aborting the run — one malformed row must not make every contributor
/// unresolvable.
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

/// Resolve a contributor against the database at `db_path`, closing it after.
///
/// Why: a one-shot caller that needs only the identity should not have to hold
/// a [`ContributorSelector`] alive.
///
/// # Errors
///
/// As [`ContributorSelector::resolve`], plus [`ProfileError::Db`] on open.
///
/// Test: covered through `ContributorSelector::resolve`'s own tests.
pub fn resolve_contributor(db_path: &Path, query: &str) -> Result<ResolvedIdentity> {
    let sel = ContributorSelector::open(db_path)?;
    sel.resolve(query)
}

/// Determine the tga database path from flag, environment, or convention.
///
/// Why: three sources can name the database and every entry point has to agree
/// on which wins, or a profile silently reads a different database than the
/// caller's other commands.
/// What: returns the first of an explicit path, the [`ENV_TGA_DB`] variable, or
/// `<data-dir>/tga/tga.db`. A leading `~` is expanded in the first two; an empty
/// or whitespace-only environment value is treated as unset.
///
/// # Errors
///
/// [`ProfileError::DbNotConfigured`] when no source yields a path — which in
/// practice means a stripped environment with no home directory.
///
/// Test: `resolve_db_path_uses_env_var`, `resolve_db_path_explicit_beats_env`,
/// `resolve_db_path_blank_env_falls_through`.
pub fn resolve_db_path(explicit_path: Option<&Path>) -> Result<PathBuf> {
    // #6405: read the process env here so the precedence rule below stays pure.
    resolve_db_path_from(explicit_path, std::env::var(ENV_TGA_DB).ok().as_deref())
}

/// [`resolve_db_path`], with the [`ENV_TGA_DB`] value supplied by the caller.
///
/// Why (#6405): proving the precedence used to require `set_var`, which races
/// every other thread's `getenv`; the three `#[serial]` tests that guarded it
/// serialized only against each other, never against a concurrent reader.
/// What: same precedence as [`resolve_db_path`], with `env_value` standing in
/// for the variable.
///
/// # Errors
///
/// As [`resolve_db_path`].
///
/// Test: `resolve_db_path_uses_env_var`, `resolve_db_path_explicit_beats_env`,
/// `resolve_db_path_blank_env_falls_through`.
pub(crate) fn resolve_db_path_from(
    explicit_path: Option<&Path>,
    env_value: Option<&str>,
) -> Result<PathBuf> {
    if let Some(p) = explicit_path {
        return Ok(expand_path(p));
    }
    if let Some(val) = env_value {
        let trimmed = val.trim();
        if !trimmed.is_empty() {
            return Ok(expand_path(Path::new(trimmed)));
        }
    }
    if let Some(data_dir) = dirs::data_dir() {
        return Ok(data_dir.join("tga").join("tga.db"));
    }
    Err(ProfileError::DbNotConfigured)
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

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
        // Alice — plain entry, no aliases.
        seed_author(sel.database(), "Alice Smith", "alice@example.com", "[]");
        // Bob — with a GitHub login alias.
        seed_author(
            sel.database(),
            "Bob Jones",
            "bob@example.com",
            r#"["bob-gh", "bob.jones@old.example.com"]"#,
        );
        // Rebuild the resolver so it sees the newly seeded authors.
        sel.resolver = build_resolver_from_db(sel.database()).expect("rebuild resolver");
        sel
    }

    /// Why: an exact canonical-email query must resolve without fuzzy logic.
    /// What: queries `alice@example.com`, asserts both canonical fields.
    /// Test: this test itself.
    #[test]
    fn selector_resolves_canonical_email() {
        let sel = make_selector_with_authors();
        let id = sel.resolve("alice@example.com").expect("resolve");
        assert_eq!(id.canonical_email, "alice@example.com");
        assert_eq!(id.canonical_name, "Alice Smith");
    }

    /// Why: a display-name query must resolve case-insensitively.
    /// What: queries `Bob Jones`, asserts the canonical pair.
    /// Test: this test itself.
    #[test]
    fn selector_resolves_by_name() {
        let sel = make_selector_with_authors();
        let id = sel.resolve("Bob Jones").expect("resolve by name");
        assert_eq!(id.canonical_email, "bob@example.com");
        assert_eq!(id.canonical_name, "Bob Jones");
    }

    /// Why: a GitHub login stored in `aliases` must reach the author.
    /// What: queries `bob-gh`, asserts Bob's canonical pair.
    /// Test: this test itself.
    #[test]
    fn selector_resolves_via_alias() {
        let sel = make_selector_with_authors();
        let id = sel.resolve("bob-gh").expect("resolve via alias");
        assert_eq!(id.canonical_email, "bob@example.com");
        assert_eq!(id.canonical_name, "Bob Jones");
    }

    /// Why: an unknown query must return `ContributorNotFound` with the hint,
    /// not panic and not silently resolve to something wrong.
    /// What: queries an unseeded email, asserts the variant and the hint text.
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

    /// Why: `resolve_db_path` must honour `TGA_DB` when no explicit path is given.
    /// What: passes the variable's value, calls with no explicit path, asserts
    /// the path matches.
    /// Test: this test itself. #6405: the value is a parameter, so the
    /// `#[serial]` and the `set_var` it guarded are both gone.
    #[test]
    fn resolve_db_path_uses_env_var() {
        let expected = "/tmp/test-tga.db";
        let path = resolve_db_path_from(None, Some(expected)).expect("resolve path");
        assert_eq!(path, PathBuf::from(expected));
    }

    /// Why: an explicit path must beat the environment, or a `--db` flag would
    /// be silently ignored in a shell that exports `TGA_DB`.
    /// What: supplies both, asserts explicit wins.
    /// Test: this test itself.
    #[test]
    fn resolve_db_path_explicit_beats_env() {
        let explicit = Path::new("/tmp/explicit-tga.db");
        let path =
            resolve_db_path_from(Some(explicit), Some("/tmp/env-tga.db")).expect("resolve path");
        assert_eq!(path, explicit.to_path_buf());
    }

    /// Why: an exported-but-empty `TGA_DB` is a common shell accident and must
    /// fall through to the convention default rather than resolving to `""`.
    /// What: supplies a whitespace-only value, asserts the convention default.
    /// Test: this test itself.
    #[test]
    fn resolve_db_path_blank_env_falls_through() {
        let path = resolve_db_path_from(None, Some("   ")).expect("resolve path");
        assert!(
            path.ends_with("tga/tga.db"),
            "blank env must fall through to the convention default, got {}",
            path.display()
        );
    }
}
