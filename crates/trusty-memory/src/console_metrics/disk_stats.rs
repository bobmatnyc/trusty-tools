//! Count a palace's drawers, vectors, rooms and triples without opening it.
//!
//! Why (#6372): `console_metrics` reports counts only for palaces resident in
//! the registry's LRU cache, because #1924 established that force-opening a
//! palace per poll thrashes the cache and drives RSS up. On a host with 94
//! palaces and 2 resident, that renders 92 rows as `—`, which reads as "those
//! palaces are empty" rather than "nobody asked". The counts, however, do not
//! need an open palace: every one of them is a redb B-tree length, and a
//! B-tree length is a field in the tree's root header.
//! What: [`read`] opens each of the palace's two redb files as a
//! [`redb::ReadOnlyDatabase`] — a SHARED `flock`, never a write transaction,
//! never a snapshot copy, never a migration — reads four table lengths plus one
//! subject-count sum, and closes. It never allocates a `Drawer`, never
//! rebuilds the HNSW graph, and never enters the LRU cache, so it cannot
//! reintroduce #1924.
//! Test: `disk_stats_counts_a_palace_that_was_never_opened`,
//! `disk_stats_refuses_a_palace_held_open_for_writing`.

use std::path::Path;

use redb::{ReadOnlyDatabase, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableError};
use trusty_common::memory_core::store::kg_store::{
    ACTIVE_SUBJECT_COUNTS, DELETED_VECTORS, DRAWERS, ROOMS, VECTORS,
};

/// The redb file holding drawers, rooms and triples, relative to the palace dir.
///
/// `PalaceHandle::open` passes `kg.db` and `KnowledgeGraph` rewrites the
/// extension; this reader names the real file because it does not go through
/// that open path.
const KG_FILE: &str = "kg.redb";

/// The redb file holding the vector index, relative to the palace dir.
///
/// `UsearchStore` appends `.redb` to the historical `index.usearch` name.
const VECTOR_FILE: &str = "index.usearch.redb";

/// One palace's counts, read from disk with the palace closed.
///
/// Why: the fields mirror what a cache-resident palace reports through
/// `PalaceRegistry::peek`, so a row's numbers mean the same thing whichever
/// path produced them and the dashboard needs no per-source arithmetic.
/// What: plain counts. `room_count` excludes the nil-uuid schema-marker row
/// that `ROOMS` reserves.
/// Test: `disk_stats_counts_a_palace_that_was_never_opened`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PalaceDiskStats {
    pub drawer_count: usize,
    pub vector_count: usize,
    pub kg_triple_count: usize,
    pub room_count: usize,
}

/// Count `data_dir`'s palace without opening it.
///
/// Why: see the module docs — this is the path that gives a non-resident
/// palace real numbers instead of a `—`.
/// What: reads `kg.redb` for drawers, rooms and active triples, then
/// `index.usearch.redb` for live vectors. Both opens take a shared lock, so a
/// palace another process (or this daemon) holds open for writing returns
/// `Err` rather than a wrong number — the caller renders that as unavailable.
///
/// # Errors
///
/// The palace directory has no redb file yet, a file is unreadable, or a
/// writer holds it. The message is operator-facing and reaches the dashboard
/// as the row's hint.
///
/// Test: `disk_stats_counts_a_palace_that_was_never_opened`,
/// `disk_stats_refuses_a_palace_held_open_for_writing`,
/// `disk_stats_reports_a_missing_palace_directory`.
pub(super) fn read(data_dir: &Path) -> Result<PalaceDiskStats, String> {
    let kg = open_read_only(&data_dir.join(KG_FILE))?;
    let rtx = kg
        .begin_read()
        .map_err(|e| format!("begin read on {KG_FILE}: {e}"))?;

    let drawer_count = table_len(&rtx, DRAWERS, "drawers")?;
    let room_count = room_count(&rtx)?;
    let kg_triple_count = active_triple_count(&rtx)?;
    drop(rtx);
    drop(kg);

    // A palace can hold drawers before anything is embedded, so a missing
    // vector file is zero vectors, not a failed read.
    let vector_path = data_dir.join(VECTOR_FILE);
    let vector_count = if vector_path.exists() {
        let vectors = open_read_only(&vector_path)?;
        let rtx = vectors
            .begin_read()
            .map_err(|e| format!("begin read on {VECTOR_FILE}: {e}"))?;
        let live = table_len(&rtx, VECTORS, "vectors")?;
        let dead = table_len(&rtx, DELETED_VECTORS, "deleted_vectors")?;
        // Matches `HnswStore::len`: tombstones are still rows until compaction.
        live.saturating_sub(dead)
    } else {
        0
    };

    Ok(PalaceDiskStats {
        drawer_count,
        vector_count,
        kg_triple_count,
        room_count,
    })
}

/// Open one redb file for reading only.
///
/// Why: [`redb::ReadOnlyDatabase::open`] takes a SHARED, non-blocking `flock`
/// and reads through a backend that cannot write. That is what makes this safe
/// to run on a poll: it never repairs, never migrates, never recreates an
/// incompatible file, and never waits on a writer. `Database::create` — the
/// call `KgStoreRedb::open` makes — does all four.
/// What: maps redb's error onto an operator-facing string; a writer holding the
/// file surfaces as `DatabaseAlreadyOpen`.
fn open_read_only(path: &Path) -> Result<ReadOnlyDatabase, String> {
    ReadOnlyDatabase::open(path).map_err(|e| {
        format!(
            "{} is not readable: {e}",
            path.file_name().unwrap_or(path.as_os_str()).display()
        )
    })
}

/// Row count of one table, treating an absent table as zero.
///
/// Why: redb stores a B-tree's length in its root header, so this is a header
/// read rather than a scan — the property the whole module rests on. A table
/// that was never created belongs to a palace written before that table
/// existed, which is zero rows, not a failure.
/// What: `open_table` + `len`, with `TableDoesNotExist` folded to `Ok(0)`.
fn table_len<K, V>(
    rtx: &redb::ReadTransaction,
    table: redb::TableDefinition<'_, K, V>,
    name: &str,
) -> Result<usize, String>
where
    K: redb::Key + 'static,
    V: redb::Value + 'static,
{
    match rtx.open_table(table) {
        Ok(t) => t
            .len()
            .map(|n| n as usize)
            .map_err(|e| format!("read {name} length: {e}")),
        Err(TableError::TableDoesNotExist(_)) => Ok(0),
        Err(e) => Err(format!("open {name} table: {e}")),
    }
}

/// Rooms registered in this palace, excluding the schema-marker row.
///
/// Why: `ROOMS` reserves the nil uuid for a `RoomSchemaMarker`, and
/// `list_room_summaries` skips it. Counting it would report one room more than
/// the room list shows, for every palace.
/// What: the table length, minus one when the marker row is present.
fn room_count(rtx: &redb::ReadTransaction) -> Result<usize, String> {
    let total = table_len(rtx, ROOMS, "rooms")?;
    let marker = match rtx.open_table(ROOMS) {
        Ok(t) => t
            .get(uuid::Uuid::nil().as_bytes().as_slice())
            .map_err(|e| format!("read rooms schema marker: {e}"))?
            .is_some(),
        Err(TableError::TableDoesNotExist(_)) => false,
        Err(e) => return Err(format!("open rooms table: {e}")),
    };
    Ok(total.saturating_sub(usize::from(marker)))
}

/// Active triples across every subject.
///
/// Why: the same figure `KgStoreRedb::count_active_triples` returns, so a
/// disk-read row and a cache-read row report the one number. It is the only
/// count here that scans rather than reading a header — one row per SUBJECT,
/// not per triple.
/// What: sums the LE-`u64` values of `ACTIVE_SUBJECT_COUNTS`, saturating.
fn active_triple_count(rtx: &redb::ReadTransaction) -> Result<usize, String> {
    let counts = match rtx.open_table(ACTIVE_SUBJECT_COUNTS) {
        Ok(t) => t,
        Err(TableError::TableDoesNotExist(_)) => return Ok(0),
        Err(e) => return Err(format!("open active_subject_counts table: {e}")),
    };
    let mut total: u64 = 0;
    for entry in counts
        .iter()
        .map_err(|e| format!("iter active_subject_counts: {e}"))?
    {
        let (_, v) = entry.map_err(|e| format!("read active_subject_counts row: {e}"))?;
        let raw = v.value();
        let mut buf = [0u8; 8];
        let take = raw.len().min(8);
        buf[..take].copy_from_slice(&raw[..take]);
        total = total.saturating_add(u64::from_le_bytes(buf));
    }
    Ok(total as usize)
}

// ─── tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use trusty_common::memory_core::{Palace, PalaceId, PalaceRegistry};

    /// Build a palace at `root/<name>` and put `drawers` drawers in it, then
    /// let the registry drop so no handle holds the redb lock.
    fn seed_palace(root: &Path, name: &str, drawers: usize) {
        let registry = PalaceRegistry::with_max_open(4);
        let palace = Palace {
            id: PalaceId::new(name),
            name: name.to_string(),
            description: None,
            created_at: chrono::Utc::now(),
            data_dir: root.join(name),
        };
        let handle = registry
            .create_palace(root, palace)
            .unwrap_or_else(|e| panic!("create_palace({name}): {e:#}"));
        for i in 0..drawers {
            let drawer = trusty_common::memory_core::Drawer::new(
                uuid::Uuid::new_v4(),
                format!("seeded drawer {i}"),
            );
            handle
                .kg
                .store()
                .upsert_drawer(&drawer)
                .unwrap_or_else(|e| panic!("upsert_drawer({i}): {e:#}"));
        }
        drop(handle);
        registry.remove(&PalaceId::new(name));
        drop(registry);
    }

    /// Why (#6372): this is the whole point of the module — a palace nobody has
    /// opened must still report its real drawer count, because the alternative
    /// the dashboard shipped was a `—` that reads as "empty".
    /// What: seeds three drawers, drops every handle, then reads from disk.
    /// Test: this is the test.
    #[test]
    fn disk_stats_counts_a_palace_that_was_never_opened() {
        let tmp = tempfile::tempdir().expect("tempdir");
        seed_palace(tmp.path(), "cold", 3);

        let stats = read(&tmp.path().join("cold")).expect("a closed palace must be readable");

        assert_eq!(
            stats.drawer_count, 3,
            "the drawer count must come off disk, not from a cache miss"
        );
        assert_eq!(stats.vector_count, 0, "nothing was embedded");
    }

    /// Why: the shared lock is what keeps this safe to run on a poll. A palace
    /// the daemon holds open for writing must fail the read rather than block
    /// on it or hand back a torn number — the caller already has real counts
    /// for that palace from the LRU cache.
    /// What: keeps a live `PalaceHandle` (and therefore redb's exclusive lock)
    /// while reading, and asserts the read is refused.
    /// Test: this is the test.
    #[test]
    fn disk_stats_refuses_a_palace_held_open_for_writing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let registry = PalaceRegistry::with_max_open(4);
        let palace = Palace {
            id: PalaceId::new("hot"),
            name: "hot".to_string(),
            description: None,
            created_at: chrono::Utc::now(),
            data_dir: tmp.path().join("hot"),
        };
        let _handle = registry
            .create_palace(tmp.path(), palace)
            .expect("create_palace");

        let err = read(&tmp.path().join("hot"))
            .expect_err("a writer-held palace must not be read from disk");
        assert!(
            err.contains("kg.redb"),
            "the error must name the file it could not read: {err}"
        );
    }

    /// Why: a palace directory that is missing or empty is a real state on a
    /// host mid-delete, and it must report a reason rather than zero — zero is
    /// what #6372 is about.
    /// What: reads a directory that was never a palace.
    /// Test: this is the test.
    #[test]
    fn disk_stats_reports_a_missing_palace_directory() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let err = read(&tmp.path().join("absent")).expect_err("no redb file to read");
        assert!(
            err.contains("kg.redb"),
            "the error must name the file it looked for: {err}"
        );
    }
}
