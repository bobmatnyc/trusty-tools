//! The parity report: shadow records against the live poller's output.
//!
//! Why: the Architect decides the cutover from numbers. The report answers
//! two questions over a time range: would the scheduler have raised the same
//! alerts, and would it have woken the Architect in the same cycles?
//! What: [`compute`] joins each ok [`ShadowRecord`] to the live side by time
//! window `[slot, slot + interval)`. Alerts compare as the set of
//! `(session, kind)`: in both is `matched`, only in the record is
//! `scheduler_only`, only in `inbox/alerts.jsonl` is `poller_only`. Wakes
//! compare per record: the shadow would wake (`dry-run: would send`) against a
//! live `poll.log` cycle line in the window with `wake=sent`. [`load`] reads
//! the three files from a [`Layout`] since a time; [`render`] prints counts.
//! Rows with no timestamp or an unparsable one are counted in `unread`, never
//! guessed. Expect noise (R4): the live poller dedups against state the dry
//! run only copies once per cycle.
//! Test: `parity_joins_matched_scheduler_only_and_poller_only`,
//! `parity_counts_wakes_per_window`, `parity_ignores_failed_shadow_records`,
//! `parity_load_reads_the_three_files`.

use std::collections::HashSet;

use serde_json::Value;

use super::shadow::{ShadowAlert, ShadowRecord};
use super::slot::parse_iso;
use super::{Layout, SchedError};

/// Matched / scheduler-only / poller-only.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    /// In both.
    pub matched: u64,
    /// Only the scheduler's shadow.
    pub scheduler_only: u64,
    /// Only the live poller.
    pub poller_only: u64,
}

/// The report.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// Shadow records joined.
    pub cycles: u64,
    /// Shadow records that failed and were left out.
    pub failed_cycles: u64,
    /// Alert counts.
    pub alerts: Counts,
    /// Wake counts, one per joined record.
    pub wakes: Counts,
    /// Live rows without a readable timestamp.
    pub unread: u64,
}

/// One live `alerts.jsonl` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveAlert {
    /// Unix seconds.
    pub ts: i64,
    /// The key.
    pub key: ShadowAlert,
}

/// Joins `records` to the live rows; `live_wakes` are the times of
/// `poll.log` cycle lines that sent a wake.
pub fn compute(records: &[ShadowRecord], live: &[LiveAlert], live_wakes: &[i64]) -> Report {
    let mut rep = Report::default();
    for r in records {
        if !r.ok {
            rep.failed_cycles += 1;
            continue;
        }
        rep.cycles += 1;
        let (from, to) = (r.slot, r.slot + r.interval_s as i64);
        let theirs: HashSet<&ShadowAlert> = live
            .iter()
            .filter(|a| (from..to).contains(&a.ts))
            .map(|a| &a.key)
            .collect();
        let ours: HashSet<&ShadowAlert> = r.alerts.iter().collect();
        let both = ours.intersection(&theirs).count() as u64;
        rep.alerts.matched += both;
        rep.alerts.scheduler_only += ours.len() as u64 - both;
        rep.alerts.poller_only += theirs.len() as u64 - both;
        let ours_wake = r
            .wake_phrase
            .as_deref()
            .is_some_and(|w| w.starts_with("dry-run: would send"));
        let theirs_wake = live_wakes.iter().any(|t| (from..to).contains(t));
        match (ours_wake, theirs_wake) {
            (true, true) => rep.wakes.matched += 1,
            (true, false) => rep.wakes.scheduler_only += 1,
            (false, true) => rep.wakes.poller_only += 1,
            (false, false) => {}
        }
    }
    rep
}

/// Reads the shadow record, `alerts.jsonl` and `poll.log` for rows at or after
/// `since` (Unix seconds) and joins them.
///
/// # Errors
/// [`SchedError::Io`] when the shadow record exists but cannot be read;
/// [`SchedError::Parse`] when one of its lines is not a record. Missing
/// live files read as empty (the report then shows poller-only zero).
pub fn load(layout: &Layout, since: i64) -> Result<Report, SchedError> {
    let shadow_path = layout.shadow_log();
    let text = std::fs::read_to_string(&shadow_path)
        .map_err(|e| SchedError::io(format!("read {}", shadow_path.display()), e))?;
    let mut records = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let r: ShadowRecord = serde_json::from_str(line).map_err(|e| SchedError::Parse {
            what: shadow_path.display().to_string(),
            detail: e.to_string(),
        })?;
        if r.slot >= since {
            records.push(r);
        }
    }
    let mut unread = 0;
    let mut live = Vec::new();
    for line in read_lines(&layout.alerts()) {
        let row: Option<Value> = serde_json::from_str(&line).ok();
        let key = row.as_ref().and_then(|v| {
            Some(LiveAlert {
                ts: parse_iso(v["ts"].as_str()?)?,
                key: ShadowAlert {
                    session: v["session"].as_str()?.to_owned(),
                    kind: v["kind"].as_str()?.to_owned(),
                },
            })
        });
        match key {
            Some(a) if a.ts >= since => live.push(a),
            Some(_) => {}
            None => unread += 1,
        }
    }
    let mut wakes = Vec::new();
    for line in read_lines(&layout.poll_log()) {
        if !(line.contains(" panes=") && line.contains(" wake=sent")) {
            continue;
        }
        match line.split(' ').next().and_then(parse_iso) {
            Some(t) if t >= since => wakes.push(t),
            Some(_) => {}
            None => unread += 1,
        }
    }
    let mut rep = compute(&records, &live, &wakes);
    rep.unread = unread;
    Ok(rep)
}

fn read_lines(path: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .map(|t| t.lines().map(str::to_owned).collect())
        .unwrap_or_default()
}

/// The report as text, one line per count.
pub fn render(r: &Report) -> String {
    format!(
        "cycles joined: {} (failed shadow runs left out: {})\n\
         alerts: matched {} / scheduler-only {} / poller-only {}\n\
         wakes: matched {} / scheduler-only {} / poller-only {}\n\
         live rows unread: {}\n",
        r.cycles,
        r.failed_cycles,
        r.alerts.matched,
        r.alerts.scheduler_only,
        r.alerts.poller_only,
        r.wakes.matched,
        r.wakes.scheduler_only,
        r.wakes.poller_only,
        r.unread,
    )
}

impl Report {
    /// Alerts and wakes matched, as a percentage of every compared item
    /// (matched plus either side's extras); 100 when nothing was compared.
    pub fn match_pct(&self) -> f64 {
        let total: u64 = [self.alerts, self.wakes]
            .iter()
            .map(|c| c.matched + c.scheduler_only + c.poller_only)
            .sum();
        let matched = self.alerts.matched + self.wakes.matched;
        if total == 0 {
            100.0
        } else {
            matched as f64 * 100.0 / total as f64
        }
    }
}
