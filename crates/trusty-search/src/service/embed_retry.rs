//! Retry a `Failed` semantic stage once the embedder answers again (#10551).
//!
//! Why: an embedder fault mid-stage (`embedder stdio IPC error: child closed
//! stdout`) settles `semantic` as `Failed` and leaves chunks without vectors.
//! The embedder child restarts on its own, but nothing re-ran the stage, so the
//! index stayed `degraded` until an operator ran a full reindex.
//! What: [`spawn_embed_retry_ticker`] wakes every [`TICK`] and calls
//! [`retry_sweep`]. For each index whose semantic stage is `Failed`, the sweep
//! waits out an exponential backoff ([`backoff_delay`]) and then calls
//! `embed_missing::resume_semantic`, which probes the embedder first. A probe
//! that fails is the "embedder has not recovered" answer: the delay grows, the
//! stage is untouched, and probing continues at the cap forever. A resume that
//! was queued and failed again counts against [`MAX_RESUMES`]; after that the
//! sweep stops, since a healthy embedder with a stage that keeps failing is
//! not an embedder fault. `ResumeAction::Settled`, a `Ready` stage, or a
//! `skip_vector` toggle clears the index's entry.
//! Test: `service::embed_retry::tests` (`embed_retry_tests.rs`).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::core::registry::{IndexHandle, StageStatus};
use crate::service::embed_missing::{resume_semantic, ResumeAction, ResumeRefusal};
use crate::service::server::SearchAppState;

/// How often the ticker looks for failed semantic stages.
const TICK: Duration = Duration::from_secs(30);
/// The delay before the first retry of a failed stage.
const BASE_DELAY: Duration = Duration::from_secs(30);
/// The ceiling of the backoff.
const MAX_DELAY: Duration = Duration::from_secs(15 * 60);
/// Queued resumes that may fail again before the sweep gives up on an index.
const MAX_RESUMES: u32 = 5;

/// The wait before retry number `strikes` (0-based): 30 s doubling to 15 min.
///
/// Why: a dead embedder is probed cheaply and often at first, then rarely.
/// What: `BASE_DELAY * 2^strikes`, capped at [`MAX_DELAY`], without overflow.
/// Test: `backoff_doubles_from_the_base_and_stops_at_the_cap`.
pub fn backoff_delay(strikes: u32) -> Duration {
    BASE_DELAY
        .saturating_mul(1u32.checked_shl(strikes).unwrap_or(u32::MAX))
        .min(MAX_DELAY)
}

/// Retry state for one index with a failed semantic stage.
#[derive(Debug)]
struct Entry {
    /// Queued resumes so far; bounded by [`MAX_RESUMES`].
    resumes: u32,
    /// Failed probes and refusals; only lengthens the delay.
    probe_failures: u32,
    next_at: Instant,
}

impl Entry {
    fn delay(&self) -> Duration {
        backoff_delay(self.resumes + self.probe_failures)
    }
}

/// Per-index retry state, owned by the ticker task.
#[derive(Debug, Default)]
pub struct RetryBook {
    entries: HashMap<String, Entry>,
}

impl RetryBook {
    /// Whether the sweep still tracks `index_id` (a test seam).
    pub fn tracks(&self, index_id: &str) -> bool {
        self.entries.contains_key(index_id)
    }

    /// Resumes queued for `index_id` so far (a test seam).
    pub fn resumes(&self, index_id: &str) -> Option<u32> {
        self.entries.get(index_id).map(|e| e.resumes)
    }
}

/// One pass over `handles`: retry what is due (#10551).
///
/// Why: separate from the ticker so a test drives it with a chosen clock.
/// What: for each handle, drops its entry unless the semantic stage is
/// `Failed` (an `InProgress` stage keeps its entry, so a resume that fails
/// again is counted). A first sighting of a `Failed` stage schedules the first
/// attempt after [`BASE_DELAY`]. A due entry under [`MAX_RESUMES`] calls
/// `resume_semantic` and records the outcome as the module doc describes.
/// Test: `a_failed_stage_is_retried_after_its_backoff_and_settles`,
/// `repeated_failed_resumes_stop_at_the_cap_but_a_dead_embedder_is_probed_forever`.
pub async fn retry_sweep(handles: &[Arc<IndexHandle>], book: &mut RetryBook, now: Instant) {
    for handle in handles {
        let id = handle.id.0.clone();
        let status = handle.stages.read().await.semantic.status;
        match status {
            StageStatus::Failed => {}
            StageStatus::InProgress => continue,
            _ => {
                book.entries.remove(&id);
                continue;
            }
        }
        let entry = book.entries.entry(id.clone()).or_insert_with(|| Entry {
            resumes: 0,
            probe_failures: 0,
            next_at: now + BASE_DELAY,
        });
        if now < entry.next_at || entry.resumes >= MAX_RESUMES {
            continue;
        }
        match resume_semantic(handle, false).await {
            Ok(r) if r.action == ResumeAction::Queued => {
                entry.resumes += 1;
                entry.next_at = now + entry.delay();
            }
            Ok(r) if r.action == ResumeAction::Settled => {
                book.entries.remove(&id);
            }
            Ok(r) if r.action == ResumeAction::AlreadyRunning => {
                entry.next_at = now + BASE_DELAY;
            }
            Ok(_) => {
                entry.probe_failures += 1;
                entry.next_at = now + entry.delay();
            }
            Err(ResumeRefusal::NotApplicable(_)) => entry.resumes = MAX_RESUMES,
            Err(ResumeRefusal::Busy(_)) => entry.next_at = now + BASE_DELAY,
            Err(refusal) => {
                entry.probe_failures += 1;
                entry.next_at = now + entry.delay();
                tracing::info!(
                    "embed_retry[{id}]: not retried yet, next try in {}s: {refusal:?}",
                    entry.delay().as_secs(),
                );
            }
        }
    }
}

/// Spawn the ticker that drives [`retry_sweep`] over every resident index.
///
/// Why: see the module doc. Holds a `Weak` like every other daemon ticker, so
/// it ends when the daemon drops its last `Arc`.
/// What: ticks every [`TICK`], skipping the first so a booting daemon is not
/// probed before it serves, and sweeps `registry.list_handles()`.
/// Test: `retry_sweep` is covered directly; this wrapper only schedules it.
pub fn spawn_embed_retry_ticker(state: Arc<SearchAppState>) {
    let weak = Arc::downgrade(&state);
    drop(state);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(TICK);
        interval.tick().await;
        let mut book = RetryBook::default();
        loop {
            interval.tick().await;
            let Some(state) = weak.upgrade() else {
                break;
            };
            let handles = state.registry.list_handles();
            drop(state);
            retry_sweep(&handles, &mut book, Instant::now()).await;
        }
    });
}

#[cfg(test)]
#[path = "embed_retry_tests.rs"]
mod tests;
