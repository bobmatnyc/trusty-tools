//! Write-free steady-state open for the HNSW store (#8314).
//!
//! Why: `HnswStore::open_with_mode` used to run two redb write transactions on
//! EVERY read-write open — a schema touch and the #5005 id-floor raise — even
//! when the `Database` came from the process-wide vector-db cache, i.e. was
//! shared with a live handle. redb's `begin_write` waits without a bound for
//! any live write transaction (`TransactionTracker::start_write_transaction`),
//! so reopening a palace behind a stuck vector write parked the opening thread
//! forever. The daemon reopens a palace on a READ (`open_palace` after an
//! LRU or idle eviction), on a tokio worker, while holding the palace's open
//! lock, so one stuck write turned every later read of that palace into a hang.
//! What: both steps read first and open a write transaction only when there is
//! something to write — a brand-new file with no tables, or a counter below the
//! floor. Once a file is initialised, an open is read-only and never waits on a
//! writer. The on-disk format is unchanged.
//! Test: `a_reopen_behind_a_stuck_vector_write_still_reads`.

use redb::{Database, ReadableDatabase, ReadableTable, TableError};

use super::Result;
use crate::memory_core::store::kg_store::{
    DELETED_VECTORS, NEXT_VECTOR_ID, VECTOR_ID_SEQ, VECTOR_KEYS, VECTORS,
};

/// Create the four vector tables, but only on a file that lacks one.
///
/// Why: redb persists a table only once it has been opened for write, so a
/// brand-new file needs this; an initialised one does not, and must not wait
/// on a live writer to learn that (#8314).
/// What: opens each table in a read transaction; any `TableDoesNotExist` sends
/// the call down the original write path. Other read errors propagate.
/// Test: `a_reopen_behind_a_stuck_vector_write_still_reads`, `persist_and_reload`.
pub(super) fn ensure_schema(db: &Database) -> Result<()> {
    if schema_present(db)? {
        return Ok(());
    }
    let wtx = db.begin_write()?;
    {
        let _ = wtx.open_table(VECTORS)?;
        let _ = wtx.open_table(VECTOR_KEYS)?;
        let _ = wtx.open_table(DELETED_VECTORS)?;
        let _ = wtx.open_table(VECTOR_ID_SEQ)?;
    }
    wtx.commit()?;
    Ok(())
}

/// Whether all four vector tables already exist, judged by a read transaction.
fn schema_present(db: &Database) -> Result<bool> {
    let rtx = db.begin_read()?;
    let present = [
        rtx.open_table(VECTORS).map(drop),
        rtx.open_table(VECTOR_KEYS).map(drop),
        rtx.open_table(DELETED_VECTORS).map(drop),
        rtx.open_table(VECTOR_ID_SEQ).map(drop),
    ];
    for outcome in present {
        match outcome {
            Ok(()) => {}
            Err(TableError::TableDoesNotExist(_)) => return Ok(false),
            Err(e) => return Err(e.into()),
        }
    }
    Ok(true)
}

/// Raise the persisted `VECTOR_ID_SEQ` counter to at least `floor` (#5005).
///
/// Why: the raise is idempotent and only ever moves forward, so a counter that
/// already sits at or above the floor needs no write — and a reopen must not
/// wait on a live writer to find that out (#8314).
/// What: reads the counter; opens a write transaction only when it is below
/// `floor`, and re-checks inside it so a concurrent raise is never undone.
/// Test: `a_reopen_behind_a_stuck_vector_write_still_reads`,
/// `two_live_stores_over_one_file_never_alias_ids`.
pub(super) fn raise_id_floor(db: &Database, floor: u64) -> Result<()> {
    let current = {
        let rtx = db.begin_read()?;
        let seq = rtx.open_table(VECTOR_ID_SEQ)?;
        seq.get(NEXT_VECTOR_ID)?.map(|g| g.value()).unwrap_or(0)
    };
    if current >= floor {
        return Ok(());
    }
    let wtx = db.begin_write()?;
    {
        let mut seq = wtx.open_table(VECTOR_ID_SEQ)?;
        let current = seq.get(NEXT_VECTOR_ID)?.map(|g| g.value()).unwrap_or(0);
        if current < floor {
            seq.insert(NEXT_VECTOR_ID, floor)?;
        }
    }
    wtx.commit()?;
    Ok(())
}
