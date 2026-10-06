//! Explain each drawer-count drop from the deletion journal (#9283).
//!
//! Why: a drop is only alarming when nothing accounts for it. The journal
//! (`maintenance_deletions.jsonl`) names every drawer a maintenance pass or a
//! user forget removed, so a drop larger than the journaled removals is loss.
//! What: pure functions over the history and a journal loader. For each pair
//! of adjacent known counts in a palace, a [`Window`] counts the distinct
//! drawer ids journaled in `(earlier.at - 60 s, later.at]` plus operator acks
//! for the later day; the drop beyond that is `unexplained`. [`judge`] gives
//! the doctor check's per-palace verdict from the newest window; [`report`]
//! builds the multi-day report.
//! Test: `drawer_counts::tests` — `drawer_counts_doctor_goes_red_on_unjournaled_drop`
//! and the report tests.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, NaiveDate, Utc};
use serde::Serialize;
use trusty_common::memory_core::maintenance_log::MaintenanceDeletion;
use uuid::Uuid;

use super::{CountLine, CountSource, History};
use crate::transport::methods::HEALTH_PROBE_PALACE;

/// Journal records stamped this long before the earlier count still count:
/// a record's `at` is taken just after its delete commits (design §7).
pub const JOURNAL_SLACK: chrono::Duration = chrono::Duration::seconds(60);
/// A newest snapshot older than this is reported stale.
pub const STALE_AFTER: chrono::Duration = chrono::Duration::hours(36);

/// Reads one palace's journal: `Ok(records)` or the reason it could not.
pub type JournalLoader<'a> = dyn Fn(&str) -> Result<Vec<MaintenanceDeletion>, String> + 'a;

/// One pair of adjacent known counts, explained against the journal.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Window {
    /// The later snapshot's UTC day; the window is attributed to it.
    pub day: NaiveDate,
    /// When the earlier count was taken.
    pub from: DateTime<Utc>,
    /// When the later count was taken.
    pub to: DateTime<Utc>,
    /// The earlier count.
    pub before: usize,
    /// The later count.
    pub after: usize,
    /// Distinct journaled drawer ids in the window, by reason.
    pub journaled: BTreeMap<String, usize>,
    /// Drawers operator acks account for.
    pub acked: usize,
    /// Drop beyond the journaled and acked removals.
    pub unexplained: usize,
}

/// Explain the drop from `a` to `b` (both with a known count).
fn window(a: &CountLine, b: &CountLine, journal: &[MaintenanceDeletion], acked: usize) -> Window {
    let (before, after) = (a.drawers.unwrap_or(0), b.drawers.unwrap_or(0));
    let lo = a.at - JOURNAL_SLACK;
    // Dedupe by drawer id so a deletion recorded twice counts once.
    let mut ids: HashMap<Uuid, &'static str> = HashMap::new();
    for r in journal.iter().filter(|r| r.at > lo && r.at <= b.at) {
        ids.entry(r.drawer_id).or_insert(r.reason.as_str());
    }
    let mut journaled = BTreeMap::new();
    for reason in ids.values() {
        *journaled.entry((*reason).to_string()).or_insert(0) += 1;
    }
    let drop = before.saturating_sub(after);
    Window {
        day: b.day(),
        from: a.at,
        to: b.at,
        before,
        after,
        journaled,
        acked,
        unexplained: drop.saturating_sub(ids.len() + acked),
    }
}

/// One palace's lines, split into snapshots (by time) and acks.
struct PalaceLines<'h> {
    snapshots: Vec<&'h CountLine>,
    acks: Vec<&'h CountLine>,
}

impl PalaceLines<'_> {
    fn acked_on(&self, day: NaiveDate) -> usize {
        self.acks
            .iter()
            .filter_map(|l| l.ack.as_ref())
            .filter(|a| a.day == day)
            .map(|a| a.count)
            .sum()
    }

    /// Every adjacent pair of known counts, skipping unavailable lines so a
    /// loss on an unreadable day is caught by the next readable one.
    fn windows(&self, journal: &[MaintenanceDeletion]) -> Vec<Window> {
        let known: Vec<&CountLine> = self
            .snapshots
            .iter()
            .copied()
            .filter(|l| l.drawers.is_some())
            .collect();
        known
            .windows(2)
            .map(|p| window(p[0], p[1], journal, self.acked_on(p[1].day())))
            .collect()
    }
}

/// Group the history by palace, dropping the health-probe palace (design §2 Gap B).
fn by_palace(history: &History) -> BTreeMap<&str, PalaceLines<'_>> {
    let mut out: BTreeMap<&str, PalaceLines<'_>> = BTreeMap::new();
    for line in history
        .lines
        .iter()
        .filter(|l| l.palace != HEALTH_PROBE_PALACE)
    {
        let entry = out.entry(line.palace.as_str()).or_insert(PalaceLines {
            snapshots: Vec::new(),
            acks: Vec::new(),
        });
        if line.src == CountSource::Ack {
            entry.acks.push(line);
        } else {
            entry.snapshots.push(line);
        }
    }
    for p in out.values_mut() {
        p.snapshots.sort_by_key(|l| l.at);
    }
    out.retain(|_, p| !p.snapshots.is_empty());
    out
}

/// The newest snapshot line's time, across every palace.
pub fn newest_snapshot(history: &History) -> Option<DateTime<Utc>> {
    history
        .lines
        .iter()
        .filter(|l| l.src != CountSource::Ack)
        .map(|l| l.at)
        .max()
}

/// The doctor check's verdict for one palace.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// The newest window's drop is fully explained.
    Clean(Window),
    /// The newest window dropped more drawers than were journaled or acked.
    Unexplained(Window),
    /// Fewer than two known counts yet.
    BaselinePending,
    /// The newest count could not be read.
    Unavailable,
    /// The palace is missing from the newest snapshot day.
    Removed { last_day: NaiveDate },
    /// The palace's journal could not be read.
    JournalUnreadable(String),
}

/// Per-palace verdicts from the newest window of each palace.
///
/// Why/What: design §4 — red iff the newest adjacent known pair dropped more
/// than it journaled plus acked; unavailable and baseline-pending are
/// unknown; a palace absent from the newest snapshot day was removed.
/// Test: `drawer_counts_doctor_goes_red_on_unjournaled_drop`,
/// `drawer_counts_doctor_green_when_journaled`,
/// `drawer_counts_ignores_health_probe_palace`.
pub fn judge(history: &History, journal: &JournalLoader<'_>) -> Vec<(String, Verdict)> {
    let newest_day = newest_snapshot(history).map(|t| t.date_naive());
    let mut out = Vec::new();
    for (palace, lines) in by_palace(history) {
        let Some(last) = lines.snapshots.last() else {
            continue;
        };
        let verdict = if newest_day.is_some_and(|d| last.day() < d) {
            Verdict::Removed {
                last_day: last.day(),
            }
        } else if last.drawers.is_none() {
            Verdict::Unavailable
        } else {
            match journal(palace) {
                Err(e) => Verdict::JournalUnreadable(e),
                Ok(records) => match lines.windows(&records).pop() {
                    None => Verdict::BaselinePending,
                    Some(w) if w.unexplained > 0 => Verdict::Unexplained(w),
                    Some(w) => Verdict::Clean(w),
                },
            }
        };
        out.push((palace.to_string(), verdict));
    }
    out
}

/// One palace's rows in the multi-day report.
#[derive(Debug, Clone, Serialize)]
pub struct PalaceReport {
    /// Palace id.
    pub palace: String,
    /// Each day's count in the window (`None` when unavailable).
    pub counts: BTreeMap<NaiveDate, Option<usize>>,
    /// Newest known count in the window minus the count it started from.
    pub net_delta: Option<i64>,
    /// Journaled removals in the window, by reason.
    pub journaled: BTreeMap<String, usize>,
    /// Drawers acknowledged by the operator in the window.
    pub acked: usize,
    /// Unexplained drawers per day (only days with any).
    pub unexplained: BTreeMap<NaiveDate, usize>,
    /// The last day the palace was seen, when it is gone from the newest day.
    pub removed_after: Option<NaiveDate>,
    /// Why the journal could not be read, if it could not.
    pub journal_error: Option<String>,
}

/// One day's roll-up.
#[derive(Debug, Clone, Serialize)]
pub struct DayStatus {
    /// UTC day.
    pub day: NaiveDate,
    /// Whether any snapshot line exists for the day.
    pub snapshot: bool,
    /// Unexplained drawers across every palace.
    pub unexplained: usize,
    /// A snapshot exists and nothing is unexplained or unreadable.
    pub green: bool,
}

/// The `doctor --drawer-report` body.
#[derive(Debug, Clone, Serialize)]
pub struct DrawerReport {
    /// First day of the window.
    pub window_start: NaiveDate,
    /// Last day of the window (today, UTC).
    pub window_end: NaiveDate,
    /// The newest snapshot line, if any.
    pub newest_snapshot: Option<DateTime<Utc>>,
    /// The newest snapshot is older than [`STALE_AFTER`].
    pub stale: bool,
    /// One row per palace seen in the window.
    pub palaces: Vec<PalaceReport>,
    /// One row per day of the window, oldest first.
    pub days: Vec<DayStatus>,
    /// Days that are green.
    pub green_days: usize,
    /// `"<green>/<days> clean"` — the release signal.
    pub clean: String,
    /// Unexplained drawers across the window.
    pub unexplained_total: usize,
}

impl DrawerReport {
    /// Whether the report must exit non-zero: any unexplained drop, or any
    /// journal it could not read.
    pub fn failed(&self) -> bool {
        self.unexplained_total > 0 || self.palaces.iter().any(|p| p.journal_error.is_some())
    }
}

/// Build the report for the `days` UTC days ending on `now`'s day.
///
/// Why/What: design §5 — per palace, the daily counts, net delta, journaled
/// removals by reason and unexplained drawers per day; per day, whether it is
/// green; the footer's `k/N clean` is the 1.0 release signal.
/// Test: `report_lists_seven_days_and_exits_one_on_unexplained`.
pub fn report(
    history: &History,
    journal: &JournalLoader<'_>,
    now: DateTime<Utc>,
    days: u32,
) -> DrawerReport {
    let days = days.max(1);
    let end = now.date_naive();
    let start = end - chrono::Duration::days(i64::from(days) - 1);
    let in_range = |d: NaiveDate| d >= start && d <= end;
    let newest = newest_snapshot(history);
    let newest_day = newest.map(|t| t.date_naive());
    let mut palaces = Vec::new();
    let mut unexplained_by_day: BTreeMap<NaiveDate, usize> = BTreeMap::new();
    let mut error_days: Vec<NaiveDate> = Vec::new();
    for (palace, lines) in by_palace(history) {
        let mut row = PalaceReport {
            palace: palace.to_string(),
            counts: BTreeMap::new(),
            net_delta: None,
            journaled: BTreeMap::new(),
            acked: 0,
            unexplained: BTreeMap::new(),
            removed_after: None,
            journal_error: None,
        };
        for l in lines.snapshots.iter().filter(|l| in_range(l.day())) {
            row.counts.insert(l.day(), l.drawers);
        }
        let last_day = lines.snapshots.last().map(|l| l.day());
        row.removed_after = match (last_day, newest_day) {
            (Some(last), Some(newest))
                if last < newest && last >= start - chrono::Duration::days(1) =>
            {
                Some(last)
            }
            _ => None,
        };
        let windows: Vec<Window> = match journal(palace) {
            Ok(records) => lines.windows(&records),
            Err(e) => {
                error_days.extend(row.counts.keys().copied());
                row.journal_error = Some(e);
                Vec::new()
            }
        };
        let in_window: Vec<&Window> = windows.iter().filter(|w| in_range(w.day)).collect();
        if row.counts.is_empty() && in_window.is_empty() && row.removed_after.is_none() {
            continue;
        }
        let known_before = lines
            .snapshots
            .iter()
            .filter(|l| l.day() < start)
            .filter_map(|l| l.drawers)
            .next_back();
        let known_in: Vec<usize> = row.counts.values().filter_map(|c| *c).collect();
        if let (Some(base), Some(last)) =
            (known_before.or(known_in.first().copied()), known_in.last())
        {
            row.net_delta = Some(*last as i64 - base as i64);
        }
        for w in in_window {
            for (reason, n) in &w.journaled {
                *row.journaled.entry(reason.clone()).or_insert(0) += n;
            }
            row.acked += w.acked;
            if w.unexplained > 0 {
                *row.unexplained.entry(w.day).or_insert(0) += w.unexplained;
                *unexplained_by_day.entry(w.day).or_insert(0) += w.unexplained;
            }
        }
        palaces.push(row);
    }
    let snapshot_days: Vec<NaiveDate> = history
        .lines
        .iter()
        .filter(|l| l.src != CountSource::Ack)
        .map(CountLine::day)
        .collect();
    let day_rows: Vec<DayStatus> = (0..days)
        .map(|i| start + chrono::Duration::days(i64::from(i)))
        .map(|day| {
            let snapshot = snapshot_days.contains(&day);
            let unexplained = unexplained_by_day.get(&day).copied().unwrap_or(0);
            DayStatus {
                day,
                snapshot,
                unexplained,
                green: snapshot && unexplained == 0 && !error_days.contains(&day),
            }
        })
        .collect();
    let green_days = day_rows.iter().filter(|d| d.green).count();
    DrawerReport {
        window_start: start,
        window_end: end,
        newest_snapshot: newest,
        stale: newest.is_none_or(|t| now - t > STALE_AFTER),
        palaces,
        green_days,
        clean: format!("{green_days}/{days} clean"),
        unexplained_total: unexplained_by_day.values().sum(),
        days: day_rows,
    }
}
