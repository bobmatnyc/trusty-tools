//! #9735 PR 1b: split a drawer over the chunk cap into linked drawers.
//!
//! Why: a stored drawer whose text needs more than `MAX_CHUNKS_PER_DRAWER`
//! chunks can never be embedded whole, so the backfill ledgered it and
//! retried it once a day forever. Owner ruling D0c: the backfill splits it.
//! What: [`PalaceHandle::split_oversized`] cuts the text at chunk starts into
//! parts of at most [`PART_CHUNKS`] chunks. The head keeps its id, its
//! `fact_key` and its KG triples, and shrinks to part 1 in place; parts 2..N
//! are new drawers with ids `v5(head, "split:k")`. Every part, the head
//! included, carries `split-of:<head>` and `split-part:k/N`. The split embeds
//! nothing: later slices embed the parts as ordinary drawers. The original
//! content hash is journalled as `split_oversized`. `forget(head)` removes
//! every part ([`PalaceHandle::forget_split_parts`]).
//! Test: `chunk_split_tests`.

use super::chunk_backfill::{SliceReport, SliceStop};
use super::embed_store::{ChunkPlanner, TooManyChunks};
use super::handle::PalaceHandle;
use crate::memory_core::embed::Embedder;
use crate::memory_core::maintenance_log::{self, DeletionReason, MaintenanceDeletion};
use crate::memory_core::palace::Drawer;
use crate::memory_core::store::embed_ledger;
use crate::memory_core::store::vector::VectorStore;
use crate::memory_core::timeouts;
use parking_lot::RwLock;
use std::collections::HashSet;
use std::ops::Range;
use std::time::Duration;
use uuid::Uuid;

/// Chunks per part. Architect ruling 18:21Z: 1024 chunks embed in about 24 s
/// on EVO, inside the 30 s target, and re-planning a part stays far under
/// the 4096 cap.
pub(crate) const PART_CHUNKS: usize = 1024;

const SPLIT_OF: &str = "split-of:";
const SPLIT_PART: &str = "split-part:";

/// The tag every part of `head` carries.
pub(crate) fn split_of_tag(head: Uuid) -> String {
    format!("{SPLIT_OF}{head}")
}

/// Whether `drawer` is a part of a split, the head included.
pub(crate) fn is_split_part(drawer: &Drawer) -> bool {
    drawer.tags.iter().any(|t| t.starts_with(SPLIT_PART))
}

/// The id of part `k` (2..=N) of `head`; part 1 is the head itself.
pub(crate) fn part_id(head: Uuid, k: usize) -> Uuid {
    Uuid::new_v5(&head, format!("split:{k}").as_bytes())
}

/// Group chunk byte ranges into part ranges of at most `per_part` chunks.
///
/// Why: a cut inside a chunk could split a fact; starting part k+1 at its
/// first chunk's start keeps the chunker's overlap on both sides of a seam.
/// What: part k spans the start of its first chunk to the end of its last,
/// so consecutive parts overlap by exactly the chunk overlap.
/// Test: `a_needle_at_every_seam_is_recalled_at_rank_1`.
pub(crate) fn partition(chunks: &[Range<usize>], per_part: usize) -> Vec<Range<usize>> {
    debug_assert!(per_part > 0, "a part holds at least one chunk");
    chunks
        .chunks(per_part.max(1))
        .filter_map(|g| Some(g.first()?.start..g.last()?.end))
        .collect()
}

/// What a split did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SplitOutcome {
    /// The tails and the shrunk head committed.
    Split(usize),
    /// The head is gone or its text changed; nothing was written.
    Stale,
}

/// A split that did not commit.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum SplitError {
    /// The text could not be cut (no tokenizer, or nothing to split).
    #[error("split drawer {id}: {reason}")]
    Plan { id: Uuid, reason: String },
    /// `write_mutex` was not granted within `lock_wait`.
    #[error("palace '{palace}': write lock not granted within {waited:?}")]
    LockWait { palace: String, waited: Duration },
    /// redb refused a tail or the head; parts already written stay.
    #[error("palace '{palace}': split of {id} not stored: {reason}")]
    Store {
        palace: String,
        id: Uuid,
        reason: String,
    },
}

/// What the slice does after a plan error.
pub(crate) enum SplitStep {
    /// Go on to the next drawer.
    Next,
    /// End the slice; `report.stop` is set.
    Stop,
    /// Ledger the drawer with this reason, as before PR 1b.
    Fail(String),
}

impl PalaceHandle {
    /// The backfill's answer to a drawer whose plan failed.
    ///
    /// Why (#9735 PR 1b): an oversized drawer must split, not back off.
    /// What: any error but [`TooManyChunks`] is `Fail`. Otherwise runs
    /// [`Self::split_oversized`]: `Stale` counts `report.stale`; a lock
    /// timeout or a store error ends the slice with no ledger row written or
    /// cleared (Fail-Open b and c); a plan error is `Fail`.
    /// Test: `a_lock_timeout_stops_the_split_and_keeps_the_ledger_row`,
    /// `split_is_idempotent_after_a_crash_between_tails_and_head`.
    pub(crate) async fn split_in_slice(
        &self,
        embedder: &dyn Embedder,
        id: Uuid,
        text: &str,
        error: &anyhow::Error,
        lock_wait: Duration,
        report: &mut SliceReport,
    ) -> SplitStep {
        if error.downcast_ref::<TooManyChunks>().is_none() {
            return SplitStep::Fail(format!("{error:#}"));
        }
        match self.split_oversized(embedder, id, text, lock_wait).await {
            Ok(SplitOutcome::Split(_)) => SplitStep::Next,
            Ok(SplitOutcome::Stale) => {
                report.stale += 1;
                SplitStep::Next
            }
            Err(e @ SplitError::LockWait { .. }) => {
                report.stop = SliceStop::LockTimeout(e.to_string());
                SplitStep::Stop
            }
            Err(e @ SplitError::Store { .. }) => {
                report.attempted += 1;
                report.failed += 1;
                report.stop = SliceStop::StoreError(e.to_string());
                SplitStep::Stop
            }
            Err(e @ SplitError::Plan { .. }) => SplitStep::Fail(e.to_string()),
        }
    }

    /// Split drawer `id`, whose text the caller read as `text`.
    ///
    /// Why: see the module doc. The recheck under the lock is the
    /// `commit_if_current` rule: a drawer forgotten or rewritten since the
    /// read must not gain parts.
    /// What: plans the parts with no lock held. Then, under `write_mutex`
    /// (bounded by `lock_wait`) and `commit_mutex`, rechecks the head and
    /// commits in crash-safe order: tails to redb in one transaction and the
    /// table, then the shrunk head; a crash between them leaves tails a
    /// rerun overwrites by id. Drops the head's stale vector group and
    /// refreshes L1 (failures logged). After the locks: journals the
    /// original hash, rebuilds the closets and clears the head's ledger row.
    /// Test: `split_skips_when_head_content_changed_or_head_forgotten`,
    /// `head_keeps_id_fact_key_and_kg_triples`, `tail_ids_are_deterministic`,
    /// `l1_serves_the_shrunk_head`, `ledger_row_cleared_after_split`.
    pub(crate) async fn split_oversized(
        &self,
        embedder: &dyn Embedder,
        id: Uuid,
        text: &str,
        lock_wait: Duration,
    ) -> Result<SplitOutcome, SplitError> {
        let plan = |reason: String| SplitError::Plan { id, reason };
        let planner = ChunkPlanner::for_embedder(embedder)
            .map_err(|e| plan(format!("{e:#}")))?
            .ok_or_else(|| plan("the embedder does not chunk".to_string()))?;
        let ranges = planner
            .chunk_ranges(text)
            .map_err(|e| plan(format!("{e:#}")))?;
        let parts: Vec<&str> = partition(&ranges, PART_CHUNKS)
            .into_iter()
            .filter_map(|r| text.get(r))
            .collect();
        if parts.len() < 2 {
            return Err(plan(format!("{} chunks make one part", ranges.len())));
        }
        let Some(original) = self.commit_split(id, text, &parts, lock_wait).await? else {
            return Ok(SplitOutcome::Stale);
        };
        tracing::warn!(
            palace = %self.id, drawer = %id, parts = parts.len(),
            "#9735: split an oversized drawer; the head keeps its id"
        );
        let rec = MaintenanceDeletion::new(&self.id, id, DeletionReason::SplitOversized)
            .with_content_hash_of(&original);
        maintenance_log::record(self.data_dir.as_deref(), &rec);
        self.rebuild_closets();
        self.clear_ledger_row(id).await;
        Ok(SplitOutcome::Split(parts.len()))
    }

    /// The locked commit; returns the original head, or `None` when stale.
    async fn commit_split(
        &self,
        id: Uuid,
        text: &str,
        parts: &[&str],
        lock_wait: Duration,
    ) -> Result<Option<Drawer>, SplitError> {
        let palace = || self.id.as_str().to_string();
        let _write = timeouts::lock_with_timeout(&self.write_mutex, lock_wait, self.id.as_str())
            .await
            .map_err(|_| SplitError::LockWait {
                palace: palace(),
                waited: lock_wait,
            })?;
        let _order = self.commit_mutex.lock().await;
        let head = self.drawers.read().iter().find(|d| d.id == id).cloned();
        let Some(head) = head.filter(|h| h.content() == text) else {
            return Ok(None);
        };
        let store = |e: anyhow::Error| SplitError::Store {
            palace: palace(),
            id,
            reason: format!("{e:#}"),
        };
        let n = parts.len();
        let tails: Vec<Drawer> = (2..=n)
            .zip(&parts[1..])
            .map(|(k, part)| tail_of(&head, part, k, n))
            .collect();
        // Fail-Open (a): the tails land first, under ids a rerun reuses.
        self.kg
            .upsert_drawers_atomic(tails.clone())
            .await
            .map_err(store)?;
        mirror(&self.drawers, &tails);
        let mut shrunk = head.clone();
        shrunk.set_content(parts[0]);
        tag_part(&mut shrunk, id, 1, n);
        self.kg.upsert_drawer(&shrunk).await.map_err(store)?;
        mirror(&self.drawers, std::slice::from_ref(&shrunk));
        // The old group no longer matches the text; the head re-embeds.
        if let Err(e) = self.vector_store.remove(id).await {
            tracing::warn!(drawer = %id, "#9735: drop the split head's old vectors: {e:#}");
        }
        // #9773: the live L1 must not keep serving the original text.
        if let Err(e) = self.save_and_refresh_l1() {
            tracing::warn!(palace = %self.id, "#9735: L1 refresh after a split: {e:#}");
        }
        Ok(Some(head))
    }

    /// Clear `id`'s ledger row; a failure only delays the head a backoff.
    async fn clear_ledger_row(&self, id: Uuid) {
        let Some(dir) = self.data_dir.clone() else {
            return;
        };
        let cleared =
            tokio::task::spawn_blocking(move || embed_ledger::clear(&dir, &HashSet::from([id])))
                .await;
        if let Err(e) = cleared.map_err(anyhow::Error::from).and_then(|r| r) {
            tracing::warn!(drawer = %id, "#9735: clear the split head's ledger row: {e:#}");
        }
    }

    /// Forget every part of the split headed by `head`.
    ///
    /// Why (D0c): forgetting the head must not leave its text recallable in
    /// the tails; each removal is journalled (#9283) so the doctor
    /// drawer-count check explains the drop.
    /// What: finds drawers tagged `split-of:<head>` and runs
    /// `forget_removing` plus the user-forget record on each. Runs even when
    /// the head is already gone, so a retried forget finishes the cascade.
    /// Returns the first error after trying every part.
    /// Test: `forget_head_removes_every_part`, `forget_part_removes_only_that_part`,
    /// `forget_head_journals_every_part`.
    pub(crate) async fn forget_split_parts(&self, head: Uuid) -> anyhow::Result<()> {
        let tag = split_of_tag(head);
        let parts: Vec<Uuid> = self
            .drawers
            .read()
            .iter()
            .filter(|d| d.id != head && d.tags.contains(&tag))
            .map(|d| d.id)
            .collect();
        let mut first_error = None;
        for part in parts {
            match self.forget_removing(part).await {
                Ok((removed, l1_saved)) => {
                    if let Some(drawer) = &removed {
                        maintenance_log::record_user_forget(self, drawer);
                    }
                    if let Err(e) = l1_saved {
                        first_error.get_or_insert(e);
                    }
                }
                Err(e) => {
                    tracing::warn!(drawer = %part, "#9735: forget a split part: {e:#}");
                    first_error.get_or_insert(e.context(format!("forget split part {part}")));
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

/// Part `k` of `n` of `head`: the head's room, importance, type, times, tags
/// and source; no Tier C slot.
fn tail_of(head: &Drawer, text: &str, k: usize, n: usize) -> Drawer {
    let mut d = Drawer::new(head.room_id, text);
    d.id = part_id(head.id, k);
    d.importance = head.importance;
    d.drawer_type = head.drawer_type;
    d.created_at = head.created_at;
    d.expires_at = head.expires_at;
    d.source_file = head.source_file.clone();
    d.tags = head.tags.clone();
    tag_part(&mut d, head.id, k, n);
    d
}

/// Replace any split tags on `d` with `split-of:<head>` and `split-part:k/n`.
fn tag_part(d: &mut Drawer, head: Uuid, k: usize, n: usize) {
    d.tags
        .retain(|t| !t.starts_with(SPLIT_OF) && !t.starts_with(SPLIT_PART));
    d.tags.push(split_of_tag(head));
    d.tags.push(format!("{SPLIT_PART}{k}/{n}"));
}

/// Put each of `rows` into the table, replacing a row with the same id.
fn mirror(drawers: &RwLock<Vec<Drawer>>, rows: &[Drawer]) {
    let mut table = drawers.write();
    for row in rows {
        match table.iter_mut().find(|d| d.id == row.id) {
            Some(slot) => *slot = row.clone(),
            None => table.push(row.clone()),
        }
    }
}
