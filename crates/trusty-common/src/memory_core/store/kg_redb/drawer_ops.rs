//! In-transaction drawer helpers plus the `DRAWERS_BY_FACT_KEY` slot index.
//!
//! Why: the drawer write path and the triple write path share nothing but a
//! transaction, and `write_ops.rs` sat at 495 of its 500 permitted SLOC. Moving
//! the drawer half out is what makes room for the #4810 triple-key work without
//! either file straddling the cap.
//! What: `stored_fact_key`, `release_slot_if_owned_by`, `batch_upsert_drawer`,
//! `batch_delete_drawer` — the single implementation behind both the
//! `KgStoreRedb::{upsert_drawer, delete_drawer}` single-op methods and
//! `apply_batch` / `import_all`.
//! Test: `fact_key_index_tracks_upsert_and_delete`,
//! `fact_key_index_follows_the_slot_on_reassignment`,
//! `clearing_a_fact_key_drops_the_index_entry`,
//! `deleting_a_drawer_that_lost_its_slot_leaves_the_new_owner_indexed`.

use crate::memory_core::palace::Drawer;
use crate::memory_core::store::kg_store::encode_value;
use anyhow::{Context, Result};
use redb::ReadableTable;
use uuid::Uuid;

use super::types::{Tbl, decode_drawer_record, drawer_to_record};

/// The `fact_key` currently stored on the `DRAWERS` row for `id` (#4884).
///
/// Why: index maintenance is a diff, not an overwrite — to drop the entry a
/// drawer is about to stop owning, the writer must know which slot it owned
/// before the new record lands. Callers therefore run this BEFORE writing.
/// What: point-reads the row and decodes it through
/// [`decode_drawer_record`]'s migration chain. A row that fails every shape is
/// reported as owning no slot: undecodable rows necessarily predate the field,
/// and failing the write over one would make a single corrupt drawer block
/// every unrelated write.
/// Test: `fact_key_index_follows_the_slot_on_reassignment`,
/// `clearing_a_fact_key_drops_the_index_entry`.
fn stored_fact_key(drawers: &Tbl<'_>, id: Uuid) -> Result<Option<String>> {
    let id_bytes = *id.as_bytes();
    let Some(guard) = drawers
        .get(id_bytes.as_slice())
        .context("read prior drawer row for fact_key index")?
    else {
        return Ok(None);
    };
    Ok(decode_drawer_record(guard.value())
        .ok()
        .and_then(|r| r.fact_key))
}

/// Release `key` from the slot index, but only if `owner` still holds it
/// (#4884).
///
/// Why: the ownership guard is what makes the index survive reassignment.
/// Drawer A owns `pr:4818/state`, drawer B takes the slot, then A is deleted —
/// an unguarded removal would evict B's entry and report a slot as free while a
/// live drawer occupies it. Checking that the entry still names the drawer
/// letting go of it makes the stale delete a no-op instead.
/// What: reads the index at `key`; removes it only when the stored uuid bytes
/// equal `owner`'s. Absent or differently-owned entries are left alone.
/// Test: `deleting_a_drawer_that_lost_its_slot_leaves_the_new_owner_indexed`.
fn release_slot_if_owned_by(by_fact_key: &mut Tbl<'_>, key: &str, owner: Uuid) -> Result<()> {
    let owned = by_fact_key
        .get(key.as_bytes())
        .context("read fact_key index for release")?
        .is_some_and(|g| g.value() == owner.as_bytes().as_slice());
    if owned {
        by_fact_key
            .remove(key.as_bytes())
            .context("remove fact_key index entry")?;
    }
    Ok(())
}

/// In-transaction drawer upsert helper.
///
/// Why: the single implementation behind both `KgStoreRedb::upsert_drawer` and
/// `apply_batch`, so the #4884 slot-index maintenance cannot be present in one
/// path and missing from the other.
/// What: releases the slot the stored row used to own when the new record names
/// a different one (or none), writes the record under its UUID key in DRAWERS,
/// then points `DRAWERS_BY_FACT_KEY` at this drawer for the new key. Writing a
/// key another drawer already holds moves the index onto the newer drawer —
/// that is the "one slot, one live fact" shape ADR-0028 D5 needs; retiring the
/// displaced drawer itself belongs to the Tier C write path, not to storage.
/// Test: `apply_batch_mixes_drawer_and_triple_ops`,
/// `fact_key_index_tracks_upsert_and_delete`,
/// `fact_key_index_follows_the_slot_on_reassignment`.
pub(super) fn batch_upsert_drawer(
    drawers: &mut Tbl<'_>,
    by_fact_key: &mut Tbl<'_>,
    drawer: &Drawer,
) -> Result<()> {
    // #4884: read the prior slot BEFORE overwriting the row — afterwards the
    // old key is gone and its index entry would be unreachable garbage.
    let prior = stored_fact_key(drawers, drawer.id)?;
    if let Some(prev) = prior.as_deref()
        && drawer.fact_key.as_deref() != Some(prev)
    {
        release_slot_if_owned_by(by_fact_key, prev, drawer.id)?;
    }

    let record = drawer_to_record(drawer);
    let bytes = encode_value(&record).context("encode drawer record (batch)")?;
    let id_bytes = *drawer.id.as_bytes();
    drawers
        .insert(id_bytes.as_slice(), bytes.as_slice())
        .context("insert drawer record (batch)")?;

    if let Some(key) = drawer.fact_key.as_deref() {
        by_fact_key
            .insert(key.as_bytes(), id_bytes.as_slice())
            .context("insert fact_key index entry")?;
    }
    Ok(())
}

/// In-transaction drawer delete helper.
///
/// Why: the single implementation behind both `KgStoreRedb::delete_drawer` and
/// `apply_batch`. A delete that removed the row but left the index entry would
/// leave the occupancy check answering "taken" for a slot whose occupant is
/// gone — the exact stale-index bug the index is supposed to prevent.
/// What: releases the deleted drawer's slot (guarded on ownership, so deleting
/// a drawer that already lost the slot leaves the current owner indexed), then
/// removes the row keyed by UUID bytes from DRAWERS.
/// Test: `fact_key_index_tracks_upsert_and_delete`,
/// `deleting_a_drawer_that_lost_its_slot_leaves_the_new_owner_indexed`.
pub(super) fn batch_delete_drawer(
    drawers: &mut Tbl<'_>,
    by_fact_key: &mut Tbl<'_>,
    id: Uuid,
) -> Result<()> {
    // #4884: same ordering rule as the upsert — the row is the only place the
    // slot name is recorded, so read it before removing it.
    if let Some(key) = stored_fact_key(drawers, id)? {
        release_slot_if_owned_by(by_fact_key, &key, id)?;
    }
    let id_bytes = *id.as_bytes();
    drawers
        .remove(id_bytes.as_slice())
        .context("remove drawer record (batch)")?;
    Ok(())
}
