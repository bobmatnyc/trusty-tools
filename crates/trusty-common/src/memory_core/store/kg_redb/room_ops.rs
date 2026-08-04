//! Room-registry read/write methods for `KgStoreRedb` (ADR-0027 T1).
//!
//! Why: the `ROOMS` / `ROOM_KEYS` tables live in the palace's `kg.db`, so their
//! accessors need the crate-private `db()` handle — they belong inside
//! `kg_redb` rather than in the higher-level `store::rooms` module, which owns
//! the record shape and the resolve-or-create policy. Splitting them this way
//! also keeps `read_ops.rs` / `write_ops.rs` clear of the 500-SLOC cap.
//! What: `impl KgStoreRedb` for `lookup_room_id`, `get_room`,
//! `insert_room_if_absent`, `list_rooms`, and the schema-version marker.
//! Every write here is INSERT-ONLY — nothing in this file ever updates or
//! deletes a row, which is what makes the backfill idempotent and a human
//! rename survive every subsequent palace open (ADR-0027 D1.4).
//! Test: `room_key_lookup_returns_registered_id`, `room_insert_is_insert_only`,
//! `room_key_collision_keeps_both_rows`,
//! `backfill_stamps_schema_version_and_skips_marker_row` (all in
//! `store::room_backfill::tests`).

use crate::memory_core::store::kg_store::{ROOM_KEYS, ROOMS, decode_value, encode_value};
use crate::memory_core::store::rooms::{ROOM_SCHEMA_VERSION, RoomRecord, RoomSchemaMarker};
use anyhow::{Context, Result};
use redb::{ReadableDatabase, ReadableTable};
use uuid::Uuid;

use super::store::KgStoreRedb;

impl KgStoreRedb {
    /// Look up the room id registered under `key`.
    ///
    /// Why: this is the "ids are read from the table, never recomputed" rule
    /// (ADR-0027 D1.3) in one function — both the write path and the read
    /// filters resolve through it.
    /// What: point read of `ROOM_KEYS`; `None` when the key is unregistered.
    /// A value that is not 16 bytes is treated as absent rather than as an
    /// error, so a corrupt index degrades to "unregistered" instead of making
    /// the palace unusable.
    /// Test: `room_key_lookup_returns_registered_id`.
    pub fn lookup_room_id(&self, key: &str) -> Result<Option<Uuid>> {
        let rtx = self.db().begin_read().context("begin lookup_room_id txn")?;
        let table = rtx.open_table(ROOM_KEYS).context("open room_keys table")?;
        let Some(v) = table.get(key).context("read room_keys row")? else {
            return Ok(None);
        };
        let bytes = v.value();
        let Ok(raw) = <[u8; 16]>::try_from(bytes) else {
            tracing::warn!(
                key,
                len = bytes.len(),
                "room_keys row is not a uuid; ignoring"
            );
            return Ok(None);
        };
        Ok(Some(Uuid::from_bytes(raw)))
    }

    /// Read one room row by id.
    ///
    /// Why: the backfill must know whether an id already has a row before it
    /// inserts — that check is what preserves a human rename.
    /// What: point read of `ROOMS`, postcard-decoded.
    /// Test: `room_insert_is_insert_only`.
    pub fn get_room(&self, id: Uuid) -> Result<Option<RoomRecord>> {
        let rtx = self.db().begin_read().context("begin get_room txn")?;
        let table = rtx.open_table(ROOMS).context("open rooms table")?;
        let Some(v) = table
            .get(id.as_bytes().as_slice())
            .context("read room row")?
        else {
            return Ok(None);
        };
        let record =
            decode_room_record(v.value()).with_context(|| format!("decode room row for {id}"))?;
        Ok(Some(record))
    }

    /// Register `id` under `key`, without ever overwriting an existing row.
    ///
    /// Why: insert-only is the whole safety argument. Re-running the backfill
    /// cannot clobber a rename, and two racing writers cannot fight over a
    /// label — the loser simply reads the winner's id back.
    /// What: one write transaction over both tables. The `ROOMS` row is
    /// written only when `id` is absent; the `ROOM_KEYS` entry only when `key`
    /// is absent. Returns `true` when a new `ROOMS` row was written.
    ///
    /// Key collision (accepted, ADR-0027 D4.1): two *legacy* rooms can
    /// normalise onto one canonical key — `Backend` and `Custom("backend")`
    /// both lower to `backend`. Both keep their own `ROOMS` row (ids are
    /// unique) and stay enumerable; only the first one seen owns the name.
    /// The backfill visits ids in sorted order so which one that is stays
    /// deterministic.
    /// Test: `room_insert_is_insert_only`, `room_key_collision_keeps_both_rows`.
    pub fn insert_room_if_absent(&self, id: Uuid, key: &str, record: &RoomRecord) -> Result<bool> {
        self.check_writable()?;
        let value = encode_value(record).context("encode RoomRecord")?;
        let wtx = self.db().begin_write().context("begin insert_room txn")?;
        let inserted = {
            let mut rooms = wtx.open_table(ROOMS).context("open rooms table")?;
            let fresh = rooms
                .get(id.as_bytes().as_slice())
                .context("probe rooms row")?
                .is_none();
            if fresh {
                rooms
                    .insert(id.as_bytes().as_slice(), value.as_slice())
                    .context("insert rooms row")?;
            }
            let mut keys = wtx.open_table(ROOM_KEYS).context("open room_keys table")?;
            if keys.get(key).context("probe room_keys row")?.is_none() {
                keys.insert(key, id.as_bytes().as_slice())
                    .context("insert room_keys row")?;
            }
            fresh
        };
        wtx.commit().context("commit insert_room txn")?;
        Ok(inserted)
    }

    /// Every registered room, id-ordered, excluding the schema marker.
    ///
    /// Why: the discovery primitive that did not exist — and the backfill's
    /// own "which ids already have a row" probe.
    /// What: full scan of `ROOMS`, skipping the reserved nil-uuid marker key.
    /// A row that fails to decode is logged and skipped so one bad row cannot
    /// hide the rest.
    /// Test: `room_key_collision_keeps_both_rows`.
    pub fn list_rooms(&self) -> Result<Vec<(Uuid, RoomRecord)>> {
        let rtx = self.db().begin_read().context("begin list_rooms txn")?;
        let table = rtx.open_table(ROOMS).context("open rooms table")?;
        let mut out = Vec::new();
        for entry in table.iter().context("iter rooms")? {
            let (k, v) = entry.context("read rooms row")?;
            let Ok(raw) = <[u8; 16]>::try_from(k.value()) else {
                continue;
            };
            let id = Uuid::from_bytes(raw);
            if id.is_nil() {
                continue; // reserved: schema marker
            }
            match decode_room_record(v.value()) {
                Ok(record) => out.push((id, record)),
                Err(e) => tracing::warn!(%id, "skipping undecodable room row: {e:#}"),
            }
        }
        Ok(out)
    }

    /// Read the room-schema version marker, if one has been written.
    ///
    /// Why: lets a future migration recognise which shape wrote these rows.
    /// Backfill idempotency does NOT depend on it (see `room_ops` module doc).
    /// What: point read of the nil-uuid `ROOMS` row.
    /// Test: `backfill_stamps_schema_version_and_skips_marker_row`.
    pub fn room_schema_version(&self) -> Result<Option<u32>> {
        let rtx = self.db().begin_read().context("begin schema marker txn")?;
        let table = rtx.open_table(ROOMS).context("open rooms table")?;
        let Some(v) = table
            .get(Uuid::nil().as_bytes().as_slice())
            .context("read schema marker")?
        else {
            return Ok(None);
        };
        let marker: RoomSchemaMarker =
            decode_value(v.value()).context("decode RoomSchemaMarker")?;
        Ok(Some(marker.schema_version))
    }

    /// Stamp the current room-schema version.
    ///
    /// Why/What: written once by the backfill so the on-disk shape is
    /// self-describing. Idempotent — writing the same version twice is a
    /// no-op in effect.
    /// Test: `backfill_stamps_schema_version_and_skips_marker_row`.
    pub fn set_room_schema_version(&self) -> Result<()> {
        self.check_writable()?;
        let value = encode_value(&RoomSchemaMarker {
            schema_version: ROOM_SCHEMA_VERSION,
        })
        .context("encode RoomSchemaMarker")?;
        let wtx = self.db().begin_write().context("begin schema marker txn")?;
        {
            let mut rooms = wtx.open_table(ROOMS).context("open rooms table")?;
            rooms
                .insert(Uuid::nil().as_bytes().as_slice(), value.as_slice())
                .context("insert schema marker")?;
        }
        wtx.commit().context("commit schema marker txn")?;
        Ok(())
    }

    /// Overwrite one room row unconditionally — test-only.
    ///
    /// Why: the "insert-only preserves a human rename" guarantee cannot be
    /// exercised until `room_rename` ships (ticket T6), yet it is the property
    /// the whole backfill design rests on. This is the seam that stands in for
    /// that future update path; production code has no way to overwrite a row.
    /// What: unconditional `insert` into `ROOMS`, leaving `ROOM_KEYS` alone.
    /// Test: `backfill_rename_survives_second_run`.
    #[cfg(test)]
    pub(crate) fn force_put_room(&self, id: Uuid, record: &RoomRecord) -> Result<()> {
        let value = encode_value(record).context("encode RoomRecord")?;
        let wtx = self
            .db()
            .begin_write()
            .context("begin force_put_room txn")?;
        {
            let mut rooms = wtx.open_table(ROOMS).context("open rooms table")?;
            rooms
                .insert(id.as_bytes().as_slice(), value.as_slice())
                .context("force insert rooms row")?;
        }
        wtx.commit().context("commit force_put_room txn")?;
        Ok(())
    }

    /// Raw `(key, value)` bytes of every `DRAWERS` row — test-only.
    ///
    /// Why: ADR-0027's headline safety property is "not one existing drawer
    /// row changes". Proving that needs a byte-level snapshot, not a decoded
    /// comparison (a decode round-trip could mask a rewrite).
    /// What: full scan returning owned bytes, key-ordered.
    /// Test: `backfill_changes_no_drawer_rows`.
    #[cfg(test)]
    pub(crate) fn raw_drawer_rows(&self) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        use crate::memory_core::store::kg_store::DRAWERS;
        let rtx = self
            .db()
            .begin_read()
            .context("begin raw_drawer_rows txn")?;
        let table = rtx.open_table(DRAWERS).context("open drawers table")?;
        let mut out = Vec::new();
        for entry in table.iter().context("iter drawers")? {
            let (k, v) = entry.context("read drawers row")?;
            out.push((k.value().to_vec(), v.value().to_vec()));
        }
        Ok(out)
    }
}

/// Decode a `RoomRecord`, with room for a fallback chain.
///
/// Why: postcard is positional, so the day a trailing field is appended to
/// `RoomRecord`, rows written today stop decoding as the new shape. The
/// `DrawerRecord` precedent (`kg_redb/types.rs`) handles that by trying the
/// current shape and falling back through the historical ones; this function
/// is the single place that chain will grow, so no call site has to learn it.
/// What: today there is exactly one shape, so this is a direct decode.
/// Test: `store::rooms::tests::room_record_decodes_under_a_future_field`
/// exercises the pattern this chain implements.
fn decode_room_record(bytes: &[u8]) -> Result<RoomRecord> {
    decode_value::<RoomRecord>(bytes).context("decode RoomRecord")
}
