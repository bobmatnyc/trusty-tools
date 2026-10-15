//! The palace format migration, run lazily at a writer-intent open (#9274;
//! the 1 → 2 step since #9275, chained from any older format since #9918).
//!
//! Why: ADR-0067 D3 rules 2–4. A palace behind this format is migrated by its
//! first writer-intent open — the daemon's startup hydration opens every palace
//! that way (ADR-0067 Consequences). The migration may not write
//! until a verified backup exists, and a crash or error at any step must leave
//! the palace at the format of its last committed step — never opened
//! half-done. #9918: a palace several formats behind runs every step in order.
//! What: [`open_gate`] classifies the palace (`palace_format::classify_with`).
//! A current palace opens; a writer also rewrites a missing or lower mirror.
//! A palace behind is refused under read-only intent and migrated under
//! writer intent, in idempotent steps, one migration of a palace at a time
//! (`format_migration_lock`):
//! 1. backup — free-space check, copy, verify, manifest written last
//!    (`format_backup`); reused only when it matches the current source;
//! 2. `kg.redb` is opened exclusively and its marker read, so a newer,
//!    unreadable or held file fails here with no record written; then the
//!    migration record, naming that backup, fsynced and renamed into place;
//! 3. the `palace.json` mirror, written before the marker so an older binary
//!    refuses the palace from the moment the first palace byte changes;
//! 4. the `kg.redb` step — guarded, its table work and both markers in one
//!    redb transaction, that step's commit point (`kg_redb::migrate`); the
//!    vector index is not touched and nothing is re-embedded; #9918: steps 3–4
//!    repeat for each format from the one `kg.redb` holds up to this one;
//! 5. the record is removed, then `kg.redb` is released.
//!
//! A record on disk means "resume": the next writer open re-runs steps 3–5 on
//! the files as they are, from the format `kg.redb` holds, behind the backup
//! the record names, after checking
//! that backup still verifies. Any failure leaves the palace refused, never
//! opened un-migrated.
//! Test: format_migration_tests.rs, starting with
//! `a_format_1_palace_migrates_to_2_at_first_writer_open`.

use super::concurrent_open::{OpenIntent, OpenMode, SnapshotGuard, try_open_primary_or_snapshot};
use super::format_backup::{self, backup_data_root, verify_backup};
use super::format_migration_lock::{MigrationLock, PalaceTurn};
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
/// Test: `a_format_1_palace_migrates_to_2_at_first_writer_open`,
/// `a_read_only_open_of_a_format_1_palace_is_refused_and_writes_nothing`,
/// `n_plus_one_palace_is_refused_and_bytes_unchanged`.
pub(crate) fn open_gate(data_dir: &Path, palace: &str, intent: OpenIntent) -> anyhow::Result<()> {
    open_gate_with(data_dir, palace, intent, Seams::default())
}

/// [`open_gate`] with explicit [`Seams`].
///
/// What: a writer runs the gate under this process's [`PalaceTurn`] for the
/// palace, and a migration also under the cross-process [`MigrationLock`],
/// classifying again once each is held (#9274, PR3 critic HIGH 1).
pub(crate) fn open_gate_with(
    data_dir: &Path,
    palace: &str,
    intent: OpenIntent,
    seams: Seams,
) -> anyhow::Result<()> {
    let to = PALACE_FORMAT_SUPPORTED;
    // #9659: an observer reads no marker on the live kg.redb, not even shared.
    if intent == OpenIntent::Observer {
        return super::observer_open::observer_gate(data_dir, palace);
    }
    if intent != OpenIntent::Writer {
        return match classify_with(data_dir, palace, to)? {
            FormatState::Behind { found } => Err(PalaceStoreError::FormatNeedsMigration {
                palace: palace.to_string(),
                found,
                supported: to,
            }
            .into()),
            // A deferred marker is read by the KG's own post-open gate.
            FormatState::Current { .. } | FormatState::Unverified => Ok(()),
        };
    }
    // #9274: a second writer gate of this palace waits here, then sees the
    // first one's result.
    let _turn = PalaceTurn::acquire(data_dir);
    let Some(found) = writer_must_migrate(data_dir, palace, to)? else {
        return Ok(());
    };
    // #9274: another process may migrate this palace; fail before any write,
    // and classify again in case it finished before the lock was free.
    let _lock = MigrationLock::try_acquire(data_dir, palace, found, to)?;
    if let Some(found) = writer_must_migrate(data_dir, palace, to)? {
        migrate(data_dir, palace, found, to, seams)?;
    }
    Ok(())
}

/// Classify for a writer: the format to migrate from, when it must be.
///
/// What: current → `None`, after rewriting a missing or lower mirror;
/// unverified → the marker read on an exclusive open, unless this process
/// already holds the KG (it was gated when it was opened); behind → `found`.
fn writer_must_migrate(data_dir: &Path, palace: &str, to: u32) -> anyhow::Result<Option<u32>> {
    let kg_path = data_dir.join(KG_FILE);
    match classify_with(data_dir, palace, to)? {
        FormatState::Current { repair_mirror } => {
            if repair_mirror {
                write_mirror(data_dir, to)?;
            }
            Ok(None)
        }
        FormatState::Unverified if KgStoreRedb::is_cached(&kg_path) => Ok(None),
        FormatState::Unverified => verify_kg_for_writer(&kg_path, palace, to),
        FormatState::Behind { found } => Ok(Some(found)),
    }
}

/// Read a deferred marker on an exclusive open; its format when the KG is
/// behind and must be migrated.
///
/// Why: a crash-repair or redb 2 file cannot be opened read-only; without this
/// the vector store would open, and write, before the KG refused the palace.
/// What: the open's own error (lock held, incompatible file) is returned as
/// is; a newer or further-behind marker is refused; the handle is closed
/// before the migration or the stores open.
/// Test: `a_writer_open_refuses_an_incompatible_kg_with_bytes_unchanged`.
fn verify_kg_for_writer(kg_path: &Path, palace: &str, to: u32) -> anyhow::Result<Option<u32>> {
    let (db, _guard, _mode) = try_open_primary_or_snapshot(kg_path, OpenIntent::Writer)?;
    let found = match read_marker(&*db, kg_path)? {
        KgMarker::Fresh => return Ok(None),
        KgMarker::Stamped(v) => v,
        KgMarker::Unstamped | KgMarker::Deferred => 0,
    };
    match check_format(palace, found, to) {
        Ok(()) => Ok(None),
        Err(PalaceStoreError::FormatNeedsMigration { .. }) => Ok(Some(found)),
        Err(e) => Err(e.into()),
    }
}

/// Run (or resume) the migration from `from` to `to`; see the module doc.
///
/// What: runs under the caller's [`MigrationLock`]. The record is read under
/// it; `kg.redb` is opened exclusively before the record is written (or, on a
/// resume, before anything is written) and held until the record is removed,
/// so a newer, unreadable or held KG fails the migration with no record left.
/// #9918: one backup and one record cover the chain; a failed step is
/// `FormatMigrationFailed` for that step and stops it.
/// Test: `a_format_0_palace_migrates_to_2_at_first_writer_open`,
/// `a_fault_between_chain_steps_leaves_a_resumable_palace`,
/// `a_fault_after_each_step_leaves_a_palace_the_next_writer_open_completes`,
/// `a_blocked_backup_aborts_before_any_palace_byte_changes`,
/// `two_writer_opens_in_one_process_migrate_a_palace_once`,
/// `a_failed_step_does_not_advance_the_stored_format_or_remove_the_record`.
fn migrate(data_dir: &Path, palace: &str, from: u32, to: u32, seams: Seams) -> Result<()> {
    let failed = |reason: String| PalaceStoreError::FormatMigrationFailed {
        palace: palace.to_string(),
        from,
        to,
        reason,
    };
    let fault =
        |step: Step| (seams.after)(step).map_err(|e| failed(format!("after {step:?}: {e}")));
    let kg_path = data_dir.join(KG_FILE);

    let (backup, kg) = match read_record(data_dir)? {
        Some(record) => {
            check_resume(&record, palace, from, to).map_err(failed)?;
            (record.backup, open_kg(&kg_path, palace, to, &failed)?)
        }
        None => {
            let root = backup_data_root(data_dir);
            let backup =
                format_backup::ensure_with(&root, palace, data_dir, from, to, seams.backup)?;
            fault(Step::Backup)?;
            // #9274: held from before the record exists until it is removed;
            // the backup is taken first so it keeps the unopened KG bytes.
            let kg = open_kg(&kg_path, palace, to, &failed)?;
            let record = MigrationRecord {
                palace: palace.to_string(),
                from,
                to,
                backup: backup.clone(),
            };
            write_record(data_dir, &record)?;
            fault(Step::Record)?;
            (backup, kg)
        }
    };

    // #9918: the chain starts where kg.redb stands; with no kg.redb, at `from`.
    let at = kg.as_ref().map_or(from, |held| held.at);
    if at < from {
        return Err(failed(format!("kg.redb is at format {at}, below {from}")));
    }
    for step in at..to {
        write_mirror(data_dir, step + 1)?;
        fault(Step::Mirror)?;
        if let Some(held) = &kg {
            migrate_kg_to_format(&held.db, step + 1).map_err(|e| {
                PalaceStoreError::FormatMigrationFailed {
                    palace: palace.to_string(),
                    from: step,
                    to: step + 1,
                    reason: format!("{e:#}"),
                }
            })?;
        }
        fault(Step::Kg)?;
    }
    if at == to {
        write_mirror(data_dir, to)?; // a resume after the last step committed
    }

    remove_record(data_dir)?;
    drop(kg);
    tracing::info!(palace, from, to, backup = %backup.display(),
        "#9274: migrated palace format");
    Ok(())
}

/// `kg.redb`, opened exclusively for the migration.
struct HeldKg {
    db: std::sync::Arc<redb::Database>,
    /// The format the file holds; 0 when unstamped (#9918).
    at: u32,
    _guard: SnapshotGuard,
}

/// Open `kg.redb` exclusively for the migration and refuse a newer file.
///
/// Why: the pre-open read can be deferred (a held or crash-repair file), so
/// the marker is read again on the opened database before anything is
/// written (ADR-0067 D3 rule 1). An incompatible file is refused, never
/// recreated (ADR-0067 D5). The open is the KG half of the cross-process
/// exclusion: another process's open of the file fails while it is held.
/// What: `None` when there is no file. A readable marker above `to` is refused
/// before the open writes a byte.
fn open_kg(
    kg_path: &Path,
    palace: &str,
    to: u32,
    failed: &dyn Fn(String) -> PalaceStoreError,
) -> Result<Option<HeldKg>> {
    let too_new = |found| PalaceStoreError::FormatTooNew {
        palace: palace.to_string(),
        found,
        supported: to,
    };
    if matches!(kg_path.try_exists(), Ok(false)) {
        return Ok(None);
    }
    if let KgMarker::Stamped(v) = read_marker_without_writing(kg_path)?
        && v > to
    {
        return Err(too_new(v));
    }
    let (db, guard, mode) = try_open_primary_or_snapshot(kg_path, OpenIntent::Writer)
        .map_err(|e| failed(format!("open {}: {e:#}", kg_path.display())))?;
    if mode != OpenMode::ReadWrite {
        return Err(failed(format!("{} opened as {mode:?}", kg_path.display())));
    }
    let at = match read_marker(&*db, kg_path)? {
        KgMarker::Stamped(v) if v > to => return Err(too_new(v)),
        KgMarker::Stamped(v) => v,
        _ => 0,
    };
    Ok(Some(HeldKg {
        db,
        at,
        _guard: guard,
    }))
}

/// A resumed migration must name this palace, this move, and a backup that
/// still verifies against its own manifest (PR2 critic MEDIUM, #9274).
/// #9918: a record may end below `to` (0.33.x's 0 → 1); its backup predates
/// every step, so the chain resumes behind it.
/// Test: `a_resume_refuses_a_record_whose_backup_no_longer_verifies`,
/// `a_stale_0_to_1_record_is_resumed_by_the_chain`.
fn check_resume(
    record: &MigrationRecord,
    palace: &str,
    from: u32,
    to: u32,
) -> std::result::Result<(), String> {
    if record.palace != palace || record.from != from || !(from..=to).contains(&record.to) {
        return Err(format!(
            "the migration record names palace {} format {} -> {}",
            record.palace, record.from, record.to
        ));
    }
    let manifest = verify_backup(&record.backup)
        .map_err(|e| format!("the backup the migration record names does not verify: {e}"))?;
    if manifest.palace != palace || manifest.from != from || manifest.to != record.to {
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

/// Write `bytes` to `path` via a temp file, fsynced, renamed, dir fsynced.
fn write_atomic(dir: &Path, path: &Path, bytes: &[u8]) -> Result<()> {
    let io = |p: &Path, source| PalaceStoreError::Io {
        path: p.to_path_buf(),
        source,
    };
    let tmp = tmp_path(path);
    let written = (|| {
        let mut f = std::fs::File::create(&tmp).map_err(|e| io(&tmp, e))?;
        f.write_all(bytes).map_err(|e| io(&tmp, e))?;
        f.sync_all().map_err(|e| io(&tmp, e))?;
        drop(f);
        std::fs::rename(&tmp, path).map_err(|e| io(path, e))
    })();
    if let Err(e) = written {
        if let Err(rm) = std::fs::remove_file(&tmp)
            && rm.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(tmp = %tmp.display(), "#9274: temp file left behind: {rm}");
        }
        return Err(e);
    }
    sync_dir(dir)
}

/// A temp name no other writer in any process uses: `<path>.<pid>.<n>.format.tmp`.
///
/// Why: a fixed name let two openers write one temp file (PR3 critic HIGH 1).
/// Test: `temp_names_never_repeat`.
fn tmp_path(path: &Path) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".{}.{n}.format.tmp", std::process::id()));
    PathBuf::from(tmp)
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

#[cfg(test)]
#[path = "format_migration_lock_tests.rs"]
mod lock_tests;

#[cfg(test)]
#[path = "format_migration_v2_tests.rs"]
mod v2_tests;

#[cfg(test)]
#[path = "format_migration_chain_tests.rs"]
mod chain_tests;
