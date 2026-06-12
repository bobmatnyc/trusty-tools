//! Wall-clock ticker that keeps the reindex stats line moving between SSE
//! events.
//!
//! Why: SSE events are sparse mid-batch (e.g. embedding 256 chunks), so without
//! a 1 s ticker the operator sees a frozen stats line during the slowest part
//! of a reindex. The ticker reads the shared atomics and re-renders the footer.
//! What: `spawn_ticker` launches a `tokio` task that refreshes the stats bar
//! once per second until `tick_done` is set, computing Files N/total, ETA, and
//! the per-batch embed rate from [`SharedProgress`].
//! Test: rendering is side-effect-only (indicatif); ETA/phase logic is covered
//! by `tests::eta_logic_loading_model_and_zero_denom`.

use super::phase_map::{phase_to_u64, u64_to_label};
use super::progress_state::SharedProgress;
use crate::commands::format::format_with_commas;
use crate::commands::reindex_ui::ReindexPhase;
use indicatif::ProgressBar;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;

/// Spawn the once-per-second stats-line ticker.
///
/// Why: the ticker fires every second so the operator sees movement even when
/// no SSE event has arrived; it owns the Files N/total denominator fix (#744),
/// the "loading model…" ETA during embedder cold-start, and the `embed/s`
/// throughput label.
/// What: launches a `tokio::spawn` loop reading `progress`, rendering into
/// `stats_bar`, and exiting when `tick_done` flips to `true`. Returns the
/// `JoinHandle` so the caller can await a clean shutdown.
/// Test: side-effect-only render; logic mirrored by ETA-logic unit test.
pub(super) fn spawn_ticker(
    progress: SharedProgress,
    stats_bar: ProgressBar,
    tick_done: Arc<AtomicBool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let started = progress.started;
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        interval.tick().await; // discard immediate tick
        loop {
            interval.tick().await;
            if tick_done.load(Ordering::Acquire) {
                break;
            }
            let elapsed = started.elapsed().as_secs();
            let indexed = progress.indexed_now.load(Ordering::Acquire);
            // Show the larger of the committed count (chunks_now, updated
            // by `batch` events) and the in-flight preview (chunks_embed_preview,
            // updated by per-wave `chunk_progress` events every ~32 chunks).
            // This gives the operator a live chunk counter that ticks up
            // continuously during the embed phase rather than jumping once per
            // file-batch.
            let chunks = progress
                .chunks_now
                .load(Ordering::Acquire)
                .max(progress.chunks_embed_preview.load(Ordering::Acquire));
            let skipped = progress.skipped_now.load(Ordering::Acquire);
            let cps = progress.cps_now.load(Ordering::Acquire);
            // Fix #744: use the authoritative total from walk_complete/start,
            // not embed_bar.length() which starts at 1.
            let total = progress.total_files_now.load(Ordering::Acquire);
            let phase = progress.phase_disc.load(Ordering::Acquire);
            let is_model_loading = phase == phase_to_u64(ReindexPhase::InitializingEmbedder);
            let fps = indexed.checked_div(elapsed).unwrap_or(0);
            // Fix #744: show "loading model…" during InitializingEmbedder so the
            // operator understands why ETA is unavailable, not "chunking is slow".
            let eta = if is_model_loading {
                "loading model\u{2026}".to_string()
            } else if fps > 0 && total > indexed {
                crate::commands::format::fmt_secs((total - indexed) / fps)
            } else {
                "?".to_string()
            };
            // Use the active phase label so footer matches header (Problem 1 fix).
            let phase_label = u64_to_label(phase);
            // Fix #744: label the per-batch embed rate clearly as "embed/s"
            // (not "cps") to distinguish it from a cumulative cold-start rate.
            let cps_label = if cps > 0 {
                format!("{cps} embed/s")
            } else {
                "---".to_string()
            };
            stats_bar.set_message(format!(
                "{phase_label} {chunks} chunks \u{2014} {cps_label} \u{2014} \
                 Files {indexed}/{total}  Skipped {skipped}  Elapsed {elapsed}s  ETA {eta}",
                chunks = format_with_commas(chunks),
                indexed = format_with_commas(indexed),
                total = format_with_commas(total),
                skipped = format_with_commas(skipped),
                elapsed = elapsed,
                eta = eta,
            ));
        }
    })
}
