//! Linear GraphQL API client for issue enrichment.
//!
//! Uses the Linear GraphQL API (<https://api.linear.app/graphql>).
//! Authentication: `Authorization: <api_key>` header (no "Bearer" prefix).
//! The key comes from `linear.api_key` in config, or — when config is silent —
//! from `trusty_common::credentials::resolve_key` (#5983). There is no Linear
//! CLI on this path.
//!
//! Issue identifiers are matched against commit messages with the pattern
//! `[A-Z][A-Z0-9]{0,9}-\d+` (e.g. `ENG-123`, `FE-456`), then filtered through
//! [`crate::collect::ticket::is_non_ticket_identifier`] so that documentation,
//! standard, digest and advisory tokens of the same shape never reach the
//! network (#5664).

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use reqwest::Client;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use trusty_common::credentials::scrub_secrets;

use crate::collect::errors::{CollectError, Result};
// #7139: shared with JIRA — see the `retry`/`budget` field doc on
// `LinearClient` for why this is reuse, not a JIRA-specific dependency.
use crate::collect::jira::retry::{with_retry, RetryBudget, RetryPolicy};
use crate::collect::linear::sync;
use crate::collect::ticket::is_non_ticket_identifier;
use crate::core::config::LinearConfig;
use crate::core::db::Database;

/// HTTP `User-Agent` string sent on every request.
const USER_AGENT_VALUE: &str = "trusty-git-analytics/0.1";

/// Linear GraphQL endpoint.
const LINEAR_GRAPHQL_URL: &str = "https://api.linear.app/graphql";

/// Extra pages [`LinearClient::fetch_team_issues`] will follow beyond the
/// ideal page count before declaring the server's cursor runaway (issue
/// #7139, mirroring `collect::jira::client::SEARCH_PAGE_BUDGET_SLACK`,
/// #6812). A server that hands out a continuation token with every page —
/// even an empty one — would otherwise loop without end.
const LINEAR_PAGE_BUDGET_SLACK: usize = 8;

/// Characters of a Linear-authored payload carried into operator-visible text.
///
/// Linear's auth rejection is under 300 bytes; the cap only stops a large
/// HTML error page from being pasted into `stats.errors` or a warn log.
const MAX_ERROR_BODY_CHARS: usize = 500;

/// A Linear issue fetched from the API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LinearIssue {
    /// Linear issue ID (e.g. "ENG-123").
    pub identifier: String,
    /// Issue title.
    pub title: String,
    /// Current state name (e.g. "In Progress", "Done").
    pub state: String,
    /// Team name.
    pub team: String,
    /// Assignee display name (if any).
    pub assignee: Option<String>,
    /// Issue priority (0=none, 1=urgent, 2=high, 3=medium, 4=low).
    pub priority: u8,
    /// URL to the issue in Linear.
    pub url: String,
    /// When the issue was created (issue #7139).
    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,
    /// When the issue was last updated. Drives the bulk sync's incremental
    /// cursor — see [`crate::collect::linear::sync::next_cursor`].
    #[serde(default)]
    pub updated_at: Option<DateTime<Utc>>,
    /// When the issue entered a "started" state, if it ever did.
    #[serde(default)]
    pub started_at: Option<DateTime<Utc>>,
    /// When the issue was completed, if it was. Paired with `created_at`,
    /// this is what makes lead-time-from-ticket computable for a
    /// Linear-only engagement.
    #[serde(default)]
    pub completed_at: Option<DateTime<Utc>>,
    /// When the issue was canceled, if it was.
    #[serde(default)]
    pub canceled_at: Option<DateTime<Utc>>,
}

/// Async Linear GraphQL client.
///
/// `Debug` is implemented by hand, not derived — see the impl below (#5733).
pub struct LinearClient {
    client: Client,
    api_key: String,
    /// GraphQL endpoint every request is sent to.
    ///
    /// Always [`LINEAR_GRAPHQL_URL`] in production. Tests override it via
    /// [`LinearClient::with_endpoint`] so a mock server can answer, which is
    /// what makes the #5665 auth-failure arm assertable without a live key.
    endpoint: String,
    /// Retry schedule applied to bulk-page reads (issue #7139, review
    /// finding: a 429/503 on page N of a large backfill used to discard the
    /// whole walk). Shares [`crate::collect::jira::retry`] — its types are
    /// generic over [`CollectError`], not JIRA-specific, so this is the same
    /// implementation JIRA uses, not a copy of it.
    retry: RetryPolicy,
    /// Whole-run backoff allowance shared by every bulk-page request this
    /// client makes. See [`crate::collect::jira::retry::RetryBudget`].
    budget: RetryBudget,
}

/// What [`LinearClient`]'s `Debug` prints in place of the API key.
const REDACTED_API_KEY: &str = "<redacted>";

/// Redacting `Debug`: the derived one printed `api_key` verbatim (#5733).
///
/// Why: a derived `Debug` puts the live key into every `{:?}` of the client —
/// a `tracing` field, an `anyhow` context, a panic message. No call site did
/// that when this was written, so the fix is by construction: the type can no
/// longer disclose the key, and a future call site needs no audit.
/// What: renders `endpoint`, the field worth debugging, and replaces `api_key`
/// with [`REDACTED_API_KEY`] — no prefix, no length, nothing derived from the
/// value. The mask is unconditional because `LinearConfig::api_key` is an
/// unvalidated `Option<String>` and nothing checks the key's shape: a
/// fingerprint helper that echoes a head — such as
/// [`trusty_common::credentials::redact_secret`], which returns the first four
/// characters of any input longer than four — discloses four characters of
/// real entropy for a key that is not `lin_`-prefixed. A guarantee that holds
/// only for well-formed keys is not one this path can state. The `reqwest`
/// client carries no credential (the key goes on a per-request header) and is
/// dropped as noise; `finish_non_exhaustive` marks the elision.
/// Test: `debug_never_renders_the_api_key`.
impl std::fmt::Debug for LinearClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinearClient")
            .field("endpoint", &self.endpoint)
            .field("api_key", &REDACTED_API_KEY)
            .finish_non_exhaustive()
    }
}

impl LinearClient {
    /// Create a new Linear client from config.
    ///
    /// Resolves the API key through [`resolve_api_key`]: `linear.api_key` from
    /// config first (with `${LINEAR_API_KEY}` expansion), then the shared
    /// credential resolver (#5983).
    ///
    /// # Errors
    ///
    /// - [`CollectError::Config`] if no tier yields a non-empty key.
    /// - [`CollectError::Http`] if the HTTP client cannot be built.
    pub fn new(config: &LinearConfig) -> Result<Self> {
        let api_key = resolve_api_key(config)?;
        let client = Client::builder()
            .user_agent(USER_AGENT_VALUE)
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(CollectError::Http)?;
        let retry = RetryPolicy::default();
        let budget = RetryBudget::new(&retry);
        Ok(Self {
            client,
            api_key,
            endpoint: LINEAR_GRAPHQL_URL.to_string(),
            retry,
            budget,
        })
    }

    /// Override the retry schedule used by bulk-page reads.
    ///
    /// Mirrors [`crate::collect::jira::client::JiraClient::with_retry_policy`]:
    /// the default is tuned for an unattended cron backfill, which is the
    /// wrong trade-off for tests, which must not spend real seconds asleep.
    #[must_use]
    pub fn with_retry_policy(mut self, policy: RetryPolicy) -> Self {
        self.budget = RetryBudget::new(&policy);
        self.retry = policy;
        self
    }

    /// Build a client that talks to `endpoint` instead of Linear itself.
    ///
    /// Test-only seam (#5665): the HTTP status arm of [`Self::fetch_issue`] is
    /// only reachable through a server that answers non-2xx, and asserting it
    /// against the live API would need a revoked key in CI.
    #[cfg(test)]
    pub(crate) fn with_endpoint(
        config: &LinearConfig,
        endpoint: impl Into<String>,
    ) -> Result<Self> {
        Ok(Self {
            endpoint: endpoint.into(),
            ..Self::new(config)?
        })
    }

    /// Fetch a single Linear issue by identifier (e.g. "ENG-123").
    ///
    /// Why: `Ok(None)` is the answer to "does this issue exist", and callers
    /// act on it by moving to the next identifier. A failed call has no answer
    /// to that question, so it must not share the return value (#5665).
    /// What: `Ok(None)` means Linear replied successfully and the issue is not
    /// there — HTTP 200 with `data.issue: null`, or a 200 carrying GraphQL
    /// errors. Every non-2xx status is an `Err`, including the 401 an invalid
    /// API key produces.
    /// Test: `fetch_issue_errors_on_auth_failure`,
    /// `fetch_issue_errors_on_server_failure`,
    /// `fetch_issue_returns_none_for_absent_issue`,
    /// `graphql_errors_are_scrubbed_before_they_reach_the_log`.
    ///
    /// # Errors
    ///
    /// - [`CollectError::LinearApi`] on any non-2xx response, carrying the
    ///   status and Linear's body.
    /// - [`CollectError::Http`] on transport-level failures and on a response
    ///   body that is not JSON.
    pub async fn fetch_issue(&self, identifier: &str) -> Result<Option<LinearIssue>> {
        let query = format!(
            r#"query {{
                issue(id: "{identifier}") {{
                    identifier
                    title
                    state {{ name }}
                    team {{ name }}
                    assignee {{ displayName }}
                    priority
                    url
                    createdAt
                    updatedAt
                    startedAt
                    completedAt
                    canceledAt
                }}
            }}"#
        );

        let body = serde_json::json!({ "query": query });

        let resp = self
            .client
            .post(&self.endpoint)
            .header("Authorization", &self.api_key)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(CollectError::Http)?;

        // #5665: a non-2xx is a failed call, never an absent issue.
        let status = resp.status();
        if !status.is_success() {
            // The status is the finding; a body that will not read is not
            // worth losing it over, so an unreadable body degrades to empty.
            let body = resp.text().await.unwrap_or_default();
            return Err(CollectError::LinearApi {
                status: status.as_u16(),
                identifier: identifier.to_string(),
                message: redacted_body_excerpt(&body, &self.api_key),
            });
        }

        let json: serde_json::Value = resp.json().await.map_err(CollectError::Http)?;

        // GraphQL errors are returned with 200 OK; check for errors array.
        if let Some(errors) = json.get("errors") {
            if errors.as_array().is_some_and(|a| !a.is_empty()) {
                // #5733: Linear authored this array, so it can quote the key
                // back — scrub before it reaches an operator's stderr.
                let detail = redacted_body_excerpt(&errors.to_string(), &self.api_key);
                tracing::warn!(
                    identifier = %identifier,
                    errors = %detail,
                    "Linear GraphQL errors"
                );
                return Ok(None);
            }
        }

        let issue_val = &json["data"]["issue"];
        if issue_val.is_null() {
            return Ok(None);
        }

        Ok(Some(parse_issue_node(identifier, issue_val)))
    }

    /// Extract Linear issue identifiers from a commit message.
    ///
    /// Why: the shape `[A-Z][A-Z0-9]{0,9}-\d+` is also the shape of every
    /// `UTF-8`, `SHA-256`, `ADR-0029`, `RFC-2119`, `ISO-8601` and `RUSTSEC-2026`
    /// a commit message carries, and [`Self::fetch_referenced_issues`] resolves
    /// whatever this yields. A live 52-week `tga collect` on this repository
    /// issued 369 distinct GraphQL lookups, not one of which was a Linear
    /// ticket (#5664): the round-trip was the only thing that could tell an
    /// encoding name from an issue key, and its answer — "no such issue" — is
    /// indistinguishable from a real ticket that has been deleted.
    ///
    /// What: matches the pattern, then drops every token whose prefix is a
    /// known non-ticket family before returning, so no lookup is issued for
    /// one. [`crate::collect::ticket::is_non_ticket_identifier`] owns that
    /// decision and its calibration; this function holds no list of its own.
    /// Returns a deduplicated list of identifiers found, order preserved.
    ///
    /// Ticket-shaped tokens that simply do not resolve (`WI-1`, `AC-1`,
    /// `CREDPANEL-01`) are still returned and still cost a lookup: nothing
    /// distinguishes them from another org's real board keys, and DOC-70 §9.1
    /// counts an unresolved key as the signal it is.
    ///
    /// Test: `extract_issue_ids_finds_linear_patterns`,
    /// `extract_issue_ids_rejects_non_ticket_identifiers`,
    /// `extract_issue_ids_deduplicates`,
    /// `extract_issue_ids_ignores_lowercase_prefix`.
    pub fn extract_issue_ids(message: &str) -> Vec<String> {
        let re = regex::Regex::new(r"\b([A-Z][A-Z0-9]{0,9}-\d+)\b").expect("valid regex");
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for cap in re.captures_iter(message) {
            let id = cap[1].to_string();
            // #5664: reject before the lookup — a resolved "not found" costs a
            // round-trip and answers nothing this test cannot answer offline.
            if is_non_ticket_identifier(&id) {
                continue;
            }
            if seen.insert(id.clone()) {
                out.push(id);
            }
        }
        out
    }

    /// Fetch all issues referenced in the given commit messages.
    ///
    /// Why: this used to return a bare `Vec` and log fetch failures at warn
    /// level, so an invalid API key produced an empty vec that read exactly
    /// like "no commit referenced a Linear issue" (#5665).
    /// What: deduplicates issue IDs across messages, optionally filtered by
    /// `team_filter` (matched case-insensitively against the prefix before the
    /// `-`), then fetches each one. An identifier Linear does not have is
    /// skipped; the first fetch that *fails* stops the walk and returns its
    /// error, because an auth or transport failure applies to the whole run —
    /// continuing only spends hundreds of doomed requests to reach the same
    /// answer.
    /// Test: `fetch_referenced_issues_propagates_auth_failure`,
    /// `fetch_referenced_issues_skips_absent_issues`.
    ///
    /// # Errors
    ///
    /// Propagates the first error from [`Self::fetch_issue`].
    pub async fn fetch_referenced_issues(
        &self,
        messages: &[&str],
        team_filter: &[String],
    ) -> Result<Vec<LinearIssue>> {
        let mut seen = HashSet::new();
        let mut all_ids: Vec<String> = Vec::new();
        for msg in messages {
            for id in Self::extract_issue_ids(msg) {
                if seen.insert(id.clone()) {
                    all_ids.push(id);
                }
            }
        }

        let ids: Vec<String> = if team_filter.is_empty() {
            all_ids
        } else {
            all_ids
                .into_iter()
                .filter(|id| {
                    let team_key = id.split('-').next().unwrap_or("");
                    team_filter.iter().any(|t| t.eq_ignore_ascii_case(team_key))
                })
                .collect()
        };

        let mut issues = Vec::new();
        for id in &ids {
            match self.fetch_issue(id).await? {
                Some(issue) => issues.push(issue),
                None => tracing::debug!("Linear issue not found: {id}"),
            }
        }
        Ok(issues)
    }

    /// Persist a batch of [`LinearIssue`] rows into the `linear_issues` table.
    ///
    /// Uses `INSERT OR REPLACE` keyed on `identifier`, so re-running collection
    /// refreshes the cached state, title, assignee, etc. The `fetched_at`
    /// column is set to the current UTC timestamp for every persisted row.
    ///
    /// Returns the number of rows written.
    ///
    /// # Errors
    ///
    /// Propagates [`crate::core::TgaError::DbError`] on SQL failures.
    pub fn store_issues(
        &self,
        db: &Database,
        issues: &[LinearIssue],
    ) -> crate::core::Result<usize> {
        store_linear_issues(db, issues)
    }

    /// Fetch one page of a team's issues, ordered by `updatedAt` ascending,
    /// retrying a 429/503 with backoff.
    ///
    /// Why: `tga linear sync` needs a team's FULL issue set, not just the
    /// ones referenced by a commit message — [`Self::fetch_referenced_issues`]
    /// answers a different question. This is the primitive [`Self::fetch_team_issues`]
    /// pages over. A first-time backfill can walk hundreds of pages, and
    /// without a retry a single rate-limit response on page 150 discarded
    /// the whole in-memory walk (review finding, #7139) — [`Self::send_team_issues_page`]
    /// classifies 429/503 as [`CollectError::Throttled`], the same variant
    /// [`crate::collect::jira::retry::is_retryable`] already knows to retry,
    /// so this reuses [`crate::collect::jira::retry::with_retry`] rather than
    /// inventing a Linear-specific retry loop.
    /// What: wraps [`Self::send_team_issues_page`] in `with_retry`.
    /// Test: `tests::fetch_team_issues_page_maps_timestamps`,
    /// `tests::fetch_team_issues_page_handles_an_empty_team`,
    /// `tests::fetch_team_issues_page_errors_on_non_2xx`,
    /// `tests::fetch_team_issues_page_retries_a_429_then_succeeds`.
    ///
    /// # Errors
    ///
    /// - [`CollectError::LinearBulkApi`] on a non-2xx response (other than
    ///   429/503) or a GraphQL `errors` array, once the retry budget for a
    ///   429/503 is exhausted.
    /// - [`CollectError::Http`] on transport failures or a non-JSON body.
    pub async fn fetch_team_issues_page(
        &self,
        team_key: &str,
        since: Option<DateTime<Utc>>,
        after: Option<&str>,
        page_size: usize,
        page_number: usize,
    ) -> Result<LinearIssuesPage> {
        with_retry("linear issues page", &self.retry, &self.budget, || {
            self.send_team_issues_page(team_key, since, after, page_size, page_number)
        })
        .await
    }

    /// One un-retried attempt at [`Self::fetch_team_issues_page`]. Factored
    /// out because `with_retry` re-runs the whole request/decode round-trip
    /// on each attempt, and a `reqwest::RequestBuilder` is single-use.
    async fn send_team_issues_page(
        &self,
        team_key: &str,
        since: Option<DateTime<Utc>>,
        after: Option<&str>,
        page_size: usize,
        page_number: usize,
    ) -> Result<LinearIssuesPage> {
        const QUERY: &str = r#"query($first: Int!, $after: String, $filter: IssueFilter, $orderBy: PaginationOrderBy) {
            issues(first: $first, after: $after, filter: $filter, orderBy: $orderBy) {
                nodes {
                    identifier
                    title
                    state { name }
                    team { name key }
                    assignee { displayName }
                    priority
                    url
                    createdAt
                    updatedAt
                    startedAt
                    completedAt
                    canceledAt
                }
                pageInfo { hasNextPage endCursor }
            }
        }"#;

        let variables = serde_json::json!({
            "first": page_size,
            "after": after,
            "filter": sync::build_issues_filter(team_key, since),
            "orderBy": "updatedAt",
        });
        let body = serde_json::json!({ "query": QUERY, "variables": variables });

        let resp = self
            .client
            .post(&self.endpoint)
            .header("Authorization", &self.api_key)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(CollectError::Http)?;

        let status = resp.status();
        // #7139: 429/503 are classified as `Throttled` — the same variant
        // `collect::jira::http::decode` uses — so `with_retry`'s
        // `is_retryable` backs off and resumes instead of discarding the
        // walk. Every other non-2xx is a hard, non-retried failure.
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS
            || status == reqwest::StatusCode::SERVICE_UNAVAILABLE
        {
            let retry_after = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .map(std::time::Duration::from_secs);
            return Err(CollectError::Throttled {
                status: status.as_u16(),
                retry_after,
            });
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(CollectError::LinearBulkApi {
                status: status.as_u16(),
                team_key: team_key.to_string(),
                page: page_number,
                message: redacted_body_excerpt(&body, &self.api_key),
            });
        }

        let json: serde_json::Value = resp.json().await.map_err(CollectError::Http)?;

        if let Some(errors) = json.get("errors") {
            if errors.as_array().is_some_and(|a| !a.is_empty()) {
                let detail = redacted_body_excerpt(&errors.to_string(), &self.api_key);
                return Err(CollectError::LinearBulkApi {
                    status: status.as_u16(),
                    team_key: team_key.to_string(),
                    page: page_number,
                    message: detail,
                });
            }
        }

        let nodes = json["data"]["issues"]["nodes"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let issues = nodes
            .iter()
            .map(|node| parse_issue_node(node["identifier"].as_str().unwrap_or(""), node))
            .collect();
        let has_next_page = json["data"]["issues"]["pageInfo"]["hasNextPage"]
            .as_bool()
            .unwrap_or(false);
        let end_cursor = json["data"]["issues"]["pageInfo"]["endCursor"]
            .as_str()
            .map(String::from);

        Ok(LinearIssuesPage {
            issues,
            has_next_page,
            end_cursor,
        })
    }

    /// Walk every page of a team's issue set (bounded by `max_issues`),
    /// ordered by `updatedAt` ascending.
    ///
    /// Why: the CLI-facing `tga linear sync` command needs the whole set
    /// assembled and a truncation flag, not the raw per-page primitive.
    /// A misbehaving (or malicious) server that keeps answering
    /// `hasNextPage: true` with an `endCursor` but zero `nodes` would
    /// otherwise loop forever — security review finding, #7139 — so the walk
    /// is bounded the same way [`crate::collect::jira::client::JiraClient::search_issues`]
    /// bounds its own page budget (#6812): `max_pages` derived from
    /// `max_issues.div_ceil(PAGE_SIZE)` plus [`LINEAR_PAGE_BUDGET_SLACK`],
    /// erroring with [`CollectError::PagingBudgetExceeded`] rather than
    /// spinning.
    /// What: calls [`Self::fetch_team_issues_page`] until `hasNextPage` is
    /// `false`, `max_issues` is reached, or the page budget is exhausted.
    /// Any page's error (after its own retries) aborts the walk and
    /// propagates — unlike JIRA's per-ticket circuit breaker, a bulk page has
    /// no partial-success shape to isolate.
    /// Test: `tests::fetch_team_issues_walks_every_page`,
    /// `tests::fetch_team_issues_stops_at_max_issues`,
    /// `tests::fetch_team_issues_errors_when_the_page_budget_is_exhausted`.
    ///
    /// # Errors
    ///
    /// - Propagates the first page's error from [`Self::fetch_team_issues_page`].
    /// - [`CollectError::PagingBudgetExceeded`] when the walk does not
    ///   terminate within its page budget.
    pub async fn fetch_team_issues(
        &self,
        team_key: &str,
        since: Option<DateTime<Utc>>,
        max_issues: usize,
    ) -> Result<(Vec<LinearIssue>, bool)> {
        const PAGE_SIZE: usize = 50;
        let max_pages = max_issues.div_ceil(PAGE_SIZE) + LINEAR_PAGE_BUDGET_SLACK;
        let mut issues = Vec::new();
        let mut after: Option<String> = None;
        let mut page_number = 0usize;
        loop {
            page_number += 1;
            let page = self
                .fetch_team_issues_page(team_key, since, after.as_deref(), PAGE_SIZE, page_number)
                .await?;
            issues.extend(page.issues);
            if issues.len() >= max_issues {
                issues.truncate(max_issues);
                return Ok((issues, true));
            }
            if !page.has_next_page || page.end_cursor.is_none() {
                return Ok((issues, false));
            }
            if page_number >= max_pages {
                return Err(CollectError::PagingBudgetExceeded {
                    endpoint: "linear/issues",
                    key: team_key.to_string(),
                    pages: page_number,
                });
            }
            after = page.end_cursor;
        }
    }
}

/// One page of [`LinearClient::fetch_team_issues_page`].
#[derive(Debug, Clone, PartialEq)]
pub struct LinearIssuesPage {
    /// Issues on this page.
    pub issues: Vec<LinearIssue>,
    /// Whether Linear reports another page after this one.
    pub has_next_page: bool,
    /// Opaque cursor for the next page's `after` argument, when
    /// `has_next_page` is `true`.
    pub end_cursor: Option<String>,
}

/// Parse one GraphQL issue node (from either [`LinearClient::fetch_issue`] or
/// [`LinearClient::fetch_team_issues_page`]) into a [`LinearIssue`].
///
/// `identifier_fallback` is used only when the node itself carries no
/// `identifier` field, which the single-issue query relies on since it
/// addresses the node by identifier already.
fn parse_issue_node(identifier_fallback: &str, node: &serde_json::Value) -> LinearIssue {
    let parse_dt = |field: &str| -> Option<DateTime<Utc>> {
        node[field]
            .as_str()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc))
    };
    LinearIssue {
        identifier: node["identifier"]
            .as_str()
            .unwrap_or(identifier_fallback)
            .to_string(),
        title: node["title"].as_str().unwrap_or("").to_string(),
        state: node["state"]["name"]
            .as_str()
            .unwrap_or("Unknown")
            .to_string(),
        team: node["team"]["name"]
            .as_str()
            .unwrap_or("Unknown")
            .to_string(),
        assignee: node["assignee"]["displayName"].as_str().map(String::from),
        priority: node["priority"].as_u64().unwrap_or(0) as u8,
        url: node["url"].as_str().unwrap_or("").to_string(),
        created_at: parse_dt("createdAt"),
        updated_at: parse_dt("updatedAt"),
        started_at: parse_dt("startedAt"),
        completed_at: parse_dt("completedAt"),
        canceled_at: parse_dt("canceledAt"),
    }
}

/// Persist Linear issues to the database (free function for reuse from tests
/// and contexts where no [`LinearClient`] instance is available).
///
/// # Errors
///
/// Propagates [`crate::core::TgaError::DbError`] on SQL failures.
/// Why one transaction: before #7139's review, each issue was a separate
/// autocommit `INSERT OR REPLACE` — fine for the old per-commit-reference
/// callers (a handful of issues), but the new bulk sync can hand this
/// hundreds or thousands of rows in one call, turning a first-time backfill
/// into that many individual fsync'd commits. [`unchecked_transaction`] (not
/// [`Connection::transaction`]) is used because this function's signature
/// takes `&Database`, not `&mut Database` — widening it would ripple into
/// every caller (`linear_pipeline`, `commands::linear`, and both modules'
/// tests) for no behavioral gain: `tga` is single-process and nothing else
/// holds this connection concurrently, the same precondition
/// `unchecked_transaction`'s own contract requires.
///
/// # Errors
///
/// Propagates [`crate::core::TgaError::DbError`] on SQL failures; the
/// transaction rolls back on drop if it never reaches `commit()`, so a
/// mid-batch failure leaves no partial write.
pub fn store_linear_issues(db: &Database, issues: &[LinearIssue]) -> crate::core::Result<usize> {
    let conn = db.connection();
    let tx = conn.unchecked_transaction()?;
    let fetched_at = chrono::Utc::now().to_rfc3339();
    let mut count = 0usize;
    for issue in issues {
        let team_key = issue.identifier.split('-').next().unwrap_or("").to_string();
        tx.execute(
            "INSERT OR REPLACE INTO linear_issues \
             (identifier, title, state, team, team_key, assignee, priority, url, fetched_at, \
              created_at, updated_at, started_at, completed_at, canceled_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                issue.identifier,
                issue.title,
                issue.state,
                issue.team,
                team_key,
                issue.assignee,
                issue.priority as i64,
                issue.url,
                fetched_at,
                issue.created_at.map(|d| d.to_rfc3339()),
                issue.updated_at.map(|d| d.to_rfc3339()),
                issue.started_at.map(|d| d.to_rfc3339()),
                issue.completed_at.map(|d| d.to_rfc3339()),
                issue.canceled_at.map(|d| d.to_rfc3339()),
            ],
        )?;
        count += 1;
    }
    tx.commit()?;
    Ok(count)
}

/// A credential-free excerpt of a Linear-authored response payload, capped at
/// [`MAX_ERROR_BODY_CHARS`] characters.
///
/// Why: the payload is text this process did not author, and it reaches an
/// operator either through `stats.errors` (the non-2xx body) or through
/// `tracing::warn!` (the 200-with-GraphQL-errors array, #5733). A provider that
/// echoes the submitted key back ("your key `lin_api_…` is invalid") would put
/// a live credential in both. Both paths route here so the guard lands once.
/// What: scrubs `api_key` out of the raw body through
/// [`trusty_common::credentials::scrub_secrets`] FIRST, then trims and
/// truncates — the #5239 ordering. Truncating first would cut a credential
/// that straddles the boundary into a prefix the scrubber can no longer match,
/// leaving a partial secret behind. Scrubbing and truncation are one function
/// with the key as a required argument, so no call site can get the order
/// wrong. The cap applies to the scrubbed text, so `[REDACTED]` being longer
/// than what it replaces cannot push the excerpt over budget.
/// Test: `redacted_body_excerpt_scrubs_before_truncating`,
/// `redacted_body_excerpt_clips_long_input`,
/// `redacted_body_excerpt_keeps_short_input`,
/// `an_api_key_echoed_in_the_error_body_never_reaches_the_message`,
/// `graphql_errors_are_scrubbed_before_they_reach_the_log`.
///
/// This removes the one credential this client holds. Per `scrub_secrets`'s own
/// contract the result is lower-risk, not proven secret-free: a key under
/// `MIN_SCRUBBABLE_SECRET_CHARS` (8) is skipped, and a credential the process
/// does not hold — one Linear quotes from its own side — passes through.
fn redacted_body_excerpt(body: &str, api_key: &str) -> String {
    // #5239: scrub the full body, THEN cut.
    let clean = scrub_secrets(body, &[api_key]);
    let trimmed = clean.trim();
    match trimmed.char_indices().nth(MAX_ERROR_BODY_CHARS) {
        Some((idx, _)) => format!("{}…", &trimmed[..idx]),
        None => trimmed.to_string(),
    }
}

/// Thin local alias so existing call-sites in this module require no changes.
///
/// Why: delegates to the canonical shared implementation in
/// [`crate::collect::env_expand::expand_env_var`] to avoid duplication.
/// What: passes `raw` straight through to the shared function.
/// Test: the shared function's own test suite covers all cases; see
/// `crate::collect::env_expand`.
fn expand_env_var(raw: &str) -> String {
    crate::collect::env_expand::expand_env_var(raw)
}

/// Provider key this client resolves its credential under.
///
/// The workspace registry maps it to `LINEAR_API_KEY`
/// (`trusty_common::credentials::registry`).
const LINEAR_CREDENTIAL_PROVIDER: &str = "linear";

/// The Linear API key this client will authenticate with.
///
/// Why: #5983. Collection used to read `linear.api_key` and nothing else, so an
/// operator holding the credential anywhere the rest of the workspace looks —
/// `LINEAR_API_KEY` in the environment, `.env.local`, the OS keychain — still
/// could not collect until they hand-edited YAML to say `${LINEAR_API_KEY}`.
/// Config stays the first tier; the shared resolver is what answers when config
/// is silent, which is the one entry point this repo permits for reading a
/// secret across crates.
/// What: delegates to [`resolve_api_key_with`] with
/// [`trusty_common::credentials::resolve_key`] as the fallback.
///
/// # Errors
///
/// [`CollectError::Config`] when neither tier yields a non-empty key.
fn resolve_api_key(config: &LinearConfig) -> Result<String> {
    resolve_api_key_with(config, || {
        trusty_common::credentials::resolve_key(LINEAR_CREDENTIAL_PROVIDER)
    })
}

/// [`resolve_api_key`] with the fallback tier supplied by the caller.
///
/// Why: the production fallback reads the process environment, `.env.local`,
/// and an OS keychain, none of which a test may depend on. A caller-supplied
/// lookup makes both tiers and the failure provable with no global state — the
/// same seam `collect::env_expand::expand_env_var_with` uses for #5313.
/// What: returns the expanded `config.api_key` when it is non-empty; otherwise
/// `fallback()`, ignoring an empty answer the same way; otherwise an error
/// naming every place the operator may put the key.
/// Test: `tests::config_api_key_wins_over_the_resolver`,
/// `tests::an_absent_config_key_falls_back_to_the_resolver`,
/// `tests::an_empty_resolver_answer_is_not_a_key`,
/// `tests::new_rejects_missing_api_key`.
///
/// # Errors
///
/// [`CollectError::Config`] when neither tier yields a non-empty key.
fn resolve_api_key_with(
    config: &LinearConfig,
    fallback: impl FnOnce() -> Option<String>,
) -> Result<String> {
    let configured = expand_env_var(config.api_key.as_deref().unwrap_or(""));
    if !configured.is_empty() {
        return Ok(configured);
    }
    fallback().filter(|k| !k.is_empty()).ok_or_else(|| {
        CollectError::Config(
            "Linear api_key is required — set `linear.api_key` in the config (a \
             `${LINEAR_API_KEY}` reference is expanded), or provide LINEAR_API_KEY \
             in the environment, in .env.local, or in the credential store"
                .into(),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_issue_ids_finds_linear_patterns() {
        let msg = "ENG-123: add login feature, also fixes FE-456";
        let ids = LinearClient::extract_issue_ids(msg);
        assert!(ids.contains(&"ENG-123".to_string()));
        assert!(ids.contains(&"FE-456".to_string()));
    }

    #[test]
    fn extract_issue_ids_deduplicates() {
        let msg = "ENG-123 ENG-123 duplicate";
        let ids = LinearClient::extract_issue_ids(msg);
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0], "ENG-123");
    }

    #[test]
    fn extract_issue_ids_ignores_lowercase_prefix() {
        let msg = "abc-123 should not match";
        let ids = LinearClient::extract_issue_ids(msg);
        assert!(ids.is_empty());
    }

    /// Why: #5664 — every token here is the shape `extract_issue_ids` matches
    /// and none is a Linear issue, so before this filter each one bought a
    /// GraphQL round-trip whose only possible answer was "no such issue".
    /// A live 52-week `tga collect` on this repository issued 369 such lookups.
    /// What: the identifiers the #5664 measurement observed, plus the families
    /// named alongside them, yield nothing at all — no lookup can be issued for
    /// a token that never leaves the extractor.
    /// Test: this test itself.
    #[test]
    fn extract_issue_ids_rejects_non_ticket_identifiers() {
        for msg in [
            // Observed in the #5664 measurement.
            "docs: DOC-67 §5 sweep order",
            "spec: DOC-70 board axis",
            "docs: amend ADR-0029 point 3",
            "docs: supersede ADR-0038",
            "fix: a multi-byte UTF-8 name breaks the stem",
            "chore: verify the artifact against the published SHA-256",
            "feat: strip ECMA-48 control sequences",
            "chore: avoid reintroducing RUSTSEC-2026-0187",
            // Families named alongside them in #5664.
            "fix: ISO-8601 offsets were dropped",
            "docs: RFC-2119 keyword sweep",
            "chore: drop the MD-5 fallback",
            "chore: patch CVE-2024-3094",
            "docs: map the finding to CWE-79",
            "build: mirror the AL2023/GCC-11 desync",
            "chore: rotate to RSA-4096 and AES-256",
            "docs: SPEC-14 tightened",
        ] {
            assert_eq!(
                LinearClient::extract_issue_ids(msg),
                Vec::<String>::new(),
                "message: {msg:?}"
            );
        }
    }

    /// Why: #5664's filter is a deny-list, and a deny-list that over-reaches
    /// loses genuine board links silently — the failure mode the network cost
    /// it removes is far cheaper than.
    /// What: real tracker keys still extract, including the two-letter and
    /// single-digit shapes closest to the excluded families. The ticket-shaped
    /// tokens the measurement could not resolve (`WI-1`, `AC-1`,
    /// `CREDPANEL-01`) still extract too: that is the deliberate calibration
    /// boundary recorded on `NON_TICKET_PREFIXES`, not an oversight.
    /// Test: this test itself.
    #[test]
    fn extract_issue_ids_keeps_genuine_tracker_ids() {
        for (msg, want) in [
            ("ABC-123: add the thing", "ABC-123"),
            ("fix: land GH-12", "GH-12"),
            ("PROJ-9 tighten the check", "PROJ-9"),
            ("ENG-123: add login feature", "ENG-123"),
            ("also fixes FE-456", "FE-456"),
            // Unresolvable but ticket-shaped: deliberately still extracted.
            ("WI-1 spike", "WI-1"),
            ("AC-1 acceptance", "AC-1"),
            ("CREDPANEL-01 wiring", "CREDPANEL-01"),
        ] {
            assert_eq!(
                LinearClient::extract_issue_ids(msg),
                vec![want.to_string()],
                "message: {msg:?}"
            );
        }
        // A GitHub bare `#N` is a genuine reference of a different shape — this
        // extractor never matched it, and `ticket::extract_ticket_id` still does.
        assert!(LinearClient::extract_issue_ids("closes #1234").is_empty());
        assert_eq!(
            crate::collect::ticket::extract_ticket_id("closes #1234"),
            Some("#1234".to_string())
        );
    }

    /// #5983: asserted against [`resolve_api_key_with`], not `LinearClient::new`.
    /// `new` now consults the shared credential resolver, which reads the real
    /// environment, `.env.local`, and the OS keychain — so on a developer
    /// machine that exports `LINEAR_API_KEY` this test would have started
    /// passing the resolution it exists to prove fails.
    #[test]
    fn new_rejects_missing_api_key() {
        let cfg = LinearConfig::default();
        let err = resolve_api_key_with(&cfg, || None).expect_err("should reject empty key");
        match err {
            CollectError::Config(msg) => assert!(msg.contains("api_key")),
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// #5983: config is the first tier, so a key on disk is never overridden by
    /// whatever the environment or keychain happens to hold.
    #[test]
    fn config_api_key_wins_over_the_resolver() {
        let cfg = LinearConfig {
            api_key: Some("lin_api_from_config".into()),
            ..LinearConfig::default()
        };
        let key = resolve_api_key_with(&cfg, || Some("lin_api_from_store".into())).expect("key");
        assert_eq!(key, "lin_api_from_config");
    }

    /// #5983 (primary reproduction): before the fix this path returned
    /// `CollectError::Config`, so an operator whose key lived in the
    /// environment, `.env.local`, or the keychain could not collect at all.
    #[test]
    fn an_absent_config_key_falls_back_to_the_resolver() {
        let cfg = LinearConfig::default();
        let key = resolve_api_key_with(&cfg, || Some("lin_api_from_store".into())).expect("key");
        assert_eq!(key, "lin_api_from_store");
        // An unresolvable `${VAR}` placeholder expands to empty, which is the
        // same "config said nothing" state and must reach the same tier.
        let placeholder = LinearConfig {
            api_key: Some("${TGA_LINEAR_KEY_THAT_IS_NEVER_SET}".into()),
            ..LinearConfig::default()
        };
        let key =
            resolve_api_key_with(&placeholder, || Some("lin_api_from_store".into())).expect("key");
        assert_eq!(key, "lin_api_from_store");
    }

    /// An empty answer from the resolver is absence, not a key — otherwise the
    /// client would send `Authorization: ` and read Linear's 401 as a defect.
    #[test]
    fn an_empty_resolver_answer_is_not_a_key() {
        let cfg = LinearConfig::default();
        let err = resolve_api_key_with(&cfg, || Some(String::new()))
            .expect_err("empty is not a credential");
        match err {
            CollectError::Config(msg) => assert!(msg.contains("LINEAR_API_KEY"), "{msg}"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    fn sample_issue(identifier: &str) -> LinearIssue {
        LinearIssue {
            identifier: identifier.to_string(),
            title: format!("Title for {identifier}"),
            state: "In Progress".to_string(),
            team: "Engineering".to_string(),
            assignee: Some("Alice".to_string()),
            priority: 2,
            url: format!("https://linear.app/x/issue/{identifier}"),
            created_at: None,
            updated_at: None,
            started_at: None,
            completed_at: None,
            canceled_at: None,
        }
    }

    #[test]
    fn store_linear_issues_inserts_rows() {
        let db = Database::open_in_memory().expect("db");
        let issues = vec![sample_issue("ENG-1"), sample_issue("FE-42")];
        let n = store_linear_issues(&db, &issues).expect("store");
        assert_eq!(n, 2);

        let conn = db.connection();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM linear_issues", [], |r| r.get(0))
            .expect("count");
        assert_eq!(count, 2);

        let (identifier, team_key, priority): (String, String, i64) = conn
            .query_row(
                "SELECT identifier, team_key, priority FROM linear_issues WHERE identifier = ?1",
                ["ENG-1"],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect("query");
        assert_eq!(identifier, "ENG-1");
        assert_eq!(team_key, "ENG");
        assert_eq!(priority, 2);
    }

    #[test]
    fn store_linear_issues_is_idempotent_on_identifier() {
        let db = Database::open_in_memory().expect("db");
        let mut issue = sample_issue("ENG-9");
        store_linear_issues(&db, &[issue.clone()]).expect("first");

        // Re-store with updated state — should replace, not duplicate.
        issue.state = "Done".to_string();
        issue.assignee = Some("Bob".to_string());
        store_linear_issues(&db, &[issue]).expect("second");

        let conn = db.connection();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM linear_issues", [], |r| r.get(0))
            .expect("count");
        assert_eq!(count, 1);

        let (state, assignee): (String, Option<String>) = conn
            .query_row(
                "SELECT state, assignee FROM linear_issues WHERE identifier = ?1",
                ["ENG-9"],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("query");
        assert_eq!(state, "Done");
        assert_eq!(assignee.as_deref(), Some("Bob"));
    }

    #[test]
    fn store_linear_issues_handles_missing_assignee() {
        let db = Database::open_in_memory().expect("db");
        let mut issue = sample_issue("OPS-7");
        issue.assignee = None;
        store_linear_issues(&db, &[issue]).expect("store");

        let conn = db.connection();
        let assignee: Option<String> = conn
            .query_row(
                "SELECT assignee FROM linear_issues WHERE identifier = ?1",
                ["OPS-7"],
                |r| r.get(0),
            )
            .expect("query");
        assert!(assignee.is_none());
    }

    #[test]
    fn migration_v2_creates_linear_issues_table() {
        let db = Database::open_in_memory().expect("db");
        let conn = db.connection();
        let name: String = conn
            .query_row(
                "SELECT name FROM sqlite_master WHERE type='table' AND name='linear_issues'",
                [],
                |r| r.get(0),
            )
            .expect("table exists");
        assert_eq!(name, "linear_issues");
        assert!(db.schema_version().expect("version") >= 2);
    }

    /// A credential-shaped key, long enough to clear `scrub_secrets`'
    /// eight-character floor. Fake — matches Linear's `lin_api_` prefix only so
    /// the fixture reads like the real thing.
    const FAKE_API_KEY: &str = "lin_api_averyrealisticlookingkey0123456789";

    #[test]
    fn redacted_body_excerpt_keeps_short_input() {
        assert_eq!(
            redacted_body_excerpt("  {\"errors\":[]}  ", FAKE_API_KEY),
            "{\"errors\":[]}"
        );
    }

    #[test]
    fn redacted_body_excerpt_clips_long_input() {
        let out = redacted_body_excerpt(&"x".repeat(MAX_ERROR_BODY_CHARS + 50), FAKE_API_KEY);
        assert_eq!(out.chars().count(), MAX_ERROR_BODY_CHARS + 1);
        assert!(out.ends_with('…'));
    }

    /// The #5239 ordering, pinned: the key straddles the truncation boundary.
    /// Scrub-then-truncate removes it whole. Truncate-then-scrub would cut it
    /// into a prefix no scrubber can match and leave that fragment in the
    /// operator's terminal.
    #[test]
    fn redacted_body_excerpt_scrubs_before_truncating() {
        let pad = "x".repeat(MAX_ERROR_BODY_CHARS - 30);
        let body = format!("{pad}{FAKE_API_KEY} trailing detail");

        let out = redacted_body_excerpt(&body, FAKE_API_KEY);

        assert!(!out.contains(FAKE_API_KEY), "whole key survived: {out}");
        assert!(
            !out.contains(&FAKE_API_KEY[..30]),
            "a prefix of the key survived the cut — truncation ran first: {out}"
        );
        assert!(out.contains("[REDACTED]"), "key was not scrubbed: {out}");
    }

    /// Endpoint used by [`client_holding`]. Distinct from every key fixture, so
    /// a "did the key survive" assertion cannot be satisfied by this instead.
    const PROBE_ENDPOINT: &str = "http://endpoint.invalid/graphql";

    /// Build a client holding `key` verbatim.
    ///
    /// Bypasses [`LinearClient::new`], which rejects an empty key — that arm is
    /// why the empty case is otherwise unreachable, and `Debug` lives on the
    /// type rather than on the constructor.
    fn client_holding(key: &str) -> LinearClient {
        let retry = RetryPolicy::default();
        let budget = RetryBudget::new(&retry);
        LinearClient {
            client: Client::new(),
            api_key: key.to_string(),
            endpoint: PROBE_ENDPOINT.to_string(),
            retry,
            budget,
        }
    }

    /// The #5733 regression. `LinearClient` derived `Debug` over `api_key`, so
    /// any `{:?}` of the client — a tracing field, an `anyhow` context, a panic
    /// message — printed the live Linear key. Nothing formatted the client at
    /// the time, which made the exposure latent rather than absent: it lived in
    /// the type, so the next call site to debug-format one would have leaked
    /// without touching this file.
    ///
    /// The shapes matter because nothing validates the key's format:
    /// `LinearConfig::api_key` is a plain `Option<String>`. A masking rule that
    /// echoed a fixed-length head would be safe only for `lin_`-prefixed keys
    /// and would disclose real entropy for the rest, so the table covers a key
    /// with no recognisable prefix, keys at and under a head length, and empty.
    #[test]
    fn debug_never_renders_the_api_key() {
        // No single-character key here: `rendered` contains the mask and the
        // endpoint, so a one-letter needle trips `contains` against those and
        // fails a correct mask. Same trap `redact_secret`'s own contract test
        // documents. Two characters is the shortest honest case.
        let cases: &[(&str, &str)] = &[
            (FAKE_API_KEY, "the lin_-prefixed key production expects"),
            (
                "9f3Kq7Zt2Wm4Bx8Lv6Nc1Rd5Ph0Sj",
                "no prefix: entropy up front",
            ),
            ("ab7Q", "exactly a four-character head"),
            ("x9", "shorter than a head"),
            ("", "empty — unreachable via new(), guarded anyway"),
        ];

        for (key, why) in cases {
            let client = client_holding(key);
            let compact = format!("{client:?}");
            let pretty = format!("{client:#?}");

            for rendered in [&compact, &pretty] {
                if !key.is_empty() {
                    assert!(
                        !rendered.contains(key),
                        "{why}: the whole key reached Debug output: {rendered}"
                    );
                    // A head-echoing mask would pass the check above and still
                    // disclose the first characters, which is the #5733 gap.
                    let head: String = key.chars().take(4).collect();
                    assert!(
                        !rendered.contains(&head),
                        "{why}: a leading fragment of the key survived: {rendered}"
                    );
                }
                assert!(
                    rendered.contains(REDACTED_API_KEY),
                    "{why}: the key field was not masked: {rendered}"
                );
                assert!(
                    rendered.contains("endpoint.invalid"),
                    "{why}: redaction must not cost the endpoint, the field \
                     worth debugging: {rendered}"
                );
            }
        }
    }

    /// The other half of #5733: Linear answers 200 with an `errors` array, and
    /// that array is text this process did not author. A provider that quotes
    /// the submitted key back put it on an operator's stderr on every such
    /// response — not a rare path. `Ok(None)` stays the answer (#5665); only
    /// the logging changes.
    #[tokio::test]
    #[tracing_test::traced_test]
    async fn graphql_errors_are_scrubbed_before_they_reach_the_log() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "errors": [{
                    "message": format!("API key {FAKE_API_KEY} lacks the read scope")
                }]
            })))
            .mount(&server)
            .await;

        let got = mock_client(&server.uri())
            .fetch_issue("ENG-1")
            .await
            .expect("a 200 carrying GraphQL errors is still a successful call");
        assert!(got.is_none(), "the #5665 control flow is deliberately kept");

        assert!(
            !logs_contain(FAKE_API_KEY),
            "the key reached the operator's terminal"
        );
        assert!(logs_contain("[REDACTED]"), "the key was not scrubbed");
        assert!(
            logs_contain("lacks the read scope"),
            "redaction must not cost the reader Linear's diagnosis"
        );
    }

    /// Build a client pointed at `endpoint`, holding [`FAKE_API_KEY`].
    fn mock_client(endpoint: &str) -> LinearClient {
        let cfg = LinearConfig {
            api_key: Some(FAKE_API_KEY.into()),
            ..Default::default()
        };
        LinearClient::with_endpoint(&cfg, endpoint).expect("client builds")
    }

    /// Linear's verbatim 401 body for a key it rejects, captured from
    /// `POST https://api.linear.app/graphql` with an invalid key.
    const AUTH_ERROR_BODY: &str = r#"{"errors":[{"message":"Authentication required, not authenticated","extensions":{"type":"authentication error","code":"AUTHENTICATION_ERROR","statusCode":401,"userPresentableMessage":"You need to authenticate to access this operation."}}]}"#;

    /// The #5665 regression: a rejected API key must not answer the question
    /// "does this issue exist". Before the fix this returned `Ok(None)`, which
    /// every caller reads as "issue absent", so a run against an invalid key
    /// wrote zero rows and exited 0 with nothing in the summary.
    #[tokio::test]
    async fn fetch_issue_errors_on_auth_failure() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(401).set_body_raw(AUTH_ERROR_BODY, "application/json"),
            )
            .mount(&server)
            .await;

        let err = mock_client(&server.uri())
            .fetch_issue("ENG-1")
            .await
            .expect_err("a 401 must not read as an absent issue");

        match err {
            CollectError::LinearApi {
                status,
                identifier,
                message,
            } => {
                assert_eq!(status, 401);
                assert_eq!(identifier, "ENG-1");
                assert!(
                    message.contains("You need to authenticate"),
                    "Linear's own diagnosis must survive into the error: {message}"
                );
            }
            other => panic!("expected LinearApi, got {other:?}"),
        }
    }

    /// A provider that quotes the rejected key back must not put it in the
    /// operator's terminal. `stats.errors` is printed to stderr by
    /// `commands::collect`, so this body reaches a human — it did not before
    /// #5665, which is what makes the scrub load-bearing now.
    #[tokio::test]
    async fn an_api_key_echoed_in_the_error_body_never_reaches_the_message() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let echoing_body = format!(
            r#"{{"errors":[{{"message":"API key {FAKE_API_KEY} is not valid for this workspace"}}]}}"#
        );
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(401).set_body_raw(echoing_body, "application/json"))
            .mount(&server)
            .await;

        let err = mock_client(&server.uri())
            .fetch_issue("ENG-1")
            .await
            .expect_err("a 401 must surface");
        let rendered = err.to_string();

        assert!(
            !rendered.contains(FAKE_API_KEY),
            "the key reached the error message: {rendered}"
        );
        assert!(
            rendered.contains("[REDACTED]"),
            "the key was not scrubbed: {rendered}"
        );
        assert!(
            rendered.contains("is not valid for this workspace"),
            "redaction must not cost the reader Linear's diagnosis: {rendered}"
        );
    }

    /// The same arm for a server-side failure — a 500 is no more an absent
    /// issue than a 401 is.
    #[tokio::test]
    async fn fetch_issue_errors_on_server_failure() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500).set_body_raw("upstream down", "text/plain"))
            .mount(&server)
            .await;

        let err = mock_client(&server.uri())
            .fetch_issue("ENG-1")
            .await
            .expect_err("a 500 must surface");
        assert!(
            matches!(err, CollectError::LinearApi { status: 500, .. }),
            "expected a 500 LinearApi, got {err:?}"
        );
    }

    /// The other side of the boundary: an issue Linear genuinely does not
    /// have still returns `Ok(None)`, so a commit mentioning a non-Linear
    /// `ABC-123` string does not fail the run.
    #[tokio::test]
    async fn fetch_issue_returns_none_for_absent_issue() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": { "issue": null }
            })))
            .mount(&server)
            .await;

        let got = mock_client(&server.uri())
            .fetch_issue("ENG-404")
            .await
            .expect("an absent issue is a successful answer");
        assert!(got.is_none());
    }

    /// The batch walk must carry the failure out to the pipeline rather than
    /// returning an empty vec that is indistinguishable from "no references".
    #[tokio::test]
    async fn fetch_referenced_issues_propagates_auth_failure() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(401).set_body_raw(AUTH_ERROR_BODY, "application/json"),
            )
            .mount(&server)
            .await;

        let err = mock_client(&server.uri())
            .fetch_referenced_issues(&["ENG-1: work", "FE-2: more"], &[])
            .await
            .expect_err("an invalid key must reach the caller");
        assert!(
            matches!(err, CollectError::LinearApi { status: 401, .. }),
            "expected a 401 LinearApi, got {err:?}"
        );
        assert_eq!(
            server.received_requests().await.map(|r| r.len()),
            Some(1),
            "the walk stops at the first failure instead of retrying every id"
        );
    }

    /// Absent issues stay non-fatal: the walk skips them and returns the
    /// issues it did resolve.
    #[tokio::test]
    async fn fetch_referenced_issues_skips_absent_issues() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": { "issue": null }
            })))
            .mount(&server)
            .await;

        let issues = mock_client(&server.uri())
            .fetch_referenced_issues(&["ENG-1 and FE-2"], &[])
            .await
            .expect("absent issues are not a failure");
        assert!(issues.is_empty());
    }

    /// Live integration test — only runs when `LINEAR_API_KEY` env var is set.
    ///
    /// The assertion is the #5665 closure condition: against a revoked key
    /// this must FAIL. It used to pass, because a 401 arrived as `Ok(None)` —
    /// the same value a genuinely absent `ENG-1` returns.
    #[tokio::test]
    async fn fetch_issue_live() {
        let key = match std::env::var("LINEAR_API_KEY") {
            Ok(k) => k,
            Err(_) => {
                eprintln!("SKIP: set LINEAR_API_KEY to run");
                return;
            }
        };
        let config = LinearConfig {
            api_key: Some(key),
            ..Default::default()
        };
        let client = LinearClient::new(&config).expect("client");
        let result = client.fetch_issue("ENG-1").await;
        assert!(
            result.is_ok(),
            "fetch must not error — a revoked key lands here: {result:?}"
        );
        println!("Result: {result:?}");
    }

    // #7139: `tga linear sync` bulk-page fetch. No real network anywhere
    // below — every server is `wiremock::MockServer`, mirroring the JIRA
    // `paged_http` fixtures in `collect::jira::client_tests`.
    mod bulk_sync {
        use super::*;
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        fn node(identifier: &str, updated_at: &str) -> serde_json::Value {
            serde_json::json!({
                "identifier": identifier,
                "title": format!("Title for {identifier}"),
                "state": {"name": "In Progress"},
                "team": {"name": "Engineering", "key": "ENG"},
                "assignee": {"displayName": "Alice"},
                "priority": 2,
                "url": format!("https://linear.app/x/issue/{identifier}"),
                "createdAt": "2026-01-01T00:00:00.000Z",
                "updatedAt": updated_at,
                "startedAt": "2026-01-02T00:00:00.000Z",
                "completedAt": serde_json::Value::Null,
                "canceledAt": serde_json::Value::Null,
            })
        }

        fn page_response(
            nodes: Vec<serde_json::Value>,
            has_next: bool,
            cursor: Option<&str>,
        ) -> ResponseTemplate {
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": {
                    "issues": {
                        "nodes": nodes,
                        "pageInfo": {"hasNextPage": has_next, "endCursor": cursor},
                    }
                }
            }))
        }

        /// Deliverable #7139.3: pagination — a two-page team walk assembles
        /// every issue from both pages via `after`/`endCursor`.
        #[tokio::test]
        async fn fetch_team_issues_walks_every_page() {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(wiremock::matchers::body_string_contains("\"after\":null"))
                .respond_with(page_response(
                    vec![node("ENG-1", "2026-01-01T00:01:00.000Z")],
                    true,
                    Some("cursor-1"),
                ))
                .mount(&server)
                .await;
            Mock::given(method("POST"))
                .and(wiremock::matchers::body_string_contains("cursor-1"))
                .respond_with(page_response(
                    vec![node("ENG-2", "2026-01-01T00:02:00.000Z")],
                    false,
                    None,
                ))
                .mount(&server)
                .await;

            let client = mock_client(&server.uri());
            let (issues, truncated) = client
                .fetch_team_issues("ENG", None, 10_000)
                .await
                .expect("walk succeeds");

            let ids: Vec<&str> = issues.iter().map(|i| i.identifier.as_str()).collect();
            assert_eq!(ids, vec!["ENG-1", "ENG-2"]);
            assert!(!truncated);
        }

        /// Deliverable #7139.3: the `--max-issues` cap truncates the walk
        /// and reports it, mirroring JIRA's `walk.truncated`.
        #[tokio::test]
        async fn fetch_team_issues_stops_at_max_issues() {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(page_response(
                    vec![
                        node("ENG-1", "2026-01-01T00:01:00.000Z"),
                        node("ENG-2", "2026-01-01T00:02:00.000Z"),
                        node("ENG-3", "2026-01-01T00:03:00.000Z"),
                    ],
                    true,
                    Some("cursor-1"),
                ))
                .mount(&server)
                .await;

            let client = mock_client(&server.uri());
            let (issues, truncated) = client
                .fetch_team_issues("ENG", None, 2)
                .await
                .expect("walk succeeds");

            assert_eq!(issues.len(), 2);
            assert!(truncated);
        }

        /// Deliverable #7139.3: timestamp mapping — every lifecycle field
        /// round-trips from the GraphQL node into `LinearIssue`.
        #[tokio::test]
        async fn fetch_team_issues_page_maps_timestamps() {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(page_response(
                    vec![node("ENG-1", "2026-01-01T00:01:00.000Z")],
                    false,
                    None,
                ))
                .mount(&server)
                .await;

            let page = mock_client(&server.uri())
                .fetch_team_issues_page("ENG", None, None, 50, 1)
                .await
                .expect("page fetch succeeds");

            let issue = &page.issues[0];
            assert_eq!(
                issue.created_at,
                Some(
                    DateTime::parse_from_rfc3339("2026-01-01T00:00:00.000Z")
                        .unwrap()
                        .with_timezone(&Utc)
                )
            );
            assert_eq!(
                issue.updated_at,
                Some(
                    DateTime::parse_from_rfc3339("2026-01-01T00:01:00.000Z")
                        .unwrap()
                        .with_timezone(&Utc)
                )
            );
            assert!(issue.started_at.is_some());
            assert_eq!(issue.completed_at, None);
            assert_eq!(issue.canceled_at, None);
        }

        /// Deliverable #7139.3: empty team — a team with no issues yields an
        /// empty, non-error result.
        #[tokio::test]
        async fn fetch_team_issues_page_handles_an_empty_team() {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(page_response(vec![], false, None))
                .mount(&server)
                .await;

            let page = mock_client(&server.uri())
                .fetch_team_issues_page("ENG", None, None, 50, 1)
                .await
                .expect("empty page is not an error");

            assert!(page.issues.is_empty());
            assert!(!page.has_next_page);
        }

        /// Deliverable #7139.3: a non-2xx on a bulk page is a
        /// `LinearBulkApi` error, not an absent-team result — the bulk-page
        /// counterpart to `fetch_issue_errors_on_auth_failure`.
        #[tokio::test]
        async fn fetch_team_issues_page_errors_on_non_2xx() {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(401).set_body_raw(
                    r#"{"errors":[{"message":"Authentication required"}]}"#,
                    "application/json",
                ))
                .mount(&server)
                .await;

            let err = mock_client(&server.uri())
                .fetch_team_issues_page("ENG", None, None, 50, 1)
                .await
                .expect_err("a 401 must not read as an empty team");

            match err {
                CollectError::LinearBulkApi {
                    status,
                    team_key,
                    page,
                    ..
                } => {
                    assert_eq!(status, 401);
                    assert_eq!(team_key, "ENG");
                    assert_eq!(page, 1);
                }
                other => panic!("expected LinearBulkApi, got {other:?}"),
            }
        }

        /// A retry policy with near-zero delays, so
        /// `fetch_team_issues_page_retries_a_429_then_succeeds` does not
        /// spend real seconds asleep. Mirrors
        /// `collect::jira::client_tests::paged_http::fast_retry`.
        fn fast_policy() -> RetryPolicy {
            RetryPolicy {
                max_attempts: 3,
                base_delay: std::time::Duration::from_millis(1),
                max_delay: std::time::Duration::from_millis(1),
                max_total_delay: std::time::Duration::from_millis(100),
            }
        }

        /// Deliverable #7139 fix-round item 2 (critic HIGH): a 429 on one
        /// page must back off and resume, not discard the walk. Uses a
        /// stateful responder — the first request gets a 429, every
        /// subsequent one gets a normal page — asserting the retry actually
        /// ran (not that the mock happened to be lenient).
        #[tokio::test]
        async fn fetch_team_issues_page_retries_a_429_then_succeeds() {
            use std::sync::atomic::{AtomicUsize, Ordering};
            use std::sync::Arc;
            use wiremock::{Request, Respond};

            struct OnceThrottled {
                calls: Arc<AtomicUsize>,
            }
            impl Respond for OnceThrottled {
                fn respond(&self, _request: &Request) -> ResponseTemplate {
                    if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                        ResponseTemplate::new(429)
                            .insert_header("Retry-After", "0")
                            .set_body_raw(
                                r#"{"errors":[{"message":"rate limited"}]}"#,
                                "application/json",
                            )
                    } else {
                        page_response(vec![node("ENG-1", "2026-01-01T00:01:00.000Z")], false, None)
                    }
                }
            }

            let server = MockServer::start().await;
            let calls = Arc::new(AtomicUsize::new(0));
            Mock::given(method("POST"))
                .respond_with(OnceThrottled {
                    calls: Arc::clone(&calls),
                })
                .mount(&server)
                .await;

            let client = mock_client(&server.uri()).with_retry_policy(fast_policy());
            let page = client
                .fetch_team_issues_page("ENG", None, None, 50, 1)
                .await
                .expect("the 429 is retried, not surfaced");

            assert_eq!(page.issues.len(), 1);
            assert_eq!(
                calls.load(Ordering::SeqCst),
                2,
                "expected exactly one retry (429 then 200)"
            );
        }

        /// Deliverable #7139 fix-round item 1 (security): a server that keeps
        /// answering `hasNextPage: true` with zero nodes must error via the
        /// page budget, not loop forever. Before the fix `fetch_team_issues`
        /// had no bound at all on the walk's page count.
        #[tokio::test]
        async fn fetch_team_issues_errors_when_the_page_budget_is_exhausted() {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(page_response(vec![], true, Some("always-more")))
                .mount(&server)
                .await;

            let client = mock_client(&server.uri());
            let err = client
                .fetch_team_issues("ENG", None, 10)
                .await
                .expect_err("an endless hasNextPage:true must not loop forever");

            match err {
                CollectError::PagingBudgetExceeded {
                    endpoint,
                    key,
                    pages,
                } => {
                    assert_eq!(endpoint, "linear/issues");
                    assert_eq!(key, "ENG");
                    assert!(pages > 0);
                }
                other => panic!("expected PagingBudgetExceeded, got {other:?}"),
            }
        }
    }
}
