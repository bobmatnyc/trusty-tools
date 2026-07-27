//! Full-history changelog retrieval for JIRA issues (issue #4084).
//!
//! Why: JIRA's search-embedded changelog (`expand=changelog`) is itself
//! paged — `changelog.total` can exceed `changelog.histories.len()`, and the
//! entries JIRA drops are the OLDEST ones, which are exactly the transitions
//! a historical backfill exists to capture. Before this module the shortfall
//! was undetectable downstream: the data looked complete and wasn't.
//!
//! What: [`JiraClient::fetch_changelog`] walks the dedicated
//! `GET /rest/api/3/issue/{key}/changelog` endpoint to exhaustion, erroring
//! loudly when it cannot retrieve every entry the server claims exists. The
//! status-transition extraction itself is shared with the embedded path via
//! [`super::model::transitions_from_histories`], so both paths yield
//! identical values for the same history entry.
//!
//! Declared as a child module of `client` (via `#[path]`, the same mechanism
//! `client_tests.rs` uses) rather than a sibling, so it can reach
//! `JiraClient`'s private HTTP/credential/retry fields without widening their
//! visibility to the rest of the crate.

use tracing::debug;

use super::JiraClient;
use crate::collect::errors::{CollectError, Result};
use crate::collect::jira::model::{
    transitions_from_histories, ApiChangelogHistory, ChangelogPageResponse, JiraTransition,
};
use crate::collect::jira::retry::with_retry;

/// Page size for the dedicated `/issue/{key}/changelog` pagination.
///
/// Matches `COMMENT_PAGE_SIZE`: both endpoints are Atlassian paged beans
/// that cap `maxResults` at 100.
const CHANGELOG_PAGE_SIZE: usize = 100;

/// Hard cap on pages one changelog walk may request.
///
/// Every termination condition below depends on the server behaving: honouring
/// `startAt`, or eventually returning an empty page. A server (or caching
/// proxy) that replays page 1 forever satisfies neither, and this walk runs
/// once per ticket across a window of up to 10,000 tickets. 200 pages is
/// 20,000 history entries — orders of magnitude beyond any real issue — so
/// hitting it means the server is misbehaving, and that is reported as an
/// error rather than accepted as a result.
const MAX_CHANGELOG_PAGES: usize = 200;

impl JiraClient {
    /// Fetch an issue's COMPLETE status-transition history from the
    /// dedicated changelog endpoint, paging in [`CHANGELOG_PAGE_SIZE`]
    /// chunks.
    ///
    /// Why: see the module doc — the search-embedded changelog truncates
    /// long histories from the oldest end. This is the escape hatch taken
    /// whenever JIRA's `changelog.total` exceeds the number of embedded
    /// entries, driven from `run_sync`'s per-ticket loop so a failure here is
    /// isolated to its ticket (see
    /// [`super::model::ChangelogIssue::truncated_history_total`]).
    ///
    /// What: `GET /rest/api/3/issue/{key}/changelog?startAt=&maxResults=`,
    /// looping until the accumulated entry count reaches the largest `total`
    /// anyone reported, then extracting `status` items exactly as the
    /// embedded path does. Transient failures are retried with backoff, like
    /// every other paged read on this client (see [`super::retry`]).
    ///
    /// `expected` is the count the SEARCH response reported for this issue —
    /// the number that proved the embedded copy short in the first place. It
    /// seeds the completeness check so that a dedicated endpoint which omits
    /// `total` cannot quietly satisfy it: without that seed, a server
    /// answering "here is one entry, and I won't say how many exist" would
    /// hand back one entry as a complete history and REPLACE a truncated
    /// embedded copy with something no larger. Pass `None` only when no such
    /// expectation exists.
    ///
    /// Fails loudly rather than short: if the walk finishes holding fewer
    /// entries than anyone reported, this returns
    /// [`CollectError::IncompleteChangelog`] naming the ticket and the
    /// expected-vs-retrieved counts. A partial history is never handed back
    /// as though it were complete — that silent shortfall is the entire bug
    /// this function exists to close, and replacing one silent truncation
    /// with another would be no fix at all. A `total` the server never sent
    /// is `None`, never `0`: an unknown bound ends the walk on an EMPTY page
    /// and on nothing else.
    ///
    /// Test: `fetch_changelog_pages_to_exhaustion`,
    /// `fetch_changelog_errors_when_server_returns_fewer_than_total`,
    /// `fetch_changelog_keeps_paging_when_the_endpoint_omits_total`,
    /// `fetch_changelog_errors_when_an_absent_total_hides_a_shortfall`.
    ///
    /// # Errors
    ///
    /// - [`CollectError::Http`] on transport / non-success HTTP responses,
    ///   after the retry budget is exhausted.
    /// - [`CollectError::Json`] on payload parse failures.
    /// - [`CollectError::IncompleteChangelog`] when the paged walk cannot
    ///   retrieve every entry the server reported.
    /// - [`CollectError::PagingBudgetExceeded`] when the walk does not
    ///   terminate within [`MAX_CHANGELOG_PAGES`].
    pub async fn fetch_changelog(
        &self,
        key: &str,
        expected: Option<u64>,
    ) -> Result<Vec<JiraTransition>> {
        let mut histories = Vec::new();
        let mut start_at = 0u64;
        // Seeded from the search's own count, then raised (never lowered) by
        // whatever the dedicated endpoint reports. Keeping the LARGEST total
        // anyone stated stops a server that shrinks `total` mid-walk — or
        // drops it entirely — from talking the completeness check down to the
        // number of entries it happened to hand over.
        let mut reported_total = expected;
        for _ in 0..MAX_CHANGELOG_PAGES {
            let url = format!(
                "{}/rest/api/3/issue/{}/changelog?startAt={}&maxResults={}",
                self.base_url, key, start_at, CHANGELOG_PAGE_SIZE
            );
            debug!(url = %url, "GET (changelog fallback)");
            let parsed: ChangelogPageResponse =
                with_retry("fetch_changelog", &self.retry, &self.budget, || {
                    self.get(&url)
                })
                .await?;
            reported_total = reported_total.max(parsed.total);
            let n = parsed.values.len() as u64;
            histories.extend(parsed.values);
            // A page with no entries means the server has nothing more to
            // give; whether that satisfies `total` is decided below, so an
            // early stop still surfaces instead of passing as complete.
            if n == 0 {
                return finish(key, histories, reported_total);
            }
            start_at += n;
            // Only a total the server actually stated may end the walk. An
            // UNKNOWN total ends nothing — it falls through to the next page
            // and terminates on the empty page above.
            if reported_total.is_some_and(|total| start_at >= total) {
                return finish(key, histories, reported_total);
            }
        }
        Err(CollectError::PagingBudgetExceeded {
            endpoint: "changelog",
            key: key.to_string(),
            pages: MAX_CHANGELOG_PAGES,
        })
    }
}

/// Turn a finished walk into transitions, or into
/// [`CollectError::IncompleteChangelog`] when it came up short of a total the
/// server (or the search that triggered the fallback) actually stated.
///
/// An unstated total cannot be compared against, so it yields no error — the
/// walk reached an empty page, which is the strongest completeness claim
/// available when the server volunteers no count. It is deliberately NOT
/// coerced to `0`: `retrieved < 0` is false for every walk, so a defaulted
/// zero would make this check pass vacuously and certify any prefix as
/// complete.
fn finish(
    key: &str,
    histories: Vec<ApiChangelogHistory>,
    reported_total: Option<u64>,
) -> Result<Vec<JiraTransition>> {
    let retrieved = histories.len() as u64;
    if let Some(expected) = reported_total {
        if retrieved < expected {
            return Err(CollectError::IncompleteChangelog {
                key: key.to_string(),
                expected,
                retrieved,
            });
        }
    }
    Ok(transitions_from_histories(key, histories))
}
