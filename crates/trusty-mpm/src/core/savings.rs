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
//! `{ts, session_id, technique, tokens_saved, tokens_before, cost_saved_usd,
//! basis}`. [`append_row`] is the single writer, [`fold_session`] and
//! [`fold_all`] the single readers. `technique` is an open string, so a new
//! producer needs no schema change here.
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
//! **[`SavingsTotal::percent_saved`] denominator (owner ruling 2026-09-08,
//! #7179).** The `💸` segment shows a whole-number percent of tokens avoided,
//! not a dollar figure: `saved / (session actual tokens + saved)` — the
//! *session share* the owner asked for. "Session actual tokens" is a
//! cumulative counter `crate::commands::statusline::compaction` (the `tm`
//! binary) folds across every auto-compaction reset of the `statusLine`
//! hook's `total_input_tokens`, since that raw figure resets on every
//! compaction and would otherwise make the percent jump non-monotonically for
//! reasons unrelated to anything the harness saved. This module has no access
//! to that counter (it lives in the `tm` binary crate, keyed by session id in
//! `~/.trusty-mpm/statusline/<session_id>.json`), so `percent_saved` takes it
//! as an `Option<u64>` argument the statusline binary supplies at render time.
//! `None` — no compaction tick has landed yet for this session — falls back to
//! the pre-#7179 formula, `tokens_saved / tokens_before` (each row's own
//! pre-saving token count, folded the same way `tokens_saved` is).
//! `#[serde(default)]` on `tokens_before` so a pre-#7179 row folds with
//! `tokens_before = 0` and is excluded from that fallback denominator, rather
//! than failing to parse.
//!
//! Everything here fails soft: a missing, unreadable, or truncated ledger folds
//! to zero rather than erroring, because the consumer is a status bar on
//! Claude Code's hot render path.
//!
//! Test: the inline suite in `savings_tests.rs` — `append_then_fold_round_trips`,
//! `fold_skips_a_malformed_line_and_keeps_the_valid_total`,
//! `a_negative_row_cannot_raise_the_total`, `fold_ignores_other_sessions`,
//! `fold_of_a_missing_ledger_is_zero`, `percent_saved_rounds_to_nearest_whole_number`.

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

/// `technique` value written by the bulk-read diversion producer (#6959).
///
/// Test: `divert_row_carries_the_named_technique`.
pub const TECHNIQUE_DIVERT: &str = "divert";

/// `technique` value written by the tool-output compression producer.
///
/// Why: `tm compress` shrinks a gate's output before an agent reads it, which
/// avoids sending tokens exactly the way the other two producers do. Until it
/// wrote a row, the `💸` segment reported everything the harness saves EXCEPT
/// bash and tool output.
/// Test: `a_compress_row_carries_the_named_technique`.
pub const TECHNIQUE_COMPRESS: &str = "compress";

/// The environment variable Claude Code exports carrying the session's own id.
///
/// Why (#7209): every row on this ledger is folded back by
/// [`fold_session`] under the `session_id` Claude Code sends the statusline on
/// stdin, which is this variable's value. A producer that keys a row by any
/// other id writes a row the segment can never match, and the `💸` segment then
/// omits itself instead of rendering a percent.
/// What: `CLAUDE_CODE_SESSION_ID`.
/// Test: `claude_code_session_id_names_the_harness_variable`.
pub const CLAUDE_CODE_SESSION_ID_ENV: &str = "CLAUDE_CODE_SESSION_ID";

/// The Claude Code session id, when the harness exported one.
///
/// Why: this is the single read of [`CLAUDE_CODE_SESSION_ID_ENV`] both savings
/// producers route through — the divert producer in the `tm` binary and the
/// instruction-compression producer in this crate — so the two cannot drift
/// into keying their rows differently.
/// What: the variable's value, or `None` when it is absent or blank.
/// Test: `claude_code_session_id_names_the_harness_variable`, and the
/// producers' own suites.
pub fn claude_code_session_id() -> Option<String> {
    std::env::var(CLAUDE_CODE_SESSION_ID_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

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
    /// Estimated pre-saving token count for this row — `tokens_saved` plus the
    /// tokens actually sent instead (#7179).
    ///
    /// Why: [`SavingsTotal::percent_saved`]'s primary denominator is the
    /// session's actual-token counter (see the module header), but that value
    /// is only available once a `statusLine` tick has landed for this session.
    /// This field folds into the *fallback* denominator used when it has not —
    /// the ledger's own before-figure, folded across every row alongside
    /// `tokens_saved`.
    /// What: `u64` (a plain count, never negative by construction).
    /// `#[serde(default)]` so a row written before this field existed folds as
    /// `0` — excluded from the fallback denominator rather than failing to
    /// parse. Producers that cannot state a before-figure simply omit it.
    /// Test: `percent_saved_rounds_to_nearest_whole_number`,
    /// `a_row_written_before_tokens_before_existed_still_folds`.
    #[serde(default)]
    pub tokens_before: u64,
    /// Estimated USD not spent. Rows at or below zero are skipped.
    pub cost_saved_usd: f64,
    /// Free text stating how the two figures above were arrived at.
    pub basis: String,
    /// Which source named the model the row was priced at (#6972).
    ///
    /// Why: every producer prices its token delta at a model's published input
    /// rate, and before #6972 a row priced from the config chain's Sonnet
    /// default was indistinguishable from one priced at the model the session
    /// was really running. That is how a whole session of Opus diversions
    /// under-reported by five times without a single wrong-looking row. Naming
    /// the source makes a wrong price diagnosable off the ledger line.
    /// What: one of
    /// [`crate::core::session_model::MODEL_SOURCE_ENV`],
    /// `MODEL_SOURCE_STATUSLINE`, `MODEL_SOURCE_CONFIG_FALLBACK`, or
    /// `MODEL_SOURCE_LAUNCH_CONFIG`. `#[serde(default)]` so rows written before
    /// #6972 still fold, reading back as `""`.
    /// Test: `divert_row_names_the_model_source`,
    /// `instruction_compression_row_names_its_model_source`.
    #[serde(default)]
    pub model_source: String,
}

/// The folded total of every accepted row in one read.
///
/// Why: the statusline needs `tokens_saved` and `tokens_before` together to
/// compute a percent, and `cost_saved_usd` for `tm`'s own dollar-denominated
/// reporting commands (the ledger stays the one source both surfaces read).
/// `rows` is what lets a caller distinguish "the ledger held nothing" from
/// "every row was rejected", which read very differently in a bug report.
/// What: sums of the accepted rows only; skipped rows contribute nothing.
/// Test: `fold_skips_a_malformed_line_and_keeps_the_valid_total`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SavingsTotal {
    /// Sum of `tokens_saved` across accepted rows.
    pub tokens_saved: u64,
    /// Sum of `tokens_before` across accepted rows — the percent denominator.
    pub tokens_before: u64,
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

    /// Whole-number percent of tokens the harness avoided sending, as a share
    /// of the session (owner ruling 2026-09-08, #7179).
    ///
    /// Why: the `💸` statusline segment renders one percentage — how much of
    /// what this session actually spent plus what it avoided did the harness
    /// avoid. `session_actual_tokens` is the caller-supplied cumulative token
    /// count from `crate::commands::statusline::compaction` (see the module
    /// header for why that counter, rather than a raw `total_input_tokens`
    /// read, is what survives an auto-compaction). This method has no
    /// filesystem access of its own — it is a pure function of the fold plus
    /// whatever the caller already read — which is what keeps it unit-testable
    /// against hand-built totals with no I/O.
    /// What: primary path, `session_actual_tokens = Some(actual)`:
    /// `round(100 * tokens_saved / (actual + tokens_saved))`. Fallback path,
    /// `None` — no compaction tick has landed yet for this session:
    /// `round(100 * tokens_saved / tokens_before)`, the pre-#7179 formula.
    /// This fallback is never distinguished in the rendered string — both
    /// paths produce the identical `💸<N>%` shape; only this doc comment
    /// records which formula ran. Either path clamps to `[1, 100]` whenever
    /// the fold accepted at least one row and the denominator is nonzero — the
    /// lower clamp mirrors the pre-#7179 dollar segment's "never render a
    /// false zero" rule: a sub-0.5% ratio would otherwise round down to a
    /// literal `0%`, which reads as "we saved nothing" and states a
    /// measurement that was never made. The upper clamp covers a mixed
    /// old/new ledger on the fallback path, where rows written before
    /// `tokens_before` existed fold their share of that denominator as `0`
    /// (see the module header) and can otherwise push the raw ratio above 100.
    /// `None` when there is nothing to divide: [`Self::is_zero`], or (on the
    /// fallback path only) a `tokens_before` sum of `0` — every accepted row
    /// predates #7179 and no compaction tick has landed either.
    /// Test: `percent_saved_uses_the_session_actual_denominator_when_given`,
    /// `percent_saved_falls_back_to_tokens_before_when_actual_is_unknown`,
    /// `percent_saved_rounds_to_nearest_whole_number`,
    /// `percent_saved_is_none_on_a_zero_fold`,
    /// `percent_saved_clamps_a_legacy_mixed_fold`,
    /// `percent_saved_never_rounds_down_to_zero`.
    pub fn percent_saved(&self, session_actual_tokens: Option<u64>) -> Option<u32> {
        if self.is_zero() {
            return None;
        }
        let ratio = match session_actual_tokens {
            Some(actual) => {
                let denominator = actual + self.tokens_saved;
                if denominator == 0 {
                    return None;
                }
                self.tokens_saved as f64 / denominator as f64
            }
            None => {
                if self.tokens_before == 0 {
                    return None;
                }
                self.tokens_saved as f64 / self.tokens_before as f64
            }
        };
        let pct = (ratio * 100.0).round();
        // A true zero is already excluded by the checks above, so any ratio
        // reaching here is a real (if tiny) measurement — round it up to the
        // smallest displayable percent rather than down to a false zero.
        let pct = if pct < 1.0 { 1.0 } else { pct };
        Some(pct.clamp(1.0, 100.0) as u32)
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

/// Fold every accepted row, grouped by the session that wrote it (#7074).
///
/// Why: the `💸` segment's average is the mean of each session's OWN percentage
/// — percentages do not sum into a lifetime total but do average cleanly (owner
/// ruling 2026-09-09, option A). That needs one total per session, which no
/// existing reader produced. Deriving it from the same row-acceptance walk as
/// [`fold`] is what keeps the average and the per-session figure from
/// disagreeing about which rows count.
/// What: [`for_each_accepted_row`] with no filter, accumulating into one
/// [`SavingsTotal`] per `session_id`. A missing or unreadable ledger yields an
/// empty map — never a map of zeros.
/// Test: `fold_sessions_groups_by_session`,
/// `fold_sessions_of_a_missing_ledger_is_empty`.
pub fn fold_sessions(ledger: &Path) -> std::collections::BTreeMap<String, SavingsTotal> {
    let mut by_session: std::collections::BTreeMap<String, SavingsTotal> =
        std::collections::BTreeMap::new();
    for_each_accepted_row(ledger, None, |row| {
        let total = by_session.entry(row.session_id.clone()).or_default();
        total.tokens_saved += row.tokens_saved as u64;
        total.tokens_before += row.tokens_before;
        total.cost_saved_usd += row.cost_saved_usd;
        total.rows += 1;
    });
    by_session
}

/// The mean of every session's own savings percentage (#7074).
///
/// Why: the owner ruled the statusline shows a per-session AVERAGE beside the
/// current session's figure (2026-09-09, option A). Averaging the PERCENTAGES —
/// rather than folding all rows into one ratio — is what makes a short session
/// that saved 60 % count as much as a long one that saved 10 %, which is the
/// question "how much are we saving per session" actually asks.
/// What: computes [`SavingsTotal::percent_saved`] for each session in
/// `by_session`, passing that session's own actual-token count from
/// `session_actual_tokens` (the caller supplies the lookup, so this function
/// does no I/O and is testable against a fixture map), and returns the
/// arithmetic mean rounded to a whole percent. Sessions whose percent is `None`
/// — a zero fold, or no denominator on either path — contribute nothing, so a
/// ledger holding only such sessions yields `None` rather than a false `0%`.
/// Test: `average_percent_is_the_mean_of_each_sessions_percent`,
/// `average_of_one_session_is_that_sessions_percent`,
/// `average_of_sessions_with_no_denominator_is_none`,
/// `average_skips_corrupt_rows_and_never_reports_a_false_zero`.
pub fn average_percent_saved(
    by_session: &std::collections::BTreeMap<String, SavingsTotal>,
    session_actual_tokens: impl Fn(&str) -> Option<u64>,
) -> Option<u32> {
    let percents: Vec<u32> = by_session
        .iter()
        .filter_map(|(session_id, total)| total.percent_saved(session_actual_tokens(session_id)))
        .collect();
    if percents.is_empty() {
        return None;
    }
    let sum: u64 = percents.iter().map(|p| u64::from(*p)).sum();
    let mean = sum as f64 / percents.len() as f64;
    Some(mean.round().clamp(1.0, 100.0) as u32)
}

/// The one fold both readers share.
///
/// Why: the skip rules — unparseable line, non-positive tokens, non-positive or
/// non-finite cost — must be identical for every consumer, or a per-session
/// figure and a machine-wide figure computed from the same file could disagree
/// about which rows count.
/// What: [`for_each_accepted_row`] with the caller's filter, summed.
/// Test: `fold_skips_a_malformed_line_and_keeps_the_valid_total`,
/// `a_negative_row_cannot_raise_the_total`,
/// `fold_skips_a_row_whose_cost_is_not_a_number`.
fn fold(ledger: &Path, session_id: Option<&str>) -> SavingsTotal {
    let mut total = SavingsTotal::default();
    for_each_accepted_row(ledger, session_id, |row| {
        total.tokens_saved += row.tokens_saved as u64;
        total.tokens_before += row.tokens_before;
        total.cost_saved_usd += row.cost_saved_usd;
        total.rows += 1;
    });
    total
}

/// Walk the ledger once, handing `visit` every row that passes the skip rules.
///
/// Why (#7074): [`fold`] and [`fold_sessions`] must accept and reject exactly
/// the same rows, or the current session's figure and the average beside it
/// could be computed from different subsets of one file. One walk, one set of
/// rules, two accumulators.
/// What: reads the whole file (it is one short line per saving event), parses
/// each non-blank line, applies the optional session filter, and calls `visit`
/// for what survives. Each rejection emits one `warn!` naming the reason. A
/// missing or unreadable ledger visits nothing.
/// Test: `fold_skips_a_malformed_line_and_keeps_the_valid_total`,
/// `a_negative_row_cannot_raise_the_total`,
/// `fold_skips_a_row_whose_cost_is_not_a_number`,
/// `fold_sessions_groups_by_session`.
fn for_each_accepted_row(
    ledger: &Path,
    session_id: Option<&str>,
    mut visit: impl FnMut(&SavingsRow),
) {
    let Ok(text) = std::fs::read_to_string(ledger) else {
        // Absent or unreadable is the ordinary state before any producer has
        // run; it is not a fault and must not be logged as one.
        return;
    };

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
        visit(&row);
    }
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

#[cfg(test)]
#[path = "savings_average_tests.rs"]
mod average_tests;
