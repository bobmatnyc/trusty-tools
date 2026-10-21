//! Recall access recording: bump a served drawer's `access_count` (#9881).
//!
//! Why: the access boost (`Drawer::accumulated_boost`) feeds recall rank,
//! dream prune and fading, but no production path raised `access_count`, so a
//! drawer recalled every day looked as unused as one nobody read. A redb
//! commit per hit on the read path would serialise every recall behind the
//! writer, so the bump is buffered.
//! What: [`PalaceHandle::record_accesses`] bumps the in-memory drawers at once
//! and marks them dirty in the handle's [`AccessLog`]. The log persists the
//! dirty drawers' access fields to the `drawer_access` side table, in one redb
//! transaction, after [`ACCESS_FLUSH_DEBOUNCE`], when
//! [`PalaceHandle::flush_pending_accesses`] runs (a dream cycle calls it
//! before prune), and when the handle drops. A crash before a flush loses at
//! most that window of increments (ruling Q5). Every failure is logged and
//! never fails the recall.
//! Test: `access_tests` in this module's directory;
//! `tools::tests::recall_access_tests` end to end through the handlers.

use super::handle::PalaceHandle;
use crate::engine::palace::{Drawer, PalaceId};
use crate::engine::store::kg::KnowledgeGraph;
use anyhow::{Context, Result};
use parking_lot::{Mutex, RwLock};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;
use uuid::Uuid;

/// How long the first unflushed access waits before its batch is written (Q5).
pub(crate) const ACCESS_FLUSH_DEBOUNCE: Duration = Duration::from_secs(5);

/// One palace's unflushed recall accesses and what it needs to write them.
///
/// Why: the flush runs in a background task and in `Drop`, neither of which
/// can borrow the handle; the log carries clones of the handle's `Arc`s.
/// What: the dirty drawer ids, a "flush task scheduled" flag, and the debounce.
/// The background task holds only a `Weak`, so it never keeps a dropped
/// palace's redb files open. A write reads the CURRENT in-memory counts and
/// touches only `drawer_access`, so it needs no palace lock and can never
/// overwrite a drawer row another writer just committed.
/// Test: `a_flush_failure_requeues_and_never_fails_the_caller`,
/// `dropping_the_handle_persists_pending_accesses`.
pub(crate) struct AccessLog {
    palace: PalaceId,
    kg: Arc<KnowledgeGraph>,
    drawers: Arc<RwLock<Vec<Drawer>>>,
    pending: Mutex<HashSet<Uuid>>,
    scheduled: AtomicBool,
    // #10013: read only by `mark_dirty`, whose caller chain needs `server`.
    #[cfg_attr(not(feature = "server"), allow(dead_code))]
    debounce_ms: AtomicU64,
    /// Test seam: the next write fails as a failed redb commit would.
    #[cfg(test)]
    fail_next_write: AtomicBool,
}

impl AccessLog {
    /// Build the log over clones of the handle's drawer table and KG.
    pub(crate) fn shared(
        palace: &PalaceId,
        kg: &Arc<KnowledgeGraph>,
        drawers: &Arc<RwLock<Vec<Drawer>>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            palace: palace.clone(),
            kg: Arc::clone(kg),
            drawers: Arc::clone(drawers),
            pending: Mutex::default(),
            scheduled: AtomicBool::new(false),
            debounce_ms: AtomicU64::new(ACCESS_FLUSH_DEBOUNCE.as_millis() as u64),
            #[cfg(test)]
            fail_next_write: AtomicBool::new(false),
        })
    }

    /// Number of drawers with an access not yet written.
    #[cfg(test)]
    pub(crate) fn pending_len(&self) -> usize {
        self.pending.lock().len()
    }

    /// Mark `ids` dirty and, on the first one, schedule the debounced flush.
    ///
    /// Why: one write per debounce window, however many recalls land in it.
    /// What: with no tokio runtime the ids wait for the next flush or the drop.
    // #10013: only `record_accesses` calls it, and its caller chain needs `server`.
    #[cfg_attr(not(feature = "server"), allow(dead_code))]
    fn mark_dirty(self: &Arc<Self>, ids: impl IntoIterator<Item = Uuid>) {
        self.pending.lock().extend(ids);
        if self.scheduled.swap(true, Ordering::AcqRel) {
            return;
        }
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            self.scheduled.store(false, Ordering::Release);
            return;
        };
        let weak: Weak<Self> = Arc::downgrade(self);
        let wait = Duration::from_millis(self.debounce_ms.load(Ordering::Relaxed));
        rt.spawn(async move {
            tokio::time::sleep(wait).await;
            // The handle dropped first: its `Drop` already wrote the batch.
            if let Some(log) = weak.upgrade()
                && let Err(e) = log.flush().await
            {
                tracing::warn!(palace = %log.palace, "#9881: access flush failed: {e:#}");
            }
        });
    }

    /// Write every pending access in one transaction; returns how many rows.
    ///
    /// Why: the persistence half of [`Self::mark_dirty`].
    /// What: a read-only KG drops the batch (it can never be written). On a
    /// failed write the ids return to the pending set for the next flush.
    pub(crate) async fn flush(&self) -> Result<usize> {
        self.scheduled.store(false, Ordering::Release);
        if self.pending.lock().is_empty() {
            return Ok(0);
        }
        if self.kg.is_read_only() {
            self.pending.lock().clear();
            return Ok(0);
        }
        let ids = std::mem::take(&mut *self.pending.lock());
        let rows = self.current_rows(&ids);
        let written = rows.len();
        if let Err(e) = self.write(rows).await {
            self.pending.lock().extend(ids);
            return Err(e);
        }
        Ok(written)
    }

    /// The live drawers among `ids`; a drawer forgotten meanwhile is skipped.
    fn current_rows(&self, ids: &HashSet<Uuid>) -> Vec<Drawer> {
        self.drawers
            .read()
            .iter()
            .filter(|d| ids.contains(&d.id))
            .cloned()
            .collect()
    }

    async fn write(&self, rows: Vec<Drawer>) -> Result<()> {
        self.check_seam()?;
        if rows.is_empty() {
            return Ok(());
        }
        let store = self.kg.store();
        tokio::task::spawn_blocking(move || store.put_accesses(&rows))
            .await
            .context("access write join")?
    }

    /// The test seam's failure, consumed once; always `Ok` outside tests.
    fn check_seam(&self) -> Result<()> {
        #[cfg(test)]
        if self.fail_next_write.swap(false, Ordering::SeqCst) {
            anyhow::bail!("#9881 test seam: access write failed");
        }
        Ok(())
    }

    /// Test seam: make the next write fail.
    #[cfg(test)]
    pub(crate) fn fail_next_write(&self) {
        self.fail_next_write.store(true, Ordering::SeqCst);
    }

    /// Test seam: shorten the debounce.
    #[cfg(test)]
    pub(crate) fn set_debounce(&self, wait: Duration) {
        self.debounce_ms
            .store(wait.as_millis() as u64, Ordering::Relaxed);
    }
}

impl Drop for AccessLog {
    /// Why (Q5): a handle released by the idle sweep or a recall-all batch
    /// must not lose the accesses its debounce had not written yet.
    /// What: a synchronous one-transaction write; a failure is logged.
    fn drop(&mut self) {
        let ids = std::mem::take(self.pending.get_mut());
        if ids.is_empty() || self.kg.is_read_only() {
            return;
        }
        let rows = self.current_rows(&ids);
        let written = self
            .check_seam()
            .and_then(|()| self.kg.store().put_accesses(&rows));
        if let Err(e) = written {
            tracing::warn!(palace = %self.palace, "#9881: access write at drop failed: {e:#}");
        }
    }
}

impl PalaceHandle {
    /// Record that a recall served `ids` to its caller (#9881).
    ///
    /// Why: only the recall surface knows which drawers the caller received,
    /// after fusion, the floor and the `top_k` cut; the engine's recall sees
    /// the whole fetch window. Callers pass lane hits only (ruling Q2).
    /// What: a read-only handle records nothing (Q6). Otherwise, under one
    /// short `drawers` write lock and with no I/O, each matching drawer's
    /// `access_count` saturates up by one and `last_accessed_at` becomes now;
    /// the live L1 copy follows. The ids are queued for the debounced flush.
    /// Returns nothing: a failure to persist is logged, never surfaced.
    /// Test: `record_accesses_bumps_in_memory_and_persists_on_flush`,
    /// `a_read_only_handle_records_nothing`,
    /// `a_served_lane_hit_persists_its_access_count_across_reopen`.
    // #10013: the only caller, `tools::recall_access`, compiles under `server`.
    #[cfg_attr(not(feature = "server"), allow(dead_code))]
    pub(crate) fn record_accesses(&self, ids: &[Uuid]) {
        if ids.is_empty() || self.is_read_only() {
            return;
        }
        let wanted: HashSet<Uuid> = ids.iter().copied().collect();
        let mut bumped = Vec::with_capacity(wanted.len());
        for drawer in self.drawers.write().iter_mut() {
            if wanted.contains(&drawer.id) {
                drawer.record_access();
                bumped.push((drawer.id, drawer.access_count, drawer.last_accessed_at));
            }
        }
        if bumped.is_empty() {
            return;
        }
        for copy in self.l1_drawers.write().iter_mut() {
            if let Some((_, count, at)) = bumped.iter().find(|(id, ..)| *id == copy.id) {
                copy.access_count = *count;
                copy.last_accessed_at = *at;
            }
        }
        self.accesses
            .mark_dirty(bumped.into_iter().map(|(id, ..)| id));
    }

    /// Write this palace's pending recall accesses now (#9881).
    ///
    /// Why: a dream cycle reads the counts prune and fading depend on; flushing
    /// first makes them durable before it can delete anything (Q5).
    /// What: [`AccessLog::flush`]. Errors (lock wait, failed commit) leave the
    /// ids pending for the next flush.
    /// Test: `a_flush_failure_requeues_and_never_fails_the_caller`.
    pub(crate) async fn flush_pending_accesses(&self) -> Result<usize> {
        self.accesses.flush().await
    }
}

#[cfg(test)]
#[path = "access_tests.rs"]
mod access_tests;
