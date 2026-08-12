//! Response-size bound for the `session_context_catchup` MCP tool (#5557).
//!
//! Why: the response grew with the project's whole snapshot history and had no
//! ceiling. On this repo `full: true` returned 112k characters — more than the
//! harness could hand back to the calling model, so it spilled the body to a
//! file and the resuming session had to read that instead. A resume tool whose
//! own output the resumer cannot read has lost its one purpose. Measurement
//! (2026-08-12, 31 snapshots) put `sessions` at 184,056 of 194,434 bytes —
//! 94.7% — with a 5,961-byte median and a 9,890-byte maximum. No single record
//! is oversized; the COUNT is what grows without bound, and it only ever grows.
//! So the bound is a page over records, not a clamp on any field: truncating a
//! summary mid-string would corrupt the exact prose a resume reads, while
//! dropping whole records off the end is recoverable by asking for the next
//! page.
//! What: [`bound_catchup`] fills a page with WHOLE records, newest-first, until
//! the serialized budget is spent, and reports what it left behind plus the
//! offset that retrieves it. Nothing is ever silently withheld — see
//! [`BoundedCatchup::truncation_notice`].
//! Test: `page_stops_at_the_budget`, `page_always_makes_progress`,
//! `bound_catchup_leaves_a_fitting_digest_alone`,
//! `paging_reaches_every_session_in_order`.

use serde::Serialize;
use trusty_common::catchup::git::CommitSummary;
use trusty_common::catchup::{CatchupJson, PausedSessionJson, RecentMemoryJson};

/// Serialized-byte ceiling for one `session_context_catchup` response body.
///
/// Why: the observed failure was a 112,096-character body the harness could not
/// deliver. A tool result is capped near 25k tokens, and dense JSON tokenizes
/// worse than prose, so this sits at roughly half that in characters — enough
/// headroom that the page is deliverable with the JSON-RPC framing around it,
/// and still ~6 snapshots per page at this repo's 5,961-byte median.
/// What: the total the three digest arrays are fitted into, not a hard limit on
/// the encoded response — the fixed scalar keys and a single over-budget record
/// can carry it past this number (see [`page`]).
/// Test: `page_stops_at_the_budget`, `catchup_payload_bounds_an_oversized_store`
/// (in `super::mcp_context`).
pub const CATCHUP_BUDGET_BYTES: usize = 48_000;

/// Share of [`CATCHUP_BUDGET_BYTES`] reserved for commits + palace drawers.
///
/// Why: `git_limit` bounds commits PER PROJECT, so `all_projects: true`
/// multiplies them by the project count — bounded arrays that are collectively
/// unbounded. Reserving a fixed share caps that product and leaves the rest for
/// the sessions, which are what a resume actually reads.
/// Test: `bound_catchup_trims_commits_into_their_share`.
const AUX_BUDGET_BYTES: usize = 12_000;

/// One page of a catch-up digest, plus the receipt for whatever it left behind.
///
/// Why: a capped response that looks identical to a complete one is the defect,
/// not the fix — the caller must be able to tell from the RESPONSE that content
/// was withheld, how much, and how to get the rest. Every field here exists so
/// no drop can be silent: the `*_total` counts say what existed, and
/// [`Self::next_offset`] says exactly what to pass back for the remainder.
/// What: the three trimmed arrays and the totals they were trimmed from.
/// Test: `bound_catchup_leaves_a_fitting_digest_alone`,
/// `paging_reaches_every_session_in_order`.
pub struct BoundedCatchup {
    /// The sessions on this page, newest-first.
    pub sessions: Vec<PausedSessionJson>,
    /// How many sessions matched the filter before paging.
    pub sessions_total: usize,
    /// The offset this page starts at.
    pub sessions_offset: usize,
    /// Commits on this page, newest-first.
    pub recent_commits: Vec<CommitSummary>,
    /// How many commits were collected before trimming.
    pub recent_commits_total: usize,
    /// Palace drawers on this page, newest-first.
    pub recent_memory: Vec<RecentMemoryJson>,
    /// How many drawers were collected before trimming.
    pub recent_memory_total: usize,
}

impl BoundedCatchup {
    /// The `sessions_offset` that retrieves the next page, or `None` at the end.
    ///
    /// Why: this is what keeps `full: true` a real feature rather than a
    /// truncated one. `full` exists to bypass the watermark and show history a
    /// caller cannot otherwise see; a hard cap would delete that, whereas an
    /// offset the caller can walk delivers all of it, bounded per call.
    /// What: `offset + sessions.len()` while that is short of the total.
    /// Test: `paging_reaches_every_session_in_order`.
    pub fn next_offset(&self) -> Option<usize> {
        let next = self.sessions_offset + self.sessions.len();
        (next < self.sessions_total).then_some(next)
    }

    /// Whether anything was withheld from this page.
    ///
    /// What: true when sessions remain after this page, or when either
    /// auxiliary array was trimmed to fit.
    /// Test: `bound_catchup_leaves_a_fitting_digest_alone`.
    pub fn truncated(&self) -> bool {
        self.next_offset().is_some()
            || self.recent_commits.len() < self.recent_commits_total
            || self.recent_memory.len() < self.recent_memory_total
    }

    /// Prose naming what was withheld and how to retrieve it, or `None` when
    /// the page is complete.
    ///
    /// Why: the machine-readable counts are only half of loud. The consumer
    /// here is a language model reading a JSON body, and a sentence that names
    /// the counts and the exact parameter is what actually gets acted on — a
    /// `sessions_total` it has to compare against an array length is a
    /// difference it can miss. Both go on the wire.
    /// What: one sentence per withheld array; the sessions clause names the
    /// literal `sessions_offset` value to pass back.
    /// Test: `truncation_notice_names_the_counts_and_the_recovery`.
    pub fn truncation_notice(&self) -> Option<String> {
        if !self.truncated() {
            return None;
        }
        let mut parts = Vec::new();
        if let Some(next) = self.next_offset() {
            parts.push(format!(
                "Showing paused sessions {}–{} of {} (newest first); re-call \
                 session_context_catchup with sessions_offset: {next} for the rest.",
                self.sessions_offset,
                next.saturating_sub(1),
                self.sessions_total
            ));
        }
        if self.recent_commits.len() < self.recent_commits_total {
            parts.push(format!(
                "Showing {} of {} recent commits; the oldest were dropped to fit \
                 the response budget — use git log for the remainder.",
                self.recent_commits.len(),
                self.recent_commits_total
            ));
        }
        if self.recent_memory.len() < self.recent_memory_total {
            parts.push(format!(
                "Showing {} of {} memory drawers; the oldest were dropped to fit \
                 the response budget.",
                self.recent_memory.len(),
                self.recent_memory_total
            ));
        }
        Some(parts.join(" "))
    }
}

/// The serialized cost of one record, including its array separator.
fn cost_of<T: Serialize>(item: &T, on_error: usize) -> usize {
    serde_json::to_string(item).map_or(on_error, |s| s.len() + 1)
}

/// Take whole records from `items[from..]` while their serialized size fits.
///
/// Why: a page that can return zero records while records remain is a caller
/// that can never advance — an infinite loop instead of a truncation. So the
/// first record of a page is taken unconditionally, even when it alone exceeds
/// the budget. That is the one case where the response can exceed
/// [`CATCHUP_BUDGET_BYTES`]; it is bounded by one record and always makes
/// progress, which a mid-string truncation of that record would not.
/// What: clones records in order, charging each its serialized length plus one
/// byte for the separator, stopping before the first record that would push the
/// running total past `budget`. A record that cannot be serialized is charged
/// the whole budget rather than nothing, so it can only ever land alone.
/// Test: `page_stops_at_the_budget`, `page_always_makes_progress`.
fn page<T: Serialize + Clone>(items: &[T], from: usize, budget: usize) -> Vec<T> {
    let mut out = Vec::new();
    let mut used = 0usize;
    for item in items.iter().skip(from) {
        let cost = cost_of(item, budget);
        if !out.is_empty() && used.saturating_add(cost) > budget {
            break;
        }
        used = used.saturating_add(cost);
        out.push(item.clone());
    }
    out
}

/// Fit a merged digest into one bounded page starting at `offset`.
///
/// Why: the entry point the MCP tool calls instead of serializing the whole
/// digest. See the module doc for why the bound is a record page rather than a
/// field clamp.
/// What: commits and drawers are trimmed first, into a fixed
/// [`AUX_BUDGET_BYTES`] share; sessions get whatever the budget has left, so
/// the array that grows without bound is the one that absorbs the pressure.
/// `offset` is clamped to the session count, so an out-of-range page is empty
/// rather than an error.
/// Test: `bound_catchup_leaves_a_fitting_digest_alone`,
/// `bound_catchup_trims_commits_into_their_share`,
/// `paging_reaches_every_session_in_order`.
pub fn bound_catchup(merged: CatchupJson, offset: usize, budget: usize) -> BoundedCatchup {
    let aux_budget = AUX_BUDGET_BYTES.min(budget);
    let recent_commits = page(&merged.recent_commits, 0, aux_budget);
    let commit_bytes: usize = recent_commits.iter().map(|c| cost_of(c, 0)).sum();
    let recent_memory = page(
        &merged.recent_memory,
        0,
        aux_budget.saturating_sub(commit_bytes),
    );
    let memory_bytes: usize = recent_memory.iter().map(|d| cost_of(d, 0)).sum();

    let sessions_offset = offset.min(merged.sessions.len());
    let sessions = page(
        &merged.sessions,
        sessions_offset,
        budget.saturating_sub(commit_bytes + memory_bytes),
    );

    BoundedCatchup {
        sessions,
        sessions_total: merged.sessions.len(),
        sessions_offset,
        recent_commits_total: merged.recent_commits.len(),
        recent_commits,
        recent_memory_total: merged.recent_memory.len(),
        recent_memory,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusty_common::catchup::{CatchupOptions, generate_catchup_json};

    fn commit(msg_bytes: usize) -> CommitSummary {
        CommitSummary {
            sha: "0".repeat(40),
            msg: "m".repeat(msg_bytes),
            author: "a".to_string(),
            ts: None,
        }
    }

    /// Build a digest of `n` paused sessions whose summaries are
    /// `summary_bytes` long.
    ///
    /// `PausedSessionJson` is `#[non_exhaustive]`, so trusty-mpm cannot build
    /// one by hand — the fixture goes through a real snapshot store, which also
    /// keeps these tests honest about the shape the parser actually produces.
    async fn digest(n: usize, summary_bytes: usize) -> CatchupJson {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join(".trusty-mpm").join("sessions");
        std::fs::create_dir_all(&dir).unwrap();
        let body = "x".repeat(summary_bytes);
        for i in 0..n {
            std::fs::write(
                dir.join(format!("session-20260801-12{:02}{:02}.md", i / 60, i % 60)),
                format!("## Summary\n{body}\n"),
            )
            .unwrap();
        }
        generate_catchup_json(&CatchupOptions {
            project_dir: tmp.path().to_path_buf(),
            memory_url: "http://127.0.0.1:19999".to_string(),
            include_git: false,
            include_palace: false,
            git_limit: 50,
            drawer_limit: 15,
            full: true,
        })
        .await
    }

    /// Why: the bound is only real if the walk actually stops — a `page` that
    /// took everything would leave every downstream assertion vacuous.
    /// What: records that individually fit are taken until the next would
    /// exceed the budget, and no further.
    /// Test: itself.
    #[test]
    fn page_stops_at_the_budget() {
        let items: Vec<String> = (0..20).map(|_| "y".repeat(100)).collect();
        let taken = page(&items, 0, 500);
        assert!(
            !taken.is_empty() && taken.len() < items.len(),
            "took {} of {}",
            taken.len(),
            items.len()
        );
        let bytes: usize = taken.iter().map(|s| s.len() + 3).sum();
        assert!(
            bytes <= 500 + 103,
            "page spent {bytes} against a 500 budget"
        );
    }

    /// Why: a page that returns zero records while records remain is not a
    /// truncation, it is a caller stuck in a loop that can never advance. One
    /// oversized record must ship whole rather than be clamped mid-string.
    /// What: a single record larger than the whole budget is still returned.
    /// Test: itself.
    #[test]
    fn page_always_makes_progress() {
        let items = vec!["z".repeat(10_000), "z".repeat(10_000)];
        let taken = page(&items, 0, 10);
        assert_eq!(taken.len(), 1, "the first record is unconditional");
        assert_eq!(taken[0].len(), 10_000, "and is not clamped mid-string");
    }

    /// Why: a regression here breaks every resume in every project — the
    /// overwhelming majority of catch-ups are far under budget and must come
    /// back exactly as they did before the bound existed.
    /// What: a digest that fits produces byte-identical arrays and reports no
    /// truncation, no notice, and no next page.
    /// Test: itself.
    #[tokio::test]
    async fn bound_catchup_leaves_a_fitting_digest_alone() {
        let merged = CatchupJson {
            recent_commits: (0..5).map(|_| commit(40)).collect(),
            recent_memory: vec![RecentMemoryJson {
                title: "t".to_string(),
                tags: vec!["a".to_string()],
            }],
            undatable_sessions_dropped: 3,
            ..digest(4, 200).await
        };
        let before = (
            serde_json::to_string(&merged.sessions).unwrap(),
            serde_json::to_string(&merged.recent_commits).unwrap(),
            serde_json::to_string(&merged.recent_memory).unwrap(),
        );

        let bounded = bound_catchup(merged, 0, CATCHUP_BUDGET_BYTES);

        assert_eq!(serde_json::to_string(&bounded.sessions).unwrap(), before.0);
        assert_eq!(
            serde_json::to_string(&bounded.recent_commits).unwrap(),
            before.1
        );
        assert_eq!(
            serde_json::to_string(&bounded.recent_memory).unwrap(),
            before.2
        );
        assert!(!bounded.truncated());
        assert!(bounded.truncation_notice().is_none());
        assert!(bounded.next_offset().is_none());
    }

    /// Why: `all_projects: true` multiplies the per-project `git_limit` by the
    /// project count, so bounded-per-project commits are collectively
    /// unbounded — and a commit list that ate the whole budget would starve
    /// the sessions a resume is actually for.
    /// What: 400 fat commits are trimmed into the auxiliary share, and the
    /// count they were trimmed from survives on the receipt.
    /// Test: itself.
    #[tokio::test]
    async fn bound_catchup_trims_commits_into_their_share() {
        let merged = CatchupJson {
            recent_commits: (0..400).map(|_| commit(120)).collect(),
            ..digest(3, 200).await
        };
        let bounded = bound_catchup(merged, 0, CATCHUP_BUDGET_BYTES);

        assert!(bounded.recent_commits.len() < 400);
        assert_eq!(bounded.recent_commits_total, 400);
        assert_eq!(bounded.sessions.len(), 3, "sessions keep their own share");
        let commit_bytes = serde_json::to_string(&bounded.recent_commits)
            .unwrap()
            .len();
        assert!(
            commit_bytes <= AUX_BUDGET_BYTES + 200,
            "commits spent {commit_bytes}"
        );
        assert!(bounded.truncated());
    }

    /// Why: `full: true` exists to hand back history the watermark hides. A cap
    /// that cannot deliver that history has removed the feature instead of
    /// bounding it — so walking the offsets must reach every session, in the
    /// original order, with no record dropped or repeated.
    /// What: pages a 40-session digest to exhaustion and compares the
    /// concatenation against the input.
    /// Test: itself.
    #[tokio::test]
    async fn paging_reaches_every_session_in_order() {
        let merged = digest(40, 4_000).await;
        let expected: Vec<String> = merged.sessions.iter().map(|s| s.summary.clone()).collect();

        let mut seen: Vec<String> = Vec::new();
        let mut offset = Some(0usize);
        let mut pages = 0;
        while let Some(o) = offset {
            let bounded = bound_catchup(merged.clone(), o, CATCHUP_BUDGET_BYTES);
            assert!(!bounded.sessions.is_empty(), "page at {o} made no progress");
            seen.extend(bounded.sessions.iter().map(|s| s.summary.clone()));
            offset = bounded.next_offset();
            pages += 1;
            assert!(pages < 100, "paging did not terminate");
        }
        assert!(pages > 1, "the fixture must actually need paging");
        assert_eq!(seen, expected);
    }

    /// Why: the counts alone are a difference the reader has to compute. The
    /// sentence is what a model acts on, so it has to carry the totals AND the
    /// literal parameter value that retrieves the remainder.
    /// What: the notice names the withheld total and the exact
    /// `sessions_offset` for the next page.
    /// Test: itself.
    #[tokio::test]
    async fn truncation_notice_names_the_counts_and_the_recovery() {
        let bounded = bound_catchup(digest(40, 4_000).await, 0, CATCHUP_BUDGET_BYTES);
        let notice = bounded.truncation_notice().expect("must announce the drop");
        let next = bounded.next_offset().unwrap();
        assert!(notice.contains("40"), "names the total: {notice}");
        assert!(
            notice.contains(&format!("sessions_offset: {next}")),
            "names the recovery: {notice}"
        );
    }

    /// Why: an offset past the end must be an empty last page, not a panic and
    /// not a wrapped-around first page.
    /// What: `offset` beyond the session count clamps and reports no next page.
    /// Test: itself.
    #[tokio::test]
    async fn bound_catchup_clamps_an_out_of_range_offset() {
        let bounded = bound_catchup(digest(3, 100).await, 99, CATCHUP_BUDGET_BYTES);
        assert!(bounded.sessions.is_empty());
        assert_eq!(bounded.sessions_offset, 3);
        assert_eq!(bounded.sessions_total, 3);
        assert!(bounded.next_offset().is_none());
        assert!(!bounded.truncated());
    }
}
