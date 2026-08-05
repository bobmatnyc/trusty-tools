//! Wing-registry read/write methods for `KgStoreRedb` (ADR-0027 T9).
//!
//! Why: the `WINGS` / `WING_KEYS` tables live in the palace's `kg.db` for the
//! same reason `ROOMS` does (ADR-0027 D1.1) — the redb open path recreates an
//! unreadable database empty, so a sidecar would survive and then
//! authoritatively describe wings whose rooms are gone. Their accessors need
//! the crate-private `db()` handle, so they belong inside `kg_redb` rather
//! than in `store::wings`, which owns the record shape and the policy.
//! Splitting them this way also keeps `read_ops.rs` / `write_ops.rs` (450/500)
//! clear of the SLOC cap.
//! What: `impl KgStoreRedb` for `lookup_wing_id`, `get_wing`,
//! `insert_wing_if_absent`, `list_wings`, `rename_wing_in_place`, and the
//! schema-version marker.
//!
//! Write posture: every method here is INSERT-ONLY except
//! `rename_wing_in_place`, which exists solely to serve `wing_rename` — an
//! explicit human act, unlike the seeding pass. That asymmetry is deliberate:
//! it is what makes seeding idempotent AND makes a rename survive every
//! subsequent palace open (ADR-0027 D1.4, applied to the wing axis). The
//! rename applies all of its effects in ONE transaction, so it can never leave
//! the retired label resolving as a stale alias.
//! Test: `default_wing_is_seeded_once`, `wing_rename_survives_reseed`,
//! `wing_insert_is_insert_only` (all in `store::wings::tests`).

use crate::memory_core::store::kg_store::{WING_KEYS, WINGS, decode_value, encode_value};
use crate::memory_core::store::wings::{WING_SCHEMA_VERSION, WingRecord, WingSchemaMarker};
use crate::memory_core::wing_identity::canonical_wing_key;
use anyhow::{Context, Result, bail};
use redb::{ReadableDatabase, ReadableTable};
use uuid::Uuid;

use super::store::KgStoreRedb;

impl KgStoreRedb {
    /// Look up the wing id registered under `key`.
    ///
    /// Why: the wing-side half of "ids are read from the table, never
    /// recomputed" (ADR-0027 D1.3). It is what makes `wing_create` idempotent
    /// and what stops a caller minting a second id for the default wing.
    /// What: point read of `WING_KEYS`; `None` when the key is unregistered.
    /// A value that is not 16 bytes is treated as absent rather than as an
    /// error, so a corrupt index degrades to "unregistered" instead of making
    /// the palace unusable — the same posture `lookup_room_id` takes.
    /// Test: `wing_create_is_idempotent`.
    pub fn lookup_wing_id(&self, key: &str) -> Result<Option<Uuid>> {
        let rtx = self.db().begin_read().context("begin lookup_wing_id txn")?;
        let table = rtx.open_table(WING_KEYS).context("open wing_keys table")?;
        let Some(v) = table.get(key).context("read wing_keys row")? else {
            return Ok(None);
        };
        let bytes = v.value();
        let Ok(raw) = <[u8; 16]>::try_from(bytes) else {
            tracing::warn!(
                key,
                len = bytes.len(),
                "wing_keys row is not a uuid; ignoring"
            );
            return Ok(None);
        };
        Ok(Some(Uuid::from_bytes(raw)))
    }

    /// Read one wing row by id.
    ///
    /// Why: the seeding pass asks "does the default wing already exist?" by
    /// ID, not by key — that is what lets a renamed default wing keep its new
    /// name instead of having the original key resurrected as an alias.
    /// What: point read of `WINGS`, postcard-decoded.
    /// Test: `wing_rename_survives_reseed`.
    pub fn get_wing(&self, id: Uuid) -> Result<Option<WingRecord>> {
        let rtx = self.db().begin_read().context("begin get_wing txn")?;
        let table = rtx.open_table(WINGS).context("open wings table")?;
        let Some(v) = table
            .get(id.as_bytes().as_slice())
            .context("read wing row")?
        else {
            return Ok(None);
        };
        let record =
            decode_wing_record(v.value()).with_context(|| format!("decode wing row for {id}"))?;
        Ok(Some(record))
    }

    /// Register `id` under `key`, without ever overwriting an existing row.
    ///
    /// Why: insert-only is what makes seeding re-runnable. Re-opening a palace
    /// cannot clobber a rename, and two racing writers cannot fight over a
    /// label — the loser reads the winner's id back.
    /// What: one write transaction over both tables. The `WINGS` row is
    /// written only when `id` is absent; the `WING_KEYS` entry only when `key`
    /// is absent. Returns `true` when a new `WINGS` row was written.
    /// Test: `wing_insert_is_insert_only`, `wing_create_is_idempotent`.
    pub fn insert_wing_if_absent(&self, id: Uuid, key: &str, record: &WingRecord) -> Result<bool> {
        self.check_writable()?;
        let value = encode_value(record).context("encode WingRecord")?;
        let wtx = self.db().begin_write().context("begin insert_wing txn")?;
        let inserted = {
            let mut wings = wtx.open_table(WINGS).context("open wings table")?;
            let fresh = wings
                .get(id.as_bytes().as_slice())
                .context("probe wings row")?
                .is_none();
            if fresh {
                wings
                    .insert(id.as_bytes().as_slice(), value.as_slice())
                    .context("insert wings row")?;
            }
            let mut keys = wtx.open_table(WING_KEYS).context("open wing_keys table")?;
            if keys.get(key).context("probe wing_keys row")?.is_none() {
                keys.insert(key, id.as_bytes().as_slice())
                    .context("insert wing_keys row")?;
            }
            fresh
        };
        wtx.commit().context("commit insert_wing txn")?;
        Ok(inserted)
    }

    /// Every registered wing, id-ordered, excluding the schema marker.
    ///
    /// Why: the discovery primitive behind `wing_list` — and the reason this
    /// ticket ships a consumer rather than a dark table.
    /// What: full scan of `WINGS`, skipping the reserved nil-uuid marker key.
    /// A row that fails to decode is logged and skipped so one bad row cannot
    /// hide the rest.
    /// Test: `wing_list_reports_seeded_and_created_wings`.
    pub fn list_wings(&self) -> Result<Vec<(Uuid, WingRecord)>> {
        let rtx = self.db().begin_read().context("begin list_wings txn")?;
        let table = rtx.open_table(WINGS).context("open wings table")?;
        let mut out = Vec::new();
        for entry in table.iter().context("iter wings")? {
            let (k, v) = entry.context("read wings row")?;
            let Ok(raw) = <[u8; 16]>::try_from(k.value()) else {
                continue;
            };
            let id = Uuid::from_bytes(raw);
            if id.is_nil() {
                continue; // reserved: schema marker
            }
            match decode_wing_record(v.value()) {
                Ok(record) => out.push((id, record)),
                Err(e) => tracing::warn!(%id, "skipping undecodable wing row: {e:#}"),
            }
        }
        Ok(out)
    }

    /// Apply a wing rename ATOMICALLY: row, new key, and old-key retirement.
    ///
    /// Why: all three effects must land or none of them. Splitting them across
    /// two commits — as an earlier draft of this file did, with a `put_wing`
    /// followed by a separate `remove_wing_key` — leaves a crash window in
    /// which the row carries the new label while the OLD key still resolves to
    /// it. That is precisely the alias this method's caller documents as
    /// impossible, and a stale alias in a scope mechanism is a leak: a caller
    /// that names the retired label would silently reach the wing it was
    /// renamed away from. redb gives us a single write transaction over both
    /// tables, so the split bought nothing.
    ///
    /// This is also the ONLY non-insert-only write on the wing axis. It exists
    /// for exactly one caller — `wing_rename` — so no automatic path (seeding,
    /// palace open) can reach an overwrite by accident.
    ///
    /// What: one write transaction that overwrites the `WINGS` row, points
    /// `new_key` at `id`, and removes `old_key` when it differs. Never touches
    /// `ROOMS` or `DRAWERS` — a rename cannot move a room, let alone a drawer,
    /// because rooms reference a wing by ID and the id is unchanged.
    /// Test: `wing_rename_changes_no_room_or_drawer_rows`,
    /// `wing_rename_retires_the_old_label`,
    /// `wing_rename_applies_every_effect_together`.
    pub fn rename_wing_in_place(
        &self,
        id: Uuid,
        new_key: &str,
        new_label: &str,
    ) -> Result<WingRecord> {
        self.check_writable()?;
        let wtx = self.db().begin_write().context("begin rename_wing txn")?;
        let renamed = {
            let mut wings = wtx.open_table(WINGS).context("open wings table")?;
            // Read the row INSIDE the transaction and derive the old key from
            // it. Deriving it outside would let two concurrent renames of the
            // same wing each retire a label the other had already replaced,
            // stranding one of them as a live alias.
            let Some(existing) = wings
                .get(id.as_bytes().as_slice())
                .context("read wing to rename")?
            else {
                bail!("unknown wing: {id}");
            };
            let record = decode_wing_record(existing.value())
                .with_context(|| format!("decode wing row for {id}"))?;
            drop(existing);
            let old_key = canonical_wing_key(&record.label);

            let mut keys = wtx.open_table(WING_KEYS).context("open wing_keys table")?;
            // Uniqueness probe INSIDE the write transaction. Probing outside it
            // left a window in which a concurrent `resolve_or_create_wing_sync`
            // could claim `new_key`, after which this unconditional insert
            // would steal it — leaving the loser's row with a label that no
            // longer resolved to it. redb's single-writer lock closes that
            // window entirely; bailing here aborts the whole transaction, so a
            // rejected rename writes nothing at all.
            if let Some(holder) = keys.get(new_key).context("probe new wing label")? {
                let taken = <[u8; 16]>::try_from(holder.value())
                    .map(Uuid::from_bytes)
                    .unwrap_or_else(|_| Uuid::nil());
                if taken != id {
                    bail!("wing label {new_label:?} is already used by wing {taken}");
                }
            }

            let renamed = WingRecord {
                label: new_label.to_string(),
                ..record
            };
            let value = encode_value(&renamed).context("encode WingRecord")?;
            wings
                .insert(id.as_bytes().as_slice(), value.as_slice())
                .context("insert wings row")?;
            keys.insert(new_key, id.as_bytes().as_slice())
                .context("insert wing_keys row")?;
            if old_key != new_key {
                keys.remove(old_key.as_str())
                    .context("retire old wing_keys row")?;
            }
            renamed
        };
        wtx.commit().context("commit rename_wing txn")?;
        Ok(renamed)
    }

    /// Read the wing-schema version marker, if one has been written.
    ///
    /// Why/What: mirrors `room_schema_version` — it lets a future migration
    /// recognise which shape wrote these rows. Seeding idempotency does NOT
    /// depend on it; that comes from the by-id existence probe.
    /// Test: `default_wing_is_seeded_once`.
    pub fn wing_schema_version(&self) -> Result<Option<u32>> {
        let rtx = self
            .db()
            .begin_read()
            .context("begin wing schema marker txn")?;
        let table = rtx.open_table(WINGS).context("open wings table")?;
        let Some(v) = table
            .get(Uuid::nil().as_bytes().as_slice())
            .context("read wing schema marker")?
        else {
            return Ok(None);
        };
        let marker: WingSchemaMarker =
            decode_value(v.value()).context("decode WingSchemaMarker")?;
        Ok(Some(marker.schema_version))
    }

    /// Stamp the current wing-schema version. UNCONDITIONAL OVERWRITE.
    ///
    /// Why: the marker records which shape wrote these rows, so a future
    /// migration can tell whether it has work to do. That makes an
    /// unconditional write dangerous in exactly one way: stamping a bumped
    /// [`WING_SCHEMA_VERSION`] onto a palace whose rows were NOT migrated
    /// would erase the only evidence that they still need migrating.
    ///
    /// **Call contract:** call this only when creating the wing schema, or at
    /// the end of a migration that has actually converted the rows. Never call
    /// it unconditionally on palace open — [`super::super::wings::ensure_default_wing`]
    /// deliberately reaches it only on the branch that just created the schema,
    /// which is why an already-seeded palace performs no write at open.
    /// What: overwrites the nil-uuid `WINGS` row with the current version.
    /// Test: `default_wing_is_seeded_once`,
    /// `reseeding_does_not_restamp_the_schema_version`.
    pub fn set_wing_schema_version(&self) -> Result<()> {
        self.check_writable()?;
        let value = encode_value(&WingSchemaMarker {
            schema_version: WING_SCHEMA_VERSION,
        })
        .context("encode WingSchemaMarker")?;
        let wtx = self
            .db()
            .begin_write()
            .context("begin wing schema marker txn")?;
        {
            let mut wings = wtx.open_table(WINGS).context("open wings table")?;
            wings
                .insert(Uuid::nil().as_bytes().as_slice(), value.as_slice())
                .context("insert wing schema marker")?;
        }
        wtx.commit().context("commit wing schema marker txn")?;
        Ok(())
    }

    /// Raw `(key, value)` bytes of every `WINGS` row — test-only.
    ///
    /// Why: proving that re-seeding an already-seeded palace performs NO write
    /// at all — including no restamp of the schema marker — needs a byte-level
    /// snapshot. Comparing decoded values would miss a marker rewritten to the
    /// same version, which is exactly the write the call contract forbids.
    /// What: full scan returning owned bytes, key-ordered. Includes the
    /// nil-uuid marker row, unlike `list_wings`.
    /// Test: `reseeding_does_not_restamp_the_schema_version`.
    #[cfg(test)]
    pub(crate) fn raw_wing_rows(&self) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let rtx = self.db().begin_read().context("begin raw_wing_rows txn")?;
        let table = rtx.open_table(WINGS).context("open wings table")?;
        let mut out = Vec::new();
        for entry in table.iter().context("iter wings")? {
            let (k, v) = entry.context("read wings row")?;
            out.push((k.value().to_vec(), v.value().to_vec()));
        }
        Ok(out)
    }
}

/// Decode a `WingRecord`, with room for a fallback chain.
///
/// Why: postcard is positional, so the day a trailing field is appended to
/// `WingRecord` — the mechanism ADR-0027 D2 relies on for hanging #3064's
/// per-wing access configuration later — rows written today stop decoding as
/// the new shape. The `DrawerRecord` precedent (`kg_redb/types.rs`) handles
/// that by trying the current shape and falling back through the historical
/// ones; this function is the single place that chain will grow, so no call
/// site has to learn it.
/// What: today there is exactly one shape, so this is a direct decode.
/// Test: `store::wings::tests::wing_record_decodes_under_a_future_field`
/// exercises the pattern this chain implements.
fn decode_wing_record(bytes: &[u8]) -> Result<WingRecord> {
    decode_value::<WingRecord>(bytes).context("decode WingRecord")
}
