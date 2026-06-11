//! The two largest SSE event handlers — `batch` and `complete` — split out of
//! [`super::events`] to keep each file under the 500-line cap.
//!
//! Why: the `batch` arm (first-batch header flip + dual-bar advance + stats)
//! and the terminal `complete` arm (outcome parse + four-bar close-out) are the
//! biggest single transitions; factoring them keeps `handle_event` legible.
//! What: `handle_batch` advances the Chunk/Embed bars and counters; `handle_complete`
//! populates the outcome and finalises every bar.
//! Test: `tests::chunk_bar_not_frozen_at_first_batch`; live-daemon `complete`
//! coverage under `--include-ignored`.

use super::events::LoopState;
use super::phase_map::phase_to_u64;
use super::progress_state::SharedProgress;
use crate::commands::reindex_ui::{ReindexPhase, ReindexTimings, ReindexUi};
use std::sync::atomic::Ordering;

/// Handle a `batch` SSE event: advance the Chunk/Embed bars and counters.
///
/// Why: the batch arm is the largest single transition (header flip on first
/// batch + dual-bar advance + stats); factoring it keeps `handle_event` legible.
/// What: flips into Embedding on the first batch, stores the new file/chunk/cps
/// counts, advances slots 1/2 (guarding against the #827 double-advance), and
/// resets the stall clock.
/// Test: `tests::chunk_bar_not_frozen_at_first_batch` covers the bar behaviour.
pub(super) fn handle_batch(
    state: &mut LoopState,
    ui: &mut ReindexUi,
    progress: &SharedProgress,
    evt: &serde_json::Value,
    index_id: &str,
) {
    let started = progress.started;
    // Issue #823 Bug 1: do NOT call mark_stage_done(1) here.
    // The old code froze the Chunk bar at the batch-transition
    // boundary (e.g. 512/2094). Both Chunk and Embed bars must
    // remain live throughout the CHUNK+EMBED phase.
    //
    // On the first batch event (three-phase flow): activate the
    // Embed bar (slot 2) and transition the header to "Embedding…"
    // if embedder_ready was not received (in-process embedder that
    // didn't emit the event, or legacy daemon).
    if state.received_walk_complete && !state.entered_embedding && !state.lexical_only {
        state.embed_started_ms = started.elapsed().as_millis() as u64;
        ui.set_phase(ReindexPhase::Embedding, index_id);
        progress
            .phase_disc
            .store(phase_to_u64(ReindexPhase::Embedding), Ordering::Release);
        state.entered_embedding = true;
    }
    // Always ensure the Embed bar is visually active once batches start
    // (covers the case where embedder_ready arrived but activate_embed_bar
    // was not called from that handler since set_phase(Embedding) already
    // activates slot 2 via the normal path).

    let indexed = evt.get("indexed").and_then(|v| v.as_u64()).unwrap_or(0);
    let batch_chunks = evt
        .get("batch_chunks")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let chunks_per_sec = evt
        .get("chunks_per_sec")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let total = evt.get("total_files").and_then(|v| v.as_u64()).unwrap_or(0);
    if total > 0 {
        // Issue #744: also update the ticker's total so ETA uses
        // the correct denominator from the very first batch event.
        progress.total_files_now.store(total, Ordering::Release);
        ui.set_total(total);
        // Issue #823 Bug 2: ensure the Embed bar total is always
        // up to date even if walk_complete/start didn't prime it.
        ui.set_embed_total(total);
    }
    progress.indexed_now.store(indexed, Ordering::Release);
    progress.cps_now.store(chunks_per_sec, Ordering::Release);
    let new_chunks = progress
        .chunks_now
        .fetch_add(batch_chunks, Ordering::AcqRel)
        + batch_chunks;
    // The authoritative commit count is now in `chunks_now`; reset
    // the in-flight preview so the ticker shows committed chunks
    // rather than the (now stale) embedding preview.
    progress.chunks_embed_preview.store(0, Ordering::Release);
    // Advance the active phase bar and the Embed bar (slot 2).
    // Chunk bar (slot 1) = files parsed; Embed bar (slot 2) = files
    // committed/embedded. Both use `indexed` (files processed so far)
    // as a proxy — Chunk should lead, but without a separate "files
    // parsed" event from the daemon, `indexed` is the best we have.
    // The visual gap comes from chunk_progress advancing the Chunk
    // bar between batch events (parsed but not yet committed).
    //
    // Issue #827: guard advance_embed_bar so it only runs when the
    // ACTIVE phase is NOT already slot 2 (Embed). When phase==Embed,
    // set_position() already advances slot 2 via the active-slot path;
    // calling advance_embed_bar() as well would double-advance it,
    // causing the Embed bar to jump ahead by 2× the actual progress.
    ui.set_position(indexed); // advances active phase's bar (Chunk or Embed)
    if !ui.active_phase_is_embed() {
        // Only advance slot 2 independently when it is NOT the active bar;
        // when it IS active, set_position() above already advanced it.
        ui.advance_embed_bar(indexed);
    }
    ui.update_stats(
        indexed,
        new_chunks,
        progress.skipped_now.load(Ordering::Acquire),
        chunks_per_sec,
        started.elapsed().as_secs(),
    );
    // Any batch event is forward progress — reset the stall clock.
    state.note_progress(indexed);
}

/// Handle the terminal `complete` SSE event: populate the outcome and close
/// out every progress bar.
///
/// Why: `complete` carries the authoritative totals + per-subsystem timings and
/// must finalise all four bars regardless of which optional events the daemon
/// emitted; missing any close-out leaves a bar stuck mid-animation.
/// What: parses the outcome fields/timings, snaps + marks the Crawl/Chunk/Embed/
/// KG bars done (idempotent), and sets `state.done`.
/// Test: covered indirectly by `run_reindex_with` integration tests.
pub(super) fn handle_complete(
    state: &mut LoopState,
    ui: &mut ReindexUi,
    progress: &SharedProgress,
    evt: &serde_json::Value,
) {
    let started = progress.started;
    let outcome = &mut state.outcome;
    outcome.indexed = evt.get("indexed").and_then(|v| v.as_u64()).unwrap_or(0);
    outcome.total_chunks = evt
        .get("total_chunks")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    outcome.skipped = evt
        .get("skipped")
        .and_then(|v| v.as_u64())
        .unwrap_or_else(|| progress.skipped_now.load(Ordering::Acquire));
    outcome.errors = evt.get("errors").and_then(|v| v.as_u64()).unwrap_or(0);
    outcome.elapsed_ms = evt.get("elapsed_ms").and_then(|v| v.as_u64()).unwrap_or(0);
    // Per-subsystem timings (added in 0.3.11). Absent when talking
    // to an older daemon — outcome.timings stays `None`.
    if let Some(t) = evt.get("timings") {
        let get = |k: &str| t.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
        outcome.timings = Some(ReindexTimings {
            // Issue #744: walk_ms added; zero on old daemons that omit it.
            walk_ms: get("walk_ms"),
            parse_ms: get("parse_ms"),
            embed_ms: get("embed_ms"),
            bm25_ms: get("bm25_ms"),
            vector_upsert_ms: get("vector_upsert_ms"),
            kg_ms: get("kg_ms"),
            vector_count: get("vector_count"),
            symbol_count: get("symbol_count"),
            edge_count: get("edge_count"),
        });
    }
    outcome.completed = true;

    // Snap both Chunk and Embed bars to full position.
    // Chunk bar (slot 1) may still be Active if kg_start was never received.
    // Issue #827: guard advance_embed_bar here too — if the active phase
    // is already Embed, set_position advances slot 2; calling
    // advance_embed_bar additionally would double-advance it.
    ui.set_position(outcome.indexed);
    if !ui.active_phase_is_embed() {
        ui.advance_embed_bar(outcome.indexed);
    }

    // Mark Embed bar done if it wasn't marked by kg_start (old daemon
    // or lexical_only index).
    if state.entered_embedding && !state.lexical_only {
        // Only mark done if not already done by kg_start.
        let embed_ms = outcome
            .timings
            .map(|t| t.embed_ms)
            .unwrap_or_else(|| started.elapsed().as_millis() as u64 - state.embed_started_ms);
        // Use mark_stage_done which is idempotent on Done bars.
        ui.mark_stage_done(2, embed_ms);
    }

    // Issue #823 Bug 1: Mark Chunk bar done unconditionally here
    // (if not already done by kg_start). This covers:
    //   - the three-phase flow where kg_start froze it already (idempotent)
    //   - the two-phase / lexical path where kg_start was never received
    //   - the skip_kg path where the Chunk bar must still close
    let chunk_ms = outcome.timings.map(|t| t.parse_ms).unwrap_or(0);
    ui.mark_stage_done(1, chunk_ms);

    // Mark Crawl bar done for old daemons that never sent walk_complete.
    if !state.received_walk_complete {
        ui.mark_stage_done(0, 0);
    }

    // Mark KG bar done if it wasn't marked by kg_complete (old daemon).
    let kg_ms_final = outcome.timings.map(|t| t.kg_ms).unwrap_or(0);
    ui.mark_stage_done(3, kg_ms_final);

    state.done = true;
}
