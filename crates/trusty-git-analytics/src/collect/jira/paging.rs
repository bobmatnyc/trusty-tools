//! Keyset pagination state machine for the JIRA changelog walk
//! (issue #3966, PR #4067 review round 1).
//!
//! Why offset pagination is wrong here: the changelog walk runs
//! `... ORDER BY updated ASC` and pages it with `startAt`. That paginates a
//! *live-mutating* dataset on the very field it sorts by. If any ticket
//! already behind the read boundary is edited mid-walk, it re-sorts to the
//! end of the result set and every ticket after it shifts down one index —
//! so the next `startAt` boundary steps over exactly one ticket that was
//! never read. Its `updated` is below the run's maximum, so the advanced
//! cursor excludes it from every later incremental run. A 10,000-ticket
//! backfill is 200 sequential round-trips against an active project;
//! concurrent edits inside that window are expected, not exceptional.
//!
//! What this does instead: re-anchor the query window after every page.
//! Each page is requested as `updated >= <last page's max updated>` with a
//! small intra-minute offset, so tickets in minutes we have already walked
//! past cannot shift our boundary at all — they simply leave a window we no
//! longer read from and reappear later in the ordering, where we read them
//! again.
//!
//! Why an offset survives at all: JQL date literals are minute-resolution
//! (`"yyyy-MM-dd HH:mm"`, see [`super::sync::jql_date`](crate::collect::jira::jql_time::jql_date)), and JQL offers no
//! tiebreak key, so a pure keyset walk cannot make progress through a minute
//! containing more tickets than one page. Inside a single minute the pager
//! therefore falls back to offset paging — bounding the residual race to
//! "one minute of tickets, edited in the seconds between two requests"
//! rather than "the entire remaining walk".
//!
//! Re-anchoring re-reads the boundary minute, so the caller must deduplicate;
//! [`KeysetPager::record_page`] reports which items in a page are new.
//!
//! # Why the residual is reported, not clamped away
//!
//! Inside an offset-paged minute a concurrent edit can still drop exactly one
//! ticket, and that loss is permanent. The obvious remedy — hold the run's
//! cursor at the start of such a minute so the next run re-reads it — is
//! **not** safe: a minute containing more tickets than one page is precisely
//! the case that triggers the offset branch, so every subsequent run would
//! offset-page it again, clamp again, and never advance. That converts a
//! rare, bounded loss into a guaranteed permanent stall with no forward
//! progress at all.
//!
//! So the pager records the minute ([`KeysetPager::offset_paged_minute`]) and
//! `run_sync` surfaces it in the run summary, letting an operator re-cover
//! that window explicitly with `--since`. A bounded, *visible* residual beats
//! an unbounded stall.

use std::collections::HashSet;

use chrono::{DateTime, Timelike, Utc};
use tracing::warn;

/// The minimum a page needs to expose to the pager: the issue key (for
/// deduplication across re-read boundaries) and its `updated` timestamp
/// (the sort/keyset key).
pub type PagedItem = (String, Option<DateTime<Utc>>);

/// Where the next page should be requested from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageRequest {
    /// Lower bound for the JQL `updated >=` clause. `None` = no bound
    /// (first page of a full backfill).
    pub since: Option<DateTime<Utc>>,
    /// Offset within that window, used only to walk through a minute
    /// containing more tickets than one page.
    pub start_at: u64,
}

/// Result of feeding one fetched page back to the pager.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageStep {
    /// Parallel to the recorded items: `true` where the item had not been
    /// returned by an earlier page of this walk.
    pub is_new: Vec<bool>,
    /// Whether another page should be requested.
    pub more: bool,
}

/// Window-re-anchoring pager over a `ORDER BY updated ASC` JQL result set.
#[derive(Debug)]
pub struct KeysetPager {
    since: Option<DateTime<Utc>>,
    start_at: u64,
    seen: HashSet<String>,
    pages: usize,
    max_pages: usize,
    offset_paged_minute: Option<DateTime<Utc>>,
}

impl KeysetPager {
    /// Start a walk at `since` (the sync scope's lower bound), refusing to
    /// issue more than `max_pages` requests.
    ///
    /// The page cap is a defensive stop against a server that never reports
    /// a short page. Hitting it truncates the walk, which is safe: the
    /// caller's cursor only advances over tickets it actually ingested, so a
    /// truncated walk resumes exactly where it stopped (the same property
    /// `--max-tickets` relies on).
    pub fn new(since: Option<DateTime<Utc>>, max_pages: usize) -> Self {
        Self {
            since,
            start_at: 0,
            seen: HashSet::new(),
            pages: 0,
            max_pages: max_pages.max(1),
            offset_paged_minute: None,
        }
    }

    /// The window/offset the next request should use.
    pub fn request(&self) -> PageRequest {
        PageRequest {
            since: self.since,
            start_at: self.start_at,
        }
    }

    /// The earliest minute the walk had to traverse by offset rather than by
    /// window re-anchoring, if any.
    ///
    /// Why a caller wants this: inside such a minute the walk is exposed to
    /// the same shift-under-pagination race that keyset paging exists to
    /// eliminate, so a concurrent edit there can drop one ticket. The loss is
    /// bounded (one ticket per concurrent edit, within one minute) but it is
    /// not self-healing, so `run_sync` reports it rather than letting it pass
    /// unremarked. See the module header for why the window cannot simply be
    /// held back to this minute instead.
    ///
    /// Test: `pager_reports_the_minute_it_offset_paged`.
    pub fn offset_paged_minute(&self) -> Option<DateTime<Utc>> {
        self.offset_paged_minute
    }

    /// Record one fetched page and advance the window.
    ///
    /// `page_size` is the `maxResults` that was requested; a page shorter
    /// than that means the server has nothing further to give.
    ///
    /// # Invariant
    ///
    /// Every call strictly advances the read position — either `since` moves
    /// to a later minute (with `start_at` reset past the items already read
    /// in that minute) or `start_at` grows — so the walk always terminates.
    ///
    /// Test: `pager_reanchors_window_on_a_later_minute`,
    /// `pager_falls_back_to_offset_within_one_minute`,
    /// `pager_deduplicates_reread_boundary_items`,
    /// `pager_stops_on_a_short_page`, `pager_stops_at_max_pages`.
    pub fn record_page(&mut self, items: &[PagedItem], page_size: usize) -> PageStep {
        self.pages += 1;

        let mut is_new = Vec::with_capacity(items.len());
        let mut page_max: Option<DateTime<Utc>> = None;
        for (key, updated) in items {
            is_new.push(self.seen.insert(key.clone()));
            if let Some(u) = updated {
                page_max = Some(page_max.map_or(*u, |m: DateTime<Utc>| m.max(*u)));
            }
        }

        if items.len() < page_size {
            return PageStep {
                is_new,
                more: false,
            };
        }
        if self.pages >= self.max_pages {
            warn!(
                pages = self.pages,
                "JIRA changelog walk hit its page cap; truncating this run \
                 (the sync cursor only advances over ingested tickets, so the \
                 next run resumes here)"
            );
            return PageStep {
                is_new,
                more: false,
            };
        }

        match page_max {
            // The page reached a later minute than the current window start:
            // re-anchor there, skipping past the items of that minute we
            // already hold. The skip count can only ever be an UNDER-estimate
            // (earlier pages may also hold items in that minute), which costs
            // a deduplicated re-read — never a skipped ticket.
            Some(max) if minute(Some(max)) > minute(self.since) => {
                let skip = items
                    .iter()
                    .filter(|(_, u)| minute(*u) == minute(Some(max)))
                    .count();
                self.since = Some(max);
                self.start_at = skip as u64;
            }
            // A full page that did not advance the minute (a bulk edit, or a
            // page with no usable timestamps): keep offset-paging inside the
            // current window so the walk still makes progress. Record it —
            // this is the one band where the shift race survives, and the
            // caller surfaces it rather than letting it pass silently.
            _ => {
                let stalled = minute(self.since).or_else(|| minute(page_max));
                self.offset_paged_minute = match (self.offset_paged_minute, stalled) {
                    (Some(existing), Some(new)) => Some(existing.min(new)),
                    (existing, new) => existing.or(new),
                };
                self.start_at += items.len() as u64;
            }
        }

        PageStep { is_new, more: true }
    }
}

/// Truncate to minute resolution — the granularity JQL date literals can
/// actually express. `None` sorts below every timestamp so an unbounded
/// (full-backfill) first window always re-anchors on the first page.
fn minute(dt: Option<DateTime<Utc>>) -> Option<DateTime<Utc>> {
    dt.map(|d| {
        d.with_second(0)
            .and_then(|d| d.with_nanosecond(0))
            .unwrap_or(d)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s)
            .expect("valid rfc3339 fixture")
            .with_timezone(&Utc)
    }

    fn item(key: &str, updated: &str) -> PagedItem {
        (key.to_string(), Some(dt(updated)))
    }

    #[test]
    fn pager_starts_at_the_scope_lower_bound() {
        let pager = KeysetPager::new(Some(dt("2026-01-01T00:00:00Z")), 10);
        assert_eq!(
            pager.request(),
            PageRequest {
                since: Some(dt("2026-01-01T00:00:00Z")),
                start_at: 0,
            }
        );
    }

    #[test]
    fn pager_stops_on_a_short_page() {
        let mut pager = KeysetPager::new(None, 10);
        let step = pager.record_page(&[item("P-1", "2026-01-01T00:00:00Z")], 2);
        assert!(!step.more, "a page shorter than page_size ends the walk");
        assert_eq!(step.is_new, vec![true]);
    }

    /// The core fix: after a full page the window re-anchors to the page's
    /// max `updated`, so already-walked minutes can no longer shift the read
    /// boundary when a ticket in them is edited mid-walk.
    #[test]
    fn pager_reanchors_window_on_a_later_minute() {
        let mut pager = KeysetPager::new(Some(dt("2026-01-01T00:00:00Z")), 10);
        let page = vec![
            item("P-1", "2026-01-01T00:01:00Z"),
            item("P-2", "2026-01-01T00:02:00Z"),
        ];
        let step = pager.record_page(&page, 2);
        assert!(step.more);
        assert_eq!(
            pager.request(),
            PageRequest {
                since: Some(dt("2026-01-01T00:02:00Z")),
                // exactly one item of this page lives in the 00:02 minute
                start_at: 1,
            },
            "the next window must start at the page's max updated, not at an \
             absolute offset into a mutating result set"
        );
    }

    /// More tickets in one minute than fit in a page: JQL cannot express a
    /// sub-minute bound, so the pager must offset-page through that minute
    /// instead of stalling (which would silently truncate the walk).
    #[test]
    fn pager_falls_back_to_offset_within_one_minute() {
        let mut pager = KeysetPager::new(Some(dt("2026-01-01T00:00:00Z")), 10);
        let page = vec![
            item("P-1", "2026-01-01T00:00:10Z"),
            item("P-2", "2026-01-01T00:00:20Z"),
        ];
        let step = pager.record_page(&page, 2);
        assert!(step.more);
        assert_eq!(
            pager.request(),
            PageRequest {
                since: Some(dt("2026-01-01T00:00:00Z")),
                start_at: 2,
            },
            "a page confined to the window's own minute must advance by offset"
        );
    }

    /// Re-anchoring deliberately re-reads the boundary minute; the caller
    /// must be told which items it has already seen.
    #[test]
    fn pager_deduplicates_reread_boundary_items() {
        let mut pager = KeysetPager::new(None, 10);
        let first = vec![
            item("P-1", "2026-01-01T00:01:00Z"),
            item("P-2", "2026-01-01T00:02:00Z"),
        ];
        pager.record_page(&first, 2);

        let second = vec![
            // P-2 comes back because the window re-anchored onto its minute.
            item("P-2", "2026-01-01T00:02:00Z"),
            item("P-3", "2026-01-01T00:03:00Z"),
        ];
        let step = pager.record_page(&second, 2);
        assert_eq!(step.is_new, vec![false, true]);
    }

    #[test]
    fn pager_stops_at_max_pages() {
        let mut pager = KeysetPager::new(None, 2);
        let page = vec![
            item("P-1", "2026-01-01T00:01:00Z"),
            item("P-2", "2026-01-01T00:02:00Z"),
        ];
        assert!(pager.record_page(&page, 2).more);
        let page2 = vec![
            item("P-3", "2026-01-01T00:03:00Z"),
            item("P-4", "2026-01-01T00:04:00Z"),
        ];
        assert!(
            !pager.record_page(&page2, 2).more,
            "the page cap must stop the walk rather than loop forever"
        );
    }

    /// The offset branch is the one band where the shift race survives, so
    /// the pager must name the minute it happened in for the run summary.
    #[test]
    fn pager_reports_the_minute_it_offset_paged() {
        let mut pager = KeysetPager::new(Some(dt("2026-01-01T00:00:30Z")), 10);
        assert_eq!(pager.offset_paged_minute(), None);

        let page = vec![
            item("P-1", "2026-01-01T00:00:40Z"),
            item("P-2", "2026-01-01T00:00:50Z"),
        ];
        pager.record_page(&page, 2);
        assert_eq!(
            pager.offset_paged_minute(),
            Some(dt("2026-01-01T00:00:00Z")),
            "the reported value is the start of the stalled minute"
        );
    }

    /// A clean keyset walk must report nothing — the residual warning has to
    /// mean something when it does fire.
    #[test]
    fn pager_reports_no_offset_minute_on_a_clean_walk() {
        let mut pager = KeysetPager::new(None, 10);
        let page = vec![
            item("P-1", "2026-01-01T00:01:00Z"),
            item("P-2", "2026-01-01T00:02:00Z"),
        ];
        pager.record_page(&page, 2);
        assert_eq!(pager.offset_paged_minute(), None);
    }

    /// A full page with no parseable timestamps must still make progress
    /// (offset branch) rather than re-requesting the same window forever.
    #[test]
    fn pager_advances_by_offset_when_a_page_has_no_timestamps() {
        let mut pager = KeysetPager::new(Some(dt("2026-01-01T00:00:00Z")), 10);
        let page = vec![("P-1".to_string(), None), ("P-2".to_string(), None)];
        let step = pager.record_page(&page, 2);
        assert!(step.more);
        assert_eq!(pager.request().start_at, 2);
    }
}
