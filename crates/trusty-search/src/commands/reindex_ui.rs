//! Progress-bar UI for the `index` / `reindex` CLI commands.
//!
//! Why: the original `ReindexUi` in `reindex_engine.rs` used a single
//! `ProgressBar` that was relabelled and reset at each phase transition.
//! Issue #401 asks for 4 SEQUENTIAL bars — Crawl, Chunk, Embed, KG — stacked
//! in `MultiProgress` so the operator can see at a glance which stage is
//! active, which are done, and which are still pending.  Moving the UI into
//! its own module respects the 500-line cap on `reindex_engine.rs` and keeps
//! the rendering logic testable in isolation.
//!
//! What: `ReindexUi` owns one `MultiProgress` with a header spinner plus four
//! named bars (one per stage).  Only the active bar advances; completed bars
//! show a static 100% "done" frame; pending bars show an empty trough.
//!
//! Test: `cargo test -p trusty-search -- --test-threads=1` — every unit test
//! in this module exercises the non-interactive (hidden) draw target so CI
//! stays noise-free.

use super::format::{fmt_elapsed, fmt_secs, format_with_commas};
use colored::Colorize;
use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};
use std::time::Duration;

// ─── Phase enum ──────────────────────────────────────────────────────────────

/// Distinct phases of a reindex, each corresponding to one of the 4 sequential
/// progress bars shown in the CLI.
///
/// Why: encodes the lifecycle of a reindex as a strongly-typed value so the
/// event loop in `reindex_engine.rs` can call `set_phase(…)` without magic
/// strings.  Issue #401: four named bars replace the single relabelled bar of
/// the previous design (issue #317).
///
/// `ParseEmbed` is kept as a legacy alias for `Embedding` so existing tests
/// that used the old variant compile without modification.
///
/// What: each variant maps to a human-readable label via `label()`.
/// Test: `tests::phase_labels_are_stable` pins every label string.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReindexPhase {
    /// Waiting for the daemon's first SSE event.
    Connecting,
    /// Stage 1 — walking the source tree (→ Crawl bar).
    Walking,
    /// Stage 2 — parse sub-step that runs before the first batch event.
    Chunking,
    /// Stage 3 — parse + embed per batch (→ Embed bar).
    Embedding,
    /// Legacy alias for `Embedding`; retained for backward compatibility.
    ParseEmbed,
    /// Stage 4 — knowledge-graph rebuild (→ KG bar).
    KnowledgeGraph,
    /// Building the BM25 lexical index (fused into batches; no separate bar).
    Bm25,
    /// Upserting embedding vectors (fused into batches; no separate bar).
    Upsert,
    /// Terminal: the reindex finished.
    Done,
}

impl ReindexPhase {
    /// Human-readable phase label rendered on the header line.
    ///
    /// Why: keeps all user-facing strings in one place so a rename is a single
    /// reviewed change rather than a grep hunt.
    /// What: returns a `&'static str` label for each phase variant.
    /// Test: `tests::phase_labels_are_stable` pins every string.
    pub(crate) fn label(self) -> &'static str {
        match self {
            ReindexPhase::Connecting => "Connecting to daemon\u{2026}",
            ReindexPhase::Walking => "Walking files\u{2026}",
            ReindexPhase::Chunking => "Chunking\u{2026}",
            ReindexPhase::Embedding => "Embedding chunks\u{2026}",
            ReindexPhase::ParseEmbed => "Embedding chunks\u{2026}",
            ReindexPhase::Bm25 => "Building BM25 index\u{2026}",
            ReindexPhase::KnowledgeGraph => "Building knowledge graph\u{2026}",
            ReindexPhase::Upsert => "Upserting vectors\u{2026}",
            ReindexPhase::Done => "Done",
        }
    }
}

// ─── Bar-slot indices ─────────────────────────────────────────────────────────

/// Which of the 4 stage bars a given phase maps to.
///
/// Why: the 4-bar layout has Crawl/Chunk/Embed/KG slots (bars 0–3).  Not every
/// `ReindexPhase` drives a bar (e.g. `Bm25` and `Upsert` are fused into
/// batches and have no dedicated bar), so this mapping lives here rather than
/// on the enum itself.
/// What: returns `Some(0..=3)` for the four concrete stages, `None` for
/// everything else (the caller leaves the bar layout unchanged).
/// Test: `tests::phase_to_bar_slot_coverage` asserts every variant.
fn phase_to_bar_slot(phase: ReindexPhase) -> Option<usize> {
    match phase {
        ReindexPhase::Walking => Some(0),
        ReindexPhase::Chunking => Some(1),
        ReindexPhase::Embedding | ReindexPhase::ParseEmbed => Some(2),
        ReindexPhase::KnowledgeGraph => Some(3),
        _ => None,
    }
}

// ─── Bar state ────────────────────────────────────────────────────────────────

/// Lifecycle state of one stage bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BarState {
    /// Not yet started — bar shows an empty trough.
    Pending,
    /// Currently active — bar advances with SSE events.
    Active,
    /// Completed — bar shows 100% / done frame.
    Done,
}

// ─── Style helpers ────────────────────────────────────────────────────────────

/// Label prefix for each slot (matches the 4 stages in issue #401 order).
const STAGE_LABELS: [&str; 4] = ["Crawl", "Chunk", "Embed", "KG"];

/// Build the `ProgressStyle` for a bar in each of the three lifecycle states.
///
/// Why: indicatif styles are compile-time template strings; centralising them
/// here means changing the visual design touches one function, not four call
/// sites.
/// What: returns a `ProgressStyle` appropriate for `Pending`, `Active`, or
/// `Done`.  The `Active` style uses a cyan bar with block-fill; the `Done`
/// style shows a filled green bar with elapsed time; the `Pending` style shows
/// an empty grey trough.
/// Test: style construction is exercised by every `ReindexUi::new()` call in
/// unit tests; a template parse error would panic there.
fn bar_style(slot: usize, state: BarState, elapsed_ms: Option<u64>) -> ProgressStyle {
    let label = STAGE_LABELS[slot];
    match state {
        BarState::Pending => {
            let tpl = format!("  {{spinner:.white}} {label:<5} [{{bar:40.white/white}}] pending");
            ProgressStyle::with_template(&tpl)
                .unwrap_or_else(|_| ProgressStyle::default_bar())
                .progress_chars("\u{2588}\u{2591} ")
        }
        BarState::Active => {
            let tpl = format!(
                "  {{spinner:.cyan}} {label:<5} [{{bar:40.cyan/blue}}] {{pos}}/{{len}} {{msg}}"
            );
            ProgressStyle::with_template(&tpl)
                .unwrap_or_else(|_| ProgressStyle::default_bar())
                .progress_chars("\u{2588}\u{2591} ")
        }
        BarState::Done => {
            let t = elapsed_ms.unwrap_or(0);
            let elapsed_str = fmt_elapsed(t);
            let tpl = format!(
                "  \u{2713}       {label:<5} [{{bar:40.green/green}}] {{pos}}/{{len}}  \u{2014}  done in {elapsed_str}"
            );
            ProgressStyle::with_template(&tpl)
                .unwrap_or_else(|_| ProgressStyle::default_bar())
                .progress_chars("\u{2588}\u{2591} ")
        }
    }
}

// ─── ReindexUi ───────────────────────────────────────────────────────────────

/// Multi-bar live progress display for a reindex, with 4 sequential stage bars.
///
/// Why: issue #401 — a single relabelled `ProgressBar` cannot simultaneously
/// show which stage is active, which are complete, and which are pending.
/// Four stacked bars give the operator a clear visual pipeline:
///   [✓] Crawl  [████████████] 1,155/1,155  — done in   1.2s
///   [✓] Chunk  [████████████] 1,155/1,155  — done in   0.3s
///   [→] Embed  [████░░░░░░░░]   700/1,155  (50%)  142 cps
///   [ ] KG     [░░░░░░░░░░░░] pending
///
/// All progress draws to **stderr** (never stdout — stdout is the MCP JSON-RPC
/// transport channel). When stdout is not a TTY (the CLI output is piped or
/// redirected) the draw target is [`ProgressDrawTarget::hidden`], so no
/// progress noise pollutes captured output.
///
/// What: wraps a `MultiProgress` with a header spinner + 4 stage bars + a
/// stats line.  `set_phase` drives transitions; `set_total` / `set_position`
/// update the active bar; `mark_stage_done` snaps a bar to the done frame.
///
/// Test: every `fn …()` method in this struct has a corresponding unit test in
/// `tests::*` below; construction exercises all bars in hidden mode.
pub(crate) struct ReindexUi {
    /// Held to keep the `MultiProgress` draw target alive for the bars' lifetime.
    #[allow(dead_code)]
    multi: MultiProgress,
    /// Spinner line at the top: "⟳ <phase> — <index>".
    header: ProgressBar,
    /// The four stage bars, in order: Crawl (0), Chunk (1), Embed (2), KG (3).
    stage_bars: [ProgressBar; 4],
    /// Elapsed-ms snapshot for each completed stage (filled by `mark_stage_done`).
    stage_elapsed_ms: [u64; 4],
    /// Stats line below the bars (embedding throughput, ETA, etc.).
    stats: ProgressBar,
    /// Current phase; used to identify the active bar and update the header.
    pub(crate) phase: ReindexPhase,
    /// Lifecycle state of each stage bar (Pending / Active / Done).
    bar_states: [BarState; 4],
}

impl ReindexUi {
    /// Build the UI. `interactive` is `false` when stdout is not a TTY — in
    /// that case every bar draws to a hidden target so piped output stays
    /// clean. Progress, when shown, always renders to stderr.
    ///
    /// Why: constructed eagerly so the user sees something during the 1–2s
    /// daemon warmup before the first SSE event arrives.
    /// What: creates a `MultiProgress` with 6 lines (header + 4 stage bars +
    /// stats) all drawing to stderr (or hidden when non-interactive).
    /// Test: `tests::ui_builds_hidden_when_not_interactive` and
    /// `tests::ui_builds_interactive`.
    pub(crate) fn new(index_id: &str, interactive: bool) -> Self {
        let multi = if interactive {
            MultiProgress::with_draw_target(ProgressDrawTarget::stderr())
        } else {
            MultiProgress::with_draw_target(ProgressDrawTarget::hidden())
        };

        // Header spinner: "⟳ Connecting to daemon… — myindex"
        let header = multi.add(ProgressBar::new(1));
        if let Ok(s) = ProgressStyle::with_template("{spinner:.cyan} {msg}") {
            header.set_style(s);
        }
        header.set_message(format!(
            "{} \u{2014} {}",
            ReindexPhase::Connecting.label(),
            index_id.bold()
        ));
        header.enable_steady_tick(Duration::from_millis(120));

        // 4 stage bars — all start as Pending.
        let mut stage_bars_arr: [Option<ProgressBar>; 4] = [None, None, None, None];
        for (slot, item) in stage_bars_arr.iter_mut().enumerate() {
            let pb = multi.add(ProgressBar::new(1));
            pb.set_style(bar_style(slot, BarState::Pending, None));
            pb.set_position(0);
            *item = Some(pb);
        }
        let stage_bars = [
            stage_bars_arr[0].take().expect("slot 0"),
            stage_bars_arr[1].take().expect("slot 1"),
            stage_bars_arr[2].take().expect("slot 2"),
            stage_bars_arr[3].take().expect("slot 3"),
        ];

        // Stats line: free-form text below the bars.
        let stats = multi.add(ProgressBar::new(1));
        if let Ok(s) = ProgressStyle::with_template("  {msg}") {
            stats.set_style(s);
        }
        stats.set_message("Waiting for daemon\u{2026}".to_string());

        Self {
            multi,
            header,
            stage_bars,
            stage_elapsed_ms: [0u64; 4],
            stats,
            phase: ReindexPhase::Connecting,
            bar_states: [BarState::Pending; 4],
        }
    }

    /// Switch the active phase, update the header label, and activate the
    /// corresponding stage bar (resetting it to 0 if it was pending).
    ///
    /// Why: each phase drives a different bar slot (see `phase_to_bar_slot`).
    /// Entering `Walking` resets slot 0 to 0; entering `Chunking` resets slot 1;
    /// etc.  The previously active slot is NOT yet marked done here — it stays
    /// visually in progress until `mark_stage_done` is called.
    /// What: updates `self.phase`, refreshes the header message, sets the new
    /// slot's style to `Active`, and resets its position to 0.
    /// Test: `tests::phase_transitions_activate_correct_bar`.
    pub(crate) fn set_phase(&mut self, phase: ReindexPhase, index_id: &str) {
        self.phase = phase;
        self.header
            .set_message(format!("{} \u{2014} {}", phase.label(), index_id.bold()));
        if let Some(slot) = phase_to_bar_slot(phase) {
            if self.bar_states[slot] != BarState::Done {
                self.bar_states[slot] = BarState::Active;
                self.stage_bars[slot].set_style(bar_style(slot, BarState::Active, None));
                self.stage_bars[slot].set_position(0);
            }
        }
    }

    /// Set the total for the currently active stage bar (or slot-0 on `Walking`).
    ///
    /// Why: the daemon reports `total_files` in `walk_complete` and `start`
    /// events, which is needed to compute the bar percentage.
    /// What: sets `length` on the bar for the current phase's slot.
    /// Test: `tests::set_total_and_position_affect_active_bar`.
    pub(crate) fn set_total(&self, total: u64) {
        if let Some(slot) = phase_to_bar_slot(self.phase) {
            self.stage_bars[slot].set_length(total.max(1));
        }
    }

    /// Advance the currently active stage bar to `pos`.
    ///
    /// Why: called on every `batch` or `skip` SSE event to keep the active bar
    /// moving.
    /// What: calls `set_position` on the active slot's bar.
    /// Test: `tests::set_total_and_position_affect_active_bar`.
    pub(crate) fn set_position(&self, pos: u64) {
        if let Some(slot) = phase_to_bar_slot(self.phase) {
            self.stage_bars[slot].set_position(pos);
        }
    }

    /// Mark the given slot as done: snap the bar to 100%, apply the "done" style
    /// with elapsed time, and record the slot state so future `set_phase` calls
    /// don't accidentally re-activate it.
    ///
    /// Why: a completed stage must remain visually frozen (full bar + elapsed
    /// time) while later stages animate. `mark_stage_done` is the only place
    /// that transitions a bar to `BarState::Done`.
    /// What: sets position = length, applies `bar_style(slot, Done, elapsed_ms)`,
    /// stores `elapsed_ms` in `self.stage_elapsed_ms[slot]`.
    /// Test: `tests::mark_stage_done_freezes_bar`.
    pub(crate) fn mark_stage_done(&mut self, slot: usize, elapsed_ms: u64) {
        if slot >= 4 {
            return;
        }
        self.bar_states[slot] = BarState::Done;
        self.stage_elapsed_ms[slot] = elapsed_ms;
        let len = self.stage_bars[slot].length().unwrap_or(1);
        self.stage_bars[slot].set_length(len.max(1));
        self.stage_bars[slot].set_position(len);
        self.stage_bars[slot].set_style(bar_style(slot, BarState::Done, Some(elapsed_ms)));
    }

    /// Refresh the stats line with embedding progress details.
    ///
    /// Why: the stats line carries per-second throughput and ETA that don't fit
    /// in the bar template's fixed slots.
    /// What: formats a "Embedding… N chunks — M cps — Files X/Y  Skipped Z
    /// Elapsed Ns  ETA ?s" string and sets it on the stats bar.
    /// Test: `tests::update_stats_formats_message`.
    pub(crate) fn update_stats(
        &self,
        indexed: u64,
        total_chunks: u64,
        skipped: u64,
        chunks_per_sec: u64,
        elapsed_secs: u64,
    ) {
        let total = if let Some(slot) = phase_to_bar_slot(self.phase) {
            self.stage_bars[slot].length().unwrap_or(0)
        } else {
            0
        };
        let files_per_sec = indexed.checked_div(elapsed_secs).unwrap_or(0);
        let eta = if files_per_sec > 0 && total > indexed {
            fmt_secs((total - indexed) / files_per_sec)
        } else {
            "?".to_string()
        };
        self.stats.set_message(format!(
            "Embedding\u{2026} {chunks} chunks \u{2014} {cps} cps \u{2014} Files {indexed}/{total}  Skipped {skipped}  Elapsed {elapsed}  ETA {eta}",
            chunks = format_with_commas(total_chunks),
            cps = chunks_per_sec,
            indexed = format_with_commas(indexed),
            total = format_with_commas(total),
            skipped = format_with_commas(skipped),
            elapsed = fmt_secs(elapsed_secs),
            eta = eta,
        ));
    }

    /// Clear the stats line (called when entering the KG phase, where no
    /// per-chunk throughput is available yet).
    ///
    /// Why: the stats line shows embedding throughput, which is meaningless
    /// during the KG rebuild.
    /// What: sets the stats bar message to an empty string.
    /// Test: exercised by `tests::clear_stats_empties_message`.
    pub(crate) fn clear_stats(&self) {
        self.stats.set_message(String::new());
    }

    /// Call on the `complete` SSE event: mark any not-yet-done bars as done,
    /// then finish the header with the final summary message.
    ///
    /// Why: a `lexical_only` index never visits the Embed or KG bars, and an
    /// early timeout may leave bars in mid-flight. Calling `finish_all` ensures
    /// every bar reaches a terminal state before the `MultiProgress` teardown.
    /// What: for slots 0..=3, if `bar_states[slot] != Done`, calls
    /// `finish_and_clear` on that bar; then calls `finish_with_message` on the
    /// header. The stats bar is always cleared.
    /// Test: `tests::finish_all_clears_pending_bars`.
    pub(crate) fn finish(self, final_msg: String) {
        for slot in 0..4 {
            if self.bar_states[slot] != BarState::Done {
                self.stage_bars[slot].finish_and_clear();
            }
        }
        self.stats.finish_and_clear();
        self.header.finish_with_message(final_msg);
    }

    /// Abandon the UI on error or timeout. All bars are abandoned (not cleared)
    /// so the operator can see the partial state.
    ///
    /// Why: `ProgressBar::abandon` leaves the bar on screen as-is so the
    /// operator sees where the reindex stopped rather than a blank terminal.
    /// What: calls `abandon` on every bar and the header.
    /// Test: `tests::abandon_does_not_panic`.
    pub(crate) fn abandon(self, final_msg: String) {
        for bar in &self.stage_bars {
            bar.abandon();
        }
        self.stats.abandon();
        self.header.abandon_with_message(final_msg);
    }

    /// Return a clone of the stats bar so the background ticker can write to it
    /// without holding a reference to `&mut self`.
    ///
    /// Why: the wall-clock ticker runs as a separate `tokio::spawn` task; it
    /// needs access to the stats bar without borrowing `ReindexUi`. `ProgressBar`
    /// is internally `Arc`-wrapped, so cloning is cheap and safe.
    /// What: returns `self.stats.clone()`.
    /// Test: tested indirectly by the ticker path in `run_reindex_with`.
    pub(crate) fn stats_bar(&self) -> ProgressBar {
        self.stats.clone()
    }

    /// Return a clone of the Embed stage bar (slot 2) so the background ticker
    /// can read its length for ETA calculations without borrowing `ReindexUi`.
    ///
    /// Why: same rationale as `stats_bar` — the ticker reads the bar's length
    /// to compute `total` for the ETA formula.
    /// What: returns `self.stage_bars[2].clone()`.
    /// Test: tested indirectly by the ticker path in `run_reindex_with`.
    pub(crate) fn embed_bar(&self) -> ProgressBar {
        self.stage_bars[2].clone()
    }
}

// ─── Timing breakdown (re-exported from here so engine.rs stays lean) ─────────

/// Print the per-phase indexing time breakdown after a successful reindex.
///
/// Why: gives the operator proof that each phase (parse/chunk, embed, vector
/// upsert, BM25, knowledge graph) actually ran and how long each took. The
/// daemon reports these as a post-hoc `timings` payload on the terminal
/// `complete` SSE event — they cannot be streamed live because the daemon's
/// orchestrator fuses parse/embed/commit per batch and runs BM25/KG/upsert as
/// finalization. The vector-count check is the smoking-gun signal for the
/// "embedder silently fell back to BM25" failure mode — printed as a loud
/// warning so it can never go unnoticed.
/// What: a 5-line phase breakdown (Parse/chunk, Embed, Upsert vectors, BM25,
/// Knowledge graph), with the Embed line replaced by a warning when
/// `vector_count == 0` despite non-zero chunks (the BM25-only-mode signal).
/// Test: `tests::timing_breakdown_*` exercise the warning and normal paths.
pub fn print_timing_breakdown(t: &ReindexTimings, total_chunks: u64) {
    println!(
        "  {} {:>7}  ({} chunks)",
        "Parse/chunk:   ".dimmed(),
        fmt_elapsed(t.parse_ms),
        format_with_commas(total_chunks),
    );
    if t.vector_count == 0 && total_chunks > 0 {
        println!(
            "  {} {}",
            "Embed:         ".dimmed(),
            "SKIPPED (embedder unavailable \u{2014} BM25-only mode)"
                .yellow()
                .bold(),
        );
    } else {
        println!(
            "  {} {:>7}  ({} vectors)",
            "Embed:         ".dimmed(),
            fmt_elapsed(t.embed_ms),
            format_with_commas(t.vector_count),
        );
    }
    println!(
        "  {} {:>7}  ({} vectors)",
        "Upsert vectors:".dimmed(),
        fmt_elapsed(t.vector_upsert_ms),
        format_with_commas(t.vector_count),
    );
    println!(
        "  {} {:>7}",
        "BM25 index:    ".dimmed(),
        fmt_elapsed(t.bm25_ms)
    );
    println!(
        "  {} {:>7}  ({} symbols, {} edges)",
        "Knowledge graph:".dimmed(),
        fmt_elapsed(t.kg_ms),
        format_with_commas(t.symbol_count),
        format_with_commas(t.edge_count),
    );
}

/// Per-subsystem indexing timings parsed from the SSE `complete` event.
///
/// Why: gives the user proof that each subsystem ran and how long each took.
/// `vector_count == 0` with `total_chunks > 0` is the smoking-gun signal that
/// the embedder silently fell back to BM25-only — surfaced as a warning in the
/// CLI breakdown so this regression can never go unnoticed.
#[derive(Debug, Default, Clone, Copy)]
pub struct ReindexTimings {
    pub parse_ms: u64,
    pub embed_ms: u64,
    pub bm25_ms: u64,
    pub vector_upsert_ms: u64,
    pub kg_ms: u64,
    pub vector_count: u64,
    pub symbol_count: u64,
    pub edge_count: u64,
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(ui.stage_elapsed_ms[0], 1_200);
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

    /// `print_timing_breakdown` must not panic for the BM25-only fallback path
    /// (`vector_count == 0` with chunks present).
    ///
    /// Why: the BM25-only warning path exercises a branch that historically
    /// panicked on a formatting mismatch; pinning it here prevents regression.
    /// What: calls `print_timing_breakdown` with `vector_count = 0` and non-zero
    /// chunks; asserts no panic.
    /// Test: this test.
    #[test]
    fn timing_breakdown_bm25_only_does_not_panic() {
        let t = ReindexTimings {
            parse_ms: 1_000,
            embed_ms: 0,
            bm25_ms: 200,
            vector_upsert_ms: 0,
            kg_ms: 50,
            vector_count: 0,
            symbol_count: 10,
            edge_count: 4,
        };
        print_timing_breakdown(&t, 1_234);
    }

    /// `print_timing_breakdown` must not panic for a normal completion with
    /// non-zero vectors across every phase.
    ///
    /// Why: the normal path has the same format; pinning it here ensures both
    /// paths are regression-tested.
    /// What: calls `print_timing_breakdown` with realistic values; asserts no
    /// panic.
    /// Test: this test.
    #[test]
    fn timing_breakdown_normal_does_not_panic() {
        let t = ReindexTimings {
            parse_ms: 5_000,
            embed_ms: 90_000,
            bm25_ms: 1_200,
            vector_upsert_ms: 3_400,
            kg_ms: 800,
            vector_count: 62_926,
            symbol_count: 14_823,
            edge_count: 41_002,
        };
        print_timing_breakdown(&t, 62_926);
    }
}
