//! redb 2.x → 4.x corpus migration that PRESERVES data (no re-embedding).
//!
//! Why: the redb 2.6 → 4.x upgrade (#702 / #707) changed the on-disk file
//! format. redb 4.x cannot open a 2.x `index.redb` — the open returns
//! `DatabaseError::UpgradeRequired(_)`. The auto-recovery path
//! ([`crate::core::corpus_recovery`]) handles that today by moving the stale
//! file aside to `*.v2-incompatible` and creating a fresh EMPTY corpus, which
//! forces a full reindex. On a large corpus that reindex is expensive precisely
//! because it RE-EMBEDS every chunk (an ONNX forward pass per chunk). The
//! chunk text, entity lists, knowledge-graph adjacency, file hashes and schema
//! version are all already in the old file — only the *container format*
//! changed, not the row payloads. This module copies every row out of the 2.x
//! file and into a new 4.x file verbatim, so an upgrade preserves the index and
//! skips re-embedding entirely.
//!
//! What: [`migrate_redb_corpus`] opens the source with the redb **2.6** engine
//! (aliased `redb2` in `Cargo.toml`), iterates every known table, and writes
//! each row into a staging redb **4.x** database. It preserves the stored
//! `_meta` `schema_version` byte-for-byte so the normal in-app migration chain
//! (M001…M00x) still runs afterwards against the correct starting version. The
//! original file is backed up (numbered, non-clobbering, via the existing
//! [`crate::core::corpus_recovery`] backup convention) before the verified
//! staging file is atomically renamed into place. The source is never destroyed
//! until the new file is fully written and row-count-verified. A destination
//! that already holds redb 4.x data is never replaced, even with a
//! `*.v2-incompatible` sibling beside it, and a destination whose format cannot
//! be read stops the run (#9453).
//!
//! Test: `tests` builds a small redb 2.6 fixture with chunks / entities / KG /
//! `_meta` rows, migrates it, and asserts the resulting 4.x corpus opens via
//! [`crate::core::corpus::CorpusStore`] and contains the same rows with the
//! same schema version. A `#[ignore]`-gated test points at a real
//! `*.v2-incompatible` file on the developer's machine.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::core::corpus_recovery::{
    backup_incompatible_corpus, is_incompatible_corpus_format, INCOMPATIBLE_CORPUS_SUFFIX,
};

mod copy;
#[cfg(test)]
mod tests;

// ── Errors ──────────────────────────────────────────────────────────────────

/// Typed refusals from the migration's destination and sibling probes.
///
/// Why: #9453 — a probe that swallowed an open error let the migration fall
/// through to deleting a live 4.x corpus. Every probe failure now stops the
/// run with a typed error the caller (and tests) can match on.
/// What: `DestUnreadable` — `dest`'s format or contents could not be read;
/// `DestChanged` — `dest` gained data or changed format between the first
/// probe and the replacement; `SiblingUnreadable` — a `*.v2-incompatible`
/// candidate could not be probed.
/// Test: `tests::unreadable_dest_refuses_and_keeps_live_corpus`,
/// `tests::unclean_v4_dest_refuses_and_keeps_live_corpus`,
/// `tests::preserve_source_refuses_a_dest_holding_data`.
#[derive(Debug, thiserror::Error)]
pub enum RedbMigrateError {
    /// `dest` exists but its redb format or table contents could not be read.
    #[error(
        "cannot read the redb corpus at {path} ({source}); refusing to migrate so it is never \
         replaced. Stop any trusty-search daemon that holds the file and re-run; a 4.x corpus \
         left unclean by a crash must first be opened once by the daemon, which repairs it"
    )]
    DestUnreadable {
        /// The destination corpus path.
        path: PathBuf,
        /// The underlying stat or redb error.
        #[source]
        source: redb::Error,
    },
    /// `dest` was no longer an empty 4.x corpus when the replacement ran.
    #[error(
        "the corpus at {path} changed during the migration and is no longer an empty redb 4.x \
         corpus; refusing to replace it"
    )]
    DestChanged {
        /// The destination corpus path.
        path: PathBuf,
    },
    /// A `*.v2-incompatible` candidate exists but could not be probed.
    #[error("cannot read the redb 2.x candidate at {path} ({source}); refusing to migrate")]
    SiblingUnreadable {
        /// The sibling path that failed the probe.
        path: PathBuf,
        /// The underlying stat or redb error.
        #[source]
        source: redb::Error,
    },
}

// ── Outcome ─────────────────────────────────────────────────────────────────

/// Result of a migration attempt.
///
/// Why: the caller (CLI handler) needs to distinguish "nothing to do, already
/// 4.x" from "migrated N rows" to print the right operator message and choose
/// an exit status, without re-opening the file itself.
/// What: `AlreadyV4` when the destination already holds a redb 4.x corpus that
/// the run leaves untouched (no-op); `Migrated` carrying per-table row counts,
/// the backup path, and the total rows copied.
/// Test: the round-trip test asserts `Migrated` with the expected row total;
/// `idempotent_on_v4` and the #9453 second-run tests assert `AlreadyV4`.
#[derive(Debug)]
pub enum MigrationOutcome {
    /// The destination already holds a redb 4.x corpus — with data, or empty
    /// with no 2.x sibling to restore from. Nothing was moved or written.
    AlreadyV4,
    /// The 2.x source was copied into a fresh 4.x corpus.
    Migrated {
        /// Per-table `(name, rows_copied)` in catalogue order.
        per_table: Vec<(&'static str, u64)>,
        /// Total rows copied across all tables.
        total_rows: u64,
        /// Where the original 2.x bytes were preserved.
        backup: PathBuf,
        /// The stored `schema_version` carried over from the source (or `0` if
        /// the source had no `_meta`/legacy corpus).
        schema_version: u32,
    },
}

// ── Public entry point ──────────────────────────────────────────────────────

/// Migrate a redb 2.x corpus at `dest` (or its `*.v2-incompatible` backup)
/// into a redb 4.x corpus at `dest`, preserving every row.
///
/// Why: see module docs — lets an upgrade preserve the index instead of
/// recreating it empty and re-embedding every chunk.
/// What: classifies `dest` first and fails closed if it cannot be read
/// ([`RedbMigrateError::DestUnreadable`]). A 2.x `dest` is migrated in place.
/// A 4.x `dest` holding any data row returns [`MigrationOutcome::AlreadyV4`]
/// and is never touched, whether or not a `*.v2-incompatible` sibling exists
/// (#9453). An empty 4.x `dest` (the auto-recovery's fresh corpus) or a
/// missing one is filled from the first 2.x sibling; with no sibling, an empty
/// 4.x `dest` is `AlreadyV4` and a missing one is an error. A migration opens
/// the source read-only with redb 2.6, copies all catalogued tables into a
/// staging 4.x file, verifies per-table row counts, preserves the original via
/// the [`crate::core::corpus_recovery`] numbered-backup convention, and
/// atomically renames the staging file into `dest`.
/// Test: `tests::round_trip_v2_to_v4`, `tests::round_trip_from_incompatible_sibling`,
/// `tests::idempotent_on_v4`, `tests::second_run_after_in_place_migration_keeps_live_v4`,
/// `tests::second_run_after_auto_recovery_reindex_keeps_live_v4`,
/// `tests::unreadable_dest_refuses_and_keeps_live_corpus`,
/// `tests::unclean_v4_dest_refuses_and_keeps_live_corpus`.
pub fn migrate_redb_corpus(dest: &Path) -> Result<MigrationOutcome> {
    // #9453: classify `dest` before looking at any sibling. The old resolver
    // skipped a 4.x `dest` and picked the stale 2.x sibling, then deleted the
    // live corpus to install it.
    let source = match classify_dest(dest)? {
        CorpusState::V2 => dest.to_path_buf(),
        CorpusState::V4 { data_rows } if data_rows > 0 => {
            tracing::info!(
                path = %dest.display(),
                data_rows,
                "redb corpus already holds redb 4.x data — nothing to migrate; any \
                 {INCOMPATIBLE_CORPUS_SUFFIX} sibling is left in place"
            );
            return Ok(MigrationOutcome::AlreadyV4);
        }
        state => match find_v2_sibling(dest)? {
            Some(sibling) => sibling,
            None if matches!(state, CorpusState::V4 { .. }) => {
                tracing::info!(
                    path = %dest.display(),
                    "redb corpus is an empty redb 4.x corpus and no 2.x backup was found — \
                     nothing to migrate"
                );
                return Ok(MigrationOutcome::AlreadyV4);
            }
            None => anyhow::bail!(
                "no redb corpus at {} and no readable redb 2.x corpus in its \
                 {INCOMPATIBLE_CORPUS_SUFFIX} sibling(s)",
                dest.display()
            ),
        },
    };

    tracing::info!(
        source = %source.display(),
        dest = %dest.display(),
        "migrating redb 2.x corpus → 4.x (preserving rows, no re-embedding)"
    );

    // Build the new 4.x corpus in a sibling staging file so `dest` is only ever
    // replaced atomically by a fully written, verified file.
    let staging = staging_path(dest);
    // Discard any stale staging file from a previously aborted run.
    remove_if_exists(&staging)
        .with_context(|| format!("clear stale staging file {}", staging.display()))?;

    let (per_table, total_rows, schema_version) = copy::copy_all_tables(&source, &staging)
        .with_context(|| {
            format!(
                "copy redb 2.x rows from {} into staging 4.x corpus {}",
                source.display(),
                staging.display()
            )
        })?;

    // Back up the original 2.x file (numbered, non-clobbering). If `source` IS
    // the live `dest`, this moves it aside and frees `dest` for the rename. If
    // `source` is already a `*.v2-incompatible` sibling, we still preserve it.
    let backup = preserve_source(dest, &source)
        .context("preserve the original 2.x corpus before installing the migrated 4.x corpus")?;

    // Atomically install the verified staging corpus at the canonical path.
    std::fs::rename(&staging, dest).with_context(|| {
        format!(
            "atomically rename migrated corpus {} → {}",
            staging.display(),
            dest.display()
        )
    })?;

    tracing::info!(
        dest = %dest.display(),
        total_rows,
        schema_version,
        backup = %backup.display(),
        "redb 2.x → 4.x migration complete (no re-embedding)"
    );
    for (name, rows) in &per_table {
        tracing::info!(table = name, rows, "migrated table");
    }

    Ok(MigrationOutcome::Migrated {
        per_table,
        total_rows,
        backup,
        schema_version,
    })
}

// ── Detection / source resolution ───────────────────────────────────────────

/// What a probe found at one corpus path.
///
/// Why: #9453 — the migration must tell a 4.x corpus holding data (never
/// replaced) from the empty 4.x corpus the auto-recovery creates (filled from
/// the 2.x sibling) and from a 2.x file (migrated in place).
/// What: `Missing`; `V2` — redb 4.x reports an incompatible old format;
/// `V4` — opens with redb 4.x, carrying its data-row count outside `_meta`.
/// Test: `tests::round_trip_from_incompatible_sibling` (empty `V4`) and the
/// #9453 second-run tests (`V4` with data).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CorpusState {
    Missing,
    V2,
    V4 { data_rows: u64 },
}

/// Probe the file at `path` and report its [`CorpusState`].
///
/// Why: this classifier must NOT probe with `redb2::Database::open` — redb 2.6
/// panics with an internal `unreachable!()` on a redb 4.x file's region
/// layout. The redb **4.x** engine returns clean `DatabaseError`s instead: a
/// 4.x file opens, and a 2.x file fails with `UpgradeRequired` (or a related
/// incompatible-format error), which means redb2 can read it.
/// What: opens `path` with [`redb::ReadOnlyDatabase`] — an `O_RDONLY` file
/// under a shared lock, so the probe writes nothing and a writer holding the
/// file (a running daemon) makes it fail (#9453). Returns `Missing` when the
/// path does not exist; `V2` for an [`is_incompatible_corpus_format`] open
/// error; `V4` with [`count_data_rows`] for a clean open. `RepairAborted` (a
/// 4.x file left unclean, which a read-only open cannot repair) and every
/// other stat, open or read error is returned, never read as "not 2.x".
/// Test: `tests::unreadable_dest_refuses_and_keeps_live_corpus` (open error),
/// `tests::unclean_v4_dest_refuses_and_keeps_live_corpus` (`RepairAborted`),
/// `tests::idempotent_on_v4` (a 4.x file is not 2.x and does not panic), the
/// #9453 second-run tests (the probe leaves `dest` byte-identical).
fn probe_corpus(path: &Path) -> Result<CorpusState, redb::Error> {
    if !path.try_exists()? {
        return Ok(CorpusState::Missing);
    }
    match redb::ReadOnlyDatabase::open(path) {
        Ok(db) => Ok(CorpusState::V4 {
            data_rows: count_data_rows(&db)?,
        }),
        // #9453: an unclean 4.x file, not an old format — redb2 must never read it.
        Err(e @ redb::DatabaseError::RepairAborted) => Err(e.into()),
        Err(e) if is_incompatible_corpus_format(&e) => Ok(CorpusState::V2),
        Err(e) => Err(e.into()),
    }
}

/// Count the rows of every table in `db` except `_meta`.
///
/// Why: #9453 — a 4.x corpus with any indexed row is live data. `_meta` only
/// holds bookkeeping such as `schema_version`, which a fresh recovery corpus
/// may carry too. Listing the tables, rather than reading a fixed catalogue,
/// counts tables this module does not copy (e.g. `kg_contrib`).
/// What: sums `len()` over every normal and multimap table except `_meta`,
/// propagating every redb error.
/// Test: `tests::second_run_after_auto_recovery_reindex_keeps_live_v4`.
fn count_data_rows(db: &redb::ReadOnlyDatabase) -> Result<u64, redb::Error> {
    use redb::{ReadableDatabase as _, ReadableTableMetadata as _, TableHandle as _};
    let meta = crate::core::migration::META_TABLE.name();
    let txn = db.begin_read()?;
    let mut rows = 0u64;
    for handle in txn.list_tables()? {
        if handle.name() == meta {
            continue;
        }
        rows = rows.saturating_add(txn.open_untyped_table(handle)?.len()?);
    }
    for handle in txn.list_multimap_tables()? {
        rows = rows.saturating_add(txn.open_untyped_multimap_table(handle)?.len()?);
    }
    Ok(rows)
}

/// Probe `dest`, failing closed (#9453).
///
/// Why: a `dest` whose format cannot be read (held by a running daemon, a
/// permission error) must stop the run, never be treated as replaceable.
/// What: [`probe_corpus`] with any error wrapped as
/// [`RedbMigrateError::DestUnreadable`].
/// Test: `tests::unreadable_dest_refuses_and_keeps_live_corpus`.
fn classify_dest(dest: &Path) -> Result<CorpusState, RedbMigrateError> {
    probe_corpus(dest).map_err(|source| RedbMigrateError::DestUnreadable {
        path: dest.to_path_buf(),
        source,
    })
}

/// Find the first `*.v2-incompatible[.N]` sibling of `dest` that is a 2.x
/// corpus.
///
/// Why: the auto-recovery moves a stale 2.x file aside and leaves an empty 4.x
/// corpus at `dest`; the rows to restore then live in a sibling.
/// What: probes [`incompatible_siblings`] in order and returns the first `V2`.
/// Missing and 4.x siblings are skipped; a probe error is
/// [`RedbMigrateError::SiblingUnreadable`] (#9453).
/// Test: `tests::round_trip_from_incompatible_sibling`.
fn find_v2_sibling(dest: &Path) -> Result<Option<PathBuf>, RedbMigrateError> {
    for sibling in incompatible_siblings(dest) {
        match probe_corpus(&sibling) {
            Ok(CorpusState::V2) => return Ok(Some(sibling)),
            Ok(_) => {}
            Err(source) => {
                return Err(RedbMigrateError::SiblingUnreadable {
                    path: sibling,
                    source,
                })
            }
        }
    }
    Ok(None)
}

/// Enumerate the candidate `*.v2-incompatible[.N]` sibling paths for `dest`.
///
/// Why: the auto-recovery backup convention appends `.v2-incompatible` and then
/// `.1`, `.2`, … on repeated failures (see `corpus_recovery`). Source
/// resolution must consider all of them.
/// What: yields `<dest>.v2-incompatible` followed by `<dest>.v2-incompatible.1`
/// … up to a small bound (`64`), which far exceeds any realistic number of
/// failed boots.
/// Test: covered indirectly by `tests::round_trip_from_incompatible_sibling`.
fn incompatible_siblings(dest: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut base = dest.as_os_str().to_os_string();
    base.push(INCOMPATIBLE_CORPUS_SUFFIX);
    out.push(PathBuf::from(base));
    for n in 1u32..64 {
        let mut s = dest.as_os_str().to_os_string();
        s.push(INCOMPATIBLE_CORPUS_SUFFIX);
        s.push(format!(".{n}"));
        out.push(PathBuf::from(s));
    }
    out
}

// ── Backup / staging helpers ────────────────────────────────────────────────

/// Suffix for the staging file the new 4.x corpus is built in before the
/// atomic rename into place.
///
/// Why: building the new corpus at a sibling temp path and renaming atomically
/// guarantees the canonical `index.redb` is only ever replaced by a fully
/// written, row-verified file — a crash mid-migration leaves the original
/// untouched and the half-written staging file is discarded on the next run.
/// What: the literal `".v4-migrating"` appended to the destination path.
/// Test: covered by `migrate_redb_corpus`'s round-trip test (the staging file
/// is renamed away on success and must not linger).
const STAGING_SUFFIX: &str = ".v4-migrating";

/// Compute the staging file path for a destination corpus.
///
/// Why: a single deterministic staging path keeps the migration's temp file
/// next to the destination (same filesystem → atomic rename) and easy to find
/// if a run aborts.
/// What: appends [`STAGING_SUFFIX`] to `dest`.
/// Test: covered by the round-trip test (the staging file must not survive a
/// successful run).
fn staging_path(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_os_string();
    s.push(STAGING_SUFFIX);
    PathBuf::from(s)
}

/// Ensure the original 2.x bytes are preserved, freeing `dest` for the rename.
///
/// Why: we must never lose the source data, and `std::fs::rename(staging,
/// dest)` requires `dest` to be replaceable. Two cases. (a) `source == dest`
/// (the live path is the 2.x file): move it aside to a numbered
/// `*.v2-incompatible` backup so `dest` is freed. (b) `source` is already a
/// `*.v2-incompatible` sibling (auto-recovery already moved it): the sibling IS
/// the backup; just remove the empty 4.x file the recovery created at `dest` so
/// the rename can land. Case (b) re-probes `dest` first and removes it only if
/// it is still missing or an empty 4.x corpus (#9453).
/// What: returns the path where the original bytes now live. Case (b) returns
/// [`RedbMigrateError::DestChanged`] or `DestUnreadable` instead of removing a
/// `dest` that gained data or cannot be read.
/// Test: `tests::round_trip_v2_to_v4` (case 1),
/// `tests::round_trip_from_incompatible_sibling` (case 2), and
/// `tests::preserve_source_refuses_a_dest_holding_data` (the re-probe).
fn preserve_source(dest: &Path, source: &Path) -> Result<PathBuf> {
    if source == dest {
        // Live path is the 2.x file → move it aside (numbered, non-clobbering),
        // which also frees `dest` for the rename.
        let backup = backup_incompatible_corpus(dest)
            .with_context(|| format!("back up original 2.x corpus {}", dest.display()))?;
        Ok(backup)
    } else {
        // #9453: this is the only removal of `dest`; re-probe it here so a
        // corpus that gained data since the first probe is never deleted.
        match classify_dest(dest)? {
            CorpusState::Missing | CorpusState::V4 { data_rows: 0 } => {}
            CorpusState::V2 | CorpusState::V4 { .. } => {
                return Err(RedbMigrateError::DestChanged {
                    path: dest.to_path_buf(),
                }
                .into())
            }
        }
        // `source` is already the preserved sibling. The auto-recovery may have
        // created a fresh (empty) 4.x file at `dest`; remove it so the verified
        // staging file can be renamed into place.
        remove_if_exists(dest).with_context(|| {
            format!(
                "remove the empty recovery corpus at {} so the migrated corpus can replace it",
                dest.display()
            )
        })?;
        Ok(source.to_path_buf())
    }
}

/// Remove a file if it exists; a missing file is not an error.
///
/// Why: staging-file cleanup and the empty-recovery-file removal both want
/// idempotent "delete if present" semantics so re-runs are safe.
/// What: deletes `path`, swallowing `NotFound`, surfacing other I/O errors.
/// Test: exercised by the idempotency and round-trip tests.
fn remove_if_exists(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}
