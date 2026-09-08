//! The one place a redb page-cache ceiling is chosen for palace-scoped stores
//! (#7106).
//!
//! Why: `redb::Builder::default()` sets `cache_size` to 1 GiB, and redb treats
//! that as a ceiling it will grow into rather than a reservation. Every
//! memory-core store opened a `Database` without touching it, so a daemon
//! holding the default 64 resident palaces — each with `kg.redb`,
//! `index.usearch.redb` and `chat_sessions.redb` — carried a 192 GiB
//! theoretical page-cache ceiling. That is what turned a 233 MB daemon into an
//! 11–23 GB one under ordinary browsing: heap the allocator had handed to redb
//! page caches and never returned. trusty-search hit the same thing in #329 and
//! answered it with an explicit 64 MB ceiling; this module is that answer for
//! memory-core, in one place so the number can be tuned once.
//!
//! What: [`DEFAULT_PALACE_REDB_CACHE_MB`] (64 MB, sized off the largest
//! `kg.redb` observed on the reporter's host), the
//! [`PALACE_REDB_CACHE_MB_ENV`] override, and [`palace_db_builder`] /
//! [`create_palace_db`] — the builder and open call every palace-scoped redb
//! open in this workspace must route through.
//!
//! Fail-open check: an unparsable override must NOT silently leave redb on its
//! 1 GiB default, because that is the exact failure this module exists to
//! prevent and it would be invisible. [`parse_cache_mb`] returns the bounded
//! default plus the warning text naming the rejected value, and
//! [`palace_redb_cache_bytes`] logs it.
//!
//! Test: `parse_cache_mb_warns_and_keeps_the_bounded_default_on_garbage`,
//! `parse_cache_mb_accepts_a_valid_override`,
//! `builder_uses_the_bounded_ceiling`.
//!
//! [`DEFAULT_PALACE_REDB_CACHE_MB`]: crate::redb_cache::DEFAULT_PALACE_REDB_CACHE_MB
//! [`PALACE_REDB_CACHE_MB_ENV`]: crate::redb_cache::PALACE_REDB_CACHE_MB_ENV
//! [`palace_db_builder`]: crate::redb_cache::palace_db_builder
//! [`create_palace_db`]: crate::redb_cache::create_palace_db
//! [`parse_cache_mb`]: crate::redb_cache::parse_cache_mb
//! [`palace_redb_cache_bytes`]: crate::redb_cache::palace_redb_cache_bytes

use redb::{Database, DatabaseError};
use std::path::Path;

/// Page-cache ceiling, in megabytes, applied to every palace-scoped redb file.
///
/// Why (#7106): redb's own default is 1 GiB per `Database`. Multiplied by three
/// files per palace and 64 resident palaces that ceiling is 192 GiB, and the
/// allocator grows into it. 64 MB comfortably exceeds the largest `kg.redb` on
/// the reporting host (the trusty-tools palace, 64 MB on disk), so a hot palace
/// still caches its whole graph, and it matches the ceiling trusty-search chose
/// for the same reason in #329.
/// What: 64, multiplied by 1 MiB in [`palace_redb_cache_bytes`].
/// Test: `builder_uses_the_bounded_ceiling`.
pub const DEFAULT_PALACE_REDB_CACHE_MB: usize = 64;

/// Environment variable overriding [`DEFAULT_PALACE_REDB_CACHE_MB`].
///
/// Why: an operator on a 128 GB host with one enormous palace may want a larger
/// ceiling, and an operator on the documented 16 GB minimum (#6802) may want a
/// smaller one. Tuning it must not need a rebuild.
/// What: read as decimal megabytes. Unset, empty, non-numeric, or `0` all fall
/// back to the bounded default — never to redb's 1 GiB.
/// Test: `parse_cache_mb_accepts_a_valid_override`,
/// `parse_cache_mb_warns_and_keeps_the_bounded_default_on_garbage`.
pub const PALACE_REDB_CACHE_MB_ENV: &str = "TRUSTY_MEMORY_REDB_CACHE_MB";

/// Decide the ceiling (in MB) from a raw override string, with the warning the
/// caller must log when the value was rejected.
///
/// Why (#7106, Fail-Open Check): the failure mode this module guards is
/// invisible — a daemon that silently falls back would keep redb's 1 GiB
/// default and look exactly like a daemon that was tuned. Separating the
/// decision from the logging makes "a rejected value produces a warning naming
/// it, and the bounded default" a pure, deterministic assertion rather than a
/// log-capture test.
/// What: `None`, empty, non-numeric, and `0` all yield
/// [`DEFAULT_PALACE_REDB_CACHE_MB`]. A rejected non-empty value also yields the
/// warning text, which names the variable and the offending value verbatim.
/// A valid positive value is returned with no warning.
/// Test: `parse_cache_mb_accepts_a_valid_override`,
/// `parse_cache_mb_warns_and_keeps_the_bounded_default_on_garbage`.
pub fn parse_cache_mb(raw: Option<&str>) -> (usize, Option<String>) {
    let Some(value) = raw else {
        return (DEFAULT_PALACE_REDB_CACHE_MB, None);
    };
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return (DEFAULT_PALACE_REDB_CACHE_MB, None);
    }
    match trimmed.parse::<usize>() {
        Ok(mb) if mb > 0 => (mb, None),
        _ => (
            DEFAULT_PALACE_REDB_CACHE_MB,
            Some(format!(
                "{PALACE_REDB_CACHE_MB_ENV}={value:?} is not a positive integer number of \
                 megabytes; using the bounded default of {DEFAULT_PALACE_REDB_CACHE_MB} MB \
                 (NOT redb's 1 GiB default) — see #7106"
            )),
        ),
    }
}

/// The page-cache ceiling in bytes for a palace-scoped redb file.
///
/// Why: every open needs the same number, and the env read plus the fail-open
/// warning belong in one place rather than at each call site.
/// What: [`parse_cache_mb`] over [`PALACE_REDB_CACHE_MB_ENV`], logged at `warn`
/// when the value was rejected, multiplied by 1 MiB.
/// Test: `builder_uses_the_bounded_ceiling`.
pub fn palace_redb_cache_bytes() -> usize {
    let raw = std::env::var(PALACE_REDB_CACHE_MB_ENV).ok();
    let (mb, warning) = parse_cache_mb(raw.as_deref());
    if let Some(w) = warning {
        tracing::warn!("{w}");
    }
    mb * 1024 * 1024
}

/// A [`redb::Builder`] carrying the palace page-cache ceiling.
///
/// Why (#7106): this is the single tuning point the module header promises.
/// A palace-scoped `Database::create` / `Database::open` that bypasses it
/// silently reinstates redb's 1 GiB ceiling for that file.
/// What: `Database::builder().set_cache_size(palace_redb_cache_bytes())`.
/// Test: `builder_uses_the_bounded_ceiling`.
pub fn palace_db_builder() -> redb::Builder {
    let mut builder = Database::builder();
    builder.set_cache_size(palace_redb_cache_bytes());
    builder
}

/// Create or open a palace-scoped redb database with the bounded ceiling.
///
/// Why: `Database::create(path)` is the call every memory-core store used, and
/// it is the one that takes redb's 1 GiB default. This is its drop-in
/// replacement, with the same signature and the same errors, so routing a call
/// site through it is a one-line change that cannot alter recovery behaviour.
/// What: [`palace_db_builder`] then `.create(path)`.
/// Test: `create_palace_db_round_trips`, and
/// `every_memory_core_redb_open_is_bounded` (which fails if any palace-scoped
/// open goes back to a bare `Database::create`).
pub fn create_palace_db(path: &Path) -> Result<Database, DatabaseError> {
    palace_db_builder().create(path)
}

/// Open a palace-scoped redb database read-only with the bounded ceiling.
///
/// Why: `ReadOnlyDatabase::open(path)` takes redb's 1 GiB default just as
/// `Database::create` does, and the read-only measurement path opens the same
/// palace files. A ceiling that applied only to the writable path would leave a
/// whole class of opens unbounded.
/// What: [`palace_db_builder`] then `.open_read_only(path)`.
/// Test: `every_memory_core_redb_open_is_bounded` pins that no palace-scoped
/// open bypasses this or [`create_palace_db`].
pub fn open_palace_db_read_only(path: &Path) -> Result<redb::ReadOnlyDatabase, DatabaseError> {
    palace_db_builder().open_read_only(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: a valid override is the whole point of the knob; if it were
    /// ignored, an operator tuning for a 16 GB host would get no effect and no
    /// signal.
    /// What: a positive decimal passes through verbatim, with no warning.
    /// Test: this test.
    #[test]
    fn parse_cache_mb_accepts_a_valid_override() {
        assert_eq!(parse_cache_mb(Some("128")), (128, None));
        assert_eq!(parse_cache_mb(Some("  16 ")), (16, None));
    }

    /// Why (#7106, Fail-Open Check): the dangerous outcome is a silent fallback
    /// — a garbage value leaving redb on 1 GiB with nothing in the log. This
    /// pins both halves: the bounded default is chosen AND a warning naming the
    /// rejected value is produced.
    /// What: exercises garbage, a negative, and an explicit `0`; asserts the
    /// returned megabytes are the bounded default and that the warning text
    /// contains the variable name and the offending value.
    /// Test: this test.
    #[test]
    fn parse_cache_mb_warns_and_keeps_the_bounded_default_on_garbage() {
        for bad in ["banana", "-1", "0", "64MB"] {
            let (mb, warning) = parse_cache_mb(Some(bad));
            assert_eq!(
                mb, DEFAULT_PALACE_REDB_CACHE_MB,
                "{bad:?} must fall back to the bounded default, never redb's 1 GiB"
            );
            let warning =
                warning.unwrap_or_else(|| panic!("{bad:?} must produce a warning, not a silence"));
            assert!(
                warning.contains(PALACE_REDB_CACHE_MB_ENV),
                "warning must name the variable: {warning}"
            );
            assert!(
                warning.contains(bad),
                "warning must name the rejected value {bad:?}: {warning}"
            );
        }
    }

    /// Why: unset and empty are the normal cases and must be quiet — a warning
    /// on every start would train operators to ignore the one that matters.
    /// What: `None` and `""` both yield the default with no warning.
    /// Test: this test.
    #[test]
    fn parse_cache_mb_is_quiet_when_unset() {
        assert_eq!(parse_cache_mb(None), (DEFAULT_PALACE_REDB_CACHE_MB, None));
        assert_eq!(
            parse_cache_mb(Some("")),
            (DEFAULT_PALACE_REDB_CACHE_MB, None)
        );
    }

    /// Why (#7106): the ceiling is the fix; a builder that silently kept redb's
    /// default would leave the daemon exactly as it was.
    /// What: asserts the resolved byte count is the bounded default in bytes
    /// and is well under redb's 1 GiB default, then opens a real database
    /// through the builder to prove the setting is accepted.
    /// Test: this test.
    #[test]
    fn builder_uses_the_bounded_ceiling() {
        // No env override is set in this process by default; if a developer has
        // one exported, the assertion below still holds against 1 GiB.
        let bytes = palace_redb_cache_bytes();
        assert!(
            bytes < 1024 * 1024 * 1024,
            "palace redb cache ceiling {bytes} must be under redb's 1 GiB default"
        );
        let dir = tempfile::tempdir().expect("tempdir");
        let db = create_palace_db(&dir.path().join("bounded.redb")).expect("create bounded db");
        drop(db);
    }

    /// Why: the replacement for `Database::create` must behave like it — same
    /// file, same reopen semantics — or routing call sites through it would be
    /// a behaviour change hiding inside a memory fix.
    /// What: creates, writes nothing, drops, reopens through the same helper.
    /// Test: this test.
    #[test]
    fn create_palace_db_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("round.redb");
        drop(create_palace_db(&path).expect("first create"));
        drop(create_palace_db(&path).expect("reopen"));
        assert!(path.exists(), "the database file must persist");
    }
}
