//! Write methods for `KgStoreRedb` plus the in-transaction helpers they and
//! `apply_batch` share.
//!
//! Why: Separating write operations from the struct definition and read
//! operations keeps each file under the 500-SLOC cap.
//! What: `impl KgStoreRedb` for `assert`, `retract`, `retract_triple`,
//! `upsert_drawer`, `delete_drawer`, `delete_by_subject`, plus the
//! free-function helpers `batch_assert` / `batch_retract` that both the
//! single-op methods and `apply_batch` in `import.rs` route through. The
//! drawer-side helpers live in the sibling `drawer_ops` module.
//! Test: `assert_then_query_returns_triple`, `retract_closes_active_interval`,
//! `retract_triple_closes_one_object_and_leaves_siblings_active`,
//! `assert_multiple_objects_for_multivalued_predicate_all_survive`,
//! `assert_functional_predicate_still_supersedes`,
//! `cascade_delete_removes_triples_for_subject`.

use crate::memory_core::palace::Drawer;
use crate::memory_core::store::kg_store::{
    ACTIVE_SUBJECT_COUNTS, DRAWERS, DRAWERS_BY_FACT_KEY, TRIPLES, TRIPLES_BY_OBJECT,
    TRIPLES_BY_PREDICATE, TripleValue, decode_triple_key, decode_u64, decode_value,
    encode_object_index_key, encode_predicate_index_key, encode_triple_key, encode_u64,
    encode_value, is_functional_predicate, prefix_range_end, subject_predicate_prefix,
    subject_prefix,
};
use anyhow::{Context, Result};
use redb::{ReadableDatabase, ReadableTable};
use std::collections::BTreeSet;
use uuid::Uuid;

use super::super::kg::Triple;
use super::drawer_ops::{batch_delete_drawer, batch_upsert_drawer};
use super::store::KgStoreRedb;
use super::types::{Tbl, now_ms};

impl KgStoreRedb {
    /// Assert a triple.
    ///
    /// Why: Temporal model — facts have intervals, so a new assertion closes
    /// what it supersedes instead of overwriting it. #4810 made *what* it
    /// supersedes depend on the predicate: a functional predicate (see
    /// `FUNCTIONAL_PREDICATES`) still allows one active object per subject, but
    /// every other predicate is multi-valued and a second object joins the
    /// first rather than replacing it. Before that split, three
    /// `room:General --contains--> drawer:N` asserts left one row.
    /// What: Single write transaction over TRIPLES + secondary indexes +
    /// ACTIVE_SUBJECT_COUNTS, delegating to [`batch_assert`] so the single-op
    /// and batched paths cannot drift.
    /// Test: `assert_then_query_returns_triple`,
    /// `assert_functional_predicate_still_supersedes`,
    /// `assert_multiple_objects_for_multivalued_predicate_all_survive`.
    pub fn assert(&self, triple: &Triple) -> Result<()> {
        self.check_writable()?;
        let wtx = self.db().begin_write().context("begin assert txn")?;
        {
            let mut triples = wtx.open_table(TRIPLES).context("open triples table")?;
            let mut by_object = wtx
                .open_table(TRIPLES_BY_OBJECT)
                .context("open triples_by_object table")?;
            let mut by_predicate = wtx
                .open_table(TRIPLES_BY_PREDICATE)
                .context("open triples_by_predicate table")?;
            let mut counts = wtx
                .open_table(ACTIVE_SUBJECT_COUNTS)
                .context("open active_subject_counts table")?;
            batch_assert(
                &mut triples,
                &mut by_object,
                &mut by_predicate,
                &mut counts,
                triple,
            )?;
        }
        wtx.commit().context("commit assert txn")?;
        Ok(())
    }

    /// Close every active triple at `(subject, predicate)` without inserting a
    /// replacement. Returns how many rows were closed.
    ///
    /// Why: `assert` closes-and-replaces; retract is the way to say "this is no
    /// longer true and has no successor" — used by `remove_prompt_fact`.
    /// #4810 kept the two-argument signature and widened its meaning from "the
    /// active row" to "every active row", which is behaviour-preserving for
    /// every existing caller: before the object joined the key there could
    /// only ever be one. To remove ONE object and leave its siblings live,
    /// call [`KgStoreRedb::retract_triple`] instead (#5396) — this method takes
    /// the whole pair down.
    /// What: Delegates to [`batch_retract`] inside one write transaction.
    /// Test: `retract_closes_active_interval`,
    /// `retract_closes_every_object_at_the_pair`.
    pub fn retract(&self, subject: &str, predicate: &str) -> Result<usize> {
        self.retract_in_txn(subject, predicate, None)
    }

    /// Close the active triple at exactly `(subject, predicate, object)`,
    /// leaving every other object at that pair untouched. Returns 1 when a row
    /// was closed and 0 when there was none.
    ///
    /// Why: a multi-valued pair can hold one wrong object beside correct ones —
    /// a real subject carrying `--is-a--> hard` next to its good `is-a` rows
    /// (#5396). [`KgStoreRedb::retract`] closes the whole pair, so removing the
    /// wrong object took the right ones with it, and re-asserting does not
    /// displace it either: `is-a` and friends are absent from
    /// `FUNCTIONAL_PREDICATES`, so an assert of a DIFFERENT object joins the
    /// bad row rather than superseding it. #4810 put the object in the redb key,
    /// which is what makes addressing one row possible rather than a redesign.
    /// What: same close as `retract` — history row, active row removed, both
    /// secondary index entries removed, active-subject counter decremented —
    /// but filtered to the single row whose object matches. Naming an object
    /// that is not active at the pair is a no-op, not an error, because a
    /// cleanup pass re-run over the same candidate list must stay idempotent.
    /// A functional predicate gets no special treatment here: this addresses
    /// one row by its full key, so it never closes an object the caller did not
    /// name.
    /// Test: `retract_triple_closes_one_object_and_leaves_siblings_active`,
    /// `retract_triple_on_an_absent_object_is_a_noop`,
    /// `retract_triple_on_a_functional_predicate_closes_only_the_named_object`,
    /// `retract_triple_on_the_only_object_clears_the_active_count`.
    pub fn retract_triple(&self, subject: &str, predicate: &str, object: &str) -> Result<usize> {
        // #5396: three-argument retract so object-side noise can be removed
        // without collateral loss of the good siblings at the same pair.
        self.retract_in_txn(subject, predicate, Some(object))
    }

    /// One write transaction behind both retract shapes.
    ///
    /// Why: `retract` and `retract_triple` differ only in whether the object is
    /// pinned; sharing the transaction body keeps the table set, the commit
    /// boundary, and the writability check from drifting between them.
    /// What: opens TRIPLES plus both secondary indexes and the counter table,
    /// calls [`retract_rows`] with `object`, commits.
    /// Test: covered through both public methods (see their `Test:` lists).
    fn retract_in_txn(
        &self,
        subject: &str,
        predicate: &str,
        object: Option<&str>,
    ) -> Result<usize> {
        self.check_writable()?;
        let wtx = self.db().begin_write().context("begin retract txn")?;
        let closed;
        {
            let mut triples = wtx.open_table(TRIPLES).context("open triples table")?;
            let mut by_object = wtx
                .open_table(TRIPLES_BY_OBJECT)
                .context("open triples_by_object table")?;
            let mut by_predicate = wtx
                .open_table(TRIPLES_BY_PREDICATE)
                .context("open triples_by_predicate table")?;
            let mut counts = wtx
                .open_table(ACTIVE_SUBJECT_COUNTS)
                .context("open active_subject_counts table")?;
            closed = retract_rows(
                &mut triples,
                &mut by_object,
                &mut by_predicate,
                &mut counts,
                subject,
                predicate,
                object,
            )?;
        }
        wtx.commit().context("commit retract txn")?;
        Ok(closed)
    }

    /// Persist a drawer's metadata.
    ///
    /// Why: HNSW only stores vectors keyed by UUID; without drawer metadata
    /// persisted alongside, vector hits map to nothing after a cold restart.
    /// What: Serialize the drawer to `DrawerRecord` and write under its UUID
    /// bytes in DRAWERS, maintaining `DRAWERS_BY_FACT_KEY` in the same
    /// transaction (#4884) so the slot index is never observed out of step with
    /// the row it describes.
    /// Test: `upsert_drawer_then_load_drawers_round_trips`,
    /// `fact_key_index_tracks_upsert_and_delete`.
    pub fn upsert_drawer(&self, drawer: &Drawer) -> Result<()> {
        self.check_writable()?;
        let wtx = self.db().begin_write().context("begin upsert_drawer txn")?;
        {
            let mut drawers = wtx.open_table(DRAWERS).context("open drawers table")?;
            let mut by_fact_key = wtx
                .open_table(DRAWERS_BY_FACT_KEY)
                .context("open drawers_by_fact_key table")?;
            batch_upsert_drawer(&mut drawers, &mut by_fact_key, drawer)?;
        }
        wtx.commit().context("commit upsert_drawer txn")?;
        Ok(())
    }

    /// Remove a drawer by UUID.
    ///
    /// Why: Forgetting must clear both the vector index and the persistent
    /// metadata row — otherwise restart resurrects the drawer.
    /// What: Remove the row keyed by UUID bytes from DRAWERS and drop the
    /// drawer's `DRAWERS_BY_FACT_KEY` entry in the same transaction (#4884), so
    /// a deleted drawer can never leave the slot index claiming an occupant
    /// that no longer exists. No-op on unknown id.
    /// Test: `delete_drawer_removes_row`,
    /// `fact_key_index_tracks_upsert_and_delete`.
    pub fn delete_drawer(&self, id: Uuid) -> Result<()> {
        self.check_writable()?;
        let wtx = self.db().begin_write().context("begin delete_drawer txn")?;
        {
            let mut drawers = wtx.open_table(DRAWERS).context("open drawers table")?;
            let mut by_fact_key = wtx
                .open_table(DRAWERS_BY_FACT_KEY)
                .context("open drawers_by_fact_key table")?;
            batch_delete_drawer(&mut drawers, &mut by_fact_key, id)?;
        }
        wtx.commit().context("commit delete_drawer txn")?;
        Ok(())
    }

    /// Delete all active triples whose subject matches `subject`.
    ///
    /// Why: Cascade-delete on drawer removal (issue #278) — when a drawer is
    /// forgotten, every triple extracted from it (identified by the
    /// `drawer:<uuid>` subject prefix) must be removed so the KG does not
    /// accumulate orphaned edges.
    /// What: Prefix-scans TRIPLES with `subject_prefix(subject)`, collects the
    /// DISTINCT active `(subject, predicate)` pairs — #4810 put the object in
    /// the key, so one pair can now span several rows and retracting per row
    /// would call `retract` N times for the N objects its first call already
    /// closed — and retracts each pair. Returns the number of rows closed.
    /// Test: `cascade_delete_removes_triples_for_subject`,
    /// `cascade_delete_closes_every_object_of_a_multivalued_pair`.
    pub fn delete_by_subject(&self, subject: &str) -> Result<usize> {
        self.check_writable()?;
        let prefix = subject_prefix(subject);
        let end = prefix_range_end(&prefix);
        let mut to_retract: BTreeSet<(String, String)> = BTreeSet::new();
        {
            let rtx = self
                .db()
                .begin_read()
                .context("begin delete_by_subject read")?;
            let triples = rtx
                .open_table(TRIPLES)
                .context("open triples for delete_by_subject scan")?;
            let range = triples
                .range::<&[u8]>(prefix.as_slice()..end.as_slice())
                .context("range scan for delete_by_subject")?;
            for entry in range {
                let (k, v) = entry.context("read row in delete_by_subject")?;
                if k.value().starts_with(b"hist:") {
                    continue;
                }
                let value: TripleValue =
                    decode_value(v.value()).context("decode value in delete_by_subject")?;
                if value.valid_to_ms.is_some() {
                    // Already closed — skip.
                    continue;
                }
                if let Some((s, p, _o)) = decode_triple_key(k.value()) {
                    to_retract.insert((s, p));
                }
            }
        }
        let mut closed = 0usize;
        for (s, p) in &to_retract {
            match self.retract(s, p) {
                Ok(n) => closed += n,
                Err(e) => {
                    tracing::warn!(subject = %s, predicate = %p, "delete_by_subject: retract failed: {e:#}");
                }
            }
        }
        Ok(closed)
    }
}

// ----- in-transaction helpers shared by the single-op and batch paths -----
//
// Why: `assert` and `retract` need identical semantics whether they arrive
// one at a time or inside an `apply_batch` transaction. Both entry points call
// the same free function over already-opened tables, so the txn boundary stays
// explicit (one `begin_write` per call or per batch) and the two paths cannot
// drift. #4810 collapsed the last of a copy-paste duplication here: the
// single-op methods used to carry their own copy of this logic.

/// Every ACTIVE row at `(subject, predicate)`, whatever the object (#4810).
///
/// Why: with the object in the key, "what is currently asserted here?" is a
/// range question, not a point read. Both `batch_assert` and `batch_retract`
/// ask it, and neither may assume the answer has exactly one element — that
/// assumption is what the ticket is about.
/// What: prefix range scan over TRIPLES bounded by
/// [`subject_predicate_prefix`], skipping `hist:` rows and rows already closed.
/// Returns owned keys so the caller can mutate the table afterwards.
/// Test: `assert_multiple_objects_for_multivalued_predicate_all_survive`.
fn active_rows_for_pair(
    triples: &Tbl<'_>,
    subject: &str,
    predicate: &str,
) -> Result<Vec<(Vec<u8>, TripleValue)>> {
    let prefix = subject_predicate_prefix(subject, predicate);
    let end = prefix_range_end(&prefix);
    let mut out = Vec::new();
    let range = triples
        .range::<&[u8]>(prefix.as_slice()..end.as_slice())
        .context("range scan for (subject, predicate)")?;
    for entry in range {
        let (k, v) = entry.context("read row in (subject, predicate) scan")?;
        let key = k.value().to_vec();
        if key.starts_with(b"hist:") {
            continue;
        }
        let value: TripleValue =
            decode_value(v.value()).context("decode triple in (subject, predicate) scan")?;
        if value.valid_to_ms.is_some() {
            continue;
        }
        out.push((key, value));
    }
    Ok(out)
}

/// The three tables a triple write mutates together.
///
/// Why: `close_active_row` needs all three, and threading them as separate
/// parameters alongside the row's own identity pushed it past clippy's
/// argument ceiling. Grouping them also names the real unit — a triple's
/// primary row and its two indexes move as one or the invariant breaks.
/// What: borrowed handles into the caller's open write transaction.
struct TripleTables<'a, 'txn> {
    triples: &'a mut Tbl<'txn>,
    by_object: &'a mut Tbl<'txn>,
    by_predicate: &'a mut Tbl<'txn>,
}

/// Close one active row: copy it to history, drop it, drop its index entries.
///
/// Why: the close half of both `assert` (supersede) and `retract` (no
/// successor). Keeping it in one place is what makes "closing a row" mean the
/// same three mutations everywhere.
/// What: writes `hist:<key><valid_from_ms BE>` carrying the row with
/// `valid_to = close_ms`, removes the active row, and removes its
/// `TRIPLES_BY_OBJECT` / `TRIPLES_BY_PREDICATE` entries. The caller owns the
/// `ACTIVE_SUBJECT_COUNTS` adjustment because it batches several closes into
/// one net delta.
/// Test: `assert_supersedes_prior`, `retract_closes_active_interval`.
fn close_active_row(
    tables: &mut TripleTables<'_, '_>,
    key: &[u8],
    prior: &TripleValue,
    subject: &str,
    predicate: &str,
    close_ms: i64,
) -> Result<()> {
    let mut hist_key = Vec::with_capacity(5 + key.len() + 8);
    hist_key.extend_from_slice(b"hist:");
    hist_key.extend_from_slice(key);
    hist_key.extend_from_slice(&prior.valid_from_ms.to_be_bytes());
    let closed = TripleValue {
        valid_to_ms: Some(close_ms),
        ..prior.clone()
    };
    let closed_bytes = encode_value(&closed).context("encode closed prior row")?;
    tables
        .triples
        .insert(hist_key.as_slice(), closed_bytes.as_slice())
        .context("insert closed history row")?;
    tables
        .triples
        .remove(key)
        .context("remove closed active row")?;

    let obj_key = encode_object_index_key(&prior.object, subject, predicate);
    tables
        .by_object
        .remove(obj_key.as_slice())
        .context("remove object index for closed row")?;
    let pred_key = encode_predicate_index_key(predicate, subject, &prior.object);
    tables
        .by_predicate
        .remove(pred_key.as_slice())
        .context("remove predicate index for closed row")?;
    Ok(())
}

/// Apply a net change to `subject`'s active-triple counter.
///
/// Why: an assert can close several rows and open one in the same call, so the
/// counter moves by a net delta rather than a fixed ±1. Doing the arithmetic
/// once here also keeps the "drop the row at zero" rule in one place.
/// What: reads the current count, applies `delta` saturating, and either
/// removes the row (at zero) or writes the new value. `delta == 0` is a no-op.
/// Test: `count_active_triples_returns_live_only`,
/// `assert_multiple_objects_for_multivalued_predicate_all_survive`.
fn adjust_active_count(counts: &mut Tbl<'_>, subject: &str, delta: i64) -> Result<()> {
    if delta == 0 {
        return Ok(());
    }
    let subj_key = subject.as_bytes();
    let prev = counts
        .get(subj_key)
        .context("read active count")?
        .map(|v| decode_u64(v.value()))
        .unwrap_or(0);
    let next = if delta > 0 {
        prev.saturating_add(delta.unsigned_abs())
    } else {
        prev.saturating_sub(delta.unsigned_abs())
    };
    if next == 0 {
        counts.remove(subj_key).context("remove zero count")?;
    } else {
        counts
            .insert(subj_key, encode_u64(next).as_slice())
            .context("update active count")?;
    }
    Ok(())
}

/// In-transaction assert; the single implementation behind `KgStoreRedb::assert`
/// and `apply_batch`'s `Assert` op.
///
/// Why: see [`KgStoreRedb::assert`]. #4810: which prior rows a new assertion
/// closes is a property of the predicate, not a fixed "the one row at this
/// key".
/// What: scans the `(subject, predicate)` prefix for active rows. For a
/// functional predicate it closes EVERY one it finds — plural, because a
/// palace migrated from the old key, or one whose predicate only just joined
/// the functional list, can legitimately hold more than one. For a
/// multi-valued predicate it closes only the row at this exact
/// `(subject, predicate, object)`, which makes a repeat assertion a
/// re-affirmation (new interval, same fact) and a different object an addition.
/// Then it inserts the new row, indexes it when it is itself active, and moves
/// the active counter by the net delta.
/// Test: `apply_batch_groups_asserts_into_single_commit`,
/// `assert_functional_predicate_still_supersedes`,
/// `assert_multiple_objects_for_multivalued_predicate_all_survive`.
pub(super) fn batch_assert<'txn>(
    triples: &mut Tbl<'txn>,
    by_object: &mut Tbl<'txn>,
    by_predicate: &mut Tbl<'txn>,
    counts: &mut Tbl<'_>,
    triple: &Triple,
) -> Result<()> {
    let close_ms = triple.valid_from.timestamp_millis();
    let new_value = TripleValue {
        object: triple.object.clone(),
        valid_from_ms: triple.valid_from.timestamp_millis(),
        valid_to_ms: triple.valid_to.map(|dt| dt.timestamp_millis()),
        confidence: triple.confidence,
        provenance: triple.provenance.clone(),
    };
    let key = encode_triple_key(&triple.subject, &triple.predicate, &triple.object);
    let functional = is_functional_predicate(&triple.predicate);

    let prior_rows = active_rows_for_pair(triples, &triple.subject, &triple.predicate)?;
    let mut tables = TripleTables {
        triples,
        by_object,
        by_predicate,
    };
    let mut closed: i64 = 0;
    for (prior_key, prior) in prior_rows {
        // #4810: a functional predicate supersedes every object; a
        // multi-valued one supersedes only its own object's prior interval.
        if !functional && prior_key != key {
            continue;
        }
        close_active_row(
            &mut tables,
            &prior_key,
            &prior,
            &triple.subject,
            &triple.predicate,
            close_ms,
        )?;
        closed += 1;
    }

    let new_bytes = encode_value(&new_value).context("encode new triple")?;
    tables
        .triples
        .insert(key.as_slice(), new_bytes.as_slice())
        .context("insert new triple")?;

    // Index the new row only when it is itself active — an assert carrying
    // `valid_to = Some(_)` is closed on arrival and must not appear in an index
    // that means "currently true".
    let mut opened: i64 = 0;
    if new_value.valid_to_ms.is_none() {
        let obj_key =
            encode_object_index_key(&new_value.object, &triple.subject, &triple.predicate);
        tables
            .by_object
            .insert(obj_key.as_slice(), [].as_slice())
            .context("insert new object index")?;
        let pred_key =
            encode_predicate_index_key(&triple.predicate, &triple.subject, &new_value.object);
        tables
            .by_predicate
            .insert(pred_key.as_slice(), [].as_slice())
            .context("insert new predicate index")?;
        opened = 1;
    }
    adjust_active_count(counts, &triple.subject, opened - closed)
}

/// In-transaction retract; the single implementation behind
/// `KgStoreRedb::retract` and `apply_batch`'s `Retract` op.
///
/// Why: see [`KgStoreRedb::retract`].
/// What: closes every active row at `(subject, predicate)` and decrements the
/// active counter by that many. Returns the number closed; `0` when the pair
/// has no active row.
/// Test: `apply_batch_groups_asserts_into_single_commit` (Retract variant),
/// `retract_closes_every_object_at_the_pair`.
pub(super) fn batch_retract<'txn>(
    triples: &mut Tbl<'txn>,
    by_object: &mut Tbl<'txn>,
    by_predicate: &mut Tbl<'txn>,
    counts: &mut Tbl<'_>,
    subject: &str,
    predicate: &str,
) -> Result<usize> {
    retract_rows(
        triples,
        by_object,
        by_predicate,
        counts,
        subject,
        predicate,
        None,
    )
}

/// Close the active rows at `(subject, predicate)`, optionally narrowed to one
/// object (#5396).
///
/// Why: whole-pair retract and single-object retract are the same three
/// mutations over the same tables; the only difference is which rows the scan
/// keeps. Writing them twice is how the two would drift on the next fix to
/// index maintenance or the counter delta.
/// What: scans the pair's active rows via [`active_rows_for_pair`], skips those
/// whose object is not `object` when one is given, closes each survivor through
/// [`close_active_row`], and applies one net decrement to the active-subject
/// counter. Returns the number closed.
/// Test: `retract_closes_every_object_at_the_pair` (the `None` arm),
/// `retract_triple_closes_one_object_and_leaves_siblings_active` (the `Some`
/// arm).
fn retract_rows<'txn>(
    triples: &mut Tbl<'txn>,
    by_object: &mut Tbl<'txn>,
    by_predicate: &mut Tbl<'txn>,
    counts: &mut Tbl<'_>,
    subject: &str,
    predicate: &str,
    object: Option<&str>,
) -> Result<usize> {
    let close_ms = now_ms();
    let active = active_rows_for_pair(triples, subject, predicate)?;
    let mut tables = TripleTables {
        triples,
        by_object,
        by_predicate,
    };
    let mut closed = 0usize;
    for (key, prior) in &active {
        if let Some(target) = object
            && prior.object != target
        {
            continue;
        }
        close_active_row(&mut tables, key, prior, subject, predicate, close_ms)?;
        closed += 1;
    }
    adjust_active_count(counts, subject, -(closed as i64))?;
    Ok(closed)
}
