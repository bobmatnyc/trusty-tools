#![allow(unused_imports, dead_code, unused_variables)]
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

#[path = "snapshot_restore.rs"]
mod restore;
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

// #9282 RED STUB: the API without the behaviour, so the tests compile and fail.
pub(crate) fn snapshot_palace(
    handle: &PalaceHandle,
    chat_sessions: Option<&ChatSessionStore>,
    dest: Option<&Path>,
    hook: SnapshotHook,
) -> Result<PalaceSnapshot> {
    Err(SnapshotError::Redb {
        op: "snapshot",
        path: PathBuf::new(),
        reason: "not implemented".into(),
    })
}

/// Red stub: copies every byte, torn final line included.
pub(crate) fn complete_lines(bytes: &[u8]) -> &[u8] {
    bytes
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

/// Map any displayable redb or anyhow error into [`SnapshotError::Redb`].
pub(crate) fn redb_err<E: std::fmt::Display>(
    op: &'static str,
    path: &Path,
) -> impl FnOnce(E) -> SnapshotError {
    let path = path.to_path_buf();
    move |e| SnapshotError::Redb {
        op,
        path,
        reason: format!("{e:#}"),
    }
}

#[cfg(test)]
#[path = "snapshot_tests.rs"]
mod tests;
