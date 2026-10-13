//! `vector_id` allocation and high-water bound for the HNSW store (#5005).
//!
//! Why: split out of `hnsw_store.rs` so the 500-SLOC production cap measures
//! the store surface, not the allocator's collision-recovery paths. These are
//! free functions over redb tables, called only from `HnswStore::upsert`.
//! What: [`allocate_vector_id`] reserves the next free id inside the caller's
//! write txn; [`high_water`] is the bound both of its correction paths use.
//! Test: `upsert_refuses_to_reuse_an_id_already_present_in_vectors`,
//! `upsert_refuses_an_id_that_only_vector_keys_still_claims`,
//! `two_live_stores_over_one_file_never_alias_ids`.

use redb::{ReadableTable, Table};

use super::{HnswStoreError, MAX_ALLOC_PROBES, Result};
use crate::memory_core::store::kg_store::NEXT_VECTOR_ID;

/// Reserve the next free `vector_id`, inside the caller's write transaction.
///
/// Why (#5005): the old allocator was a process-local `AtomicU64` seeded at
/// open. Two live `HnswStore`s over one database file — which the vector-db
/// cache deliberately allows, so that a palace opened twice in one process
/// shares a single redb handle — each seeded from the same high-water mark and
/// then issued the same ids. Because redb permits one write transaction at a
/// time per database, moving the reservation into the transaction that does the
/// insert makes it serialisable with every other writer on that file: the id
/// and the row that claims it commit or roll back together.
/// What: reads `VECTOR_ID_SEQ`, takes the first candidate with no `VECTORS`
/// row, writes `candidate + 1` back, and returns the candidate. When the
/// candidate IS occupied — a counter left behind by a pre-#5005 binary, or a
/// hand-edited file — it jumps past the highest id EITHER vector table knows
/// about (see [`high_water`]) rather than overwriting, logs the correction, and
/// retries; after [`MAX_ALLOC_PROBES`] it fails with
/// [`HnswStoreError::IdAllocationFailed`] instead of aliasing.
/// A missing counter row (only reachable if the store skipped open-time
/// seeding) falls back to the same high-water mark the seed would have used.
/// Test: `upsert_refuses_to_reuse_an_id_already_present_in_vectors`,
/// `two_live_stores_over_one_file_never_alias_ids`.
pub(super) fn allocate_vector_id(
    seq: &mut Table<'_, &'static str, u64>,
    vectors: &Table<'_, u64, &'static [u8]>,
    keys: &Table<'_, &'static str, u64>,
) -> Result<u64> {
    let mut candidate = match seq.get(NEXT_VECTOR_ID)? {
        Some(g) => g.value(),
        None => {
            tracing::warn!(
                "#5005: VECTOR_ID_SEQ has no counter row at allocation time; \
                 falling back to the VECTORS high-water mark"
            );
            high_water(vectors, keys)?
        }
    };

    for probe in 0..MAX_ALLOC_PROBES {
        if vectors.get(candidate)?.is_none() {
            seq.insert(NEXT_VECTOR_ID, candidate.saturating_add(1))?;
            return Ok(candidate);
        }
        tracing::warn!(
            candidate,
            probe,
            "#5005: persisted vector_id counter is behind the VECTORS table; \
             skipping the occupied id instead of overwriting it"
        );
        // Jump past the highest occupied id so one correction is enough.
        candidate = high_water(vectors, keys)?.max(candidate.saturating_add(1));
    }

    Err(HnswStoreError::IdAllocationFailed {
        probes: MAX_ALLOC_PROBES,
        last_candidate: candidate,
    })
}

/// One past the highest `vector_id` either vector table knows about.
///
/// Why: both the allocator's collision jump and its missing-counter fallback
/// need a bound above which NO id is taken. `VECTORS` alone is not that bound.
/// A `VECTOR_KEYS` row can outlive its `VECTORS` row: a palace written by a
/// binary predating #6195 could have `compact_orphans` delete a
/// concurrently-upserted `VECTORS` row as a false orphan while the key survived
/// (that window is now closed — see `HnswStore::compact_orphans`), and a
/// hand-edited file can present the same shape. Such a state is still reachable
/// on disk, and a `VECTORS`-only bound would hand the id straight back out — the
/// same aliasing this module exists to prevent, arrived at from the other table.
/// What: `max(last VECTORS key, largest mapped VECTOR_KEYS value) + 1`, or 1
/// when both are empty (id 0 is never issued, matching the pre-#5005 seed of
/// `max_seen + 1`). `VECTORS.last()` is O(log n); the `VECTOR_KEYS` sweep is
/// O(n) but runs only on the rare correction path — never on a healthy upsert,
/// where the counter's own invariant already bounds the candidate.
/// Test: `upsert_refuses_to_reuse_an_id_already_present_in_vectors`,
/// `upsert_refuses_an_id_that_only_vector_keys_still_claims`.
fn high_water(
    vectors: &Table<'_, u64, &'static [u8]>,
    keys: &Table<'_, &'static str, u64>,
) -> Result<u64> {
    let mut max_seen = match vectors.last()? {
        Some((k, _)) => k.value(),
        None => 0,
    };
    for entry in keys.iter()? {
        let (_, v) = entry?;
        max_seen = max_seen.max(v.value());
    }
    Ok(max_seen.saturating_add(1))
}
