//! Compression-opportunity rows: what each tool result COULD have saved.
//!
//! Why (#10973): the `compress` ledger records only the Bash calls `tm hook`
//! actually rewrote. Subagent results, MCP output, Read/Grep/WebFetch and every
//! uncovered Bash call reach the model untouched and wrote no row at all, so
//! nobody could say how much compression was left on the table — or notice when
//! the rewrite stopped firing. An opportunity row is the denominator: one per
//! tool result, measured by a dry run of the existing compressor.
//! What: [`OpportunityRow`] lines go to their own ledger,
//! `<root>/usage/opportunity/<YYYY-MM-DD>.jsonl`, never to `savings.jsonl`:
//! the statusline reads that file whole on every refresh, and one row per tool
//! call would grow it by tens of megabytes a day (#10981 review). The ledger is
//! bounded on disk: [`append_opportunity`] drops a row once the day's file
//! reaches [`OPPORTUNITY_DAY_CAP_BYTES`] — counting it in `<day>.dropped`, with
//! one `day_full` sentinel line in the day file — and keeps the newest
//! [`OPPORTUNITY_KEEP_DAYS`] day files. [`tool_type_of`] names the category a
//! tool name belongs to; the `compress` producer tags its rows with the same
//! names ([`TaggedSavingsRow`]).
//! Test: `an_opportunity_row_lands_in_its_day_file`,
//! `the_opportunity_ledger_is_bounded_by_day_cap_and_retention`,
//! `tool_type_of_names_each_category`,
//! `a_tagged_compress_row_still_folds_as_a_saving`.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::core::savings::{SavingsRow, write_row_line};

/// `tool_type` of a Bash call, on both `compress` and opportunity rows.
pub const TOOL_TYPE_BASH: &str = "bash";

/// Largest a day's opportunity file may grow before further rows are dropped.
///
/// Why (#10981 review): about 88k tool calls a day at ~250 bytes a row is
/// ~22 MB; 8 MiB keeps the busiest day's file small while a normal day fits.
/// Test: `the_opportunity_ledger_is_bounded_by_day_cap_and_retention`.
pub const OPPORTUNITY_DAY_CAP_BYTES: u64 = 8 * 1024 * 1024;

/// How many day files the opportunity ledger keeps; older ones are deleted.
///
/// Why: two weeks covers the issue's 14-day median comparison. With the day
/// cap, the ledger never holds more than 112 MiB.
/// Test: `the_opportunity_ledger_is_bounded_by_day_cap_and_retention`.
pub const OPPORTUNITY_KEEP_DAYS: usize = 14;

/// One tool result's measured compression opportunity.
///
/// What: `bytes_returned` is the result's full text size; `bytes_scanned` the
/// prefix the dry run read (capped by the caller); `would_compress_bytes` the
/// size the result would have had — the dry run's output for the scanned prefix
/// plus the unscanned remainder at full size, so a capped scan can only
/// understate the opportunity. `event` is the hook event that observed it:
/// a subagent's result is seen both as the `Agent` call's `PostToolUse` and as
/// its own `SubagentStop`, so an aggregate picks one event per question.
/// Test: `an_opportunity_row_lands_in_its_day_file`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OpportunityRow {
    /// RFC 3339 UTC timestamp; its date names the day file.
    pub ts: String,
    /// Claude Code session id from the hook payload; empty when absent.
    pub session_id: String,
    /// `PostToolUse` or `SubagentStop`.
    pub event: String,
    /// Category from [`tool_type_of`].
    pub tool_type: String,
    /// The compressor's dispatch name: the tool name, or for Bash the derived
    /// program and subcommand (`git diff`).
    pub tool_name: String,
    /// Full size of the result text.
    pub bytes_returned: u64,
    /// Bytes the dry run read.
    pub bytes_scanned: u64,
    /// Size the result would have had after compression.
    pub would_compress_bytes: u64,
}

impl OpportunityRow {
    /// Build a row from a measurement, stamped now.
    pub fn new(
        event: &str,
        session_id: &str,
        tool_type: &str,
        tool_name: &str,
        bytes_returned: u64,
        bytes_scanned: u64,
        would_compress_bytes: u64,
    ) -> Self {
        Self {
            ts: crate::core::savings::now_ts(),
            session_id: session_id.to_string(),
            event: event.to_string(),
            tool_type: tool_type.to_string(),
            tool_name: tool_name.to_string(),
            bytes_returned,
            bytes_scanned,
            would_compress_bytes,
        }
    }
}

/// The opportunity ledger directory under a framework root.
///
/// What: `<root>/usage/opportunity`, beside — never inside — `savings.jsonl`.
pub fn opportunity_dir_in(root: &Path) -> PathBuf {
    root.join("usage").join("opportunity")
}

/// Largest a day's drop counter may grow: one byte per dropped row.
///
/// Why (#10981 round 2): the counter must be bounded like the day file it
/// describes; a million drops a day is far past any observed day.
/// Test: `hitting_the_day_cap_writes_one_sentinel_and_counts_the_rest`.
pub const OPPORTUNITY_DROP_COUNTER_CAP_BYTES: u64 = 1024 * 1024;

/// Whether [`append_opportunity`] wrote the row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpportunityAppend {
    /// The row is on disk.
    Written,
    /// The day's file had reached its cap: this is the day's first dropped
    /// row, so a `day_full` sentinel was written and the drop counted.
    DayFullMarked,
    /// The day's file had reached its cap; the row was dropped and counted.
    DayFull,
}

/// The one line a full day file gets when it first drops a row (#10981).
///
/// What: `event` is always `day_full`, which readers counting opportunities
/// skip; `<day>.dropped` beside the day file holds the drop count, one byte a
/// row.
#[derive(Debug, Serialize)]
struct DayFullMarker {
    ts: String,
    event: &'static str,
    day_cap_bytes: u64,
}

/// Append `row` to its day file under `root`, within the ledger's bounds.
///
/// What: [`append_opportunity_bounded`] with [`OPPORTUNITY_DAY_CAP_BYTES`] and
/// [`OPPORTUNITY_KEEP_DAYS`].
/// Test: `an_opportunity_row_lands_in_its_day_file`.
pub fn append_opportunity(root: &Path, row: &OpportunityRow) -> std::io::Result<OpportunityAppend> {
    append_opportunity_bounded(
        &opportunity_dir_in(root),
        row,
        OPPORTUNITY_DAY_CAP_BYTES,
        OPPORTUNITY_KEEP_DAYS,
    )
}

/// [`append_opportunity`] with the bounds supplied.
///
/// Why: the bounds are only testable at a size a test can write.
/// What: the day file is `<dir>/<first 10 chars of row.ts>.jsonl`. A file at or
/// over `day_cap` drops the row and counts it ([`record_drop`]). Creating a new
/// day file first prunes the oldest days until `keep_days - 1` remain, so the
/// new one makes `keep_days` ([`prune_day_files`], best effort). Other files in
/// `dir` are never touched. The row leaves in one write (see
/// [`write_row_line`]).
/// Test: `the_opportunity_ledger_is_bounded_by_day_cap_and_retention`.
pub fn append_opportunity_bounded(
    dir: &Path,
    row: &OpportunityRow,
    day_cap: u64,
    keep_days: usize,
) -> std::io::Result<OpportunityAppend> {
    append_opportunity_bounded_with(dir, row, day_cap, keep_days, &|p| std::fs::remove_file(p))
}

/// [`append_opportunity_bounded`] with the prune's file removal injected.
///
/// Why: a removal that fails or races another process is only reachable in a
/// test through this seam.
fn append_opportunity_bounded_with(
    dir: &Path,
    row: &OpportunityRow,
    day_cap: u64,
    keep_days: usize,
    remove: &dyn Fn(&Path) -> std::io::Result<()>,
) -> std::io::Result<OpportunityAppend> {
    let day = row
        .ts
        .get(..10)
        .filter(|d| is_day(d))
        .unwrap_or("unknown-day");
    let path = dir.join(format!("{day}.jsonl"));
    crate::core::home_write_fence::check(&path); // #8545
    match std::fs::metadata(&path) {
        // #10981 round 2: a dropped row is counted, never silent.
        Ok(meta) if meta.len() >= day_cap => return record_drop(dir, day, &path, day_cap),
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(dir)?;
            // #10981 round 2: best effort — never costs the row.
            prune_day_files(dir, keep_days.saturating_sub(1), remove);
        }
        Err(e) => return Err(e),
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    write_row_line(&mut file, row)?;
    Ok(OpportunityAppend::Written)
}

/// Count one dropped row for `day`; the day's first drop also writes the sentinel.
///
/// Why (#10981 round 2): a silent drop makes a full day read as a quiet one.
/// What: appends one byte to `<dir>/<day>.dropped` (up to
/// [`OPPORTUNITY_DROP_COUNTER_CAP_BYTES`]). `O_APPEND` keeps concurrent hook
/// processes from losing a count. The process that creates the counter — the
/// day's first drop — appends one [`DayFullMarker`] to the day file.
/// Test: `hitting_the_day_cap_writes_one_sentinel_and_counts_the_rest`,
/// `a_full_day_counts_drops_across_hook_processes`.
fn record_drop(
    dir: &Path,
    day: &str,
    day_file: &Path,
    day_cap: u64,
) -> std::io::Result<OpportunityAppend> {
    use std::io::Write;
    let counter_path = dir.join(format!("{day}.dropped"));
    crate::core::home_write_fence::check(&counter_path); // #8545
    let open = |create_new| {
        std::fs::OpenOptions::new()
            .append(true)
            .create_new(create_new)
            .open(&counter_path)
    };
    let (mut counter, first) = match open(true) {
        Ok(file) => (file, true),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (open(false)?, false),
        Err(e) => return Err(e),
    };
    if counter.metadata()?.len() < OPPORTUNITY_DROP_COUNTER_CAP_BYTES {
        counter.write_all(b".")?;
    }
    if !first {
        return Ok(OpportunityAppend::DayFull);
    }
    let marker = DayFullMarker {
        ts: crate::core::savings::now_ts(),
        event: "day_full",
        day_cap_bytes: day_cap,
    };
    let mut day_out = std::fs::OpenOptions::new().append(true).open(day_file)?;
    write_row_line(&mut day_out, &marker)?;
    Ok(OpportunityAppend::DayFullMarked)
}

/// Delete every file of the oldest days in `dir` until at most `keep` days remain.
///
/// Why (#10981 round 2): retention is housekeeping, so it must never cost the
/// row being appended.
/// What: a day is any `YYYY-MM-DD` with a `.jsonl` or `.dropped` file; both
/// leave together. A file already gone (another hook pruned it) is ignored;
/// any other failure, including an unreadable `dir`, is a warning.
/// Test: `a_prune_that_cannot_delete_an_old_day_still_appends`,
/// `a_prune_that_races_a_vanished_file_still_appends`,
/// `drop_counters_are_pruned_with_their_day_files`.
fn prune_day_files(dir: &Path, keep: usize, remove: &dyn Fn(&Path) -> std::io::Result<()>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::warn!(dir = %dir.display(), error = %e, "opportunity ledger: prune skipped");
            return;
        }
    };
    let files: Vec<(String, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_str()?;
            let day = name
                .strip_suffix(".jsonl")
                .or_else(|| name.strip_suffix(".dropped"))?;
            is_day(day).then(|| (day.to_string(), path.clone()))
        })
        .collect();
    let mut days: Vec<&str> = files.iter().map(|(day, _)| day.as_str()).collect();
    days.sort_unstable();
    days.dedup();
    let excess = days.len().saturating_sub(keep);
    let old: std::collections::HashSet<&str> = days[..excess].iter().copied().collect();
    for (day, path) in &files {
        if !old.contains(day.as_str()) {
            continue;
        }
        match remove(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!(
                file = %path.display(),
                error = %e,
                "opportunity ledger: could not prune an old day"
            ),
        }
    }
}

/// Whether `s` is shaped `YYYY-MM-DD`.
fn is_day(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b.iter().enumerate().all(|(i, c)| match i {
            4 | 7 => *c == b'-',
            _ => c.is_ascii_digit(),
        })
}

/// A `compress` savings row tagged with the Bash command it compressed.
///
/// Why (#10973): the per-tool breakdown the issue asks for needs the tool on
/// the row; flattening keeps every existing reader's [`SavingsRow`] parse.
#[derive(Debug, Serialize)]
pub struct TaggedSavingsRow<'a> {
    /// The saving itself, unchanged.
    #[serde(flatten)]
    pub row: &'a SavingsRow,
    /// [`TOOL_TYPE_BASH`] for a `compress` row.
    pub tool_type: &'a str,
    /// The `tm compress --tool` value, e.g. `git diff`.
    pub tool_name: &'a str,
}

/// The category a Claude Code tool name belongs to.
///
/// What: `mcp` for `mcp__<server>__<tool>`; `subagent` for `Agent`/`Task` and
/// for a `SubagentStop`; `bash` for `Bash`; otherwise the name lowercased
/// (`read`, `grep`, `webfetch`, …).
/// Test: `tool_type_of_names_each_category`.
pub fn tool_type_of(tool_name: &str) -> String {
    match tool_name {
        name if name.starts_with("mcp__") => "mcp".to_string(),
        "Agent" | "Task" | "SubagentStop" => "subagent".to_string(),
        "Bash" => TOOL_TYPE_BASH.to_string(),
        name => name.to_ascii_lowercase(),
    }
}

#[cfg(test)]
#[path = "savings_opportunity_tests.rs"]
mod tests;
