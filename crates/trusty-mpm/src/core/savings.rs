//! Per-session token-savings ledger (#6958).
//!
//! Why: trusty-mpm spends real effort not sending tokens — folding instruction
//! sources into one compiled prompt, diverting a large file read to a cheap
//! worker, compressing gate output before an agent reads it. None of that was
//! visible anywhere, so the owner's question ("how much are we saving?") had no
//! answer at all. This module is that answer's storage: one append-only log
//! every producer writes a row to, and one read-time fold the statusline (and,
//! later, the console) renders.
//!
//! What: `~/.trusty-mpm/usage/savings.jsonl` — one JSON object per line,
//! `{ts, session_id, technique, tokens_saved, cost_saved_usd, basis}`.
//! [`append_row`] is the single writer, [`fold_session`] and [`fold_all`] the
//! single readers. `technique` is an open string, so a new producer needs no
//! schema change here.
//!
//! Two properties the rest of the feature depends on:
//!
//! - **Append-only, one row per `O_APPEND` write.** No cross-process lock, no
//!   rollup file that can drift from the log. The total is derived at read
//!   time, matching the owner's 2026-07-29 ruling for the Costs surface.
//! - **A bad row cannot move the total.** A line that does not parse, a row
//!   with `tokens_saved <= 0`, and a row with a non-positive or non-finite
//!   `cost_saved_usd` are each skipped with a `warn!` and contribute nothing.
//!   A producer bug that undercounts its baseline therefore shows as a missing
//!   contribution, never as a negative or inflated displayed figure.
//!
//! Everything here fails soft: a missing, unreadable, or truncated ledger folds
//! to zero rather than erroring, because the consumer is a status bar on
//! Claude Code's hot render path.
//!
//! Test: the inline suite in `savings_tests.rs` — `append_then_fold_round_trips`,
//! `fold_skips_a_malformed_line_and_keeps_the_valid_total`,
//! `a_negative_row_cannot_raise_the_total`, `fold_ignores_other_sessions`,
//! `fold_of_a_missing_ledger_is_zero`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Characters per token used by every estimate in this feature.
///
/// Why: the owner's directive is explicit that the figure "doesn't have to be
/// exact", and a byte-delta producer has no tokenizer available at its call
/// site. Four characters per token is the conventional English-prose
/// approximation Anthropic's own guidance uses; stating it once here is what
/// keeps two producers from picking two different divisors and reporting
/// incomparable numbers.
/// What: the divisor applied to a byte delta to reach a token delta.
/// Test: `instruction_compression_tokens_use_the_shared_divisor`.
pub const BYTES_PER_TOKEN: f64 = 4.0;

/// `technique` value written by the instruction/language-compression producer.
///
/// Test: `instruction_compression_row_carries_the_named_technique`.
pub const TECHNIQUE_INSTRUCTION_COMPRESSION: &str = "instruction-compression";

/// One producer's claim that a technique avoided sending some tokens.
///
/// Why: estimates and measurements share one row shape, which is why `basis` is
/// free text rather than a typed formula — a divert row's basis is three token
/// counts, an instruction-compression row's is two byte counts, and a future
/// producer's will be something else again. Making `basis` structured would
/// force a schema change per producer, which is exactly what an open
/// `technique` string is here to avoid.
/// What: `tokens_saved` is signed so a producer bug that writes a negative is
/// *representable* and therefore rejectable at fold time (see the module
/// header) rather than deserialising into a huge unsigned value.
/// Test: `a_negative_row_cannot_raise_the_total`, `fold_ignores_other_sessions`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavingsRow {
    /// RFC 3339 UTC timestamp of the moment the producer wrote the row.
    pub ts: String,
    /// The session the saving is attributed to; the fold filters on this.
    pub session_id: String,
    /// Open technique name — `instruction-compression`, `divert`, `compress`, …
    pub technique: String,
    /// Estimated tokens not sent. Rows at or below zero are skipped.
    pub tokens_saved: i64,
    /// Estimated USD not spent. Rows at or below zero are skipped.
    pub cost_saved_usd: f64,
    /// Free text stating how the two figures above were arrived at.
    pub basis: String,
}

/// The folded total of every accepted row in one read.
///
/// Why: the statusline needs both figures — it renders dollars above a cent and
/// tokens below it — and `rows` is what lets a caller distinguish "the ledger
/// held nothing" from "every row was rejected", which read very differently in
/// a bug report.
/// What: sums of the accepted rows only; skipped rows contribute nothing.
/// Test: `fold_skips_a_malformed_line_and_keeps_the_valid_total`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SavingsTotal {
    /// Sum of `tokens_saved` across accepted rows.
    pub tokens_saved: u64,
    /// Sum of `cost_saved_usd` across accepted rows.
    pub cost_saved_usd: f64,
    /// How many rows were accepted.
    pub rows: usize,
}

impl SavingsTotal {
    /// Whether the fold found nothing to display.
    ///
    /// Why: the statusline segment is omitted entirely on this condition — a
    /// rendered `$0.00` is indistinguishable from "no savings" and states a
    /// measurement that was never made.
    /// What: true when no row was accepted, or when the accepted rows sum to
    /// zero on both axes.
    /// Test: `zero_fold_is_zero`, `savings_segment_is_absent_on_a_zero_fold`.
    pub fn is_zero(&self) -> bool {
        self.rows == 0 || (self.tokens_saved == 0 && self.cost_saved_usd <= 0.0)
    }
}

/// The savings ledger under an explicit framework root.
///
/// Why: the ledger sits in `<root>/usage/` beside where the daemon usage
/// accounting of #6873 will put `usage.redb`, so the two halves of "what did
/// this session cost and what did it avoid" live in one directory. Taking the
/// root as an argument is what lets a test — and the operator's
/// `--root`/`TRUSTY_MPM_ROOT` override — redirect it without touching `$HOME`.
/// What: `<root>/usage/savings.jsonl`.
/// Test: `savings_log_in_nests_under_usage`.
pub fn savings_log_in(root: &Path) -> PathBuf {
    root.join("usage").join("savings.jsonl")
}

/// The savings ledger under the default framework root (`~/.trusty-mpm`).
///
/// Why: producers run inside the library, where the `--root`/`TRUSTY_MPM_ROOT`
/// override the `tm` binary resolves is not in scope; they use the same
/// home-relative root every other `FrameworkPaths` consumer does.
/// What: [`savings_log_in`] against `FrameworkPaths::default().root`.
/// Test: `default_savings_log_is_under_the_framework_root`.
pub fn default_savings_log() -> PathBuf {
    savings_log_in(&crate::core::paths::FrameworkPaths::default().root)
}

/// Append one row to the ledger, creating the `usage/` directory if absent.
///
/// Why: one writer means the on-disk shape cannot drift between producers, and
/// an `O_APPEND` write of a single line is atomic enough for this file's size
/// that no cross-process lock is needed — two producers racing interleave rows,
/// never bytes within a row.
/// What: serialises `row` to one line of JSON and appends it with a trailing
/// newline. Returns the IO error unchanged; every producer treats a failure as
/// non-fatal, since a missing savings row must never cost a session its launch.
/// Test: `append_then_fold_round_trips`, `append_creates_the_usage_directory`.
pub fn append_row(ledger: &Path, row: &SavingsRow) -> std::io::Result<()> {
    use std::io::Write as _;

    if let Some(parent) = ledger.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let line = serde_json::to_string(row)
        .map_err(|source| std::io::Error::new(std::io::ErrorKind::InvalidData, source))?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(ledger)?;
    writeln!(file, "{line}")
}

/// Fold every accepted row belonging to `session_id`.
///
/// Why: the statusline renders one session's figure, and a machine's ledger
/// carries every session's rows. Filtering at read time — rather than keeping a
/// file per session — is what keeps the writer a bare append.
/// What: [`fold`] with a session filter. A missing or unreadable ledger folds
/// to zero.
/// Test: `fold_ignores_other_sessions`, `fold_of_a_missing_ledger_is_zero`.
pub fn fold_session(ledger: &Path, session_id: &str) -> SavingsTotal {
    fold(ledger, Some(session_id))
}

/// Fold every accepted row, whatever session wrote it.
///
/// Why: a machine-wide figure is what a future `tm usage`/console surface wants,
/// and deriving it from the same reader is what stops the two surfaces
/// disagreeing about a total.
/// What: [`fold`] with no filter.
/// Test: `fold_all_sums_every_session`.
pub fn fold_all(ledger: &Path) -> SavingsTotal {
    fold(ledger, None)
}

/// The one fold both readers share.
///
/// Why: the skip rules — unparseable line, non-positive tokens, non-positive or
/// non-finite cost — must be identical for every consumer, or a per-session
/// figure and a machine-wide figure computed from the same file could disagree
/// about which rows count.
/// What: reads the whole file (it is one short line per saving event), parses
/// each non-blank line, applies the filter, and sums what survives. Each
/// rejection emits one `warn!` naming the reason.
/// Test: `fold_skips_a_malformed_line_and_keeps_the_valid_total`,
/// `a_negative_row_cannot_raise_the_total`,
/// `fold_skips_a_row_whose_cost_is_not_a_number`.
fn fold(ledger: &Path, session_id: Option<&str>) -> SavingsTotal {
    let Ok(text) = std::fs::read_to_string(ledger) else {
        // Absent or unreadable is the ordinary state before any producer has
        // run; it is not a fault and must not be logged as one.
        return SavingsTotal::default();
    };

    let mut total = SavingsTotal::default();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let row: SavingsRow = match serde_json::from_str(line) {
            Ok(row) => row,
            Err(source) => {
                tracing::warn!(
                    ledger = %ledger.display(),
                    line = index + 1,
                    %source,
                    "skipping a malformed savings row"
                );
                continue;
            }
        };
        if session_id.is_some_and(|wanted| row.session_id != wanted) {
            continue;
        }
        if row.tokens_saved <= 0 {
            tracing::warn!(
                ledger = %ledger.display(),
                line = index + 1,
                technique = %row.technique,
                tokens_saved = row.tokens_saved,
                "skipping a savings row with non-positive tokens_saved"
            );
            continue;
        }
        if !row.cost_saved_usd.is_finite() || row.cost_saved_usd <= 0.0 {
            tracing::warn!(
                ledger = %ledger.display(),
                line = index + 1,
                technique = %row.technique,
                cost_saved_usd = row.cost_saved_usd,
                "skipping a savings row with non-positive cost_saved_usd"
            );
            continue;
        }
        total.tokens_saved += row.tokens_saved as u64;
        total.cost_saved_usd += row.cost_saved_usd;
        total.rows += 1;
    }
    total
}

/// The current instant as an RFC 3339 UTC timestamp, for a row's `ts`.
///
/// Why: every producer stamps its row the same way, so the ledger sorts
/// chronologically by plain string comparison.
/// What: `chrono::Utc::now()` in RFC 3339 form with second precision.
/// Test: `now_ts_is_rfc3339`.
pub fn now_ts() -> String {
    chrono::Utc::now()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        .to_string()
}

#[cfg(test)]
#[path = "savings_tests.rs"]
mod tests;
