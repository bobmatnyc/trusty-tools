//! Latest-run selection over a PR's `statusCheckRollup` (#8638).
//!
//! Why: a workflow that runs twice on one head SHA — a concurrency-cancelled
//! run, then a fresh one — leaves two rollup entries under one name, oldest
//! first. Reading the first match lets the stale run decide: a false BLOCKED
//! on PR #8637, and a fail-open when a stale SUCCESS shadows a later FAILURE.
//! What: [`latest_named`] and [`latest_per_name`] pick, per check name, the
//! entry with the greatest recency key: `completedAt`, else `startedAt`.
//! Every `tm` reader of the rollup goes through these two functions.
//! Test: `rollup_latest_prefers_later_completion`,
//! `rollup_running_entry_newer_than_completion_wins`,
//! `rollup_untimed_entry_sorts_oldest`, `rollup_latest_per_name_keeps_unnamed`,
//! plus `queue_duplicate_*` and `check_condition_duplicate_*`.

use std::collections::HashMap;

use chrono::{DateTime, Datelike as _, Utc};

/// One rollup entry, as the latest-run selection needs to see it.
pub(crate) trait RollupRun {
    /// The check name this entry reports under, or `None` when unnamed.
    fn run_name(&self) -> Option<&str>;
    /// Raw `completedAt`, as `gh` printed it.
    fn completed_at(&self) -> Option<&str>;
    /// Raw `startedAt`, as `gh` printed it.
    fn started_at(&self) -> Option<&str>;
}

/// Parse one rollup timestamp.
///
/// Why: `gh` reports a still-running check's `completedAt` as Go's zero time
/// `0001-01-01T00:00:00Z`. Read literally, that sorts a running entry as the
/// oldest one, and the older completed result would decide.
/// What: RFC 3339 to UTC; the zero time, an empty string and an unparsable
/// value are all `None`.
fn parse_stamp(raw: Option<&str>) -> Option<DateTime<Utc>> {
    let parsed = DateTime::parse_from_rfc3339(raw?.trim()).ok()?;
    let utc = parsed.with_timezone(&Utc);
    (utc.year() > 1).then_some(utc)
}

/// The recency key: `completedAt`, else `startedAt`, else `None` (oldest).
fn recency<T: RollupRun>(entry: &T) -> Option<DateTime<Utc>> {
    parse_stamp(entry.completed_at()).or_else(|| parse_stamp(entry.started_at()))
}

/// The most recent entry named `name`, or `None` when no entry carries it.
///
/// Why: a required context is proven by its latest run, never by whichever
/// run `gh` happened to list first.
/// What: the entry with the greatest [`recency`] key. `None` sorts below
/// every timestamp; a tie goes to the later entry in rollup order, because
/// `max_by_key` returns the last maximum and `gh` lists entries oldest first.
/// Test: `rollup_latest_prefers_later_completion`,
/// `rollup_running_entry_newer_than_completion_wins`,
/// `rollup_untimed_entry_sorts_oldest`.
pub(crate) fn latest_named<'a, T: RollupRun>(entries: &'a [T], name: &str) -> Option<&'a T> {
    entries
        .iter()
        .filter(|e| e.run_name() == Some(name))
        .max_by_key(|e| recency(*e))
}

/// Every entry that is the latest run of its name, in rollup order.
///
/// Why: a reader that judges the whole rollup (`tm wait --for check`) must
/// not count a superseded run as a live failure or a live pending check.
/// What: keeps one entry per name — the one [`latest_named`] would pick — and
/// every unnamed entry untouched, since nothing proves two unnamed entries
/// are runs of the same check.
/// Test: `rollup_latest_per_name_keeps_unnamed`,
/// `check_condition_duplicate_run_uses_latest`.
pub(crate) fn latest_per_name<T: RollupRun>(entries: &[T]) -> Vec<&T> {
    let mut best: HashMap<&str, (usize, Option<DateTime<Utc>>)> = HashMap::new();
    for (i, e) in entries.iter().enumerate() {
        let Some(name) = e.run_name() else { continue };
        let key = recency(e);
        // `>=` so a tie goes to the later entry, matching `latest_named`.
        match best.get(name) {
            Some((_, prev)) if key < *prev => {}
            _ => {
                best.insert(name, (i, key));
            }
        }
    }
    entries
        .iter()
        .enumerate()
        .filter(|(i, e)| {
            e.run_name()
                .is_none_or(|n| best.get(n).map(|b| b.0) == Some(*i))
        })
        .map(|(_, e)| e)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bare rollup entry: name, `completedAt`, `startedAt`.
    struct Run(
        Option<&'static str>,
        Option<&'static str>,
        Option<&'static str>,
    );

    impl RollupRun for Run {
        fn run_name(&self) -> Option<&str> {
            self.0
        }
        fn completed_at(&self) -> Option<&str> {
            self.1
        }
        fn started_at(&self) -> Option<&str> {
            self.2
        }
    }

    const ZERO: &str = "0001-01-01T00:00:00Z";

    #[test]
    fn rollup_latest_prefers_later_completion() {
        let runs = [
            Run(Some("ci"), Some("2026-09-25T22:53:26Z"), None),
            Run(Some("ci"), Some("2026-09-25T22:51:15Z"), None),
        ];
        let got = latest_named(&runs, "ci").expect("named");
        assert_eq!(got.1, Some("2026-09-25T22:53:26Z"));
        assert!(latest_named(&runs, "other").is_none());
    }

    #[test]
    fn rollup_running_entry_newer_than_completion_wins() {
        // The running entry's `completedAt` is Go's zero time, so it must
        // fall back to `startedAt` rather than sort as the oldest.
        let runs = [
            Run(Some("ci"), Some("2026-09-25T22:51:15Z"), None),
            Run(Some("ci"), Some(ZERO), Some("2026-09-25T22:52:00Z")),
        ];
        let got = latest_named(&runs, "ci").expect("named");
        assert_eq!(got.2, Some("2026-09-25T22:52:00Z"));
    }

    #[test]
    fn rollup_untimed_entry_sorts_oldest() {
        let runs = [
            Run(Some("ci"), Some("2026-09-25T22:51:15Z"), None),
            Run(Some("ci"), None, None),
            Run(Some("ci"), Some(ZERO), Some(ZERO)),
        ];
        let got = latest_named(&runs, "ci").expect("named");
        assert_eq!(got.1, Some("2026-09-25T22:51:15Z"));

        // With no timestamps at all, the later entry in rollup order wins.
        let untimed = [
            Run(Some("ci"), None, Some("a")),
            Run(Some("ci"), None, None),
        ];
        let got = latest_named(&untimed, "ci").expect("named");
        assert_eq!(got.2, None);
    }

    #[test]
    fn rollup_latest_per_name_keeps_unnamed() {
        let runs = [
            Run(Some("ci"), Some("2026-09-25T22:51:15Z"), None),
            Run(None, None, None),
            Run(Some("ci"), Some("2026-09-25T22:53:26Z"), None),
            Run(None, None, None),
            Run(Some("lint"), Some("2026-09-25T22:50:00Z"), None),
        ];
        let kept = latest_per_name(&runs);
        assert_eq!(kept.len(), 4, "one `ci`, one `lint`, both unnamed");
        assert!(std::ptr::eq(kept[0], &runs[1]));
        assert!(std::ptr::eq(kept[1], &runs[2]));
        assert!(std::ptr::eq(kept[2], &runs[3]));
        assert!(std::ptr::eq(kept[3], &runs[4]));
    }
}
