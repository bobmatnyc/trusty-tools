//! SSE event dispatch for the reindex progress stream.
//!
//! Why: the daemon emits a dozen distinct SSE event types (walk_complete,
//! start, embedder_init/ready, chunk_progress, batch, skip, kg_start/complete,
//! complete, error) and each drives a specific multi-bar UI transition. Keeping
//! the per-event state machine here keeps the driver loop a thin pump.
//! What: `LoopState` holds the mutable phase-transition flags and stall-clock
//! state; `handle_event` interprets one parsed SSE payload, updating the
//! `ReindexUi`, the shared atomics, and the loop's terminal/stall bookkeeping.
//! Test: the full loop needs a live daemon (`--include-ignored`); the UI
//! transitions are unit-tested in `tests` (e.g. `chunk_bar_not_frozen_at_first_batch`).

use super::options::ReindexOutcome;
use super::phase_map::phase_to_u64;
use super::progress_state::SharedProgress;
use crate::commands::format::format_with_commas;
use crate::commands::reindex_ui::{ReindexPhase, ReindexUi};
use colored::Colorize;
use std::sync::atomic::Ordering;
use std::time::Instant;

/// Mutable per-run state threaded through the SSE event loop.
///
/// Why: the walk→chunk→embed→KG phase machine needs several flags plus the
/// stall-detection clock to persist across events; bundling them keeps the
/// driver loop free of a dozen loose `let mut` bindings.
/// What: phase-transition flags, per-stage elapsed-ms accumulators, the
/// accumulating `outcome`, and the stall-window snapshot/instant.
/// Test: indirectly via the UI-transition unit tests in `tests`.
pub(super) struct LoopState {
    /// `complete` received — terminates the loop.
    pub done: bool,
    /// Accumulated outcome populated from SSE fields.
    pub outcome: ReindexOutcome,
    /// Whether `walk_complete` was seen (drives three- vs two-phase flow).
    pub received_walk_complete: bool,
    /// Whether this index is lexical-only (no embed phase).
    pub lexical_only: bool,
    /// Whether the Embed phase has been entered.
    pub entered_embedding: bool,
    /// Issue #929: daemon is embedding in a background job.
    pub defer_embed: bool,
    /// Wall-clock ms (relative to `started`) when chunking began.
    pub chunk_started_ms: u64,
    /// Wall-clock ms (relative to `started`) when embedding began.
    pub embed_started_ms: u64,
    /// Last `indexed` value observed advancing (stall-clock snapshot).
    pub last_indexed_snapshot: u64,
    /// Instant of the last observed progress (resets the stall window).
    pub last_progress: Instant,
}

impl LoopState {
    /// Construct the initial loop state anchored at `started`.
    ///
    /// Why: `last_progress` must start at "now" so a fresh session gets a full
    /// stall window before the first batch event could plausibly arrive.
    /// What: zeroes all flags/accumulators and seeds the stall clock at `now`.
    /// Test: indirectly via the driver's wait-strategy tests.
    pub(super) fn new(started: Instant) -> Self {
        Self {
            done: false,
            outcome: ReindexOutcome::default(),
            received_walk_complete: false,
            lexical_only: false,
            entered_embedding: false,
            defer_embed: false,
            chunk_started_ms: 0,
            embed_started_ms: 0,
            last_indexed_snapshot: 0,
            last_progress: started,
        }
    }

    /// Record forward progress at `indexed`, resetting the stall clock.
    ///
    /// Why: any `batch`/`skip`/`chunk_progress` advance must reset the
    /// stall-detection window so a healthy-but-slow run is never detached.
    /// What: bumps the snapshot + `last_progress` when `indexed` advanced.
    /// Test: stall logic unit-tested in `tests::stall_detection_*`.
    pub(super) fn note_progress(&mut self, indexed: u64) {
        if indexed > self.last_indexed_snapshot {
            self.last_indexed_snapshot = indexed;
            self.last_progress = Instant::now();
        }
    }
}

/// Dispatch a single parsed SSE event, updating the UI, shared atomics, and
/// loop state.
///
/// Why: centralises the daemon's progress protocol so the driver loop only has
/// to pump bytes; each arm mirrors a documented SSE event (see the daemon's
/// `spawn_reindex`). Unknown events are ignored for forward compatibility.
/// What: matches on `evt["event"]` and applies the corresponding bar/header/
/// counter transition; sets `state.done` on `complete`.
/// Test: live-daemon path (`--include-ignored`); UI transitions covered by the
/// `tests` module (`embed_bar_total_is_set_before_first_batch`, etc.).
pub(super) fn handle_event(
    state: &mut LoopState,
    ui: &mut ReindexUi,
    progress: &SharedProgress,
    evt: &serde_json::Value,
    index_id: &str,
) {
    let started = progress.started;
    match evt.get("event").and_then(|v| v.as_str()) {
        // ── walk_complete ──────────────────────────────────────────────
        // New daemon only. The CLI enters Walking, fills the Crawl bar to
        // 100% instantly (walk is synchronous on the daemon), then marks it
        // done. Old daemons omit this event; the CLI falls back to the
        // two-phase flow below (start → Embedding).
        Some("walk_complete") => {
            state.received_walk_complete = true;
            let total = evt.get("total_files").and_then(|v| v.as_u64()).unwrap_or(0);
            // Issue #744: set the authoritative file count so the ticker
            // shows "Files N/total" with the correct denominator.
            progress.total_files_now.store(total, Ordering::Release);
            ui.set_phase(ReindexPhase::Walking, index_id);
            progress
                .phase_disc
                .store(phase_to_u64(ReindexPhase::Walking), Ordering::Release);
            ui.set_total(total);
            // Walk is already done by the time this event arrives (sync on
            // daemon). Fill the bar to 100% and freeze it with a near-zero
            // elapsed time (walk is a fast synchronous scan on the daemon).
            ui.set_position(total);
            ui.mark_stage_done(0, 0);
            // Issue #823 Bug 2: prime the Embed bar (slot 2) with the correct
            // total_files denominator NOW, before any batch event arrives.
            // Without this, slot 2 starts at new(1) and shows "0/1" throughout
            // the model-load period. Both Chunk and Embed bars use files as
            // the unit so the pipeline gap is meaningful.
            if total > 0 && !state.lexical_only {
                ui.set_embed_total(total);
            }
        }
        // ── start ──────────────────────────────────────────────────────
        Some("start") => {
            let total = evt.get("total_files").and_then(|v| v.as_u64()).unwrap_or(0);
            state.lexical_only = evt
                .get("lexical_only")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            // Issue #929: detect defer-embed mode from the start event.
            // Old daemons (pre-#929) don't emit this field; absence → false
            // (assume synchronous, no background note).
            state.defer_embed = evt
                .get("defer_embed")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            // Issue #744: set the authoritative total so the ticker always
            // shows the correct denominator from this point on (important for
            // old daemons that don't emit walk_complete).
            if total > 0 {
                progress.total_files_now.store(total, Ordering::Release);
            }

            if state.received_walk_complete {
                // Three-phase flow: Walk bar is already done; enter Chunking.
                state.chunk_started_ms = started.elapsed().as_millis() as u64;
                ui.set_phase(ReindexPhase::Chunking, index_id);
                progress
                    .phase_disc
                    .store(phase_to_u64(ReindexPhase::Chunking), Ordering::Release);
                ui.set_total(total);
                // Issue #823 Bug 2: prime Embed bar (slot 2) immediately with
                // total_files so it shows real N/total instead of 0/1 for the
                // entire model-load period. Done here as fallback in case
                // walk_complete arrived before lexical_only was known.
                if total > 0 && !state.lexical_only {
                    ui.set_embed_total(total);
                    ui.activate_embed_bar();
                }
            } else {
                // Legacy two-phase flow (old daemon, no walk_complete):
                // jump straight to Embed (or Chunking for lexical-only).
                ui.set_total(total);
                if state.lexical_only {
                    state.chunk_started_ms = started.elapsed().as_millis() as u64;
                    ui.set_phase(ReindexPhase::Chunking, index_id);
                    progress
                        .phase_disc
                        .store(phase_to_u64(ReindexPhase::Chunking), Ordering::Release);
                } else {
                    state.embed_started_ms = started.elapsed().as_millis() as u64;
                    ui.set_phase(ReindexPhase::Embedding, index_id);
                    progress
                        .phase_disc
                        .store(phase_to_u64(ReindexPhase::Embedding), Ordering::Release);
                    state.entered_embedding = true;
                    // Issue #823 Bug 2: also prime slot 2 on the legacy path.
                    if total > 0 {
                        ui.set_embed_total(total);
                    }
                }
            }
        }
        // ── embedder_init ──────────────────────────────────────────────
        // New event (Problem 1 fix): emitted by the daemon just before
        // spawning trusty-embedderd on the first embed request.  This is
        // the 30-60s "stall" that previously showed as a frozen Chunk bar
        // at 0/N with no feedback.  Transitioning the header to
        // "Loading model…" (InitializingEmbedder) makes the wait visible.
        Some("embedder_init") => {
            ui.set_phase(ReindexPhase::InitializingEmbedder, index_id);
            progress.phase_disc.store(
                phase_to_u64(ReindexPhase::InitializingEmbedder),
                Ordering::Release,
            );
        }
        // ── embedder_ready ─────────────────────────────────────────────
        // Emitted after the embedder (sidecar or in-process) has completed
        // its first embed batch. Transitions the header to "Embedding
        // chunks…" and activates the Embed bar.
        //
        // Issue #823 Bug 3: previously only emitted for sidecar mode
        // (embedder_pid_slot.is_some()). The daemon now emits this event
        // unconditionally after the first successful parse_and_embed call,
        // regardless of embedder mode.
        //
        // Issue #823 Bug 1: do NOT call mark_stage_done(1) here — the Chunk
        // bar continues advancing in parallel with the Embed bar throughout
        // the CHUNK+EMBED phase. The Chunk bar is only frozen at kg_start
        // (or at complete if kg_start was never received).
        Some("embedder_ready") if !state.entered_embedding => {
            state.embed_started_ms = started.elapsed().as_millis() as u64;
            // Update the header to "Embedding chunks…" while keeping the
            // Chunk bar active. phase_to_bar_slot(Embedding) = 2, so
            // set_phase activates slot 2 without touching slot 1.
            ui.set_phase(ReindexPhase::Embedding, index_id);
            progress
                .phase_disc
                .store(phase_to_u64(ReindexPhase::Embedding), Ordering::Release);
            state.entered_embedding = true;
        }
        Some("embedder_ready") => {
            // Already in embedding phase; ignore duplicate event.
        }
        // ── chunk_progress ─────────────────────────────────────────────
        // Emitted after each ONNX wave (≥ PROGRESS_CHUNK_INTERVAL chunks)
        // inside `embed_chunks_in_batches`. Fires at ~32-chunk granularity
        // so the stats line advances continuously during embedding rather
        // than jumping once per 128-file file-batch.
        //
        // Issue #823 Bug 1: also advance the Chunk bar (slot 1) here using
        // the `indexed` file count from the event. The Chunk bar tracks
        // files PARSED (leading indicator); the Embed bar tracks files
        // COMMITTED (trailing). Both use files as unit — the gap between
        // them visualises the pipeline backpressure.
        Some("chunk_progress") => {
            let wave_chunks = evt.get("chunks_done").and_then(|v| v.as_u64()).unwrap_or(0);
            let wave_cps = evt
                .get("chunks_per_sec")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            if wave_cps > 0 {
                progress.cps_now.store(wave_cps, Ordering::Release);
            }
            // Accumulate in-flight chunks into the preview counter so the
            // ticker shows the live embed count between `batch` events.
            // `batch` events reset this preview to 0 so it never
            // double-counts with `chunks_now`.
            if wave_chunks > 0 {
                progress
                    .chunks_embed_preview
                    .fetch_add(wave_chunks, Ordering::AcqRel);
            }
            // Advance the Chunk bar (slot 1) with the files-parsed count
            // from the event. This keeps the Chunk bar moving between
            // `batch` events so the pipeline gap is visible.
            let chunk_indexed = evt.get("indexed").and_then(|v| v.as_u64()).unwrap_or(0);
            if chunk_indexed > 0 {
                ui.set_position(chunk_indexed);
            }
        }
        // ── batch ──────────────────────────────────────────────────────
        Some("batch") => super::event_handlers::handle_batch(state, ui, progress, evt, index_id),
        // ── skip ───────────────────────────────────────────────────────
        Some("skip") => {
            let indexed = evt.get("indexed").and_then(|v| v.as_u64()).unwrap_or(0);
            progress.indexed_now.store(indexed, Ordering::Release);
            let skipped = progress.skipped_now.fetch_add(1, Ordering::AcqRel) + 1;
            ui.set_position(indexed);
            ui.update_stats(
                indexed,
                progress.chunks_now.load(Ordering::Acquire),
                skipped,
                progress.cps_now.load(Ordering::Acquire),
                started.elapsed().as_secs(),
            );
            // skip events also represent progress (files are being processed).
            state.note_progress(indexed);
        }
        // ── kg_start ───────────────────────────────────────────────────
        // New event added by issue #401. The daemon emits this immediately
        // before `rebuild_symbol_graph_for_reindex`. The CLI marks both the
        // Chunk bar and Embed bar done, then activates the KG bar.
        //
        // Issue #823 Bug 1: this is the correct place to freeze the Chunk
        // bar (slot 1) — NOT at the first `batch` event. By waiting until
        // kg_start, both Chunk and Embed bars animate throughout CHUNK+EMBED.
        Some("kg_start") => {
            // Mark Chunk bar done (Issue #823 Bug 1: moved here from batch handler).
            let chunk_ms = started.elapsed().as_millis() as u64 - state.chunk_started_ms;
            ui.mark_stage_done(1, chunk_ms);
            // Mark Embed bar done (if it was active).
            if state.entered_embedding {
                let embed_ms = started.elapsed().as_millis() as u64 - state.embed_started_ms;
                ui.mark_stage_done(2, embed_ms);
            }
            ui.clear_stats();
            ui.set_phase(ReindexPhase::KnowledgeGraph, index_id);
            progress.phase_disc.store(
                phase_to_u64(ReindexPhase::KnowledgeGraph),
                Ordering::Release,
            );
            // KG total is unknown until completion; use 1 so the bar renders.
            ui.set_total(1);
            ui.set_position(0);
        }
        // ── kg_complete ────────────────────────────────────────────────
        // New event added by issue #401. Carries `kg_ms`, `symbol_count`,
        // `edge_count`. The CLI marks the KG bar done. Old daemons omit this
        // event; the KG bar is cleaned up in the `complete` handler.
        Some("kg_complete") => {
            let kg_ms = evt.get("kg_ms").and_then(|v| v.as_u64()).unwrap_or(0);
            let symbol_count = evt
                .get("symbol_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let edge_count = evt.get("edge_count").and_then(|v| v.as_u64()).unwrap_or(0);
            // Snap the KG bar to 100% (total was set to 1 in kg_start).
            ui.set_position(1);
            ui.mark_stage_done(3, kg_ms);
            // Show a brief summary on the stats line.
            ui.stats_bar().set_message(format!(
                "KG done \u{2014} {sym} symbols, {edges} edges",
                sym = format_with_commas(symbol_count),
                edges = format_with_commas(edge_count),
            ));
        }
        // ── complete ───────────────────────────────────────────────────
        Some("complete") => super::event_handlers::handle_complete(state, ui, progress, evt),
        // ── error ──────────────────────────────────────────────────────
        Some("error") => {
            let msg = evt
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let file = evt.get("file").and_then(|v| v.as_str()).unwrap_or("");
            ui.stats_bar()
                .println(format!("{}  {}: {}", "\u{26a0}".yellow(), file, msg));
        }
        // Unknown events (future daemon-side additions) are silently ignored
        // so older CLIs stay backward-compatible.
        _ => {}
    }
}
