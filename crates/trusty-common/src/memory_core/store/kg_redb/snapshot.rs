//! Consistent snapshot of a live palace, and its offline restore (#9282).
//!
//! Why: no path copied a palace while the daemon served it. A raw `cp` of
//! `kg.redb` races the single-writer actor, redb file growth and the dream
//! compaction's rename, so the copy can be torn. The trusty-memory 1.0 gate
//! needs a backup taken from a live daemon that reopens cleanly.
//! What: [`snapshot_palace`] (called through `PalaceHandle::snapshot`) copies
//! the ADR-0067 D2 primary files into
//! `<data root>/backups/manual/<palace>/<UTC>/`. `kg.redb` and
//! `chat_sessions.redb` are each copied row by row from ONE redb read
//! transaction (MVCC, so writers are never paused). The append-only journals
//! are copied up to their last complete line. Derived files — the vector
//! index, `bm25/`, `l1_cache.json` — are left out; a restored palace
//! re-embeds. A [`SNAPSHOT_MANIFEST`] with per-table row counts and a SHA-256
//! per file is written last, and the directory is renamed into place only
//! after it, so a snapshot without a manifest never exists under its final
//! name. [`restore_palace_snapshot`] verifies that manifest in full before it
//! writes anything, and writes only into an empty palace root.
//! Every failure is an `Err`; no branch degrades to a warning or a partial
//! snapshot.
//! Test: `snapshot_tests.rs`, starting with
//! `snapshot_under_a_concurrent_writer_matches_its_manifest`.

use super::copy_tables::{BYTE_TABLES, STR_TABLES, copy_all, copy_str_table, known_table_names};
use super::store::KgStoreRedb;
use crate::memory_core::maintenance_log::{
    MAINTENANCE_LOG_FILENAME, MAINTENANCE_LOG_ROTATED_FILENAME,
};
use crate::memory_core::retrieval::PalaceHandle;
use crate::memory_core::store::chat_sessions::{CHAT_ASSETS_TABLE, ChatSessionStore};
use crate::memory_core::store::format_backup::backup_data_root;
use crate::memory_core::store::kg_store::SESSIONS;
use crate::memory_core::store::palace_format::{KgMarker, read_marker};
use chrono::{DateTime, Utc};
use redb::{Database, ReadTransaction, ReadableDatabase, ReadableTableMetadata, TableHandle};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[path = "snapshot_io.rs"]
mod io_util;
#[path = "snapshot_restore.rs"]
mod restore;
use io_util::{
    StagingDir, copy_verified, hash_file, io_err, redb_err, sibling, sync_file, write_verified,
};
pub use restore::restore_palace_snapshot;

/// Version of the snapshot layout and manifest this build writes and reads.
pub const SNAPSHOT_FORMAT_VERSION: u32 = 1;
/// Value of [`SnapshotManifest::kind`].
pub const SNAPSHOT_KIND: &str = "palace-snapshot";
/// The manifest file; its presence marks a snapshot complete.
pub const SNAPSHOT_MANIFEST: &str = "manifest.json";
/// The KG database inside a palace and inside a snapshot.
pub const KG_FILE: &str = "kg.redb";
/// The chat-session database inside a palace and inside a snapshot.
pub const CHAT_FILE: &str = "chat_sessions.redb";
/// This palace's lines of the shared `drawer_counts.jsonl` (#9283).
pub const DRAWER_COUNTS_FILE: &str = "drawer_counts.jsonl";
/// Small metadata files copied whole.
const PLAIN_FILES: [&str; 2] = ["palace.json", "identity.txt"];
/// Append-only journals, in read order. The live file is read BEFORE the
/// rotated one: a rotation between the two reads then duplicates lines
/// rather than losing them.
const JOURNAL_FILES: [&str; 2] = [MAINTENANCE_LOG_FILENAME, MAINTENANCE_LOG_ROTATED_FILENAME];
const TMP_SUFFIX: &str = ".tmp";

/// Every way a snapshot or a restore can fail. None is recoverable in place.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SnapshotError {
    /// The handle is in-memory; there are no files to copy.
    #[error("palace {palace} has no data directory; an in-memory palace cannot be snapshotted")]
    NoDataDir { palace: String },
    /// The handle reads a raw-copied redb snapshot, which may itself be torn.
    #[error("palace {palace} is open read-only from a copied file; snapshot it through the daemon that owns it")]
    ReadOnlyHandle { palace: String },
    /// The snapshot directory already exists.
    #[error("snapshot destination {path} already exists")]
    DestinationExists { path: PathBuf },
    /// A filesystem call failed.
    #[error("{op} {path}: {source}")]
    Io {
        op: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    /// A redb open, transaction or copy failed.
    #[error("redb {op} {path}: {reason}")]
    Redb {
        op: &'static str,
        path: PathBuf,
        reason: String,
    },
    /// The database holds tables this build cannot copy; copying would drop them.
    #[error("{path} carries table(s) this build cannot copy ({tables}); nothing was written")]
    UnknownTables { path: PathBuf, tables: String },
    /// A table's row count differs between source, copy or manifest.
    #[error("{path}: table {table} has {actual} rows, expected {expected}")]
    CountMismatch {
        path: PathBuf,
        table: String,
        expected: u64,
        actual: u64,
    },
    /// A file's bytes differ from the hash recorded for it.
    #[error("checksum mismatch on {path}: expected {expected}, found {actual}")]
    ChecksumMismatch {
        path: PathBuf,
        expected: String,
        actual: String,
    },
    /// The manifest could not be written or parsed.
    #[error("manifest {path}: {source}")]
    ManifestJson {
        path: PathBuf,
        source: serde_json::Error,
    },
    /// The manifest is well-formed JSON but not something this build restores.
    #[error("manifest {path} is not restorable: {reason}")]
    InvalidManifest { path: PathBuf, reason: String },
    /// The snapshot's palace format is newer than this build supports.
    #[error("snapshot palace format {found} is newer than this build supports ({supported})")]
    PalaceFormatTooNew { found: u32, supported: u32 },
    /// Restore refuses any target that already holds an entry.
    #[error("restore target {path} is not empty; restore writes only into an empty palace root")]
    TargetNotEmpty { path: PathBuf },
}

type Result<T> = std::result::Result<T, SnapshotError>;

/// One file in a snapshot, as recorded in [`SNAPSHOT_MANIFEST`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotFile {
    /// File name inside the snapshot directory.
    pub name: String,
    /// Byte length.
    pub len: u64,
    /// Lower-case hex SHA-256.
    pub sha256: String,
}

/// The contents of [`SNAPSHOT_MANIFEST`].
///
/// Why: restore must prove a snapshot whole before it writes a byte, and an
/// operator must be able to read what a snapshot holds without opening redb.
/// What: format and source versions, per-table row counts of both databases
/// as copied, drawer and room counts (the `drawers` and `rooms` rows) and a
/// length plus SHA-256 per file.
/// Test: `snapshot_under_a_concurrent_writer_matches_its_manifest`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotManifest {
    /// [`SNAPSHOT_FORMAT_VERSION`] of the writer.
    pub format_version: u32,
    /// Always [`SNAPSHOT_KIND`].
    pub kind: String,
    /// Palace id.
    pub palace: String,
    /// When the redb read transactions were opened.
    pub taken_at: DateTime<Utc>,
    /// trusty-common version that wrote the snapshot.
    pub writer_version: String,
    /// `kg_schema.palace_format` of the copy; `None` for a fresh KG.
    pub palace_format: Option<u32>,
    /// Row count per `kg.redb` table, as copied.
    pub kg_tables: BTreeMap<String, u64>,
    /// Row count per `chat_sessions.redb` table; empty when the palace has none.
    pub chat_tables: BTreeMap<String, u64>,
    /// Rows in `drawers`.
    pub drawer_count: u64,
    /// Rows in `rooms`.
    pub room_count: u64,
    /// Every file in the snapshot except the manifest.
    pub files: Vec<SnapshotFile>,
}

/// A completed snapshot: its directory and its manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PalaceSnapshot {
    /// The snapshot directory.
    pub dir: PathBuf,
    /// What [`SNAPSHOT_MANIFEST`] in `dir` records.
    pub manifest: SnapshotManifest,
}

/// Named points a test can fail a snapshot or restore at.
///
/// Why: the fsync and write failures this module must surface cannot be
/// provoked on demand, so the seam is explicit (the `CompactStep` pattern).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SnapshotStep {
    /// Before rows stream into a copied database.
    CopyDb,
    /// Before a whole file is written into the snapshot or the restore target.
    WriteFile,
    /// Before a copied file is fsynced.
    Fsync,
    /// After fsync, before the copy is re-hashed.
    Verify,
    /// Before the manifest is written.
    WriteManifest,
}

/// Test seam; production passes [`no_fault`].
pub(crate) type SnapshotHook = fn(SnapshotStep, &Path) -> io::Result<()>;

/// The production hook: never fails.
pub(crate) fn no_fault(_: SnapshotStep, _: &Path) -> io::Result<()> {
    Ok(())
}

// #9282: the method lives here, not in `retrieval/handle.rs`, which sits at
// the 500-SLOC cap.
impl PalaceHandle {
    /// Take a consistent snapshot of this palace's primary files (#9282).
    ///
    /// Why: a backup taken while the daemon serves the palace must reopen
    /// cleanly; a raw file copy races the writer and the compaction swap.
    /// What: copies `kg.redb` and `chat_sessions.redb` each from one redb read
    /// transaction (writers are not paused), the metadata files, the journals
    /// up to their last complete line, and this palace's `drawer_counts.jsonl`
    /// lines, then writes a checksummed manifest. `chat_sessions` lends the
    /// process's open chat store for this palace, if any; `dest` overrides the
    /// default `<data root>/backups/manual/<palace>/<UTC>/` and must not exist.
    /// Blocking: run it on `spawn_blocking` from async code.
    /// Test: `snapshot_under_a_concurrent_writer_matches_its_manifest`,
    /// `a_kg_copy_failure_leaves_no_snapshot`.
    pub fn snapshot(
        &self,
        chat_sessions: Option<&ChatSessionStore>,
        dest: Option<&Path>,
    ) -> Result<PalaceSnapshot> {
        snapshot_palace(self, chat_sessions, dest, no_fault)
    }
}

/// Take a consistent snapshot of `handle`'s palace.
///
/// Why: see the module doc. `PalaceHandle::snapshot` is the public entry.
/// What: `chat_sessions` is the process's open chat store for this palace,
/// when it has one; redb allows one handle per file per process, so the
/// caller that holds it must lend it. Without it the file is opened
/// read-only, which fails closed while another process holds it. `dest`
/// overrides the default directory and must not exist. Blocking I/O: call
/// from `spawn_blocking` in async code.
/// Test: `snapshot_under_a_concurrent_writer_matches_its_manifest`,
/// `a_kg_copy_failure_leaves_no_snapshot`.
pub(crate) fn snapshot_palace(
    handle: &PalaceHandle,
    chat_sessions: Option<&ChatSessionStore>,
    dest: Option<&Path>,
    hook: SnapshotHook,
) -> Result<PalaceSnapshot> {
    let palace = handle.id.as_str().to_string();
    let palace_dir = handle
        .data_dir
        .clone()
        .ok_or_else(|| SnapshotError::NoDataDir {
            palace: palace.clone(),
        })?;
    if handle.kg.is_read_only() {
        return Err(SnapshotError::ReadOnlyHandle { palace });
    }
    let taken_at = Utc::now();
    let final_dir = match dest {
        Some(d) => d.to_path_buf(),
        None => default_snapshot_dir(&palace_dir, &palace, taken_at),
    };
    if final_dir.try_exists().map_err(io_err("stat", &final_dir))? {
        return Err(SnapshotError::DestinationExists { path: final_dir });
    }
    let tmp = StagingDir::create(sibling(&final_dir, TMP_SUFFIX))?;

    let store = handle.kg.store();
    let (kg_tables, palace_format) = snapshot_kg(&store, &tmp.path.join(KG_FILE), hook)?;
    let chat_tables = snapshot_chat(&palace_dir, chat_sessions, &tmp.path.join(CHAT_FILE), hook)?;
    let mut files = Vec::new();
    for name in [KG_FILE, CHAT_FILE] {
        let path = tmp.path.join(name);
        if path.try_exists().map_err(io_err("stat", &path))? {
            files.push(finish_db_file(name, &path, hook)?);
        }
    }
    for name in PLAIN_FILES {
        if let Some(bytes) = read_optional(&palace_dir.join(name))? {
            files.push(write_verified(&tmp.path, name, &bytes, hook)?);
        }
    }
    for name in JOURNAL_FILES {
        if let Some(bytes) = read_optional(&palace_dir.join(name))? {
            files.push(write_verified(&tmp.path, name, complete_lines(&bytes), hook)?);
        }
    }
    if let Some(bytes) = drawer_count_lines(&palace_dir, &palace)? {
        files.push(write_verified(&tmp.path, DRAWER_COUNTS_FILE, &bytes, hook)?);
    }

    let manifest = SnapshotManifest {
        format_version: SNAPSHOT_FORMAT_VERSION,
        kind: SNAPSHOT_KIND.to_string(),
        palace,
        taken_at,
        writer_version: env!("CARGO_PKG_VERSION").to_string(),
        palace_format,
        drawer_count: kg_tables.get("drawers").copied().unwrap_or(0),
        room_count: kg_tables.get("rooms").copied().unwrap_or(0),
        kg_tables,
        chat_tables,
        files,
    };
    write_manifest(&tmp.path, &manifest, hook)?;
    tmp.commit_to(&final_dir)?;
    Ok(PalaceSnapshot {
        dir: final_dir,
        manifest,
    })
}

/// `<data root>/backups/manual/<palace>/<UTC>`, beside the migration backups.
fn default_snapshot_dir(palace_dir: &Path, palace: &str, at: DateTime<Utc>) -> PathBuf {
    backup_data_root(palace_dir)
        .join("backups")
        .join("manual")
        .join(palace)
        .join(at.format("%Y%m%dT%H%M%S%.3fZ").to_string())
}

/// Copy `kg.redb` from one read transaction; return its counts and format.
///
/// Why: one read transaction sees exactly one commit, so the copy is a
/// consistent state however many writes land during it, and redb MVCC means
/// no writer waits on it (ADR-0067 D2 ruling, #9282).
/// Test: `snapshot_under_a_concurrent_writer_matches_its_manifest`,
/// `a_kg_copy_failure_leaves_no_snapshot`.
fn snapshot_kg(
    store: &KgStoreRedb,
    dest: &Path,
    hook: SnapshotHook,
) -> Result<(BTreeMap<String, u64>, Option<u32>)> {
    let src = store.path().to_path_buf();
    let rtx = store
        .db()
        .begin_read()
        .map_err(redb_err("begin read", &src))?;
    guard_tables(&rtx, &src, &known_table_names())?;
    let names: Vec<&str> = BYTE_TABLES
        .iter()
        .map(|t| t.name())
        .chain(STR_TABLES.iter().map(|t| t.name()))
        .collect();
    let expected = table_counts(&rtx, &src, &names)?;
    hook(SnapshotStep::CopyDb, dest).map_err(io_err("copy into", dest))?;
    let copy = crate::redb_cache::create_palace_db(dest).map_err(redb_err("create", dest))?;
    copy_all(&rtx, &copy, None).map_err(redb_err("copy into", dest))?;
    drop(rtx);
    let counts = verify_counts(&copy, dest, &names, &expected)?;
    let format = match read_marker(&copy, dest).map_err(redb_err("read format of", dest))? {
        KgMarker::Stamped(n) => Some(n),
        KgMarker::Unstamped => Some(0),
        KgMarker::Fresh | KgMarker::Deferred => None,
    };
    Ok((counts, format))
}

/// Copy `chat_sessions.redb` from one read transaction, when the palace has one.
///
/// Test: `snapshot_under_a_concurrent_writer_matches_its_manifest`.
fn snapshot_chat(
    palace_dir: &Path,
    lent: Option<&ChatSessionStore>,
    dest: &Path,
    hook: SnapshotHook,
) -> Result<BTreeMap<String, u64>> {
    let src = palace_dir.join(CHAT_FILE);
    let rtx = match lent {
        Some(store) => store
            .redb()
            .begin_read()
            .map_err(redb_err("begin read", &src))?,
        None => {
            if !src.try_exists().map_err(io_err("stat", &src))? {
                return Ok(BTreeMap::new());
            }
            crate::redb_cache::open_palace_db_read_only(&src)
                .map_err(redb_err("open read-only", &src))?
                .begin_read()
                .map_err(redb_err("begin read", &src))?
        }
    };
    let defs = [SESSIONS, CHAT_ASSETS_TABLE];
    let names: Vec<&str> = defs.iter().map(|t| t.name()).collect();
    guard_tables(&rtx, &src, &names)?;
    let expected = table_counts(&rtx, &src, &names)?;
    hook(SnapshotStep::CopyDb, dest).map_err(io_err("copy into", dest))?;
    let copy = crate::redb_cache::create_palace_db(dest).map_err(redb_err("create", dest))?;
    let mut counts = Default::default();
    for def in defs {
        copy_str_table(&rtx, &copy, def, &mut counts).map_err(redb_err("copy into", dest))?;
    }
    drop(rtx);
    verify_counts(&copy, dest, &names, &expected)
}

/// Refuse a database holding a table outside `known`; copying would drop it.
fn guard_tables(rtx: &ReadTransaction, path: &Path, known: &[&str]) -> Result<()> {
    let unknown: Vec<String> = rtx
        .list_tables()
        .map_err(redb_err("list tables of", path))?
        .map(|h| h.name().to_string())
        .filter(|n| !known.contains(&n.as_str()))
        .collect();
    if unknown.is_empty() {
        Ok(())
    } else {
        Err(SnapshotError::UnknownTables {
            path: path.to_path_buf(),
            tables: unknown.join(", "),
        })
    }
}

/// Row count of each named table; an absent table counts zero.
pub(crate) fn table_counts(
    rtx: &ReadTransaction,
    path: &Path,
    names: &[&str],
) -> Result<BTreeMap<String, u64>> {
    let present: Vec<redb::UntypedTableHandle> = rtx
        .list_tables()
        .map_err(redb_err("list tables of", path))?
        .collect();
    let mut out: BTreeMap<String, u64> = names.iter().map(|n| (n.to_string(), 0)).collect();
    for handle in present {
        let name = handle.name().to_string();
        if let Some(slot) = out.get_mut(&name) {
            let table = rtx
                .open_untyped_table(handle)
                .map_err(redb_err("open table of", path))?;
            *slot = table.len().map_err(redb_err("count rows of", path))?;
        }
    }
    Ok(out)
}

/// Re-count the copy and require it to equal the source's counts.
fn verify_counts(
    copy: &Database,
    path: &Path,
    names: &[&str],
    expected: &BTreeMap<String, u64>,
) -> Result<BTreeMap<String, u64>> {
    let rtx = copy.begin_read().map_err(redb_err("begin read", path))?;
    let actual = table_counts(&rtx, path, names)?;
    check_counts(path, expected, &actual)?;
    Ok(actual)
}

/// Every table in `expected` must have the same count in `actual`.
pub(crate) fn check_counts(
    path: &Path,
    expected: &BTreeMap<String, u64>,
    actual: &BTreeMap<String, u64>,
) -> Result<()> {
    for (table, want) in expected {
        let got = actual.get(table).copied().unwrap_or(0);
        if got != *want {
            return Err(SnapshotError::CountMismatch {
                path: path.to_path_buf(),
                table: table.clone(),
                expected: *want,
                actual: got,
            });
        }
    }
    Ok(())
}

/// fsync a copied database and record its length and hash.
fn finish_db_file(name: &str, path: &Path, hook: SnapshotHook) -> Result<SnapshotFile> {
    sync_file(path, hook)?;
    hook(SnapshotStep::Verify, path).map_err(io_err("verify", path))?;
    let (len, sha256) = hash_file(path)?;
    Ok(SnapshotFile {
        name: name.to_string(),
        len,
        sha256,
    })
}

/// The bytes up to and including the last `\n`; a torn final line is dropped.
///
/// Test: `a_journal_torn_final_line_is_not_copied`.
pub(crate) fn complete_lines(bytes: &[u8]) -> &[u8] {
    match bytes.iter().rposition(|b| *b == b'\n') {
        Some(i) => &bytes[..=i],
        None => &[],
    }
}

/// This palace's complete lines of the shared `drawer_counts.jsonl`.
///
/// Why: the file sits beside the palace directories and carries one line per
/// palace per day (#9283); only this palace's lines belong in its snapshot.
/// What: `None` when the file is absent or holds no line for `palace`. A line
/// that is not a JSON object naming a palace names no palace, so no snapshot
/// takes it.
/// Test: `drawer_count_lines_are_filtered_to_the_palace`.
fn drawer_count_lines(palace_dir: &Path, palace: &str) -> Result<Option<Vec<u8>>> {
    let Some(parent) = palace_dir.parent() else {
        return Ok(None);
    };
    let Some(bytes) = read_optional(&parent.join(DRAWER_COUNTS_FILE))? else {
        return Ok(None);
    };
    let mut out = Vec::new();
    for line in complete_lines(&bytes).split_inclusive(|b| *b == b'\n') {
        let owner = serde_json::from_slice::<serde_json::Value>(line)
            .ok()
            .and_then(|v| v.get("palace").and_then(|p| p.as_str().map(str::to_owned)));
        if owner.as_deref() == Some(palace) {
            out.extend_from_slice(line);
        }
    }
    Ok((!out.is_empty()).then_some(out))
}

/// Read a whole file; `None` only when it does not exist.
fn read_optional(path: &Path) -> Result<Option<Vec<u8>>> {
    if !path.try_exists().map_err(io_err("stat", path))? {
        return Ok(None);
    }
    std::fs::read(path).map(Some).map_err(io_err("read", path))
}

/// Write the manifest through a fsynced temp file and a rename.
///
/// Test: `a_manifest_write_failure_leaves_no_snapshot`.
fn write_manifest(dir: &Path, manifest: &SnapshotManifest, hook: SnapshotHook) -> Result<()> {
    let path = dir.join(SNAPSHOT_MANIFEST);
    hook(SnapshotStep::WriteManifest, &path).map_err(io_err("write", &path))?;
    let bytes = serde_json::to_vec_pretty(manifest).map_err(|source| {
        SnapshotError::ManifestJson {
            path: path.clone(),
            source,
        }
    })?;
    let tmp = sibling(&path, TMP_SUFFIX);
    let mut f = std::fs::File::create(&tmp).map_err(io_err("create", &tmp))?;
    f.write_all(&bytes).map_err(io_err("write", &tmp))?;
    f.sync_all().map_err(io_err("fsync", &tmp))?;
    std::fs::rename(&tmp, &path).map_err(io_err("rename", &path))
}

#[cfg(test)]
#[path = "snapshot_tests.rs"]
mod tests;
