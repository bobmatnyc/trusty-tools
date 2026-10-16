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
    /// Why: one commit per flush, however many drawers recall served.
    /// What: refuses a read-only store; upserts an [`AccessRecord`] per drawer
    /// under its uuid. Touches no other table, so it cannot overwrite a
    /// concurrent drawer write.
    /// Test: `accesses_round_trip_and_merge_into_loaded_drawers`,
    /// `put_accesses_refuses_a_read_only_store`.
    pub(crate) fn put_accesses(&self, drawers: &[Drawer]) -> Result<()> {
        self.check_writable()?;
        let gw = self
            .begin_write_guarded()
            .context("begin put_accesses txn")?;
        {
            let mut table = gw
                .txn
                .open_table(DRAWER_ACCESS)
                .context("open drawer_access table")?;
            for d in drawers {
                let rec = AccessRecord {
                    count: d.access_count,
                    last_ms: d.last_accessed_at.map(|t| t.timestamp_millis()),
                };
                let bytes = encode_value(&rec).context("encode access record")?;
                table
                    .insert(d.id.as_bytes().as_slice(), bytes.as_slice())
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
        read_access_map(&rtx)
    }
}

/// Read `DRAWER_ACCESS` through `rtx`; a palace that never recorded one has no
/// table, which reads as empty. An undecodable row is skipped with a warning.
pub(super) fn read_access_map(rtx: &ReadTransaction) -> Result<HashMap<Uuid, AccessRecord>> {
    let table = match rtx.open_table(DRAWER_ACCESS) {
        Ok(t) => t,
        Err(TableError::TableDoesNotExist(_)) => return Ok(HashMap::new()),
        Err(e) => return Err(e).context("open drawer_access table"),
    };
    let mut out = HashMap::new();
    for entry in table.iter().context("iter drawer_access")? {
        let (k, v) = entry.context("read access row")?;
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
    Ok(out)
}

/// Point-read one drawer's access record; absent table or row is `None`.
pub(super) fn read_access(rtx: &ReadTransaction, id: Uuid) -> Result<Option<AccessRecord>> {
    let table = match rtx.open_table(DRAWER_ACCESS) {
        Ok(t) => t,
        Err(TableError::TableDoesNotExist(_)) => return Ok(None),
        Err(e) => return Err(e).context("open drawer_access table"),
    };
    let Some(guard) = table
        .get(id.as_bytes().as_slice())
        .context("read access row")?
    else {
        return Ok(None);
    };
    Ok(decode_value::<AccessRecord>(guard.value()).ok())
}

/// Copy `rec` onto `drawer`'s access fields.
pub(super) fn merge_access(drawer: &mut Drawer, rec: AccessRecord) {
    drawer.access_count = rec.count;
    drawer.last_accessed_at = rec.last_ms.and_then(DateTime::<Utc>::from_timestamp_millis);
}

#[cfg(test)]
#[path = "access_ops_tests.rs"]
mod access_ops_tests;
