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
    transitions_from_histories, ChangelogPageResponse, JiraTransition,
};
use crate::collect::jira::retry::with_retry;

/// Page size for the dedicated `/issue/{key}/changelog` pagination.
///
/// Matches `COMMENT_PAGE_SIZE`: both endpoints are Atlassian paged beans
/// that cap `maxResults` at 100.
const CHANGELOG_PAGE_SIZE: usize = 100;

impl JiraClient {
    /// Fetch an issue's COMPLETE status-transition history from the
    /// dedicated changelog endpoint, paging in [`CHANGELOG_PAGE_SIZE`]
    /// chunks.
    ///
    /// Why: see the module doc — the search-embedded changelog truncates
    /// long histories from the oldest end. This is the escape hatch
    /// [`JiraClient::search_with_changelog`] takes whenever JIRA's
    /// `changelog.total` exceeds the number of embedded entries.
    ///
    /// What: `GET /rest/api/3/issue/{key}/changelog?startAt=&maxResults=`,
    /// looping until the accumulated entry count reaches the server-reported
    /// `total` (or the server stops returning entries), then extracting
    /// `status` items exactly as the embedded path does. Transient failures
    /// are retried with backoff, like every other paged read on this client
    /// (see [`super::retry`]).
    ///
    /// Fails loudly rather than short: if the walk finishes holding fewer
    /// entries than JIRA reported, this returns
    /// [`CollectError::IncompleteChangelog`] naming the ticket and the
    /// expected-vs-retrieved counts. A partial history is never handed back
    /// as though it were complete — that silent shortfall is the entire bug
    /// this function exists to close, and replacing one silent truncation
    /// with another would be no fix at all.
    ///
    /// Test: `fetch_changelog_pages_to_exhaustion`,
    /// `fetch_changelog_errors_when_server_returns_fewer_than_total`.
    ///
    /// # Errors
    ///
    /// - [`CollectError::Http`] on transport / non-success HTTP responses,
    ///   after the retry budget is exhausted.
    /// - [`CollectError::Json`] on payload parse failures.
    /// - [`CollectError::IncompleteChangelog`] when the paged walk cannot
    ///   retrieve every entry the server reported.
    pub async fn fetch_changelog(&self, key: &str) -> Result<Vec<JiraTransition>> {
        let mut histories = Vec::new();
        let mut start_at = 0u64;
        let mut reported_total = 0u64;
        loop {
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
            // Keep the LARGEST total any page reported: a server that
            // shrinks `total` mid-walk must not be able to talk the
            // completeness check down to the number of entries it happened
            // to hand over.
            reported_total = reported_total.max(parsed.total);
            let n = parsed.values.len() as u64;
            histories.extend(parsed.values);
            // A page with no entries means the server has nothing more to
            // give; whether that satisfies `total` is decided below, so an
            // early stop still surfaces instead of passing as complete.
            if n == 0 {
                break;
            }
            start_at += n;
            if start_at >= reported_total {
                break;
            }
        }

        let retrieved = histories.len() as u64;
        if retrieved < reported_total {
            return Err(CollectError::IncompleteChangelog {
                key: key.to_string(),
                expected: reported_total,
                retrieved,
            });
        }
        Ok(transitions_from_histories(key, histories))
    }
}
