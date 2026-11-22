//! The parity report: shadow records against the live poller's output.
//!
//! Why: the Architect decides the cutover from numbers. The report answers
//! two questions over a time range: would the scheduler have raised the same
//! alerts, and would it have woken the Architect in the same cycles?
//! What: [`compute`] joins each ok [`ShadowRecord`] to the live side by time
//! window `[slot, slot + interval)`. Alerts compare as the set of
//! `(session, kind)`: in both is `matched`, only in the record is
//! `scheduler_only`, only in `inbox/alerts.jsonl` is `poller_only`. Wakes
//! compare per record: the shadow's triage woke by a rule (`woken` above
//! `haiku_woken`) against a live rule wake in the window, read from
//! `inbox/triage.jsonl` (`decision` wake, `path` not `haiku`). Haiku-path
//! wakes on either side only count in `haiku_excluded`: the port makes no
//! Haiku call. With no triage record the live side is the `poll.log` cycle
//! lines with `woken=N`, N > 0 (or `wake=sent` before that field). The wake phrase is not
//! the decision: a dry run never sends, so its carried `wake_pending` makes
//! every later cycle print `would send` (MB1595: 102 of 116 cycles).
//! [`load`] reads the three files from a [`Layout`] since a time; [`render`]
//! prints the counts, the alert and wake match percent and a PASS/FAIL
//! [`Report::passes`] verdict against [`Thresholds`] (default 120 cycles and
//! 95% for alerts and for wakes).
//! Rows with no timestamp or an unparsable one are counted in `unread`, never
//! guessed. Expect noise (R4): the live poller dedups against state the dry
//! run only copies once per cycle.
//! Test: `parity_joins_matched_scheduler_only_and_poller_only`,
//! `parity_counts_wakes_per_window`, `parity_ignores_failed_shadow_records`,
//! `parity_counts_only_cycles_whose_triage_woke`,
//! `parity_render_states_percentages_and_a_verdict`,
//! `parity_load_reads_the_three_files`.

use std::collections::HashSet;

use serde_json::Value;

use super::cutover::Thresholds;
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
    /// Wakes no triage rule decided (the Haiku path), on either side, left
    /// out of the wake counts and the verdict (MB1595).
    pub haiku_excluded: u64,
}

/// One live `alerts.jsonl` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveAlert {
    /// Unix seconds.
    pub ts: i64,
    /// The key.
    pub key: ShadowAlert,
}

/// Joins `records` to the live rows; `live_wakes` are the times of live
/// rule-decided wakes.
pub fn compute(records: &[ShadowRecord], live: &[LiveAlert], live_wakes: &[i64]) -> Report {
    compute_with(records, live, live_wakes, &[])
}

/// [`compute`], with `live_haiku` the times of live Haiku-path wakes: they and
/// the shadow's `haiku_woken` count only in [`Report::haiku_excluded`].
pub fn compute_with(
    records: &[ShadowRecord],
    live: &[LiveAlert],
    live_wakes: &[i64],
    live_haiku: &[i64],
) -> Report {
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
        // See MB1595: the cycle's own triage decision, not the carried backlog.
        // See MB1595: Haiku-path wakes are no evidence either way.
        let haiku = r.haiku_woken.unwrap_or(0);
        let ours_wake = r.woken.map_or_else(
            || {
                r.wake_phrase
                    .as_deref()
                    .is_some_and(|w| w.starts_with("dry-run: would send"))
            },
            |w| w > haiku,
        );
        let inside = |t: &&i64| (from..to).contains(*t);
        rep.haiku_excluded += haiku + live_haiku.iter().filter(inside).count() as u64;
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
    let (mut wakes, mut haiku) = (Vec::new(), Vec::new());
    let triage = layout.root.join("inbox/triage.jsonl");
    for line in read_lines(&triage) {
        // A wake row: `"decision": "wake"`; its `path` says rule or Haiku.
        if !line.contains("\"decision\": \"wake\"") {
            continue;
        }
        let row: Option<Value> = serde_json::from_str(&line).ok();
        let at = row
            .as_ref()
            .and_then(|v| Some((parse_iso(v["ts"].as_str()?)?, v)));
        match at {
            Some((t, v)) if t >= since => {
                if v["path"].as_str() == Some("haiku") {
                    haiku.push(t);
                } else {
                    wakes.push(t);
                }
            }
            Some(_) => {}
            None => unread += 1,
        }
    }
    // No triage record: the poll.log cycle lines, with no Haiku split.
    let lines = if wakes.is_empty() && haiku.is_empty() {
        read_lines(&layout.poll_log())
    } else {
        Vec::new()
    };
    for line in lines {
        if !(line.contains(" panes=") && live_woke(&line)) {
            continue;
        }
        match line.split(' ').next().and_then(parse_iso) {
            Some(t) if t >= since => wakes.push(t),
            Some(_) => {}
            None => unread += 1,
        }
    }
    let mut rep = compute_with(&records, &live, &wakes, &haiku);
    rep.unread = unread;
    Ok(rep)
}

/// A live cycle line whose triage woke: `woken=N` with N > 0, or, on a line
/// from before the `woken=` field, `wake=sent`.
fn live_woke(line: &str) -> bool {
    match line.split(' ').find_map(|f| f.strip_prefix("woken=")) {
        Some(n) => n.parse::<u64>().is_ok_and(|n| n > 0),
        None => line.contains(" wake=sent"),
    }
}

fn read_lines(path: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .map(|t| t.lines().map(str::to_owned).collect())
        .unwrap_or_default()
}

/// The report as text against the default [`Thresholds`].
pub fn render(r: &Report) -> String {
    render_with(r, &Thresholds::default())
}

/// The report as text: one line per count, the match percentages and the
/// verdict against `th`.
pub fn render_with(r: &Report, th: &Thresholds) -> String {
    let verdict = if r.passes(th) { "PASS" } else { "FAIL" };
    let counts = format!(
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
    );
    format!(
        "{counts}haiku-path wakes excluded: {}\nalert match: {:.1}%\nwake match: {:.1}%\n\
         verdict: {verdict} (needs at least {} cycles and {:.1}% for alerts and for wakes)\n",
        r.haiku_excluded,
        r.alert_pct(),
        r.wake_pct(),
        th.min_cycles,
        th.min_match_pct,
    )
}

/// `matched` as a percent of every compared item; 100 when none was compared.
fn pct(c: &Counts) -> f64 {
    let total = c.matched + c.scheduler_only + c.poller_only;
    if total == 0 {
        100.0
    } else {
        c.matched as f64 * 100.0 / total as f64
    }
}

impl Report {
    /// Alerts matched, percent of compared alerts.
    pub fn alert_pct(&self) -> f64 {
        pct(&self.alerts)
    }

    /// Wakes matched, percent of compared wakes.
    pub fn wake_pct(&self) -> f64 {
        pct(&self.wakes)
    }

    /// PASS: at least `th.min_cycles` joined cycles, and both the alert and
    /// the wake match at least `th.min_match_pct`.
    pub fn passes(&self, th: &Thresholds) -> bool {
        self.cycles >= th.min_cycles
            && self.alert_pct() >= th.min_match_pct
            && self.wake_pct() >= th.min_match_pct
    }

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
