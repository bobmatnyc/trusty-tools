//! Progress-bar rendering for the `reindex` CLI: bar styles and `ReindexUi`.
//!
//! Why: encapsulating indicatif bar creation, style templates, and state
//! transitions in a single module keeps the rendering logic testable in
//! isolation and separates it from the phase/timing data types in `phase.rs`
//! and `timings.rs`.
//!
//! What: `ReindexUi` owns one `MultiProgress` with a header spinner plus four
//! named bars (one per stage). `bar_style` produces the correct `ProgressStyle`
//! for each lifecycle state. `STAGE_LABELS` and `EMBED_STAR_NOTE` are
//! string resources used by both the bars and the timing breakdown formatter.
//!
//! Test: every `fn …()` method in `ReindexUi` has a corresponding unit test in
//! the sibling `tests.rs` file; construction exercises all bars in hidden mode.

use super::phase::{phase_to_bar_slot, ReindexPhase};
use crate::commands::format::{fmt_elapsed, fmt_secs, format_with_commas};
use colored::Colorize;
use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};
use std::time::Duration;

// ─── Bar-slot indices ─────────────────────────────────────────────────────────

/// Label prefix for each slot (matches the 4 stages in issue #401 / #929 order).
///
/// Issue #929: labels updated to reflect the 4-stage reindex UX:
///   [1/4] Scan          (was "Crawl") — file-tree walk
///   [2/4] Chunk         — parse + chunk
///   [3/4] Lexical(BM25) (was "Embed*") — BM25 index; in defer-embed mode the
///                        foreground pass does NOT embed vectors here
///   [4/4] KG            — knowledge-graph rebuild
///
/// The old "Embed*" label and its asterisk are retired because in the default
/// defer-embed path the Embed bar no longer runs during the foreground pass —
/// embedding happens as a background job AFTER `complete`. The asterisk
/// footnote was only meaningful when Embed ran in the foreground.
pub(crate) const STAGE_LABELS: [&str; 4] = ["Scan", "Chunk", "Lexical(BM25)", "KG"];

/// Footnote displayed beneath the timing breakdown when vectors were upserted.
///
/// Shown only when `vector_count > 0` (synchronous / non-defer-embed mode)
/// to explain that the BM25 and vector-upsert stages run concurrently in the
/// synchronous path.  In the default defer-embed path, `vector_count==0` on
/// the fast pass so this note is never printed.
///
/// Why/What: even with the new "Lexical(BM25)" foreground label (issue #929),
/// the overlap note remains accurate for synchronous full-index runs where
/// BM25 + HNSW upsert still run concurrently with parse+embed.
pub(crate) const EMBED_STAR_NOTE: &str =
    "  * BM25 + vector-upsert commit runs concurrently with parse+embed (overlapping pipeline)";

// ─── Bar lifecycle state ──────────────────────────────────────────────────────

/// Lifecycle state of one stage bar.
///
/// Why: drives the visual transition between the three indicatif style
/// templates — Pending (empty trough), Active (cyan advancing bar), Done
/// (frozen green bar with elapsed time).
/// What: each variant corresponds to exactly one `bar_style` template.
/// Test: `tests::phase_transitions_activate_correct_bar` and
/// `tests::mark_stage_done_freezes_bar`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BarState {
    /// Not yet started — bar shows an empty trough.
    Pending,
    /// Currently active — bar advances with SSE events.
    Active,
    /// Completed — bar shows 100% / done frame.
    Done,
}

// ─── Style helpers ────────────────────────────────────────────────────────────

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
pub(crate) fn bar_style(slot: usize, state: BarState, elapsed_ms: Option<u64>) -> ProgressStyle {
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
/// `tests::*` in the sibling `tests.rs`; construction exercises all bars in
/// hidden mode.
pub(crate) struct ReindexUi {
    /// Held to keep the `MultiProgress` draw target alive for the bars' lifetime.
    #[allow(dead_code)]
    multi: MultiProgress,
    /// Spinner line at the top: "⟳ <phase> — <index>".
    header: ProgressBar,
    /// The four stage bars, in order: Crawl (0), Chunk (1), Embed (2), KG (3).
    pub(crate) stage_bars: [ProgressBar; 4],
    /// Elapsed-ms snapshot for each completed stage (filled by `mark_stage_done`).
    stage_elapsed_ms: [u64; 4],
    /// Stats line below the bars (embedding throughput, ETA, etc.).
    stats: ProgressBar,
    /// Current phase; used to identify the active bar and update the header.
    pub(crate) phase: ReindexPhase,
    /// Lifecycle state of each stage bar (Pending / Active / Done).
    pub(crate) bar_states: [BarState; 4],
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

    /// Set the total (length) for the Embed bar (slot 2) directly, regardless
    /// of the currently active phase.
    ///
    /// Why: the Embed bar must show `N/total_files` from the moment CHUNK+EMBED
    /// begins — before any `batch` event activates `Embedding` phase. Without
    /// this, the bar is initialised to `new(1)` and stays `0/1` for the entire
    /// model-load period. This method lets the `walk_complete`/`start` handler
    /// prime slot 2 with the correct denominator even while phase=Chunking.
    ///
    /// What: calls `set_length(total.max(1))` on `stage_bars[2]`.
    /// Test: `tests::set_embed_total_primes_slot2_while_chunking`.
    pub(crate) fn set_embed_total(&self, total: u64) {
        self.stage_bars[2].set_length(total.max(1));
    }

    /// Activate the Embed bar (slot 2) into the Active visual style without
    /// changing `self.phase`. Used when the CHUNK+EMBED phase starts so both
    /// Chunk (slot 1) and Embed (slot 2) are visually live simultaneously.
    ///
    /// Why: the agreed design calls for two concurrent bars during CHUNK+EMBED;
    /// the usual `set_phase(Embedding)` would transition the header too early.
    /// This helper just applies the Active bar style + resets position to 0
    /// without touching `self.phase` or the header.
    /// What: applies `bar_style(2, BarState::Active, None)` to slot 2 and sets
    /// `bar_states[2] = Active` if it was Pending.
    /// Test: `tests::activate_embed_bar_does_not_change_phase`.
    pub(crate) fn activate_embed_bar(&mut self) {
        if self.bar_states[2] == BarState::Pending {
            self.bar_states[2] = BarState::Active;
            self.stage_bars[2].set_style(bar_style(2, BarState::Active, None));
            self.stage_bars[2].set_position(0);
        }
    }

    /// Advance the Embed bar (slot 2) position directly, regardless of the
    /// currently active phase.
    ///
    /// Why: during CHUNK+EMBED both bars are live simultaneously. The Embed bar
    /// (slot 2) trails the Chunk bar (slot 1); it advances on `batch` events
    /// (files committed/embedded) while the Chunk bar advances on `chunk_progress`
    /// and `batch` events (files parsed). This method lets the event loop set slot
    /// 2 independently without changing `self.phase`.
    /// What: calls `set_position(pos)` on `stage_bars[2]` if it is Active or Done.
    /// Test: `tests::advance_embed_bar_sets_slot2_position`.
    pub(crate) fn advance_embed_bar(&self, pos: u64) {
        if self.bar_states[2] != BarState::Pending {
            self.stage_bars[2].set_position(pos);
        }
    }

    /// Return `true` when the currently active phase maps to slot 2 (the Embed bar).
    ///
    /// Why (issue #827): callers that want to advance the Embed bar independently
    /// must skip the `advance_embed_bar` call when `set_position` already targeted
    /// slot 2, otherwise the bar is advanced twice per event.
    /// What: checks whether `phase_to_bar_slot(self.phase) == Some(2)`.
    /// Test: `tests::active_phase_is_embed_returns_true_for_embedding_phases`.
    pub(crate) fn active_phase_is_embed(&self) -> bool {
        phase_to_bar_slot(self.phase) == Some(2)
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

    /// Refresh the stats line with current phase progress details.
    ///
    /// Why: the stats line carries per-second throughput and ETA that don't fit
    /// in the bar template's fixed slots.  The label prefix is taken from the
    /// active `phase` so the footer matches the header exactly — previously it
    /// was hard-coded to "Embedding…" and therefore disagreed with the header
    /// during the Chunking and InitializingEmbedder phases (the 46-second stall
    /// visible as "Chunking…" header / "Embedding…" footer).
    /// What: formats a "{phase_label} N chunks — M cps — Files X/Y  Skipped Z
    /// Elapsed Ns  ETA ?s" string and sets it on the stats bar.
    /// Test: `tests::update_stats_formats_message` and
    /// `tests::update_stats_label_matches_phase`.
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
        // Use the active phase label so the footer agrees with the header.
        // During Chunking / InitializingEmbedder the header says "Chunking…" or
        // "Loading model…"; the stats line must reflect the same active step.
        let phase_label = self.phase.label();
        self.stats.set_message(format!(
            "{phase_label} {chunks} chunks \u{2014} {cps} cps \u{2014} Files {indexed}/{total}  Skipped {skipped}  Elapsed {elapsed}  ETA {eta}",
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

    /// Return a clone of the Embed stage bar (slot 2).
    ///
    /// Why: previously used by the ticker to read `bar.length()` for ETA.
    /// Issue #744 replaces that with a shared `total_files_now` AtomicU64
    /// (set from `walk_complete`/`start` SSE events) so ETA is correct from
    /// the very start rather than only after the first batch. Retained here
    /// for any future caller that needs direct access to the Embed bar.
    /// What: returns `self.stage_bars[2].clone()`.
    /// Test: construction exercises all bars in hidden mode.
    #[allow(dead_code)]
    pub(crate) fn embed_bar(&self) -> ProgressBar {
        self.stage_bars[2].clone()
    }
}
