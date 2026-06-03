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
//! until the new file is fully written and row-count-verified.
//!
//! Test: `tests` builds a small redb 2.6 fixture with chunks / entities / KG /
//! `_meta` rows, migrates it, and asserts the resulting 4.x corpus opens via
//! [`crate::core::corpus::CorpusStore`] and contains the same rows with the
//! same schema version. A `#[ignore]`-gated test points at a real
//! `*.v2-incompatible` file on the developer's machine.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::core::corpus_recovery::{backup_incompatible_corpus, INCOMPATIBLE_CORPUS_SUFFIX};

mod copy;
#[cfg(test)]
mod tests;

// ── Outcome ─────────────────────────────────────────────────────────────────

/// Result of a migration attempt.
///
/// Why: the caller (CLI handler) needs to distinguish "nothing to do, already
/// 4.x" from "migrated N rows" to print the right operator message and choose
/// an exit status, without re-opening the file itself.
/// What: `AlreadyV4` when the source already opens with redb 4.x (no-op);
/// `Migrated` carrying per-table row counts, the backup path, and the total
/// rows copied.
/// Test: the round-trip test asserts `Migrated` with the expected row total;
/// the idempotency test asserts a second run returns `AlreadyV4`.
#[derive(Debug)]
pub enum MigrationOutcome {
    /// The file at the destination path already opens with redb 4.x — no
    /// migration was performed.
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
/// What: resolves the actual 2.x source (the live path, else the
/// `*.v2-incompatible` sibling the auto-recovery may have moved aside). If the
/// destination already opens with redb 4.x, returns [`MigrationOutcome::AlreadyV4`]
/// (idempotent no-op). Otherwise opens the source read-only with redb 2.6,
/// copies all catalogued tables into a staging 4.x file, verifies per-table row
/// counts match, backs up the original via the
/// [`crate::core::corpus_recovery`] numbered-backup convention, and atomically
/// renames the staging file into `dest`.
/// Test: `tests::round_trip_v2_to_v4` and `tests::idempotent_on_v4`.
pub fn migrate_redb_corpus(dest: &Path) -> Result<MigrationOutcome> {
    // Resolve which file actually holds 2.x data to migrate. This may be `dest`
    // itself (the live path is still the old 2.x file) or a `*.v2-incompatible`
    // sibling the auto-recovery already moved aside while leaving an empty 4.x
    // file at `dest`.
    let source = match resolve_source(dest) {
        Ok(s) => s,
        Err(_) => {
            // No 2.x data anywhere. If `dest` opens with redb 4.x it is already
            // migrated (or freshly created) — a safe no-op. Otherwise surface a
            // clear error.
            if dest.exists() && opens_with_v4(dest) {
                tracing::info!(
                    path = %dest.display(),
                    "redb corpus already opens with redb 4.x and no 2.x backup found — \
                     nothing to migrate"
                );
                return Ok(MigrationOutcome::AlreadyV4);
            }
            anyhow::bail!(
                "no readable redb 2.x corpus found at {} or its {INCOMPATIBLE_CORPUS_SUFFIX} \
                 sibling(s), and {} does not open as a redb 4.x corpus either",
                dest.display(),
                dest.display()
            );
        }
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

/// Report whether the file at `path` opens cleanly with the redb 4.x engine.
///
/// Why: the detection step must not treat a healthy 4.x corpus as a migration
/// candidate. A successful 4.x open is the definitive "already migrated" signal.
/// What: attempts `redb::Database::open(path)` and returns whether it succeeded.
/// Any open error (including `UpgradeRequired` for a 2.x file) returns `false`.
/// Test: `tests::idempotent_on_v4` relies on this returning `true` for a 4.x DB.
fn opens_with_v4(path: &Path) -> bool {
    redb::Database::open(path).is_ok()
}

/// Resolve the actual 2.x source file for a destination path.
///
/// Why: the auto-recovery path may already have moved the stale 2.x file aside
/// to `index.redb.v2-incompatible` and created a fresh empty 4.x file at
/// `index.redb`. In that case the rows to preserve live in the backup sibling,
/// not at the canonical path. We must find whichever file actually holds the
/// 2.x data.
/// What: prefers `dest` if it exists and is genuinely a 2.x file (opens with
/// redb2 but not redb4); otherwise falls back to the first existing
/// `*.v2-incompatible[.N]` sibling that is a 2.x file. Errors if neither
/// candidate is a readable 2.x corpus.
/// Test: `tests::round_trip_v2_to_v4` (source == dest) and
/// `tests::round_trip_from_incompatible_sibling` (source == sibling).
fn resolve_source(dest: &Path) -> Result<PathBuf> {
    // Candidate 1: the canonical path, if it is itself a 2.x file.
    if dest.exists() && is_v2_corpus(dest) {
        return Ok(dest.to_path_buf());
    }

    // Candidate 2..: the numbered `.v2-incompatible` siblings, newest-numbered
    // first is not important — any readable 2.x sibling works; we take the
    // first that opens with redb2.
    for sibling in incompatible_siblings(dest) {
        if sibling.exists() && is_v2_corpus(&sibling) {
            return Ok(sibling);
        }
    }

    anyhow::bail!(
        "no readable redb 2.x corpus found at {} or its {INCOMPATIBLE_CORPUS_SUFFIX} sibling(s)",
        dest.display()
    )
}

/// Report whether `path` is an old (pre-4.x) redb corpus the redb 2.x engine
/// should read.
///
/// Why: this is the load-bearing classifier, and it must NOT probe with
/// `redb2::Database::open` directly — redb 2.6 panics with an internal
/// `unreachable!()` when it tries to parse a redb 4.x file's region layout, so
/// using it as a probe on an arbitrary file would crash the process. Instead we
/// classify with the redb **4.x** engine, which returns clean `DatabaseError`s:
/// a 4.x file opens, and an older 2.x file fails with `UpgradeRequired` (or a
/// related incompatible-format error). Only a positive "incompatible old
/// format" classification means redb2 can safely read it.
/// What: opens `path` with redb 4.x. Returns `true` only when the open fails
/// with an incompatible/old-format error (reusing
/// [`crate::core::corpus_recovery::is_incompatible_corpus_format`]). A
/// successful 4.x open (already migrated), a missing file, or any transient
/// error returns `false`.
/// Test: covered by `resolve_source` round-trip tests and `idempotent_on_v4`
/// (which must NOT classify a 4.x file as 2.x and must not panic).
fn is_v2_corpus(path: &Path) -> bool {
    match redb::Database::open(path) {
        Ok(_) => false, // a clean 4.x open → definitely not an old 2.x file
        Err(e) => crate::core::corpus_recovery::is_incompatible_corpus_format(&e),
    }
}

/// Enumerate the candidate `*.v2-incompatible[.N]` sibling paths for `dest`.
///
/// Why: the auto-recovery backup convention appends `.v2-incompatible` and then
/// `.1`, `.2`, … on repeated failures (see `corpus_recovery`). Source
/// resolution must consider all of them.
/// What: yields `<dest>.v2-incompatible` followed by `<dest>.v2-incompatible.1`
/// … up to a small bound (`64`), which far exceeds any realistic number of
/// failed boots.
/// Test: covered indirectly by `resolve_source`'s sibling round-trip test.
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
/// the rename can land.
/// What: returns the path where the original bytes now live.
/// Test: `tests::round_trip_v2_to_v4` (case 1) and
/// `tests::round_trip_from_incompatible_sibling` (case 2).
fn preserve_source(dest: &Path, source: &Path) -> Result<PathBuf> {
    if source == dest {
        // Live path is the 2.x file → move it aside (numbered, non-clobbering),
        // which also frees `dest` for the rename.
        let backup = backup_incompatible_corpus(dest)
            .with_context(|| format!("back up original 2.x corpus {}", dest.display()))?;
        Ok(backup)
    } else {
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
