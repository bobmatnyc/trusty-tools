//! The deferred-embed pass driver: one wave at a time, with checkpoints (#10294).
//!
//! Why: the pass cloned every owed chunk, held every vector, and committed
//! once at the end. On an 800K-chunk corpus that hit a 15 GB memory cap at 62%
//! of embedding and lost every vector; a search during the pass saw none.
//! What: [`run_deferred_embed`] loads, embeds and commits one wave at a time,
//! so each wave is in the store before the next starts, and writes the vector
//! snapshot on a chunk/time cadence and on every exit. A checkpoint that still
//! fails after [`CHECKPOINT_ATTEMPTS`] fails the pass.
//! Test: `a_finished_wave_is_searchable_and_on_disk_before_the_next_wave_starts`,
//! `a_failed_checkpoint_fails_the_pass_instead_of_settling_ready`.

use std::future::Future;
use std::ops::Deref;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::core::store::VectorStore;

use super::super::persist_hnsw::checkpoint_hnsw_target_checked;
use super::super::{embed_batch_size, CodeIndexer};
use super::embed::resolve_embed_inflight;
use super::EmbedCatchUp;

/// Default for `TRUSTY_EMBED_CHECKPOINT_CHUNKS`: embedded chunks between
/// two vector snapshots. A snapshot rewrites the whole index, so a per-wave
/// snapshot of an 800K-chunk index would write ~1.2 GB per wave.
const DEFAULT_CHECKPOINT_CHUNKS: usize = 16_384;

/// Default for `TRUSTY_EMBED_CHECKPOINT_SECS`: the longest gap between two
/// vector snapshots while a pass commits waves.
const DEFAULT_CHECKPOINT_SECS: u64 = 120;

/// Attempts at one checkpoint before the pass fails.
const CHECKPOINT_ATTEMPTS: u32 = 3;

/// When the pass writes a vector snapshot.
///
/// Why: a kill loses at most the waves since the last snapshot; a snapshot
/// costs a full index write. The cadence bounds both.
/// What: `TRUSTY_EMBED_CHECKPOINT_CHUNKS` (default 16 384) and
/// `TRUSTY_EMBED_CHECKPOINT_SECS` (default 120), each a positive integer.
/// Test: `checkpoint_cadence_fires_at_either_threshold`.
struct Cadence {
    chunks: usize,
    every: Duration,
    since: usize,
    last: Instant,
}

impl Cadence {
    fn from_env() -> Self {
        fn positive<T: std::str::FromStr + PartialOrd + Default>(key: &str) -> Option<T> {
            std::env::var(key)
                .ok()
                .and_then(|v| v.parse::<T>().ok())
                .filter(|n| *n > T::default())
        }
        Self::new(
            positive("TRUSTY_EMBED_CHECKPOINT_CHUNKS").unwrap_or(DEFAULT_CHECKPOINT_CHUNKS),
            Duration::from_secs(
                positive("TRUSTY_EMBED_CHECKPOINT_SECS").unwrap_or(DEFAULT_CHECKPOINT_SECS),
            ),
        )
    }

    fn new(chunks: usize, every: Duration) -> Self {
        Self {
            chunks,
            every,
            since: 0,
            last: Instant::now(),
        }
    }

    /// Count `n` committed chunks; `true` when a snapshot is due.
    fn record(&mut self, n: usize) -> bool {
        self.since += n;
        self.since >= self.chunks || self.last.elapsed() >= self.every
    }

    fn reset(&mut self) {
        self.since = 0;
        self.last = Instant::now();
    }
}

/// Embed every owed chunk, one wave at a time (#10294).
///
/// Why: see the module docs. `access` hands out the indexer for each phase so
/// a caller holding it behind a `RwLock` takes the read guard per phase and
/// never across embedding (#8600).
/// What: plans the owed ids, then per wave: load the chunks, embed with no
/// guard, commit, and snapshot when the cadence is due. Stops at a pause,
/// drain, stall or error. Every exit after a commit writes a snapshot; a
/// snapshot that fails [`CHECKPOINT_ATTEMPTS`] times is the pass's error, and
/// on an error exit it is attached to the first error. `embedded` and
/// `rejected` sum the waves; `total` is the corpus size.
/// Test: `a_finished_wave_is_searchable_and_on_disk_before_the_next_wave_starts`,
/// `a_failed_checkpoint_fails_the_pass_instead_of_settling_ready`,
/// `a_paused_pass_owes_work_and_a_resumed_one_embeds_only_the_gap`,
/// `a_stalled_wave_commits_the_waves_before_it`.
pub(crate) async fn run_deferred_embed<F, Fut, G>(
    access: F,
    progress_tx: Option<&tokio::sync::mpsc::UnboundedSender<(usize, u64)>>,
    pause: Option<&crate::core::embed_pause::EmbeddingPause>,
) -> Result<EmbedCatchUp>
where
    F: Fn() -> Fut,
    Fut: Future<Output = G>,
    G: Deref<Target = CodeIndexer>,
{
    let mut pass = access().await.plan_deferred_pass().await;
    let mut out = EmbedCatchUp::finished(0, pass.total);
    let wave = resolve_embed_inflight() * embed_batch_size();
    let mut cadence = Cadence::from_env();
    let mut committed_any = false;
    let ended: Result<()> = async {
        loop {
            let Some(plan) = access().await.plan_deferred_group(&mut pass, wave).await? else {
                return Ok(());
            };
            let run = plan.embed(progress_tx, pause).await?;
            committed_any = true;
            let n = {
                let indexer = access().await;
                let wave_out = indexer.commit_deferred_embed(plan, run).await?;
                out.embedded += wave_out.embedded;
                out.rejected.extend(wave_out.rejected);
                out.paused = wave_out.paused;
                wave_out.total
            };
            if out.paused {
                return Ok(());
            }
            // #10294: a due snapshot is written before the next wave starts.
            if cadence.record(n) {
                checkpoint(&access).await?;
                cadence.reset();
            }
        }
    }
    .await;
    if !committed_any {
        return ended.map(|()| out);
    }
    // #10294: every exit after a commit makes the committed waves durable.
    match (ended, checkpoint(&access).await) {
        (Ok(()), Ok(())) => Ok(out),
        (Ok(()), Err(save)) => Err(save),
        (Err(e), Ok(())) => Err(e),
        (Err(e), Err(save)) => Err(e.context(format!("and the final checkpoint failed: {save:#}"))),
    }
}

/// Write the vector snapshot now, retrying up to [`CHECKPOINT_ATTEMPTS`] times.
///
/// Why: the background persist only logs a failed save; a pass that kept
/// embedding past one reported vectors durable that were not (#10294).
/// What: resolves the store and target under a short guard, then saves with
/// no guard held, so a long save does not block a queued writer (#8600).
/// Nothing to save (no store, a quarantined index, no resolvable path) is
/// `Ok`. Each attempt that fails waits 250 ms × attempt before the next.
/// Test: `a_failed_checkpoint_fails_the_pass_instead_of_settling_ready`.
async fn checkpoint<F, Fut, G>(access: &F) -> Result<()>
where
    F: Fn() -> Fut,
    Fut: Future<Output = G>,
    G: Deref<Target = CodeIndexer>,
{
    let Some((store, path)) = access().await.vector_checkpoint_target()? else {
        return Ok(());
    };
    let mut attempt = 1;
    loop {
        match store.save_to(&path).await {
            Ok(()) => return Ok(()),
            Err(e) if attempt >= CHECKPOINT_ATTEMPTS => {
                return Err(e).with_context(|| {
                    format!(
                        "checkpoint the embedded vectors to {} failed {CHECKPOINT_ATTEMPTS} times",
                        path.display()
                    )
                });
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    attempt,
                    "embed pass checkpoint failed, retrying: {e:#}"
                );
                tokio::time::sleep(Duration::from_millis(250 * u64::from(attempt))).await;
                attempt += 1;
            }
        }
    }
}

impl CodeIndexer {
    /// The store and path an embed-pass checkpoint writes (#10294).
    ///
    /// Why/What: the target the background persist picks
    /// (`checkpoint_hnsw_target_checked`), staging during a reindex. `None`
    /// with no store, a quarantined index, or no resolvable path; `Err` when
    /// the path is refused.
    /// Test: `a_failed_checkpoint_fails_the_pass_instead_of_settling_ready`.
    fn vector_checkpoint_target(&self) -> Result<Option<(Arc<dyn VectorStore>, PathBuf)>> {
        let Some(store) = self.store.clone() else {
            return Ok(None);
        };
        if self.refuse_durable_write("deferred_embed_checkpoint", &self.index_id) {
            return Ok(None);
        }
        let reindexing = self.persist_state.reindexing.load(Ordering::Acquire);
        let target = checkpoint_hnsw_target_checked(
            self.storage_layout,
            &self.index_id,
            &self.root_path,
            reindexing,
        )?;
        Ok(target.map(|path| (store, path)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cadence fires on the chunk count, and on elapsed time alone.
    /// Test: this IS the test.
    #[test]
    fn checkpoint_cadence_fires_at_either_threshold() {
        let mut by_count = Cadence::new(10, Duration::from_secs(3600));
        assert!(!by_count.record(9));
        assert!(by_count.record(1));
        by_count.reset();
        assert!(!by_count.record(9), "a reset starts the count again");

        let mut by_time = Cadence::new(usize::MAX, Duration::ZERO);
        assert!(by_time.record(0));
    }
}
