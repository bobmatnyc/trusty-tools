//! #6415: the `complete` frame's `status` field must agree with the terminal
//! status stored beside it.
//!
//! Why: the payload derived `status` from `RunTotals::mem_limit_hit` — the flag
//! the batch loop sets from its own post-commit RSS sample — while the enum
//! status derived from `mem_limit_hit || mem_abort`. The background memory
//! poller (`spawn_memory_poller`) sets `mem_abort` on its own tick and the
//! producer then halts at the next batch boundary, so a run can end with
//! `mem_abort` set and `mem_limit_hit` never set. The daemon recorded that run as
//! `AbortedMemory` while the frame on the wire said `"complete"` with
//! `memory_limit_hit: false`, and
//! `trusty_common::monitor::search_client::parse_reindex_event` reads the string,
//! not the enum — a memory-aborted reindex reported success to its consumer.
//!
//! What: [`the_payload_status_agrees_with_the_terminal_status`] drives
//! `emit_complete_event` on the divergent input (an `AbortedMemory` run whose
//! `RunTotals` carry no batch-loop trip) and reads the frame back off the replay
//! buffer. [`a_clean_run_still_reports_complete`] pins the other arm.
//!
//! Test: this file IS the test module.

use std::time::Instant;

use super::completion::{emit_complete_event, KgRebuildOutcome, RunTotals};
use super::{ReindexProgress, ReindexStatus};

/// Build totals with every accumulator at zero.
///
/// The memory verdict is deliberately absent from `RunTotals` since #6415 — the
/// point of these tests is that the frame reads it off `terminal_status` alone.
fn zeroed_totals() -> RunTotals {
    RunTotals {
        walk_ms: 0,
        parse_ms: 0,
        embed_ms: 0,
        bm25_ms: 0,
        vector_upsert_ms: 0,
        vector_count: 0,
        chunks_dropped_by_cap: 0,
        stages: super::stage_timings::StageTimings::default(),
        other_ms: 0,
    }
}

/// A KG rebuild that installed a graph cleanly.
fn clean_kg() -> KgRebuildOutcome {
    KgRebuildOutcome {
        symbol_count: 0,
        edge_count: 0,
        kg_ms: 0,
        kg_skipped: false,
        contrib_merge_error: None,
    }
}

/// Emit one terminal frame and return it parsed, alongside the stored status.
async fn emit_and_read(terminal: ReindexStatus) -> (serde_json::Value, ReindexStatus) {
    let progress = ReindexProgress::new();
    emit_complete_event(
        &progress,
        terminal,
        Instant::now(),
        0,
        None,
        &zeroed_totals(),
        &clean_kg(),
    )
    .await;
    let buf = progress.events.lock().await;
    let line = buf
        .last()
        .expect("a terminal frame must be buffered")
        .clone();
    drop(buf);
    let frame: serde_json::Value =
        serde_json::from_str(&line).expect("the terminal frame must be valid JSON");
    (frame, progress.status.load())
}

/// A `mem_abort`-only run must reach the wire as `aborted_memory`.
///
/// Pre-fix this frame read `"complete"` with `memory_limit_hit: false`, because
/// the batch loop never set `mem_limit_hit` — exactly the run the memory poller
/// halts on its own tick.
#[tokio::test]
async fn the_payload_status_agrees_with_the_terminal_status() {
    let (frame, stored) = emit_and_read(ReindexStatus::AbortedMemory).await;
    assert_eq!(
        stored,
        ReindexStatus::AbortedMemory,
        "the stored enum must be the status passed in"
    );
    assert_eq!(
        frame["status"], "aborted_memory",
        "a run stored as AbortedMemory must not report `complete` on the wire"
    );
    assert_eq!(
        frame["memory_limit_hit"], true,
        "`memory_limit_hit` must carry the same verdict as `status`"
    );
}

/// The clean arm is unchanged: no abort, no `aborted_memory` on the wire.
#[tokio::test]
async fn a_clean_run_still_reports_complete() {
    let (frame, stored) = emit_and_read(ReindexStatus::Complete).await;
    assert_eq!(stored, ReindexStatus::Complete);
    assert_eq!(frame["status"], "complete");
    assert_eq!(frame["memory_limit_hit"], false);
}
