//! #9735: chunk backfill progress, derived from the index, and its advisory
//! on-disk marker.
//!
//! Why: an operator and the background ticker both need "how far along is
//! this palace", and a counter that a crash or a failure could advance would
//! lie. A closed palace also needs to be findable without opening it.
//! What: [`EmbedHealth::chunk_progress`] recomputes progress from health;
//! [`ChunkMarker`] is a derived summary at `<palace>/chunk_backfill.json` that
//! is never read as proof of completeness.
//! Test: `chunk_backfill_tests`.

use super::embed_repair::EmbedHealth;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;
use trusty_common::json_rmw;
use uuid::Uuid;

/// File name of the advisory marker inside a palace's data directory.
pub(crate) const CHUNK_MARKER_FILE: &str = "chunk_backfill.json";

/// Marker schema version this build writes and trusts.
const MARKER_VERSION: u32 = 1;

/// A `complete` marker older than this is rescanned.
pub(crate) const MARKER_RESCAN_AFTER: Duration = Duration::from_secs(24 * 3600);

/// Where a palace stands on the chunk backfill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ChunkState {
    /// Drawers without a failure row still need a vector or chunk rows.
    Backfilling,
    /// Every live drawer has its full chunk group.
    Complete,
    /// Only drawers with a failure row remain.
    CompleteWithFailures,
    /// The chunk check did not run, so short drawers cannot be counted.
    Unknown,
}

/// Backfill progress for one palace, recomputed from the index on each call.
///
/// Why (#9735 C2): a stored counter could be advanced by a failure; the index
/// cannot.
/// What: `total` live drawers; `done` = total minus drawers missing a vector
/// or short; `failed` = those of the latter with a ledger row; `pending` = the
/// rest. `state` is `Unknown` whenever the chunk check did not run.
/// Test: `progress_counts_done_over_total_for_a_half_backfilled_palace`,
/// `progress_is_unknown_not_complete_when_the_chunk_check_is_unavailable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ChunkProgress {
    pub(crate) total: usize,
    pub(crate) done: usize,
    pub(crate) pending: usize,
    pub(crate) failed: usize,
    pub(crate) state: ChunkState,
}

impl EmbedHealth {
    /// Chunk backfill progress derived from this health snapshot.
    /// Test: `progress_counts_done_over_total_for_a_half_backfilled_palace`.
    #[cfg_attr(not(any(test, feature = "server")), allow(dead_code))] // #9735: the ticker calls it
    pub(crate) fn chunk_progress(&self) -> ChunkProgress {
        let needing: HashSet<Uuid> = self
            .missing_vector_ids
            .iter()
            .chain(&self.short_chunk_ids)
            .copied()
            .collect();
        let ledgered: HashSet<Uuid> = self.recorded_failures.iter().map(|f| f.drawer_id).collect();
        let failed = needing.iter().filter(|id| ledgered.contains(id)).count();
        let pending = needing.len() - failed;
        let state = if self.chunk_check.unavailable_reason().is_some() {
            ChunkState::Unknown
        } else if pending > 0 {
            ChunkState::Backfilling
        } else if failed > 0 {
            ChunkState::CompleteWithFailures
        } else {
            ChunkState::Complete
        };
        ChunkProgress {
            total: self.drawer_count,
            done: self.drawer_count.saturating_sub(needing.len()),
            pending,
            failed,
            state,
        }
    }
}

/// The advisory `chunk_backfill.json` summary.
///
/// Why (ruling 4): the ticker must find closed palaces that need work without
/// opening every one, and a closed palace's progress must be showable.
/// What: written whole from a fresh [`ChunkProgress`], never incremented.
/// Absent or unparsable reads as `None` (missing), never as complete. Delete
/// it with the vector index; it is derived and is not backed up.
/// Test: `unparsable_marker_is_missing_not_complete`,
/// `window_change_forces_a_rescan`, `two_writers_do_not_corrupt_the_marker`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ChunkMarker {
    pub(crate) version: u32,
    /// The embed window the counts were taken at; `None` means no chunking.
    pub(crate) window: Option<usize>,
    pub(crate) total: usize,
    pub(crate) pending: usize,
    pub(crate) failed: usize,
    pub(crate) state: Option<ChunkState>,
    pub(crate) updated_at: Option<DateTime<Utc>>,
    pub(crate) last_error: Option<String>,
}

#[cfg_attr(not(any(test, feature = "server")), allow(dead_code))] // #9735: the ticker calls it
impl ChunkMarker {
    /// A marker for `progress` taken at `window`.
    pub(crate) fn from_progress(
        progress: &ChunkProgress,
        window: Option<usize>,
        last_error: Option<String>,
        now: DateTime<Utc>,
    ) -> Self {
        Self {
            version: MARKER_VERSION,
            window,
            total: progress.total,
            pending: progress.pending,
            failed: progress.failed,
            state: Some(progress.state),
            updated_at: Some(now),
            last_error,
        }
    }

    /// The marker in `dir`, or `None` when absent or unparsable.
    /// Test: `unparsable_marker_is_missing_not_complete`.
    pub(crate) fn read(dir: &Path) -> Option<Self> {
        let bytes = std::fs::read(marker_path(dir)).ok()?;
        match serde_json::from_slice::<Self>(&bytes) {
            Ok(marker) if marker.version == MARKER_VERSION => Some(marker),
            Ok(_) => None,
            Err(e) => {
                tracing::debug!(dir = %dir.display(), "#9735: unparsable chunk marker: {e}");
                None
            }
        }
    }

    /// Replace the marker in `dir` under `json_rmw`'s flock. Blocking: call it
    /// on `spawn_blocking`. An unparsable marker is removed and rewritten.
    /// Test: `two_writers_do_not_corrupt_the_marker`,
    /// `unparsable_marker_is_missing_not_complete`.
    pub(crate) fn write(&self, dir: &Path) -> anyhow::Result<()> {
        let path = marker_path(dir);
        let replace = |path: &Path| {
            json_rmw::update(path, |m: &mut ChunkMarker| {
                *m = self.clone();
                Ok::<(), anyhow::Error>(())
            })
        };
        match replace(&path) {
            Ok(()) => Ok(()),
            Err(_) if path.is_file() && Self::read(dir).is_none() => {
                std::fs::remove_file(&path)?;
                replace(&path)
            }
            Err(e) => Err(e),
        }
    }

    /// Whether a palace with `marker` needs a health scan now.
    ///
    /// Why: the marker only narrows the search; any doubt means scan.
    /// What: true when the marker is absent, written at another `window`,
    /// not complete, or complete but older than [`MARKER_RESCAN_AFTER`].
    /// Test: `window_change_forces_a_rescan`.
    pub(crate) fn needs_scan(
        marker: Option<&Self>,
        window: Option<usize>,
        now: DateTime<Utc>,
    ) -> bool {
        let Some(m) = marker else {
            return true;
        };
        let complete = matches!(
            m.state,
            Some(ChunkState::Complete | ChunkState::CompleteWithFailures)
        );
        let stale = m.updated_at.is_none_or(|at| {
            now.signed_duration_since(at)
                .to_std()
                .map_or(true, |age| age >= MARKER_RESCAN_AFTER)
        });
        m.window != window || !complete || m.pending > 0 || stale
    }
}

fn marker_path(dir: &Path) -> PathBuf {
    dir.join(CHUNK_MARKER_FILE)
}
