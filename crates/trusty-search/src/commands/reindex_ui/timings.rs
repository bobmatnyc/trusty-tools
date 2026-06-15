//! Per-subsystem indexing timings and the CLI timing-breakdown formatter.
//!
//! Why: separating the timing data type and its formatter from the progress-bar
//! widgets keeps `bars.rs` focused on indicatif rendering and makes the timing
//! output independently testable (no TTY or `MultiProgress` required).
//!
//! What: `ReindexTimings` holds the per-subsystem millisecond counters extracted
//! from the SSE `complete` event; `format_timing_breakdown` renders them into the
//! human-readable breakdown printed after a successful reindex.
//!
//! Test: `tests::timing_breakdown_*` and `tests::embed_line_*` in the sibling
//! `tests.rs` file exercise every branch of `format_timing_breakdown`.

use super::bars::EMBED_STAR_NOTE;
use crate::commands::format::{fmt_elapsed, format_with_commas};
use colored::Colorize;

// ─── Timing data ─────────────────────────────────────────────────────────────

/// Per-subsystem indexing timings parsed from the SSE `complete` event.
///
/// Why: gives the user proof that each subsystem ran and how long each took.
/// `vector_count == 0` with `total_chunks > 0` is the smoking-gun signal that
/// the embedder silently fell back to BM25-only — surfaced as a warning in the
/// CLI breakdown so this regression can never go unnoticed.
/// Issue #744: `walk_ms` added so operators can see the file-scan time separately.
/// What: plain data struct; all fields are `u64` millisecond or count values.
/// Test: `tests::timing_breakdown_*` and `tests::embed_line_*`.
#[derive(Debug, Default, Clone, Copy)]
pub struct ReindexTimings {
    /// Issue #744: wall-clock from reindex start to end of file walk.
    pub walk_ms: u64,
    pub parse_ms: u64,
    pub embed_ms: u64,
    pub bm25_ms: u64,
    pub vector_upsert_ms: u64,
    pub kg_ms: u64,
    pub vector_count: u64,
    pub symbol_count: u64,
    pub edge_count: u64,
}

// ─── Timing formatter ────────────────────────────────────────────────────────

/// Format the per-phase indexing time breakdown into a `String`.
///
/// Why: separating formatting from printing lets tests assert on rendered text.
/// What: returns the full multi-line breakdown ending with exactly one `\n`.
///       `defer_embed`/`lexical_only` select the embed-line message for the
///       three distinct zero-vector states (see `tests::embed_line_*`).
/// Test: `tests::timing_breakdown_*` and `tests::embed_line_*`.
pub fn format_timing_breakdown(
    t: &ReindexTimings,
    total_chunks: u64,
    elapsed_ms: u64,
    defer_embed: bool,
    lexical_only: bool,
) -> String {
    let mut out = String::new();

    // Issue #744: show walk time first so the phase breakdown is in pipeline order.
    if t.walk_ms > 0 {
        out.push_str(&format!(
            "  {} {:>7}\n",
            "File walk:     ".dimmed(),
            fmt_elapsed(t.walk_ms),
        ));
    }
    // Wall-clock total — the single authoritative number the operator should
    // trust.  All subsystem times below overlap; this is the real duration.
    out.push_str(&format!(
        "  {} {:>7}\n",
        "Wall-clock total:".bold(),
        fmt_elapsed(elapsed_ms),
    ));

    // Pipeline subsystem times — overlapping, informational only.
    out.push_str(&format!(
        "  {}\n",
        "Pipeline (overlapping \u{2014} subsystem times do not sum to total):".dimmed()
    ));
    let parse_line = format!("{} {:>7}", "parse  ".dimmed(), fmt_elapsed(t.parse_ms));
    out.push_str(&format!(
        "    {}  ({} chunks)\n",
        parse_line,
        format_with_commas(total_chunks),
    ));
    if t.vector_count == 0 && total_chunks > 0 {
        // #929: three-way embed line: lexical_only→calm, defer_embed→suppress, else→loud warn.
        if lexical_only {
            out.push_str(&format!(
                "    {} {}\n",
                "embed  ".dimmed(),
                "SKIPPED (lexical-only index \u{2014} embedding disabled by config)"
                    .dimmed()
                    .bold(),
            ));
        } else if !defer_embed {
            let msg = "SKIPPED (embedder unresponsive or unreachable \u{2014} \
                 sidecar may be stalled or not running; BM25-only until re-indexed)";
            out.push_str(&format!(
                "    {} {}\n",
                "embed  ".dimmed(),
                msg.yellow().bold(),
            ));
        }
        // defer_embed=true → suppress; background note covers this case.
    } else {
        let embed_line = format!("{} {:>7}", "embed  ".dimmed(), fmt_elapsed(t.embed_ms));
        out.push_str(&format!(
            "    {}  ({} vectors)\n",
            embed_line,
            format_with_commas(t.vector_count),
        ));
    }
    // bm25 and upsert: the vector count is parenthetical to upsert only —
    // "(N vectors upserted)" makes it unambiguous which subsystem the count
    // belongs to.  bm25 has no per-call vector annotation.
    //
    // Issue #1174: on the defer-embed (fast C1) path, vector_upsert_ms == 0
    // and vector_count == 0 because upsert is deferred to the background C2
    // pass. Emitting "upsert 0ms (0 vectors upserted)" on this path is
    // actively misleading — real BM25 work happened, but the zero-upsert
    // annotation makes it look like something was skipped or broken.
    // When defer_embed=true and vector_count==0 we show only the BM25 line
    // with a "(vectors deferred to background pass)" annotation so the
    // operator knows upsert will happen asynchronously.
    let bm25_line = format!("{} {:>7}", "bm25   ".dimmed(), fmt_elapsed(t.bm25_ms));
    if defer_embed && t.vector_count == 0 {
        out.push_str(&format!(
            "    {}  (vectors deferred to background embed pass)\n",
            bm25_line,
        ));
    } else {
        let upsert_line = format!(
            "{} {:>7}",
            "upsert ".dimmed(),
            fmt_elapsed(t.vector_upsert_ms)
        );
        out.push_str(&format!(
            "    {} \u{00b7} {} ({} vectors upserted)\n",
            bm25_line,
            upsert_line,
            format_with_commas(t.vector_count),
        ));
    }

    // KG is a genuine tail stage — it runs after the batch loop completes.
    let kg_line = format!(
        "{} {:>7}",
        "Knowledge graph (tail stage):".dimmed(),
        fmt_elapsed(t.kg_ms)
    );
    out.push_str(&format!(
        "  {}  ({} symbols, {} edges)\n",
        kg_line,
        format_with_commas(t.symbol_count),
        format_with_commas(t.edge_count),
    ));
    // Footnote for the Embed* bar label — only meaningful when vectors were
    // actually committed (BM25-only mode has no concurrent upsert to explain).
    //
    // Newline discipline: the KG line above already ends with `\n`, so the
    // BM25-only path (no footnote) terminates correctly.  The vector>0 path
    // appends the footnote text and then its own `\n`, so both paths end with
    // exactly one trailing newline.  `print!` in `print_timing_breakdown` then
    // leaves the cursor on a fresh line without needing a `println!` wrapper.
    if t.vector_count > 0 {
        out.push_str(&EMBED_STAR_NOTE.dimmed().to_string());
        out.push('\n');
    }
    out
}

/// Print the per-phase indexing time breakdown after a successful reindex.
///
/// Why: thin print wrapper so callers don't need to capture a String.
/// What: delegates to `format_timing_breakdown` and prints the result.
/// Test: `tests::timing_breakdown_*` smoke-test calls exercise this path.
pub fn print_timing_breakdown(
    t: &ReindexTimings,
    total_chunks: u64,
    elapsed_ms: u64,
    defer_embed: bool,
    lexical_only: bool,
) {
    print!(
        "{}",
        format_timing_breakdown(t, total_chunks, elapsed_ms, defer_embed, lexical_only)
    );
}
