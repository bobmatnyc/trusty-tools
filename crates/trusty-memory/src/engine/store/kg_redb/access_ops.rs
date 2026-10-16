//! `DRAWER_ACCESS` reads and writes: recall access counts (#9881).
//!
//! Why: the access count lives beside `DRAWERS`, not in `DrawerRecord`, so an
//! older binary still decodes every drawer row (see [`DRAWER_ACCESS`]).
//! What: [`KgStoreRedb::put_accesses`] writes many drawers' access fields in
//! one transaction; [`read_access_map`] and [`merge_access`] lift them back
//! onto drawers as they load.
//! Test: `accesses_round_trip_and_merge_into_loaded_drawers`,
//! `put_accesses_refuses_a_read_only_store`.

use crate::engine::palace::Drawer;
use crate::engine::store::kg_store::{AccessRecord, DRAWER_ACCESS, decode_value, encode_value};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use redb::{ReadTransaction, ReadableTable, TableError};
use std::collections::HashMap;
use uuid::Uuid;

use super::store::KgStoreRedb;

impl KgStoreRedb {
    /// Write the access fields of `drawers` in one transaction (#9881).
    ///
    /// Why: one commit per flush, however many drawers recall served. The
    /// stored count must never go backwards (M1): two flushes can race, and a
    /// merge or chunk write-back can leave a drawer's in-memory count behind
    /// what a recall already persisted.
    /// What: refuses a read-only store; upserts an [`AccessRecord`] per drawer
    /// under its uuid, keeping the larger `count` and the later `last_ms` of
    /// the stored and offered values. The read and the write share one write
    /// transaction, which redb serialises, so no interleaving lowers a value.
    /// Touches no other table, so it cannot overwrite a concurrent drawer
    /// write.
    /// Test: `accesses_round_trip_and_merge_into_loaded_drawers`,
    /// `a_lower_access_write_never_lowers_the_stored_value`,
    /// `put_accesses_refuses_a_read_only_store`.
    pub(crate) fn put_accesses(&self, drawers: &[Drawer]) -> Result<()> {
        self.check_writable()?;
        if drawers.is_empty() {
            return Ok(()); // #9881: no empty commit.
        }
        let gw = self
            .begin_write_guarded()
            .context("begin put_accesses txn")?;
        {
            let mut table = gw
                .txn
                .open_table(DRAWER_ACCESS)
                .context("open drawer_access table")?;
            for d in drawers {
                let key = d.id.as_bytes().as_slice();
                // #9881 (M1): monotonic upsert; an unreadable old row is replaced.
                let stored = match table.get(key).context("read access row")? {
                    Some(g) => decode_value::<AccessRecord>(g.value()).ok(),
                    None => None,
                };
                let offered = AccessRecord {
                    count: d.access_count,
                    last_ms: d.last_accessed_at.map(|t| t.timestamp_millis()),
                };
                let rec = stored.map_or(offered, |old| old.max_with(offered));
                let bytes = encode_value(&rec).context("encode access record")?;
                table
                    .insert(key, bytes.as_slice())
                    .context("insert access record")?;
            }
        }
        gw.commit().context("commit put_accesses txn")?;
        Ok(())
    }

    /// Every stored access record, keyed by drawer id (#9881).
    #[cfg(test)]
    pub(crate) fn load_accesses(&self) -> Result<HashMap<Uuid, AccessRecord>> {
        use redb::ReadableDatabase;
        let rtx = self.db().begin_read().context("begin load_accesses txn")?;
        Ok(read_access_map(&rtx))
    }
}

/// Read `DRAWER_ACCESS` through `rtx`; a palace that never recorded one has no
/// table, which reads as empty. An undecodable row is skipped with a warning.
///
/// Why (#9881, M3): the table only carries the access boost. A fault in it
/// must cost that boost, never the drawers themselves or `remember`.
/// What: infallible. Any error other than `TableDoesNotExist` is logged and
/// the rows read so far (none, for an open failure) are returned.
/// Test: `a_corrupt_access_table_costs_only_the_boost`.
pub(super) fn read_access_map(rtx: &ReadTransaction) -> HashMap<Uuid, AccessRecord> {
    let mut out = HashMap::new();
    let table = match rtx.open_table(DRAWER_ACCESS) {
        Ok(t) => t,
        Err(TableError::TableDoesNotExist(_)) => return out,
        Err(e) => {
            tracing::warn!("#9881: drawer_access unreadable; access boost lost: {e}");
            return out;
        }
    };
    let rows = match table.iter() {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!("#9881: drawer_access unreadable; access boost lost: {e}");
            return out;
        }
    };
    for entry in rows {
        let (k, v) = match entry {
            Ok(kv) => kv,
            Err(e) => {
                tracing::warn!("#9881: drawer_access read stopped; access boost partial: {e}");
                break;
            }
        };
        let Ok(id) = Uuid::from_slice(k.value()) else {
            continue;
        };
        match decode_value::<AccessRecord>(v.value()) {
            Ok(rec) => {
                out.insert(id, rec);
            }
            Err(e) => tracing::warn!(id = %id, "#9881: skip unreadable access row: {e}"),
        }
    }
    out
}

/// Point-read one drawer's access record; absent table or row is `None`.
///
/// Why (#9881, M3): `remember` reads the incumbent through this path; a fault
/// in the side table must not fail it.
/// What: infallible; an error other than `TableDoesNotExist` is logged and
/// reads as `None`.
/// Test: `a_corrupt_access_table_costs_only_the_boost`.
pub(super) fn read_access(rtx: &ReadTransaction, id: Uuid) -> Option<AccessRecord> {
    let table = match rtx.open_table(DRAWER_ACCESS) {
        Ok(t) => t,
        Err(TableError::TableDoesNotExist(_)) => return None,
        Err(e) => {
            tracing::warn!(id = %id, "#9881: drawer_access unreadable; access boost lost: {e}");
            return None;
        }
    };
    let guard = match table.get(id.as_bytes().as_slice()) {
        Ok(g) => g?,
        Err(e) => {
            tracing::warn!(id = %id, "#9881: access row unreadable; access boost lost: {e}");
            return None;
        }
    };
    decode_value::<AccessRecord>(guard.value()).ok()
}

/// Copy `rec` onto `drawer`'s access fields.
pub(super) fn merge_access(drawer: &mut Drawer, rec: AccessRecord) {
    drawer.access_count = rec.count;
    drawer.last_accessed_at = rec.last_ms.and_then(DateTime::<Utc>::from_timestamp_millis);
}

#[cfg(test)]
#[path = "access_ops_tests.rs"]
mod access_ops_tests;
