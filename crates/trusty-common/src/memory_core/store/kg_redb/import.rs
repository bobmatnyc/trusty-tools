//! Bulk import and batch-apply methods for `KgStoreRedb`.
//!
//! Why: `import_all` is a one-shot migration helper that writes historical
//! rows verbatim; `apply_batch` is the write-coalescing hot path. Both
//! are large enough to warrant their own file.
//! What: `impl KgStoreRedb` for `import_all` and `apply_batch`.
//! Test: `apply_batch_groups_asserts_into_single_commit`,
//! `apply_batch_empty_is_noop`, `apply_batch_mixes_drawer_and_triple_ops`.

use crate::memory_core::palace::Drawer;
use crate::memory_core::store::kg_store::{
    ACTIVE_SUBJECT_COUNTS, DRAWERS, TRIPLES, TRIPLES_BY_OBJECT, TRIPLES_BY_PREDICATE, TripleValue,
    decode_u64, decode_value, encode_object_index_key, encode_predicate_index_key,
    encode_triple_key, encode_u64, encode_value,
};
use anyhow::{Context, Result};
use redb::ReadableTable;

use super::super::kg::Triple;
use super::store::KgStoreRedb;
use super::types::{BatchOpResult, BatchWriteOp, drawer_to_record};
use super::write_ops::{batch_assert, batch_delete_drawer, batch_retract, batch_upsert_drawer};

impl KgStoreRedb {
    /// Import a batch of historical triples and drawers without disturbing
    /// existing rows.
    ///
    /// Why: Issue #45's one-shot SQLite → redb migration needs to write every
    /// legacy row verbatim — both active and closed intervals — without the
    /// "close prior active" semantics that `assert` enforces. The rows
    /// originated from a temporal store and already carry their final
    /// `valid_from` / `valid_to`; we must preserve that history.
    /// What: In a single write transaction, write each triple at either its
    /// primary `(subject, predicate)` key (when active) or a `hist:`-prefixed
    /// key (when closed). Secondary indexes and `ACTIVE_SUBJECT_COUNTS` are
    /// updated only for active rows. Drawers are upserted as-is. If multiple
    /// triples share `(subject, predicate)`, the last active one wins at the
    /// primary key — earlier active rows of the same pair are stored under
    /// `hist:` keys instead so no data is silently dropped.
    /// Test: Covered by the kg_migration integration test in
    /// `crates/trusty-common/tests/kg_migration_tests.rs`.
    pub fn import_all(&self, triples: Vec<Triple>, drawers: Vec<Drawer>) -> Result<()> {
        self.check_writable()?;
        let wtx = self.db().begin_write().context("begin import txn")?;
        {
            let mut triples_t = wtx.open_table(TRIPLES).context("open triples table")?;
            let mut by_object = wtx
                .open_table(TRIPLES_BY_OBJECT)
                .context("open triples_by_object table")?;
            let mut by_predicate = wtx
                .open_table(TRIPLES_BY_PREDICATE)
                .context("open triples_by_predicate table")?;
            let mut counts = wtx
                .open_table(ACTIVE_SUBJECT_COUNTS)
                .context("open active_subject_counts table")?;

            for triple in &triples {
                let value = TripleValue {
                    object: triple.object.clone(),
                    valid_from_ms: triple.valid_from.timestamp_millis(),
                    valid_to_ms: triple.valid_to.map(|dt| dt.timestamp_millis()),
                    confidence: triple.confidence,
                    provenance: triple.provenance.clone(),
                };
                let key = encode_triple_key(&triple.subject, &triple.predicate);
                let bytes = encode_value(&value).context("encode triple value")?;

                if value.valid_to_ms.is_some() {
                    // Closed interval: store under history key, indexed by its
                    // own valid_from_ms so multiple closed rows for the same
                    // (subject, predicate) can coexist.
                    let mut hist_key = Vec::with_capacity(5 + key.len() + 8);
                    hist_key.extend_from_slice(b"hist:");
                    hist_key.extend_from_slice(&key);
                    hist_key.extend_from_slice(&value.valid_from_ms.to_be_bytes());
                    triples_t
                        .insert(hist_key.as_slice(), bytes.as_slice())
                        .context("insert closed history row")?;
                } else {
                    // Active interval: if the primary slot is already taken by
                    // a previously-imported active row, demote that row to
                    // history first so we never silently overwrite an active
                    // fact during migration.
                    let prior_opt: Option<TripleValue> = {
                        let existing = triples_t
                            .get(key.as_slice())
                            .context("read existing triple during import")?;
                        match existing {
                            Some(g) => Some(
                                decode_value(g.value()).context("decode prior during import")?,
                            ),
                            None => None,
                        }
                    };
                    #[allow(clippy::collapsible_if)]
                    if let Some(prior) = prior_opt {
                        if prior.valid_to_ms.is_none() {
                            // Demote existing active row to history (closed at
                            // the new row's valid_from), drop its indexes /
                            // counter so the new row owns them.
                            let mut hist_key = Vec::with_capacity(5 + key.len() + 8);
                            hist_key.extend_from_slice(b"hist:");
                            hist_key.extend_from_slice(&key);
                            hist_key.extend_from_slice(&prior.valid_from_ms.to_be_bytes());
                            let closed = TripleValue {
                                valid_to_ms: Some(value.valid_from_ms),
                                ..prior.clone()
                            };
                            let closed_bytes = encode_value(&closed)
                                .context("encode demoted active row during import")?;
                            triples_t
                                .insert(hist_key.as_slice(), closed_bytes.as_slice())
                                .context("insert demoted history row")?;

                            let obj_key = encode_object_index_key(
                                &prior.object,
                                &triple.subject,
                                &triple.predicate,
                            );
                            by_object
                                .remove(obj_key.as_slice())
                                .context("remove demoted object index")?;
                            let pred_key =
                                encode_predicate_index_key(&triple.predicate, &triple.subject);
                            by_predicate
                                .remove(pred_key.as_slice())
                                .context("remove demoted predicate index")?;

                            let subj_key = triple.subject.as_bytes();
                            let prev = counts
                                .get(subj_key)
                                .context("read prior count during import")?
                                .map(|v| decode_u64(v.value()))
                                .unwrap_or(0);
                            let next = prev.saturating_sub(1);
                            if next == 0 {
                                counts.remove(subj_key).context("remove zero count")?;
                            } else {
                                counts
                                    .insert(subj_key, encode_u64(next).as_slice())
                                    .context("update count after demote")?;
                            }
                        }
                    }

                    triples_t
                        .insert(key.as_slice(), bytes.as_slice())
                        .context("insert imported active triple")?;

                    let obj_key =
                        encode_object_index_key(&value.object, &triple.subject, &triple.predicate);
                    by_object
                        .insert(obj_key.as_slice(), [].as_slice())
                        .context("insert object index for imported row")?;
                    let pred_key = encode_predicate_index_key(&triple.predicate, &triple.subject);
                    by_predicate
                        .insert(pred_key.as_slice(), [].as_slice())
                        .context("insert predicate index for imported row")?;

                    let subj_key = triple.subject.as_bytes();
                    let prev = counts
                        .get(subj_key)
                        .context("read count for imported subject")?
                        .map(|v| decode_u64(v.value()))
                        .unwrap_or(0);
                    let next = prev.saturating_add(1);
                    counts
                        .insert(subj_key, encode_u64(next).as_slice())
                        .context("update count for imported subject")?;
                }
            }

            // Drawers — straight upsert. UUID keys collide cleanly on duplicates.
            let mut drawers_t = wtx.open_table(DRAWERS).context("open drawers table")?;
            for drawer in &drawers {
                let record = drawer_to_record(drawer);
                let bytes = encode_value(&record).context("encode drawer for import")?;
                let id_bytes = *drawer.id.as_bytes();
                drawers_t
                    .insert(id_bytes.as_slice(), bytes.as_slice())
                    .context("insert imported drawer")?;
            }
        }
        wtx.commit().context("commit import txn")?;
        Ok(())
    }

    /// Apply a batch of write ops inside a single redb write transaction.
    ///
    /// Why: Issue #59 follow-up — bulk `assert` / `retract` / drawer
    /// upsert workloads otherwise pay one `begin_write` + one fsync per op.
    /// Coalescing N ops into a single transaction collapses N fsyncs into
    /// one, which is the dominant cost on durable writes. The batch
    /// preserves per-op semantics: each `Assert` still closes any prior
    /// active interval, each `Retract` still moves rows to history, each
    /// drawer op still mutates the DRAWERS table. The only behavioural
    /// difference from calling each op individually is atomicity — if one
    /// op fails, the whole batch is rolled back (caller decides via the
    /// returned error whether to retry individually).
    /// What: Opens one write transaction, applies each `BatchWriteOp` by
    /// delegating to a free-function helper that takes already-opened
    /// tables, and commits once. On any per-op error the transaction is
    /// aborted and the error is returned together with the index of the
    /// failing op so callers can log it.
    /// Test: `apply_batch_groups_asserts_into_single_commit` and
    /// `apply_batch_rolls_back_on_error` in this module.
    pub fn apply_batch(&self, ops: &[BatchWriteOp]) -> Result<Vec<BatchOpResult>> {
        self.check_writable()?;
        if ops.is_empty() {
            return Ok(Vec::new());
        }

        let wtx = self.db().begin_write().context("begin batch txn")?;
        let mut results: Vec<BatchOpResult> = Vec::with_capacity(ops.len());
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
            let mut drawers_t = wtx.open_table(DRAWERS).context("open drawers table")?;

            for (idx, op) in ops.iter().enumerate() {
                let res: Result<BatchOpResult> = match op {
                    BatchWriteOp::Assert(triple) => batch_assert(
                        &mut triples,
                        &mut by_object,
                        &mut by_predicate,
                        &mut counts,
                        triple,
                    )
                    .map(|_| BatchOpResult::Asserted),
                    BatchWriteOp::Retract { subject, predicate } => batch_retract(
                        &mut triples,
                        &mut by_object,
                        &mut by_predicate,
                        &mut counts,
                        subject,
                        predicate,
                    )
                    .map(BatchOpResult::Retracted),
                    BatchWriteOp::UpsertDrawer(drawer) => {
                        batch_upsert_drawer(&mut drawers_t, drawer)
                            .map(|_| BatchOpResult::DrawerUpserted)
                    }
                    BatchWriteOp::DeleteDrawer(id) => batch_delete_drawer(&mut drawers_t, *id)
                        .map(|_| BatchOpResult::DrawerDeleted),
                };
                match res {
                    Ok(r) => results.push(r),
                    Err(e) => {
                        return Err(
                            e.context(format!("batch op #{idx} failed; transaction rolled back"))
                        );
                    }
                }
            }
        }
        wtx.commit().context("commit batch txn")?;
        Ok(results)
    }
}
