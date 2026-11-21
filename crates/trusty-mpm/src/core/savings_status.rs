//! Read side of the tool-output savings ledgers (#11033).
//!
//! Why: the `compress` rows in `usage/savings.jsonl` and the per-call
//! opportunity rows in `usage/opportunity/<day>.jsonl` were write-only. The
//! owner could not see how many tool-output tokens were compressed, how many
//! were not, or why. This module folds both ledgers into one per-day,
//! per-group report for `tm savings status`, and counts `compress` rows per
//! day for the `tm doctor` `compress_rows_per_day` row.
//! What: [`fold_status`] builds a [`StatusReport`]; [`render_text`] prints it;
//! [`compress_rows_per_day`] gives the doctor its series. Read-only. Tokens are
//! bytes / [`BYTES_PER_TOKEN`]. Opportunity rows are a dry run, so their
//! "would save" figure is an upper bound.
//! Test: `savings_status_tests.rs`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::core::savings::{BYTES_PER_TOKEN, TECHNIQUE_COMPRESS, savings_log_in};
use crate::core::savings_opportunity::{OpportunityRow, opportunity_dir_in};

/// Group label for a savings row written before rows carried `tool_type`.
pub const UNATTRIBUTED: &str = "unattributed";

/// How `tm savings status` groups each day's rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupBy {
    /// By `tool_type` (`bash`, `mcp`, `read`, …).
    Tool,
    /// By event class (`SubagentStop`, `Agent PostToolUse`, `other`). Rows
    /// carry no subagent type yet, so this is the closest grouping; each group
    /// also counts its distinct sessions.
    Subagent,
}

/// One group's figures for one day.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct GroupStats {
    /// Opportunity rows seen.
    pub opportunity_rows: u64,
    /// Tool-output tokens seen (opportunity `bytes_returned` / 4).
    pub tokens_seen: u64,
    /// Upper bound on tokens a compressor could have removed (dry run).
    pub would_save_tokens_upper_bound: u64,
    /// `compress` rows on the savings ledger.
    pub compress_rows: u64,
    /// Tokens that went through compression (`tokens_before`).
    pub tokens_compressed: u64,
    /// Tokens compression removed (`tokens_saved`).
    pub tokens_saved: u64,
    /// `tokens_compressed` as a percent of `tokens_seen`; `None` with no output seen.
    pub share_compressed_pct: Option<f64>,
    /// Opportunity rows per skip reason.
    pub skip_reasons: BTreeMap<String, u64>,
    /// Distinct sessions that wrote a row in this group.
    pub sessions: usize,
    #[serde(skip)]
    session_ids: BTreeSet<String>,
    #[serde(skip)]
    bytes_seen: u64,
    #[serde(skip)]
    bytes_would_save: u64,
}

/// One day of the report.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct DayReport {
    /// `YYYY-MM-DD` (UTC).
    pub day: String,
    /// A `<day>.dropped` marker exists: the day hit its size cap.
    pub undercounted: bool,
    /// Rows the opportunity writer dropped that day (the marker's length).
    pub dropped_rows: u64,
    /// Distinct sessions that wrote a `compress` savings row.
    pub compress_sessions: usize,
    /// Distinct sessions in the opportunity ledger.
    pub opportunity_sessions: usize,
    /// Figures per group label.
    pub groups: BTreeMap<String, GroupStats>,
}

/// The whole `tm savings status` report.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StatusReport {
    /// The grouping used.
    pub by: GroupBy,
    /// Newest day first.
    pub days: Vec<DayReport>,
    /// `compress` rows with no `tool_type` (old format) inside the window.
    pub unattributed_rows: u64,
}

/// A savings row read leniently: every field an old row may lack defaults.
#[derive(Debug, Deserialize)]
struct LedgerRow {
    #[serde(default)]
    ts: String,
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    technique: String,
    #[serde(default)]
    tokens_saved: i64,
    #[serde(default)]
    tokens_before: u64,
    #[serde(default)]
    tool_type: Option<String>,
    #[serde(default)]
    tool_name: Option<String>,
}

/// The `days` newest UTC dates ending at `today`, newest first.
fn window(today: NaiveDate, days: u32) -> Vec<String> {
    (0..days.max(1))
        .filter_map(|i| today.checked_sub_days(chrono::Days::new(u64::from(i))))
        .map(|d| d.format("%Y-%m-%d").to_string())
        .collect()
}

/// Event-class label for `--by subagent`.
fn subagent_class(event: Option<&str>, tool_name: Option<&str>) -> &'static str {
    match (event, tool_name) {
        (Some("SubagentStop"), _) | (_, Some("SubagentStop")) => "SubagentStop",
        (_, Some("Agent" | "Task")) => "Agent PostToolUse",
        (None, None) => UNATTRIBUTED,
        _ => "other",
    }
}

fn opportunity_group(by: GroupBy, row: &OpportunityRow) -> String {
    match by {
        GroupBy::Tool => row.tool_type.clone(),
        GroupBy::Subagent => {
            subagent_class(Some(row.event.as_str()), Some(row.tool_name.as_str())).to_string()
        }
    }
}

fn ledger_group(by: GroupBy, row: &LedgerRow) -> String {
    match by {
        GroupBy::Tool => row
            .tool_type
            .clone()
            .unwrap_or_else(|| UNATTRIBUTED.to_string()),
        GroupBy::Subagent => subagent_class(None, row.tool_name.as_deref()).to_string(),
    }
}

/// Fold both ledgers under `root` into a report over `days` days ending `today`.
///
/// Why: one pure-over-the-filesystem entry point the CLI and tests share.
/// What: reads `<root>/usage/savings.jsonl` (`compress` rows only) and
/// `<root>/usage/opportunity/<day>.jsonl` plus `<day>.dropped` for each day in
/// the window. Unparsable lines and marker rows are skipped; a missing file is
/// an empty day. A savings row with no `tool_type` folds into
/// [`UNATTRIBUTED`]; one with no `tokens_before` adds 0 compressed tokens.
/// Test: `fold_reports_seen_compressed_saved_and_skip_split`,
/// `old_format_savings_row_folds_as_unattributed`,
/// `dropped_marker_marks_the_day_undercounted`, `subagent_grouping_uses_event_class`.
pub fn fold_status(root: &Path, by: GroupBy, days: u32, today: NaiveDate) -> StatusReport {
    let mut reports: Vec<DayReport> = window(today, days)
        .into_iter()
        .map(|day| DayReport {
            day,
            ..DayReport::default()
        })
        .collect();
    let index: BTreeMap<String, usize> = reports
        .iter()
        .enumerate()
        .map(|(i, d)| (d.day.clone(), i))
        .collect();
    let mut compress_sessions = vec![BTreeSet::<String>::new(); reports.len()];
    let mut opportunity_sessions = vec![BTreeSet::<String>::new(); reports.len()];
    let mut unattributed_rows = 0;

    let dir = opportunity_dir_in(root);
    for (i, report) in reports.iter_mut().enumerate() {
        if let Ok(meta) = std::fs::metadata(dir.join(format!("{}.dropped", report.day))) {
            report.undercounted = true;
            report.dropped_rows = meta.len();
        }
        let text =
            std::fs::read_to_string(dir.join(format!("{}.jsonl", report.day))).unwrap_or_default();
        for row in text
            .lines()
            .filter_map(|l| serde_json::from_str::<OpportunityRow>(l).ok())
        {
            let g = report
                .groups
                .entry(opportunity_group(by, &row))
                .or_default();
            g.opportunity_rows += 1;
            g.bytes_seen += row.bytes_returned;
            g.bytes_would_save += row.bytes_returned.saturating_sub(row.would_compress_bytes);
            let reason = serde_json::to_value(row.skip_reason)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_else(|| "unknown".to_string());
            *g.skip_reasons.entry(reason).or_default() += 1;
            g.session_ids.insert(row.session_id.clone());
            opportunity_sessions[i].insert(row.session_id);
        }
    }

    let ledger = std::fs::read_to_string(savings_log_in(root)).unwrap_or_default();
    for row in ledger
        .lines()
        .filter_map(|l| serde_json::from_str::<LedgerRow>(l).ok())
        .filter(|r| r.technique == TECHNIQUE_COMPRESS)
    {
        let Some(&i) = row.ts.get(..10).and_then(|d| index.get(d)) else {
            continue;
        };
        if row.tool_type.is_none() {
            unattributed_rows += 1;
        }
        let g = reports[i].groups.entry(ledger_group(by, &row)).or_default();
        g.compress_rows += 1;
        g.tokens_compressed += row.tokens_before;
        g.tokens_saved += u64::try_from(row.tokens_saved).unwrap_or(0);
        g.session_ids.insert(row.session_id.clone());
        compress_sessions[i].insert(row.session_id);
    }

    for (i, report) in reports.iter_mut().enumerate() {
        report.compress_sessions = compress_sessions[i].len();
        report.opportunity_sessions = opportunity_sessions[i].len();
        for g in report.groups.values_mut() {
            g.tokens_seen = to_tokens(g.bytes_seen);
            g.would_save_tokens_upper_bound = to_tokens(g.bytes_would_save);
            g.share_compressed_pct = (g.tokens_seen > 0)
                .then(|| g.tokens_compressed as f64 * 100.0 / g.tokens_seen as f64);
            g.sessions = g.session_ids.len();
        }
    }
    StatusReport {
        by,
        days: reports,
        unattributed_rows,
    }
}

fn to_tokens(bytes: u64) -> u64 {
    (bytes as f64 / BYTES_PER_TOKEN) as u64
}

/// Render the report as plain text for the terminal.
///
/// Why: the owner reads this, so every figure carries its unit and the dry-run
/// column says it is an upper bound.
/// What: one header per day (sessions gap, undercount flag), then one line per
/// group with the five figures and the skip-reason split.
/// Test: `render_labels_upper_bound_and_undercount`.
pub fn render_text(report: &StatusReport) -> String {
    use std::fmt::Write;
    let by = match report.by {
        GroupBy::Tool => "tool",
        GroupBy::Subagent => "subagent (event class; rows carry no subagent type)",
    };
    let mut out = format!(
        "tm savings status — by {by}, {} day(s), tokens = bytes/4\n\
         would-save is an UPPER BOUND (opportunity rows are a dry run)\n",
        report.days.len()
    );
    for day in &report.days {
        let _ = write!(
            out,
            "\n{}  sessions: {} wrote compress rows / {} in opportunity ledger",
            day.day, day.compress_sessions, day.opportunity_sessions
        );
        if day.undercounted {
            let _ = write!(out, "  UNDERCOUNTED ({} rows dropped)", day.dropped_rows);
        }
        out.push('\n');
        if day.groups.is_empty() {
            out.push_str("  (no rows)\n");
            continue;
        }
        let _ = writeln!(
            out,
            "  {:<20} {:>12} {:>12} {:>10} {:>7} {:>18} {:>5}  skip reasons",
            "group", "seen tok", "compressed", "saved", "share", "would-save (upper)", "sess"
        );
        for (name, g) in &day.groups {
            let share = g
                .share_compressed_pct
                .map_or_else(|| "-".to_string(), |p| format!("{p:.1}%"));
            let reasons: Vec<String> = g
                .skip_reasons
                .iter()
                .map(|(r, n)| format!("{r}={n}"))
                .collect();
            let _ = writeln!(
                out,
                "  {:<20} {:>12} {:>12} {:>10} {:>7} {:>18} {:>5}  {}",
                name,
                g.tokens_seen,
                g.tokens_compressed,
                g.tokens_saved,
                share,
                g.would_save_tokens_upper_bound,
                g.sessions,
                reasons.join(" ")
            );
        }
    }
    if report.unattributed_rows > 0 {
        let _ = writeln!(
            out,
            "\n{} compress row(s) predate tool_type and are shown as `{UNATTRIBUTED}`",
            report.unattributed_rows
        );
    }
    out
}

/// `compress` rows per day for the `days` days ending `end`, oldest first.
///
/// Why: the doctor's `compress_rows_per_day` row needs the series to spot a
/// drop, which is how a hook or version gap first shows.
/// What: counts `technique == "compress"` rows by `ts[..10]`. A missing ledger
/// is all zeros; an unreadable one is `Err` with the IO error text.
/// Test: `compress_rows_per_day_counts_only_compress_rows`.
pub fn compress_rows_per_day(
    root: &Path,
    end: NaiveDate,
    days: u32,
) -> Result<Vec<(String, u64)>, String> {
    let text = match std::fs::read_to_string(savings_log_in(root)) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.to_string()),
    };
    let mut counts: BTreeMap<String, u64> = window(end, days).into_iter().map(|d| (d, 0)).collect();
    for row in text
        .lines()
        .filter_map(|l| serde_json::from_str::<LedgerRow>(l).ok())
        .filter(|r| r.technique == TECHNIQUE_COMPRESS)
    {
        if let Some(n) = row.ts.get(..10).and_then(|d| counts.get_mut(d)) {
            *n += 1;
        }
    }
    Ok(counts.into_iter().collect())
}

#[cfg(test)]
#[path = "savings_status_tests.rs"]
mod tests;
