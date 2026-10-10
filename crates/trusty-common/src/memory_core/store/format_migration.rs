//! The palace format 0 → 1 migration, run lazily at a writer-intent open (#9274).
//!
//! Why: ADR-0067 D3 rules 2–4. A format-0 palace is migrated by the first
//! writer-intent open, never by a startup sweep. The migration may not write
//! until a verified backup exists, and a crash or error at any step must leave
//! the palace either at format 0 or at format 1 — never opened half-done.
//! What: [`open_gate`] classifies the palace (`palace_format::classify_with`).
//! A current palace opens; a writer also rewrites a missing or lower mirror.
//! A palace one behind is refused under read-only intent and migrated under
//! writer intent, in idempotent steps:
//! 1. backup — free-space check, copy, verify, manifest written last
//!    (`format_backup`); reused only when it matches the current source;
//! 2. the migration record, naming that backup, fsynced and renamed into place;
//!    `kg.redb` is then opened and its marker read, so a newer or unreadable
//!    file fails here and the fresh record is removed again;
//! 3. the `palace.json` mirror, written before the marker so an older binary
//!    refuses the palace from the moment the first palace byte changes;
//! 4. the `kg.redb` step — key rewrite plus both markers in one redb
//!    transaction, the commit point (`kg_redb::migrate`);
//! 5. the record is removed.
//!
//! A record on disk means "resume": the next writer open re-runs steps 3–5 on
//! the files as they are, behind the backup the record names, after checking
//! that backup still verifies. Any failure leaves the palace refused, never
//! opened un-migrated.
//! Test: format_migration_tests.rs, starting with
//! `a_format_0_palace_migrates_at_first_writer_open`.

use super::concurrent_open::{OpenIntent, OpenMode, try_open_primary_or_snapshot};
use super::format_backup::{self, backup_data_root, verify_backup};
use super::kg_redb::KgStoreRedb;
use super::kg_redb::migrate::migrate_kg_to_format;
use super::palace_format::{
    FormatState, KG_FILE, KgMarker, MIGRATION_RECORD, PALACE_FORMAT_SUPPORTED, PALACE_JSON,
    check_format, classify_with, read_marker, read_marker_without_writing,
};
use super::palace_store::PalaceStoreError;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

type Result<T> = std::result::Result<T, PalaceStoreError>;

/// The migration-in-progress record (`format_migration.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MigrationRecord {
    /// Palace id the migration belongs to.
    pub(crate) palace: String,
    /// Format being migrated from.
    pub(crate) from: u32,
    /// Format being migrated to.
    pub(crate) to: u32,
    /// The verified backup the migration runs behind.
    pub(crate) backup: PathBuf,
}

/// A point between two migration steps, for the fault seam.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    /// The backup is complete; no record yet.
    Backup,
    /// The record is on disk; nothing under the palace changed yet.
    Record,
    /// The mirror says the new format; `kg.redb` is unchanged.
    Mirror,
    /// `kg.redb` committed the new format; the record is still on disk.
    Kg,
}

/// Fault and environment seams; production uses [`Seams::default`].
#[derive(Clone, Copy)]
pub(crate) struct Seams {
    /// Passed to the backup.
    pub(crate) backup: format_backup::Seams,
    /// Runs after each [`Step`]; an `Err` stops the migration there, exactly
    /// as a crash would leave it.
    pub(crate) after: fn(Step) -> std::io::Result<()>,
}

impl Default for Seams {
    fn default() -> Self {
        Self {
            backup: format_backup::Seams::default(),
            after: |_| Ok(()),
        }
    }
}

/// Gate a palace directory before any of its stores opens, migrating it when
/// the caller is the writer.
///
/// Why: `PalaceHandle::open_with_intent_purging` opens `index.usearch.redb`
/// before `kg.redb`, so the gate — and the migration — run on the directory
/// first (ADR-0067 D3 rules 1 and 2).
/// What: newer, split or unreadable → refused, no byte changed. Current →
/// `Ok`; a writer rewrites a missing or lower mirror. Behind → read-only is
/// `FormatNeedsMigration`; a writer runs the migration in the module doc.
/// Unverified (a held or crash-repair `kg.redb`) → a writer reads the marker
/// on an exclusive open first, so an unreadable or newer KG is refused before
/// the vector store writes; a read-only open relies on the KG's own gate.
/// Test: `a_format_0_palace_migrates_at_first_writer_open`,
/// `a_read_only_open_of_a_format_0_palace_is_refused_and_writes_nothing`,
/// `n_plus_one_palace_is_refused_and_bytes_unchanged`.
pub(crate) fn open_gate(data_dir: &Path, palace: &str, intent: OpenIntent) -> anyhow::Result<()> {
    open_gate_with(data_dir, palace, intent, Seams::default())
}

/// [`open_gate`] with explicit [`Seams`].
pub(crate) fn open_gate_with(
    data_dir: &Path,
    palace: &str,
    intent: OpenIntent,
    seams: Seams,
) -> anyhow::Result<()> {
    let to = PALACE_FORMAT_SUPPORTED;
    let writer = intent == OpenIntent::Writer;
    match classify_with(data_dir, palace, to)? {
        FormatState::Current { repair_mirror } => {
            if repair_mirror && writer {
                write_mirror(data_dir, to)?;
            }
            Ok(())
        }
        // A KG this process holds was gated when it was opened.
        FormatState::Unverified if !writer || KgStoreRedb::is_cached(&data_dir.join(KG_FILE)) => {
            Ok(())
        }
        FormatState::Unverified => {
            if verify_kg_for_writer(&data_dir.join(KG_FILE), palace, to)? {
                migrate(data_dir, palace, to, seams)?;
            }
            Ok(())
        }
        FormatState::Behind { found } if !writer => Err(PalaceStoreError::FormatNeedsMigration {
            palace: palace.to_string(),
            found,
            supported: to,
        }
        .into()),
        FormatState::Behind { .. } => Ok(migrate(data_dir, palace, to, seams)?),
    }
}

/// Read a deferred marker on an exclusive open; `true` when the KG is one
/// behind and must be migrated.
///
/// Why: a crash-repair or redb 2 file cannot be opened read-only; without this
/// the vector store would open, and write, before the KG refused the palace.
/// What: the open's own error (lock held, incompatible file) is returned as
/// is; a newer or further-behind marker is refused; the handle is closed
/// before the migration or the stores open.
/// Test: `a_writer_open_refuses_an_incompatible_kg_with_bytes_unchanged`.
fn verify_kg_for_writer(kg_path: &Path, palace: &str, to: u32) -> anyhow::Result<bool> {
    let (db, _guard, _mode) = try_open_primary_or_snapshot(kg_path, OpenIntent::Writer)?;
    let found = match read_marker(&*db, kg_path)? {
        KgMarker::Fresh => return Ok(false),
        KgMarker::Stamped(v) => v,
        KgMarker::Unstamped | KgMarker::Deferred => 0,
    };
    match check_format(palace, found, to) {
        Ok(()) => Ok(false),
        Err(PalaceStoreError::FormatNeedsMigration { .. }) => Ok(true),
        Err(e) => Err(e.into()),
    }
}

/// Run (or resume) the migration to `to`; see the module doc for the steps.
///
/// What: the KG is opened, and its marker read, before the mirror is written,
/// so a newer or unreadable `kg.redb` fails the migration while nothing under
/// the palace has changed; a fresh record is then removed again, leaving the
/// palace exactly at format 0.
/// Test: `a_fault_after_each_step_leaves_a_palace_the_next_writer_open_completes`,
/// `a_blocked_backup_aborts_before_any_palace_byte_changes`.
fn migrate(data_dir: &Path, palace: &str, to: u32, seams: Seams) -> Result<()> {
    let from = to.saturating_sub(1);
    let failed = |reason: String| PalaceStoreError::FormatMigrationFailed {
        palace: palace.to_string(),
        from,
        to,
        reason,
    };
    let fault =
        |step: Step| (seams.after)(step).map_err(|e| failed(format!("after {step:?}: {e}")));

    let (backup, resumed) = match read_record(data_dir)? {
        Some(record) => {
            check_resume(&record, palace, from, to).map_err(failed)?;
            (record.backup, true)
        }
        None => {
            let root = backup_data_root(data_dir);
            let backup =
                format_backup::ensure_with(&root, palace, data_dir, from, to, seams.backup)?;
            fault(Step::Backup)?;
            let record = MigrationRecord {
                palace: palace.to_string(),
                from,
                to,
                backup: backup.clone(),
            };
            write_record(data_dir, &record)?;
            fault(Step::Record)?;
            (backup, false)
        }
    };

    let kg_path = data_dir.join(KG_FILE);
    let kg = match open_kg(&kg_path, palace, to, &failed) {
        Ok(kg) => kg,
        Err(e) => {
            // #9274: nothing under the palace changed but the record; drop it
            // so the palace is plainly at format `from` again.
            if !resumed {
                remove_record(data_dir)?;
            }
            return Err(e);
        }
    };

    write_mirror(data_dir, to)?;
    fault(Step::Mirror)?;

    let rows = match kg {
        Some(db) => migrate_kg_to_format(&db, to).map_err(|e| failed(format!("{e:#}")))?,
        None => 0,
    };
    fault(Step::Kg)?;

    remove_record(data_dir)?;
    tracing::info!(palace, from, to, rows, backup = %backup.display(),
        "#9274: migrated palace format");
    Ok(())
}

/// Open `kg.redb` for the KG step and refuse a newer file.
///
/// Why: the pre-open read can be deferred (a held or crash-repair file), so
/// the marker is read again on the opened database before anything is
/// written (ADR-0067 D3 rule 1). An incompatible file is refused, never
/// recreated (ADR-0067 D5).
/// What: `None` when there is no file or it already names `to`.
fn open_kg(
    kg_path: &Path,
    palace: &str,
    to: u32,
    failed: &dyn Fn(String) -> PalaceStoreError,
) -> Result<Option<std::sync::Arc<redb::Database>>> {
    if read_marker_without_writing(kg_path)? == KgMarker::Stamped(to)
        || matches!(kg_path.try_exists(), Ok(false))
    {
        return Ok(None);
    }
    let (db, _guard, mode) = try_open_primary_or_snapshot(kg_path, OpenIntent::Writer)
        .map_err(|e| failed(format!("open {}: {e:#}", kg_path.display())))?;
    if mode != OpenMode::ReadWrite {
        return Err(failed(format!("{} opened as {mode:?}", kg_path.display())));
    }
    match read_marker(&*db, kg_path)? {
        KgMarker::Stamped(v) if v > to => Err(PalaceStoreError::FormatTooNew {
            palace: palace.to_string(),
            found: v,
            supported: to,
        }),
        KgMarker::Stamped(v) if v == to => Ok(None),
        _ => Ok(Some(db)),
    }
}

/// A resumed migration must name this palace, this move, and a backup that
/// still verifies against its own manifest (PR2 critic MEDIUM, #9274).
///
/// Test: `a_resume_refuses_a_record_whose_backup_no_longer_verifies`.
fn check_resume(
    record: &MigrationRecord,
    palace: &str,
    from: u32,
    to: u32,
) -> std::result::Result<(), String> {
    if record.palace != palace || record.from != from || record.to != to {
        return Err(format!(
            "the migration record names palace {} format {} -> {}",
            record.palace, record.from, record.to
        ));
    }
    let manifest = verify_backup(&record.backup)
        .map_err(|e| format!("the backup the migration record names does not verify: {e}"))?;
    if manifest.palace != palace || manifest.from != from || manifest.to != to {
        return Err(format!(
            "backup {} belongs to palace {} format {} -> {}",
            record.backup.display(),
            manifest.palace,
            manifest.from,
            manifest.to
        ));
    }
    Ok(())
}

/// Read the migration record; `None` when there is none, fail closed on bytes
/// that do not parse.
pub(crate) fn read_record(data_dir: &Path) -> Result<Option<MigrationRecord>> {
    let path = data_dir.join(MIGRATION_RECORD);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(PalaceStoreError::Io { path, source }),
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|source| PalaceStoreError::Json { path, source })
}

/// Write the record through a fsynced temp file and a rename.
fn write_record(data_dir: &Path, record: &MigrationRecord) -> Result<()> {
    let path = data_dir.join(MIGRATION_RECORD);
    let bytes = serde_json::to_vec_pretty(record).map_err(|source| PalaceStoreError::Json {
        path: path.clone(),
        source,
    })?;
    write_atomic(data_dir, &path, &bytes)
}

/// Remove the record; the migration is complete once this is durable.
fn remove_record(data_dir: &Path) -> Result<()> {
    let path = data_dir.join(MIGRATION_RECORD);
    std::fs::remove_file(&path).map_err(|source| PalaceStoreError::Io { path, source })?;
    sync_dir(data_dir)
}

/// Set `palace.json`'s `format_version` to `format`, keeping every other field.
///
/// Why: the mirror lets a binary refuse a newer palace without opening redb
/// (ADR-0067 D1); a `serde_json::Value` round trip keeps fields this binary
/// does not know. A palace with no `palace.json` has no mirror to write.
/// Test: `a_lower_mirror_is_repaired_by_a_writer_and_the_marker_wins`.
pub(crate) fn write_mirror(data_dir: &Path, format: u32) -> Result<()> {
    let path = data_dir.join(PALACE_JSON);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(PalaceStoreError::Io { path, source }),
    };
    let json_err = |source| PalaceStoreError::Json {
        path: path.clone(),
        source,
    };
    let mut value: serde_json::Value = serde_json::from_slice(&bytes).map_err(json_err)?;
    let Some(object) = value.as_object_mut() else {
        return Err(PalaceStoreError::Io {
            path: path.clone(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "palace.json is not a JSON object",
            ),
        });
    };
    object.insert("format_version".into(), serde_json::json!(format));
    let out = serde_json::to_vec_pretty(&value).map_err(json_err)?;
    write_atomic(data_dir, &path, &out)
}

/// Write `bytes` to `path` via `<path>.tmp`, fsynced, renamed, dir fsynced.
fn write_atomic(dir: &Path, path: &Path, bytes: &[u8]) -> Result<()> {
    let io = |p: &Path, source| PalaceStoreError::Io {
        path: p.to_path_buf(),
        source,
    };
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".format.tmp");
    let tmp = PathBuf::from(tmp);
    let mut f = std::fs::File::create(&tmp).map_err(|e| io(&tmp, e))?;
    f.write_all(bytes).map_err(|e| io(&tmp, e))?;
    f.sync_all().map_err(|e| io(&tmp, e))?;
    drop(f);
    std::fs::rename(&tmp, path).map_err(|e| io(path, e))?;
    sync_dir(dir)
}

/// fsync a directory so a rename inside it is durable (no-op off unix).
fn sync_dir(dir: &Path) -> Result<()> {
    if cfg!(unix) {
        std::fs::File::open(dir)
            .and_then(|f| f.sync_all())
            .map_err(|source| PalaceStoreError::Io {
                path: dir.to_path_buf(),
                source,
            })?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "format_migration_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "format_migration_fault_tests.rs"]
mod fault_tests;
