//! Read-time cost aggregation over `.trusty-agents/state/usage.jsonl` (#4098).
//!
//! Why: The Costs tab needs totals and a breakdown by agent, model and day,
//! and the only usage data that exists is the append-only JSONL log
//! `usage::append_usage` writes. Two shapes were available:
//!
//!   1. **Read-time aggregation** (this module) — parse the log on request and
//!      fold it. One code path, no schema, no migration, no second writer, and
//!      no way for a rollup to disagree with the raw rows it was derived from,
//!      because there is no rollup. The whole failure surface is "the file is
//!      missing or a line does not parse", both of which are reported rather
//!      than smoothed over.
//!   2. **A persistent rollup store** (SQLite/redb; the shape COST-06/#4104
//!      specifies) — durable `usage_daily`/`usage_weekly`/`usage_monthly`
//!      tables, a scheduled rollup job, upsert/transaction handling, and the
//!      standing obligation to keep derived totals reconcilable with
//!      `usage_raw` across crashes and concurrent writers.
//!
//! This slice takes (1) deliberately. The epic's "Done when" is about
//! trustworthiness — *"every displayed amount traces to valid raw rows and one
//! pricing table"* — and folding the raw rows on demand satisfies that by
//! construction, where a rollup satisfies it only as long as its refresh job
//! is correct. #4104 remains open and unimplemented; when durable rollups are
//! built, this fold is the reference they must reproduce. **This is a
//! deliberate deviation from COST-06/#4104's acceptance criteria, not an
//! oversight** — see the PR body.
//!
//! Volume assumption, stated so the ceiling is falsifiable rather than
//! implied: a `UsageRecord` line is ~200 bytes, and this reads + parses the
//! whole file per request. At ~10k dispatches (~2 MB) a fold costs single-digit
//! milliseconds; at ~1M dispatches (~200 MB) it is hundreds of milliseconds and
//! the whole file is resident. **Past roughly 100k records — or once the Costs
//! tab polls rather than loads on demand — read-time aggregation stops being
//! adequate and #4104's durable rollup becomes the right shape.**
//! [`CostSummary::records`] is reported in the payload so that threshold can be
//! observed rather than guessed at.
//!
//! That ceiling is about the fold's own cost and assumes the work is NOT on the
//! async reactor. This function is deliberately synchronous and blocking; every
//! caller is responsible for keeping it off a tokio worker thread. The HTTP
//! route does so with `spawn_blocking` (see `api::server::costs::get_costs`).
//! Called inline from an async context instead, the executor — not the file
//! size — becomes the binding constraint, and the ceiling above arrives far
//! sooner under concurrency.
//!
//! What: [`aggregate_usage`] folds the log into a [`CostSummary`] — totals plus
//! three [`CostRow`] breakdowns (by agent, by model, by UTC date) — pricing
//! every row through the single entry point `crate::perf::cost_usd`.
//! [`AggregateError`] separates "no log yet" from "the log could not be read",
//! because a Costs view that renders a confident `$0.00` over a missing file is
//! worse than one that says it has no data.
//! Test: `super::aggregate::tests`.

use std::path::{Path, PathBuf};

use serde::Serialize;

use super::UsageRecord;

/// Filename of the per-dispatch usage log, relative to the state dir.
const USAGE_LOG: &str = "usage.jsonl";

/// Why aggregation could not produce a summary.
///
/// Why: The two failure modes need different answers from the API. A log that
/// does not exist yet is the normal state of a fresh project and must read as
/// "no data recorded", never as "$0.00 spent". A log that exists but cannot be
/// read is a real fault and must surface as one.
/// What: `NotRecorded` carries the path we looked for; `Read` wraps the I/O
/// error alongside it.
/// Test: `aggregate_reports_not_recorded_for_missing_log`.
#[derive(Debug, thiserror::Error)]
pub enum AggregateError {
    /// No usage log exists at this path — nothing has been dispatched yet.
    #[error("no usage log recorded at {}", .path.display())]
    NotRecorded {
        /// Absolute path of the log we looked for.
        path: PathBuf,
    },
    /// The log exists but could not be read.
    #[error("could not read usage log {}: {source}", .path.display())]
    Read {
        /// Absolute path of the log that failed to read.
        path: PathBuf,
        /// Underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
}

/// One aggregate bucket — a total for some grouping key.
///
/// Why: The three breakdowns (agent, model, date) differ only in what `key`
/// means, so they share one row type instead of three near-identical structs
/// the GUI would have to special-case.
/// What: The key plus summed tokens, summed USD cost, dispatch count and
/// summed wall-clock duration. `cost_usd` is the sum of PER-ROW costs, never a
/// re-price of summed tokens: rows may carry different models, and pricing the
/// aggregate would silently apply one model's rate to another's tokens.
/// Test: `aggregate_groups_by_agent_and_model`, `aggregate_prices_per_row`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct CostRow {
    /// Agent name, model id, or `YYYY-MM-DD`, depending on the breakdown.
    pub key: String,
    /// Summed prompt/input tokens.
    pub input_tokens: u64,
    /// Summed completion/output tokens.
    pub output_tokens: u64,
    /// Summed USD cost of the rows in this bucket.
    pub cost_usd: f64,
    /// Number of dispatches folded into this bucket.
    pub dispatch_count: u64,
    /// Summed wall-clock milliseconds.
    pub duration_ms: u64,
}

impl CostRow {
    /// Fold one usage record into this bucket.
    fn add(&mut self, record: &UsageRecord, cost: f64) {
        self.input_tokens += u64::from(record.input_tokens);
        self.output_tokens += u64::from(record.output_tokens);
        self.cost_usd += cost;
        self.dispatch_count += 1;
        self.duration_ms += record.duration_ms;
    }
}

/// The full aggregation result served by `GET /api/costs`.
///
/// Why: One payload carrying totals AND all three breakdowns lets the Costs
/// tab switch its group-by without a refetch, and — more importantly — keeps
/// the three views arithmetically consistent, since they are folds of the same
/// pass over the same rows. It also carries its own provenance: which file was
/// read, how many rows were counted, and how many were skipped.
/// What: `source`/`records`/`malformed_lines`/`first_ts`/`last_ts` describe the
/// data; `totals` and the three `by_*` vectors describe the costs. Breakdowns
/// are sorted by descending cost (by ascending date for `by_date`), so the GUI
/// renders a stable order without sorting.
/// Test: `aggregate_reports_malformed_lines`, `aggregate_sorts_breakdowns`.
#[derive(Debug, Clone, Serialize)]
pub struct CostSummary {
    /// Absolute path of the log that was read.
    pub source: String,
    /// Valid rows folded into the totals.
    pub records: u64,
    /// Lines skipped because they did not parse as a `UsageRecord`.
    ///
    /// Non-zero means the totals below are INCOMPLETE. Reported rather than
    /// swallowed so the GUI can say so instead of presenting a short total as
    /// a whole one.
    pub malformed_lines: u64,
    /// Earliest `ts` among the folded rows, if any.
    pub first_ts: Option<String>,
    /// Latest `ts` among the folded rows, if any.
    pub last_ts: Option<String>,
    /// Window applied to the fold, in days, or `None` for "everything".
    pub window_days: Option<u32>,
    /// Grand totals. `key` is `"total"`.
    pub totals: CostRow,
    /// Per-agent breakdown, descending by cost.
    pub by_agent: Vec<CostRow>,
    /// Per-model breakdown, descending by cost.
    pub by_model: Vec<CostRow>,
    /// Per-UTC-date breakdown, ascending by date.
    pub by_date: Vec<CostRow>,
}

/// Resolve `<project_dir>/.trusty-agents/state/usage.jsonl`.
///
/// Why: The aggregator and `append_usage` must agree on the path or the Costs
/// tab reads an empty file next to a populated one.
/// What: Mirrors `append_usage`'s join, exposed so callers can report the exact
/// path they looked at.
/// Test: `usage_log_path_matches_writer`.
pub fn usage_log_path(project_dir: &Path) -> PathBuf {
    project_dir
        .join(".trusty-agents")
        .join("state")
        .join(USAGE_LOG)
}

/// Fold the project's usage log into a [`CostSummary`].
///
/// Why: (#4098) The single aggregation for every cost surface — the HTTP route
/// and any future CLI report call this rather than each re-deriving totals.
/// What: Reads the log at [`usage_log_path`], parses each non-blank line as a
/// [`UsageRecord`], prices it through `crate::perf::cost_usd`, and folds it
/// into the grand total plus three grouped breakdowns. `window_days`, when set,
/// keeps only rows whose UTC date is within that many days of the newest row in
/// the file (not of wall-clock "now" — a log whose last entry is a week old
/// should still render its last day of activity rather than an empty chart).
///
/// Honesty rules, each of which exists because the alternative silently lies:
///
/// - A missing log is [`AggregateError::NotRecorded`], never an all-zero
///   summary. `$0.00` and "nothing recorded" are different claims.
/// - A line that does not parse is counted in `malformed_lines` and skipped —
///   never defaulted into a zero-cost row that would inflate `dispatch_count`
///   with rows we could not read.
/// - A row whose computed cost is not finite is treated as malformed. Token
///   counts are integers and the rates are finite, so this is unreachable
///   today; it is enforced anyway because the epic's "Done when" requires that
///   NaN/Inf cannot enter a persisted total, and an unchecked `+=` of a NaN
///   would poison every bucket it touches irrecoverably.
/// Test: `aggregate_folds_totals_and_breakdowns`,
/// `aggregate_reports_not_recorded_for_missing_log`,
/// `aggregate_empty_file_is_zero_rows_not_an_error`,
/// `aggregate_reports_malformed_lines`, `aggregate_window_filters_by_date`.
pub fn aggregate_usage(
    project_dir: &Path,
    window_days: Option<u32>,
) -> Result<CostSummary, AggregateError> {
    let path = usage_log_path(project_dir);
    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(AggregateError::NotRecorded { path });
        }
        Err(source) => return Err(AggregateError::Read { path, source }),
    };

    let mut records: Vec<(UsageRecord, f64)> = Vec::new();
    let mut malformed_lines = 0u64;
    for line in contents.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_str::<UsageRecord>(line) else {
            malformed_lines += 1;
            continue;
        };
        // #4098: one pricing table for the whole crate — see perf::pricing.
        // Cache buckets are 0 until #4101 threads cache tokens into the
        // record; they are absent from the log today, not dropped here.
        let cost = crate::perf::cost_usd(
            &record.model,
            u64::from(record.input_tokens),
            u64::from(record.output_tokens),
            0,
            0,
        );
        if !cost.is_finite() {
            malformed_lines += 1;
            continue;
        }
        records.push((record, cost));
    }

    let cutoff = window_days.and_then(|days| window_cutoff(&records, days));
    Ok(fold(&path, records, malformed_lines, window_days, cutoff))
}

/// Compute the inclusive lower-bound date for a `window_days` filter.
///
/// Why: Anchoring the window on the newest RECORDED row rather than on
/// wall-clock "now" means a log that stopped a week ago still renders its final
/// days of activity instead of an honest-but-useless empty window. The Costs
/// tab is a retrospective, not a live meter.
/// What: Takes the max `ts` date present, subtracts `days - 1`, and returns the
/// resulting `YYYY-MM-DD`. `None` when no row carries a parseable date.
/// Test: `aggregate_window_filters_by_date`, `aggregate_window_anchors_on_newest_row`.
fn window_cutoff(records: &[(UsageRecord, f64)], days: u32) -> Option<String> {
    let newest = records
        .iter()
        .filter_map(|(r, _)| parse_date(&r.ts))
        .max()?;
    let span = chrono::Duration::days(i64::from(days.max(1)) - 1);
    Some((newest - span).format("%Y-%m-%d").to_string())
}

/// Parse an RFC3339 `ts` into its UTC calendar date.
///
/// Why: Grouping and windowing are per-day, and the log stores a full
/// timestamp. UTC (not local) so the same log aggregates identically on two
/// machines in different zones — a report that changes when you fly is not
/// reproducible, which the epic's "rollups are reproducible" requires.
/// What: `DateTime::parse_from_rfc3339` → `naive_utc().date()`. `None` for an
/// unparseable stamp, which the caller treats as an ungrouped row.
/// Test: `aggregate_groups_by_date_in_utc`.
fn parse_date(ts: &str) -> Option<chrono::NaiveDate> {
    chrono::DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|dt| dt.naive_utc().date())
}

/// Fold priced records into the summary, applying the date `cutoff`.
///
/// Why: Split from `aggregate_usage` so the I/O and the arithmetic are
/// separately readable, and so the fold stays under the file's share of the
/// 500-SLOC cap.
/// What: One pass building the grand total and three keyed maps, then sorts.
/// Test: covered via `aggregate_usage`'s tests.
fn fold(
    path: &Path,
    records: Vec<(UsageRecord, f64)>,
    malformed_lines: u64,
    window_days: Option<u32>,
    cutoff: Option<String>,
) -> CostSummary {
    let mut totals = CostRow {
        key: "total".to_string(),
        ..CostRow::default()
    };
    let mut by_agent: Vec<CostRow> = Vec::new();
    let mut by_model: Vec<CostRow> = Vec::new();
    let mut by_date: Vec<CostRow> = Vec::new();
    let mut first_ts: Option<String> = None;
    let mut last_ts: Option<String> = None;
    let mut counted = 0u64;

    for (record, cost) in &records {
        let date = parse_date(&record.ts).map(|d| d.format("%Y-%m-%d").to_string());
        if let (Some(min), Some(d)) = (cutoff.as_deref(), date.as_deref())
            && d < min
        {
            continue;
        }
        counted += 1;
        totals.add(record, *cost);
        bucket(&mut by_agent, label(&record.agent, "(unattributed)")).add(record, *cost);
        bucket(&mut by_model, label(&record.model, "(unknown model)")).add(record, *cost);
        bucket(
            &mut by_date,
            date.unwrap_or_else(|| "(undated)".to_string()),
        )
        .add(record, *cost);
        if first_ts.as_deref().is_none_or(|t| record.ts.as_str() < t) {
            first_ts = Some(record.ts.clone());
        }
        if last_ts.as_deref().is_none_or(|t| record.ts.as_str() > t) {
            last_ts = Some(record.ts.clone());
        }
    }

    by_agent.sort_by(cost_desc);
    by_model.sort_by(cost_desc);
    by_date.sort_by(|a, b| a.key.cmp(&b.key));

    CostSummary {
        source: path.display().to_string(),
        records: counted,
        malformed_lines,
        first_ts,
        last_ts,
        window_days,
        totals,
        by_agent,
        by_model,
        by_date,
    }
}

/// Replace an empty grouping key with an explicit placeholder.
///
/// Why: An empty `agent` is missing attribution, and the epic requires missing
/// attribution be EXPLICIT. A blank legend entry reads as a rendering bug; a
/// row labelled `(unattributed)` reads as the fact it is.
fn label(value: &str, placeholder: &str) -> String {
    if value.trim().is_empty() {
        placeholder.to_string()
    } else {
        value.to_string()
    }
}

/// Find-or-append the bucket for `key`.
///
/// Why: A `Vec` scan rather than a `HashMap` — these breakdowns have tens of
/// distinct agents/models/days, so the linear probe is faster than hashing and
/// keeps insertion order available for a deterministic sort tiebreak.
fn bucket(rows: &mut Vec<CostRow>, key: String) -> &mut CostRow {
    if let Some(i) = rows.iter().position(|r| r.key == key) {
        return &mut rows[i];
    }
    rows.push(CostRow {
        key,
        ..CostRow::default()
    });
    let last = rows.len() - 1;
    &mut rows[last]
}

/// Descending by cost, then ascending by key so ties are deterministic.
fn cost_desc(a: &CostRow, b: &CostRow) -> std::cmp::Ordering {
    b.cost_usd
        .partial_cmp(&a.cost_usd)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| a.key.cmp(&b.key))
}

#[cfg(test)]
mod tests;
