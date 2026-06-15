//! Unit tests for `reindex_ui` — bar lifecycle, phase transitions, and timing
//! breakdown rendering.
//!
//! Why: these tests exercise the non-interactive (hidden) draw target so CI
//! stays noise-free while still covering every state transition and format
//! branch. They were previously inlined in `reindex_ui.rs`; extracted here
//! (issue #571) to keep the production files under the 500-SLOC cap.
//!
//! What: covers `ReindexPhase`, `phase_to_bar_slot`, `ReindexUi`, and
//! `format_timing_breakdown` / `print_timing_breakdown`.
//!
//! Test: `cargo test -p trusty-search -- --test-threads=1` runs all tests
//! in this module.

use super::bars::{BarState, ReindexUi, STAGE_LABELS};
use super::phase::{phase_to_bar_slot, ReindexPhase};
use super::timings::{format_timing_breakdown, print_timing_breakdown, ReindexTimings};
use crate::commands::format::fmt_elapsed;

/// Every phase label must be stable across refactors — they are user-facing
/// strings that appear on the terminal and may be documented.
///
/// Why: a misspelling or accidental rename fails loudly here rather than
/// silently confusing operators.
/// What: asserts every variant's `label()` against the exact expected string.
/// Test: this test.
#[test]
fn phase_labels_are_stable() {
    assert_eq!(
        ReindexPhase::Connecting.label(),
        "Connecting to daemon\u{2026}"
    );
    assert_eq!(ReindexPhase::Walking.label(), "Walking files\u{2026}");
    assert_eq!(ReindexPhase::Chunking.label(), "Chunking\u{2026}");
    assert_eq!(
        ReindexPhase::InitializingEmbedder.label(),
        "Loading model\u{2026}"
    );
    assert_eq!(ReindexPhase::Embedding.label(), "Embedding chunks\u{2026}");
    assert_eq!(ReindexPhase::ParseEmbed.label(), "Embedding chunks\u{2026}");
    assert_eq!(ReindexPhase::Bm25.label(), "Building BM25 index\u{2026}");
    assert_eq!(
        ReindexPhase::KnowledgeGraph.label(),
        "Building knowledge graph\u{2026}"
    );
    assert_eq!(ReindexPhase::Upsert.label(), "Upserting vectors\u{2026}");
    assert_eq!(ReindexPhase::Done.label(), "Done");
}

/// Every variant of `ReindexPhase` must have a defined bar-slot mapping.
///
/// Why: a new variant added without a slot mapping would silently make its
/// bar invisible.
/// What: asserts the expected slot index (or None) for every variant.
/// Test: this test.
#[test]
fn phase_to_bar_slot_coverage() {
    assert_eq!(phase_to_bar_slot(ReindexPhase::Connecting), None);
    assert_eq!(phase_to_bar_slot(ReindexPhase::Walking), Some(0));
    assert_eq!(phase_to_bar_slot(ReindexPhase::Chunking), Some(1));
    // InitializingEmbedder shares the Chunk bar (slot 1) so the bar stays
    // focused while the header changes to "Loading model…".
    assert_eq!(
        phase_to_bar_slot(ReindexPhase::InitializingEmbedder),
        Some(1)
    );
    assert_eq!(phase_to_bar_slot(ReindexPhase::Embedding), Some(2));
    assert_eq!(phase_to_bar_slot(ReindexPhase::ParseEmbed), Some(2));
    assert_eq!(phase_to_bar_slot(ReindexPhase::KnowledgeGraph), Some(3));
    assert_eq!(phase_to_bar_slot(ReindexPhase::Bm25), None);
    assert_eq!(phase_to_bar_slot(ReindexPhase::Upsert), None);
    assert_eq!(phase_to_bar_slot(ReindexPhase::Done), None);
}

/// A non-interactive `ReindexUi` must build without panic and draw to a
/// hidden target.  All phase transitions must be exercisable without a TTY.
///
/// Why: CI has no TTY; any panic in the construction path would break `cargo
/// test`.
/// What: constructs with `interactive = false`, exercises the full 4-phase
/// sequence, then calls `finish`.
/// Test: this test.
#[test]
fn ui_builds_hidden_when_not_interactive() {
    let mut ui = ReindexUi::new("test-index", false);
    assert_eq!(ui.phase, ReindexPhase::Connecting);

    ui.set_phase(ReindexPhase::Walking, "test-index");
    assert_eq!(ui.phase, ReindexPhase::Walking);
    ui.set_total(1_000);
    ui.set_position(1_000);
    ui.mark_stage_done(0, 1_200);

    ui.set_phase(ReindexPhase::Chunking, "test-index");
    assert_eq!(ui.phase, ReindexPhase::Chunking);
    ui.set_total(1_000);
    ui.mark_stage_done(1, 300);

    ui.set_phase(ReindexPhase::Embedding, "test-index");
    assert_eq!(ui.phase, ReindexPhase::Embedding);
    ui.set_total(1_000);
    ui.set_position(500);
    ui.update_stats(500, 4_096, 3, 128, 10);
    ui.mark_stage_done(2, 90_000);

    ui.set_phase(ReindexPhase::KnowledgeGraph, "test-index");
    assert_eq!(ui.phase, ReindexPhase::KnowledgeGraph);
    ui.set_total(1);
    ui.set_position(1);
    ui.clear_stats();
    ui.mark_stage_done(3, 800);

    ui.finish("done".to_string());
}

/// An interactive `ReindexUi` must also build cleanly. indicatif's
/// `ProgressDrawTarget::stderr()` self-suppresses when stderr is not a
/// TTY (the case under `cargo test`), so this exercises the construction
/// path without emitting noise.
///
/// Why: the interactive path uses a different draw target; exercising it
/// catches construction-time panics that only appear on the non-hidden path.
/// What: constructs with `interactive = true`, then abandons.
/// Test: this test.
#[test]
fn ui_builds_interactive() {
    let ui = ReindexUi::new("test-index", true);
    assert_eq!(ui.phase, ReindexPhase::Connecting);
    ui.abandon("aborted".to_string());
}

/// `set_phase` must activate the correct bar slot and set the phase field.
///
/// Why: the bar-slot mapping is the core invariant of the 4-bar design; a
/// mistake here would animate the wrong bar.
/// What: for each of the four concrete phases, calls `set_phase` and asserts
/// `self.phase` and `self.bar_states[slot]`.
/// Test: this test.
#[test]
fn phase_transitions_activate_correct_bar() {
    let mut ui = ReindexUi::new("idx", false);

    ui.set_phase(ReindexPhase::Walking, "idx");
    assert_eq!(ui.phase, ReindexPhase::Walking);
    assert_eq!(ui.bar_states[0], BarState::Active);

    ui.set_phase(ReindexPhase::Chunking, "idx");
    assert_eq!(ui.phase, ReindexPhase::Chunking);
    assert_eq!(ui.bar_states[1], BarState::Active);

    ui.set_phase(ReindexPhase::Embedding, "idx");
    assert_eq!(ui.phase, ReindexPhase::Embedding);
    assert_eq!(ui.bar_states[2], BarState::Active);

    ui.set_phase(ReindexPhase::KnowledgeGraph, "idx");
    assert_eq!(ui.phase, ReindexPhase::KnowledgeGraph);
    assert_eq!(ui.bar_states[3], BarState::Active);

    ui.finish("done".to_string());
}

/// `mark_stage_done` must freeze the bar at 100% and record the elapsed time.
///
/// Why: a completed stage must remain visually frozen while later stages
/// animate; incorrectly leaving it in `Active` state would let `set_phase`
/// re-activate it.
/// What: activates slot 0, calls `mark_stage_done(0, 1_200)`, asserts that
/// `bar_states[0] == Done` and `stage_elapsed_ms[0] == 1_200`.
/// Test: this test.
#[test]
fn mark_stage_done_freezes_bar() {
    let mut ui = ReindexUi::new("idx", false);
    ui.set_phase(ReindexPhase::Walking, "idx");
    ui.set_total(500);
    ui.set_position(500);
    ui.mark_stage_done(0, 1_200);
    assert_eq!(ui.bar_states[0], BarState::Done);
    // Re-entering the same phase must NOT re-activate a Done bar.
    ui.set_phase(ReindexPhase::Walking, "idx");
    assert_eq!(ui.bar_states[0], BarState::Done);
    ui.finish("done".to_string());
}

/// `set_total` and `set_position` must affect the active bar's length/position.
///
/// Why: correct position tracking is needed for the percentage display.
/// What: activates slot 1 (Chunking), sets total = 200, position = 100, and
/// asserts the bar values.
/// Test: this test.
#[test]
fn set_total_and_position_affect_active_bar() {
    let mut ui = ReindexUi::new("idx", false);
    ui.set_phase(ReindexPhase::Chunking, "idx");
    ui.set_total(200);
    ui.set_position(100);
    assert_eq!(ui.stage_bars[1].length(), Some(200));
    assert_eq!(ui.stage_bars[1].position(), 100);
    ui.finish("done".to_string());
}

/// `update_stats` must not panic for any combination of edge-case inputs.
///
/// Why: edge cases (elapsed = 0, total = 0, indexed = 0) can trigger
/// division-by-zero without guarding.
/// What: calls `update_stats` with zero and non-zero values; asserts no panic.
/// Test: this test.
#[test]
fn update_stats_formats_message() {
    let mut ui = ReindexUi::new("idx", false);
    ui.set_phase(ReindexPhase::Embedding, "idx");
    // Zero elapsed — ETA must not panic.
    ui.update_stats(0, 0, 0, 0, 0);
    // Normal path.
    ui.update_stats(500, 4_096, 3, 128, 10);
    ui.finish("done".to_string());
}

/// The stats-bar message must use the active phase label, not a hard-coded
/// "Embedding…" string. This was the header/footer inconsistency that showed
/// "Chunking…" in the header while the footer said "Embedding…" during the
/// model-init stall.
///
/// Why: ensures the fix for the header/footer label mismatch (Problem 1) is
/// regression-tested. The stats line prefix must always match the phase label
/// returned by `ReindexPhase::label()`.
/// What: calls `update_stats` in Chunking and InitializingEmbedder phases;
/// asserts the stats bar message starts with the correct prefix.
/// Test: this test.
#[test]
fn update_stats_label_matches_phase() {
    let mut ui = ReindexUi::new("idx", false);

    // During Chunking the stats line must say "Chunking…", not "Embedding…".
    ui.set_phase(ReindexPhase::Chunking, "idx");
    ui.set_total(3_263);
    ui.update_stats(0, 0, 0, 0, 1);
    let msg = ui.stats_bar().message();
    assert!(
        msg.starts_with("Chunking\u{2026}"),
        "expected stats to start with 'Chunking…', got: {msg:?}"
    );

    // During InitializingEmbedder the stats line must say "Loading model…".
    ui.set_phase(ReindexPhase::InitializingEmbedder, "idx");
    ui.update_stats(0, 0, 0, 0, 10);
    let msg = ui.stats_bar().message();
    assert!(
        msg.starts_with("Loading model\u{2026}"),
        "expected stats to start with 'Loading model…', got: {msg:?}"
    );

    // During Embedding the stats line must say "Embedding chunks…".
    ui.set_phase(ReindexPhase::Embedding, "idx");
    ui.set_total(3_263);
    ui.update_stats(128, 1_024, 0, 22, 46);
    let msg = ui.stats_bar().message();
    assert!(
        msg.starts_with("Embedding chunks\u{2026}"),
        "expected stats to start with 'Embedding chunks…', got: {msg:?}"
    );

    ui.finish("done".to_string());
}

/// `clear_stats` must not panic and must clear the stats bar message.
///
/// Why: called when entering the KG phase; a panic there would crash the
/// CLI mid-reindex.
/// What: calls `clear_stats` and asserts no panic.
/// Test: this test.
#[test]
fn clear_stats_empties_message() {
    let ui = ReindexUi::new("idx", false);
    ui.clear_stats();
    ui.finish("done".to_string());
}

/// `finish` must not panic even when some bars are still in `Pending` state
/// (e.g. a `lexical_only` index that never visits the KG bar).
///
/// Why: `finish` calls `finish_and_clear` on pending bars; if a bar was
/// never started it must still reach a terminal state cleanly.
/// What: builds a UI, skips the KG phase, calls `finish`.
/// Test: this test.
#[test]
fn finish_all_clears_pending_bars() {
    let mut ui = ReindexUi::new("idx", false);
    // Only drive the first 3 stages; KG bar stays Pending.
    ui.set_phase(ReindexPhase::Walking, "idx");
    ui.set_total(100);
    ui.set_position(100);
    ui.mark_stage_done(0, 500);

    ui.set_phase(ReindexPhase::Chunking, "idx");
    ui.set_total(100);
    ui.mark_stage_done(1, 200);

    ui.set_phase(ReindexPhase::Embedding, "idx");
    ui.set_total(100);
    ui.set_position(100);
    ui.mark_stage_done(2, 80_000);

    // KG bar stays Pending — finish must not panic.
    assert_eq!(ui.bar_states[3], BarState::Pending);
    ui.finish("lexical-only done".to_string());
}

/// `abandon` must not panic under any state.
///
/// Why: called on timeout or stream error; a panic would crash the CLI.
/// What: builds a UI and immediately abandons without driving any phase.
/// Test: this test.
#[test]
fn abandon_does_not_panic() {
    let ui = ReindexUi::new("idx", false);
    ui.abandon("timed out".to_string());
}

/// `set_embed_total` must prime the Embed bar (slot 2) while phase is Chunking.
///
/// Why: Issue #823 Bug 2 — the Embed bar stays `0/1` (ProgressBar::new(1))
/// during model loading because `set_total` only sets the *active* phase's bar.
/// `set_embed_total` lets the handler prime slot 2 independently.
/// What: activates Chunking phase, calls `set_embed_total(500)`, asserts
/// slot 2 length is 500 while slot 1 is unaffected.
/// Test: this test.
#[test]
fn set_embed_total_primes_slot2_while_chunking() {
    let mut ui = ReindexUi::new("idx", false);
    ui.set_phase(ReindexPhase::Chunking, "idx");
    ui.set_total(500); // sets slot 1 (Chunk bar)
    ui.set_embed_total(500); // must prime slot 2 (Embed bar)
                             // Slot 1 length set by set_total
    assert_eq!(ui.stage_bars[1].length(), Some(500));
    // Slot 2 length set by set_embed_total, not still 1
    assert_eq!(
        ui.stage_bars[2].length(),
        Some(500),
        "Embed bar must be primed to total_files, not left at ProgressBar::new(1)"
    );
    ui.finish("done".to_string());
}

/// `activate_embed_bar` must apply Active style to slot 2 without changing
/// `self.phase`.
///
/// Why: Issue #823 Bug 1 — both Chunk and Embed bars must be visually live
/// simultaneously during CHUNK+EMBED; changing the phase would move the header.
/// What: activates Chunking phase, calls `activate_embed_bar`, asserts
/// phase is still Chunking and slot 2 state is Active.
/// Test: this test.
#[test]
fn activate_embed_bar_does_not_change_phase() {
    let mut ui = ReindexUi::new("idx", false);
    ui.set_phase(ReindexPhase::Chunking, "idx");
    assert_eq!(ui.phase, ReindexPhase::Chunking);
    assert_eq!(ui.bar_states[2], BarState::Pending);

    ui.activate_embed_bar();

    // Phase must NOT change
    assert_eq!(ui.phase, ReindexPhase::Chunking);
    // Slot 2 must be Active
    assert_eq!(ui.bar_states[2], BarState::Active);
    // Calling again must be idempotent (already Active → no change)
    ui.activate_embed_bar();
    assert_eq!(ui.bar_states[2], BarState::Active);

    ui.finish("done".to_string());
}

/// `advance_embed_bar` must set slot 2 position without requiring
/// phase == Embedding.
///
/// Why: Issue #823 Bug 1 — during CHUNK+EMBED both bars advance simultaneously;
/// the Embed bar advances from `batch` events while phase may still be Chunking.
/// What: activates Chunking phase, activates Embed bar, calls
/// `advance_embed_bar(42)`, asserts slot 2 position is 42 and slot 1 is 0.
/// Test: this test.
#[test]
fn advance_embed_bar_sets_slot2_position() {
    let mut ui = ReindexUi::new("idx", false);
    ui.set_phase(ReindexPhase::Chunking, "idx");
    ui.set_total(200);
    ui.set_embed_total(200);
    ui.activate_embed_bar();

    // Advance Embed bar without changing phase
    ui.advance_embed_bar(42);

    assert_eq!(
        ui.stage_bars[2].position(),
        42,
        "Embed bar must advance independently of active phase"
    );
    // Chunk bar (slot 1) position must remain at 0 (untouched)
    assert_eq!(ui.stage_bars[1].position(), 0);

    ui.finish("done".to_string());
}

/// Chunk bar must NOT be frozen by `mark_stage_done` at the first batch event.
/// It must remain Active and advanceable while Embed bar also advances.
///
/// Why: Issue #823 Bug 1 — the old code called `mark_stage_done(1, ...)` on
/// the first batch, which froze the Chunk bar at whatever partial position it
/// had reached. Both bars must run to completion concurrently.
/// What: simulates the CHUNK+EMBED phase: set up both bars with total=100,
/// advance Chunk bar to 50, advance Embed bar to 30, assert both still Active.
/// Then advance Chunk to 100, mark Chunk done — Embed still Active.
/// Test: this test.
#[test]
fn chunk_and_embed_bars_live_simultaneously() {
    let mut ui = ReindexUi::new("idx", false);
    // Simulate walk_complete → Chunking transition
    ui.set_phase(ReindexPhase::Walking, "idx");
    ui.set_total(100);
    ui.set_position(100);
    ui.mark_stage_done(0, 500);

    ui.set_phase(ReindexPhase::Chunking, "idx");
    ui.set_total(100);
    ui.set_embed_total(100);
    ui.activate_embed_bar();

    // Simulate batch events: Chunk leads, Embed trails
    ui.set_position(50); // Chunk bar at 50/100
    ui.advance_embed_bar(30); // Embed bar at 30/100

    assert_eq!(
        ui.bar_states[1],
        BarState::Active,
        "Chunk bar must stay Active"
    );
    assert_eq!(
        ui.bar_states[2],
        BarState::Active,
        "Embed bar must stay Active"
    );
    assert_eq!(ui.stage_bars[1].position(), 50);
    assert_eq!(ui.stage_bars[2].position(), 30);

    // Finish Chunk bar (e.g. at kg_start)
    ui.set_position(100);
    ui.mark_stage_done(1, 5_000);
    assert_eq!(
        ui.bar_states[1],
        BarState::Done,
        "Chunk bar must be Done after mark"
    );

    // Embed bar still Active, still advancing
    assert_eq!(
        ui.bar_states[2],
        BarState::Active,
        "Embed bar must still be Active after Chunk done"
    );
    ui.advance_embed_bar(100);
    ui.mark_stage_done(2, 90_000);
    assert_eq!(ui.bar_states[2], BarState::Done);

    ui.finish("done".to_string());
}

/// `print_timing_breakdown` must not panic for the BM25-only fallback path
/// (`vector_count == 0` with chunks present).
///
/// Why: the BM25-only warning path exercises a branch that historically
/// panicked on a formatting mismatch; pinning it here prevents regression.
/// What: calls `print_timing_breakdown` with `vector_count = 0` and non-zero
/// chunks and a wall-clock total; asserts no panic.
/// Test: this test.
#[test]
fn timing_breakdown_bm25_only_does_not_panic() {
    let t = ReindexTimings {
        walk_ms: 0,
        parse_ms: 1_000,
        embed_ms: 0,
        bm25_ms: 200,
        vector_upsert_ms: 0,
        kg_ms: 50,
        vector_count: 0,
        symbol_count: 10,
        edge_count: 4,
    };
    print_timing_breakdown(&t, 1_234, 1_500, false, false);
}

/// `print_timing_breakdown` must not panic for a normal completion with
/// non-zero vectors across every phase.
///
/// Why: the normal path has the same format; pinning it here ensures both
/// paths are regression-tested.
/// What: calls `print_timing_breakdown` with realistic values and a
/// wall-clock total; asserts no panic.
/// Test: this test.
#[test]
fn timing_breakdown_normal_does_not_panic() {
    let t = ReindexTimings {
        walk_ms: 300,
        parse_ms: 5_000,
        embed_ms: 90_000,
        bm25_ms: 1_200,
        vector_upsert_ms: 3_400,
        kg_ms: 800,
        vector_count: 62_926,
        symbol_count: 14_823,
        edge_count: 41_002,
    };
    print_timing_breakdown(&t, 62_926, 95_000, false, false);
}

/// `format_timing_breakdown` output must contain the "overlapping / does not
/// sum" disclaimer, the wall-clock total line, and label KG as "tail stage".
///
/// Why: the previous version of this test asserted on a locally-constructed
/// copy of the disclaimer string rather than on the actual rendered output,
/// meaning a refactor could silently remove the text while the test still
/// passed.  This version calls `format_timing_breakdown` and asserts on the
/// real string it returns.
/// What: calls `format_timing_breakdown` with vectors > 0, checks that the
/// rendered output contains "overlapping", "do not sum", "Wall-clock total",
/// "tail stage", a wall-clock time string, and the EMBED_STAR_NOTE footnote.
/// Test: this test.
#[test]
fn timing_breakdown_contains_overlap_disclaimer() {
    // Disable ANSI color codes so assertions match plain text regardless of
    // TERM, CLICOLOR_FORCE, or NO_COLOR in the test environment.
    colored::control::set_override(false);
    let t = ReindexTimings {
        walk_ms: 0,
        parse_ms: 5_000,
        embed_ms: 90_000,
        bm25_ms: 1_200,
        vector_upsert_ms: 3_400,
        kg_ms: 800,
        vector_count: 62_926,
        symbol_count: 14_823,
        edge_count: 41_002,
    };
    let out = format_timing_breakdown(&t, 62_926, 95_000, false, false);
    assert!(
        out.contains("overlapping"),
        "output must contain 'overlapping'; got:\n{out}"
    );
    assert!(
        out.contains("do not sum"),
        "output must contain 'do not sum'; got:\n{out}"
    );
    assert!(
        out.contains("Wall-clock total"),
        "output must contain 'Wall-clock total'; got:\n{out}"
    );
    assert!(
        out.contains("tail stage"),
        "output must contain 'tail stage' for KG; got:\n{out}"
    );
    // Wall-clock time string must be non-empty (fmt_elapsed sanity).
    assert!(
        !fmt_elapsed(95_000).is_empty(),
        "fmt_elapsed must return a non-empty string"
    );
    // Footnote must be present when vectors > 0.
    assert!(
        out.contains("overlapping pipeline"),
        "EMBED_STAR_NOTE footnote must appear when vector_count > 0; got:\n{out}"
    );
    // Smoke-test the print path too (no panic = structural correctness).
    print_timing_breakdown(&t, 62_926, 95_000, false, false);
}

/// The EMBED_STAR_NOTE footnote must be absent in BM25-only mode and present
/// when vectors were committed.
///
/// Why: printing the "Embed* runs concurrently with BM25 + vector-upsert"
/// footnote when no vectors were upserted is misleading — there was no
/// concurrent commit to explain.
/// What: calls `format_timing_breakdown` with vector_count==0 and asserts
/// the footnote is absent; then repeats with vector_count>0 and asserts it
/// is present.
/// Test: this test.
#[test]
fn embed_star_footnote_guarded_by_vector_count() {
    // Disable ANSI color codes so substring assertions match plain text
    // regardless of TERM / CLICOLOR_FORCE in the test environment.
    colored::control::set_override(false);
    let bm25_only = ReindexTimings {
        walk_ms: 0,
        parse_ms: 1_000,
        embed_ms: 0,
        bm25_ms: 200,
        vector_upsert_ms: 0,
        kg_ms: 50,
        vector_count: 0,
        symbol_count: 10,
        edge_count: 4,
    };
    let out_bm25 = format_timing_breakdown(&bm25_only, 1_234, 1_500, false, false);
    assert!(
        !out_bm25.contains("overlapping pipeline"),
        "EMBED_STAR_NOTE must be absent when vector_count==0; got:\n{out_bm25}"
    );

    let with_vectors = ReindexTimings {
        walk_ms: 0,
        parse_ms: 5_000,
        embed_ms: 90_000,
        bm25_ms: 1_200,
        vector_upsert_ms: 3_400,
        kg_ms: 800,
        vector_count: 62_926,
        symbol_count: 14_823,
        edge_count: 41_002,
    };
    let out_vec = format_timing_breakdown(&with_vectors, 62_926, 95_000, false, false);
    assert!(
        out_vec.contains("overlapping pipeline"),
        "EMBED_STAR_NOTE must be present when vector_count>0; got:\n{out_vec}"
    );
}

/// The `(N vectors upserted)` annotation must appear adjacent to the upsert
/// timing, not shared ambiguously between bm25 and upsert.
///
/// Why: the previous format was `bm25 1.2s · upsert 3.4s (62,926 vectors)`
/// which could be read as the count belonging to both subsystems.  The fix
/// appends "upserted" to make ownership unambiguous.
/// What: asserts "vectors upserted" appears in the rendered output and that
/// the upsert line contains the vector count.
/// Test: this test.
#[test]
fn upsert_vector_count_annotation_is_unambiguous() {
    // Disable ANSI color codes so substring assertions match plain text
    // regardless of TERM / CLICOLOR_FORCE in the test environment.
    colored::control::set_override(false);
    let t = ReindexTimings {
        walk_ms: 0,
        parse_ms: 5_000,
        embed_ms: 90_000,
        bm25_ms: 1_200,
        vector_upsert_ms: 3_400,
        kg_ms: 800,
        vector_count: 62_926,
        symbol_count: 14_823,
        edge_count: 41_002,
    };
    let out = format_timing_breakdown(&t, 62_926, 95_000, false, false);
    assert!(
        out.contains("vectors upserted"),
        "output must contain 'vectors upserted' to unambiguously attribute the \
         count to the upsert subsystem; got:\n{out}"
    );
    assert!(
        out.contains("62,926"),
        "output must contain formatted vector count; got:\n{out}"
    );
}

/// Issue #929: `STAGE_LABELS[2]` must use the "Lexical(BM25)" label to
/// reflect the 4-stage reindex UX where the foreground pass only runs
/// lexical indexing (not semantic embedding).
///
/// Why: the old "Embed*" label was misleading in the default defer-embed
/// mode where no embedding happens in the foreground pass; "Lexical(BM25)"
/// accurately describes what stage 3 actually does.
/// What: asserts `STAGE_LABELS[2]` is the expected label.
/// Test: this test.
#[test]
fn stage_label_slot2_is_lexical_bm25() {
    assert_eq!(
        STAGE_LABELS[2], "Lexical(BM25)",
        "Stage 2 label must be 'Lexical(BM25)' (issue #929); got {:?}",
        STAGE_LABELS[2]
    );
}

/// `print_timing_breakdown` with `walk_ms > 0` must print the "File walk"
/// line and not panic.
///
/// Why: Issue #744 adds `walk_ms` to the timing breakdown. This test
/// verifies the new branch (walk_ms > 0) runs without panicking.
/// What: calls `print_timing_breakdown` with `walk_ms = 150`.
/// Test: this test.
#[test]
fn timing_breakdown_shows_walk_when_nonzero() {
    let t = ReindexTimings {
        walk_ms: 150,
        parse_ms: 2_000,
        embed_ms: 40_000,
        bm25_ms: 500,
        vector_upsert_ms: 1_000,
        kg_ms: 200,
        vector_count: 10_000,
        symbol_count: 3_000,
        edge_count: 8_000,
    };
    // No assertion on output text — just no panic.
    print_timing_breakdown(&t, 10_000, 44_000, false, false);
}

/// `format_timing_breakdown` must end with exactly one `\n` in both the
/// vector>0 (footnote) path and the BM25-only (no footnote) path.
///
/// Why: `print_timing_breakdown` uses `print!` not `println!`; if the
/// returned string does not end with `\n` the subsequent output (e.g.
/// the post-reindex health check) collides with the last line of the
/// breakdown.  Conversely, two trailing newlines insert an unwanted blank
/// line.  This test pins the single-`\n` invariant so regressions are
/// caught immediately.
/// What: calls `format_timing_breakdown` for both the vector>0 and the
/// BM25-only case and asserts `ends_with('\n')` and
/// `!ends_with("\n\n")` for each.
/// Test: this test.
#[test]
fn timing_breakdown_ends_with_newline() {
    // Disable ANSI color codes so the string comparison is deterministic.
    colored::control::set_override(false);

    // vector > 0 path (footnote appended)
    let with_vectors = ReindexTimings {
        walk_ms: 0,
        parse_ms: 5_000,
        embed_ms: 90_000,
        bm25_ms: 1_200,
        vector_upsert_ms: 3_400,
        kg_ms: 800,
        vector_count: 62_926,
        symbol_count: 14_823,
        edge_count: 41_002,
    };
    let out_vec = format_timing_breakdown(&with_vectors, 62_926, 95_000, false, false);
    assert!(
        out_vec.ends_with('\n'),
        "vector>0 path: output must end with '\\n'; got:\n{out_vec:?}"
    );
    assert!(
        !out_vec.ends_with("\n\n"),
        "vector>0 path: output must not have double trailing newline; got:\n{out_vec:?}"
    );

    // BM25-only path (no footnote)
    let bm25_only = ReindexTimings {
        walk_ms: 0,
        parse_ms: 1_000,
        embed_ms: 0,
        bm25_ms: 200,
        vector_upsert_ms: 0,
        kg_ms: 50,
        vector_count: 0,
        symbol_count: 10,
        edge_count: 4,
    };
    let out_bm25 = format_timing_breakdown(&bm25_only, 1_234, 1_500, false, false);
    assert!(
        out_bm25.ends_with('\n'),
        "BM25-only path: output must end with '\\n'; got:\n{out_bm25:?}"
    );
    assert!(
        !out_bm25.ends_with("\n\n"),
        "BM25-only path: output must not have double trailing newline; got:\n{out_bm25:?}"
    );
}

/// Issue #1174: on the defer-embed (fast C1) path, `format_timing_breakdown`
/// must NOT log "upsert 0ms (0 vectors upserted)" — that annotation is
/// misleading because real BM25 work happened but vector upsert was intentionally
/// deferred to the background C2 pass. The fix emits "vectors deferred to
/// background embed pass" instead, and omits the upsert timing entirely.
///
/// Why: the pre-fix output confused operators into thinking the embedder was
/// broken (zero vectors, zero upsert time) when in fact deferred embedding
/// was working as designed.
///
/// What: calls `format_timing_breakdown` with `defer_embed=true` and
/// `vector_count==0`, then asserts that "upsert" does NOT appear, "deferred"
/// DOES appear, "bm25" DOES appear, and output ends with exactly one newline.
///
/// Test: this test.
#[test]
fn defer_embed_path_suppresses_upsert_zero_annotation() {
    // Disable ANSI color codes so substring assertions match plain text.
    colored::control::set_override(false);
    let defer_timings = ReindexTimings {
        walk_ms: 50,
        parse_ms: 800,
        embed_ms: 0,
        bm25_ms: 1_200,
        vector_upsert_ms: 0,
        kg_ms: 300,
        vector_count: 0,
        symbol_count: 100,
        edge_count: 42,
    };
    // defer_embed=true, lexical_only=false
    let out = format_timing_breakdown(&defer_timings, 5_000, 3_000, true, false);

    // The misleading "upsert 0ms (0 vectors upserted)" MUST NOT appear.
    assert!(
        !out.contains("upsert"),
        "defer-embed path must not show 'upsert' line (issue #1174); got:\n{out}"
    );
    assert!(
        !out.contains("0 vectors"),
        "defer-embed path must not show '0 vectors upserted' (issue #1174); got:\n{out}"
    );

    // Informative "deferred" annotation MUST appear.
    assert!(
        out.contains("deferred"),
        "defer-embed path must show 'deferred' annotation so operator knows \
         vectors will be committed asynchronously (issue #1174); got:\n{out}"
    );

    // Real BM25 work MUST still appear.
    assert!(
        out.contains("bm25"),
        "defer-embed path must still show 'bm25' timing; got:\n{out}"
    );

    // Newline invariant: ends with exactly one newline.
    assert!(
        out.ends_with('\n'),
        "defer-embed path: output must end with '\\n'; got:\n{out:?}"
    );
    assert!(
        !out.ends_with("\n\n"),
        "defer-embed path: output must not have double trailing newline; got:\n{out:?}"
    );
}
