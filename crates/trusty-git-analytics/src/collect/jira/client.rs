//! Minimal JIRA REST client for fetching individual issues.

use std::sync::Mutex;

use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, USER_AGENT};
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::debug;

use chrono_tz::Tz;

use crate::collect::errors::{CollectError, Result};
use crate::collect::jira::http::{expand_credential, get_json, post_json, Credentials};
use crate::collect::jira::jql_time::parse_timezone;
use crate::collect::jira::model::{
    embedded_changelog_is_truncated, ChangelogIssue, ChangelogSearchResponse, ChangelogWalk,
    CommentSearchResponse, JiraComment,
};
use crate::collect::jira::paging::{KeysetPager, PagedItem};
use crate::collect::jira::retry::{with_retry, RetryBudget, RetryPolicy};
use crate::collect::jira::sync::{build_jql, SyncScope};
use crate::core::config::JiraConfig;

/// Full-history changelog retrieval (issue #4084). A child module rather
/// than a sibling so it can use `JiraClient`'s private HTTP/retry fields.
#[path = "changelog.rs"]
mod changelog;

/// Page size for the `/issue/{key}/comment` pagination (issue #3966).
const COMMENT_PAGE_SIZE: usize = 100;

/// HTTP `User-Agent` string sent on every request.
const USER_AGENT_VALUE: &str = "trusty-git-analytics/0.1";

/// Page size for JQL search pagination.
const SEARCH_PAGE_SIZE: usize = 50;

/// Async JIRA Cloud / Server client.
pub struct JiraClient {
    client: reqwest::Client,
    base_url: String,
    /// `(username, token)` for HTTP Basic Auth.
    credentials: Option<Credentials>,
    /// Default project key for filtered queries.
    project_key: String,
    /// Cached story-point custom field key (e.g. `customfield_10016`).
    /// `None` = uncached; `Some(None)` = discovered to be absent;
    /// `Some(Some(_))` = discovered key.
    story_point_field: Mutex<Option<Option<String>>>,
    /// Operator-pinned timezone from `jira.timezone`, bypassing discovery.
    configured_timezone: Option<String>,
    /// Cached timezone in which this account's JQL date literals are
    /// evaluated. `None` = not yet discovered.
    account_timezone: Mutex<Option<Tz>>,
    /// Retry schedule applied to the paged read paths.
    retry: RetryPolicy,
    /// Whole-run backoff allowance shared by every request this client makes.
    budget: RetryBudget,
}

/// Subset of fields extracted from a JIRA issue payload.
#[derive(Debug, Clone)]
pub struct JiraIssue {
    /// Issue key, e.g. `PROJ-123`.
    pub key: String,
    /// Short summary / title.
    pub summary: String,
    /// Current status name, e.g. `Done`.
    pub status: String,
    /// Issue type, e.g. `Bug`, `Story`, `Task`.
    pub issue_type: String,
    /// Story points (numeric estimate). Extracted from the configured
    /// custom field if discoverable; `None` when the field is absent or
    /// unset on the issue.
    pub story_points: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct ApiIssue {
    key: String,
    fields: ApiFields,
}

#[derive(Debug, Deserialize)]
struct ApiFields {
    #[serde(default)]
    summary: String,
    status: ApiNamed,
    #[serde(rename = "issuetype")]
    issue_type: ApiNamed,
    /// Capture all other fields so we can pluck the story-point custom field
    /// (whose key varies per JIRA instance) without modeling each.
    #[serde(flatten)]
    extra: std::collections::HashMap<String, Value>,
}

#[derive(Debug, Deserialize)]
struct ApiNamed {
    name: String,
}

/// `GET /rest/api/3/field` returns a flat list of field descriptors.
#[derive(Debug, Deserialize)]
struct FieldDescriptor {
    id: String,
    name: String,
}

/// Subset of `GET /rest/api/3/myself` — the account's IANA timezone, which
/// is the zone JIRA uses to evaluate this account's JQL date literals.
#[derive(Debug, Deserialize)]
struct MyselfResponse {
    #[serde(rename = "timeZone", default)]
    time_zone: Option<String>,
}

/// Wire shape of a JQL search response.
///
/// `startAt` is deliberately NOT modelled: the client tracks its own offset.
/// Deriving the next offset from the server's echo is unsafe under
/// `#[serde(default)]`, which silently yields `0` when the field is absent —
/// pinning the loop on page 2 forever. The `/search/jql` successor endpoint
/// omits `startAt` entirely, so this is a live hazard, not a hypothetical.
#[derive(Debug, Deserialize)]
struct SearchResponse {
    issues: Vec<ApiIssue>,
    #[serde(default)]
    total: u64,
}

impl JiraClient {
    /// Construct a client from a [`JiraConfig`].
    ///
    /// # Errors
    ///
    /// - [`CollectError::Config`] if `url` is missing.
    /// - [`CollectError::Http`] if the underlying client cannot be built.
    pub fn new(config: &JiraConfig) -> Result<Self> {
        let base = config
            .url
            .as_ref()
            .ok_or_else(|| CollectError::Config("jira.url is required".into()))?
            .trim_end_matches('/')
            .to_string();

        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_static(USER_AGENT_VALUE));
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));

        let client = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(std::time::Duration::from_secs(30))
            .build()?;

        // Expand `${ENV_VAR}` placeholders in username/token so YAML configs
        // can reference credentials via environment indirection, matching
        // the convention already used by the GitHub/Linear/AZDO clients
        // (`collect::env_expand`). Previously this client took username/
        // token as literal strings only, unlike its siblings — issue #3966
        // aligned it during the JIRA-ingestion-ownership build-out.
        let credentials = match (&config.username, &config.token) {
            (Some(u), Some(t)) => Some((
                expand_credential("jira.username", u)?,
                expand_credential("jira.token", t)?,
            )),
            _ => None,
        };

        let retry = RetryPolicy::default();
        Ok(Self {
            client,
            base_url: base,
            credentials,
            project_key: config.project_key.clone().unwrap_or_default(),
            story_point_field: Mutex::new(None),
            configured_timezone: config.timezone.clone(),
            account_timezone: Mutex::new(None),
            budget: RetryBudget::new(&retry),
            retry,
        })
    }

    /// Override the retry schedule used by the paged read paths.
    ///
    /// Why this is a knob and not a constant: the default is tuned for an
    /// unattended cron backfill (patient), which is the wrong trade-off for
    /// an interactive caller that would rather fail fast — and for tests,
    /// which must not spend real seconds asleep.
    #[must_use]
    pub fn with_retry_policy(mut self, policy: RetryPolicy) -> Self {
        self.budget = RetryBudget::new(&policy);
        self.retry = policy;
        self
    }

    /// The timezone in which this JIRA account evaluates JQL date literals.
    ///
    /// Why this is not simply UTC — and why getting it wrong loses tickets —
    /// is argued in full in [`super::jql_time`]. Short version: JQL date
    /// literals carry no zone and JIRA resolves them in the *querying
    /// account's* profile timezone, so a UTC-rendered bound sent by an
    /// `America/New_York` account opens five hours late and silently skips
    /// every ticket in the gap.
    ///
    /// Resolution order, cached for the client's lifetime in the same shape
    /// as [`Self::get_story_point_field`]:
    ///
    /// 1. `jira.timezone` from config, when the operator has pinned it.
    /// 2. `GET /rest/api/3/myself` → `timeZone`.
    ///
    /// There is deliberately **no silent UTC fallback**. Defaulting to UTC is
    /// exactly the unstated assumption that made this a defect; if neither
    /// source answers, the run fails with an error telling the operator to
    /// set `jira.timezone`. That is a one-line fix for them and a guaranteed
    /// invariant for us.
    ///
    /// # Errors
    ///
    /// [`CollectError::Config`] when the zone cannot be determined or parsed;
    /// transport errors from the `/myself` probe are wrapped with that same
    /// remediation hint.
    ///
    /// Test: `account_timezone_prefers_configured_value`,
    /// `account_timezone_discovers_from_myself`,
    /// `account_timezone_errors_when_undiscoverable`.
    pub async fn account_timezone(&self) -> Result<Tz> {
        {
            let guard = self
                .account_timezone
                .lock()
                .map_err(|e| CollectError::Config(format!("timezone cache poisoned: {e}")))?;
            if let Some(tz) = *guard {
                return Ok(tz);
            }
        }

        let tz = match &self.configured_timezone {
            Some(name) => parse_timezone(name)?,
            None => {
                let url = format!("{}/rest/api/3/myself", self.base_url);
                debug!(url = %url, "GET (account timezone)");
                let me: MyselfResponse =
                    with_retry("myself", &self.retry, &self.budget, || self.get(&url))
                        .await
                        .map_err(|e| {
                            CollectError::Config(format!(
                                "could not determine the JIRA account timezone from \
                                 GET /rest/api/3/myself ({e}). JQL date literals are \
                                 evaluated in the account's timezone, so tga refuses to \
                                 guess — set `jira.timezone` in config.yaml (e.g. `UTC`)."
                            ))
                        })?;
                let name = me.time_zone.ok_or_else(|| {
                    CollectError::Config(
                        "the JIRA account reports no `timeZone`; set `jira.timezone` in \
                         config.yaml so JQL date bounds can be rendered correctly."
                            .to_string(),
                    )
                })?;
                parse_timezone(&name)?
            }
        };

        let mut guard = self
            .account_timezone
            .lock()
            .map_err(|e| CollectError::Config(format!("timezone cache poisoned: {e}")))?;
        *guard = Some(tz);
        Ok(tz)
    }

    /// Fetch a single issue by its key, returning `None` on 404.
    ///
    /// # Errors
    ///
    /// Returns [`CollectError::Http`] on transport / non-404 status errors,
    /// or [`CollectError::Json`] on payload parse failure.
    pub async fn fetch_issue(&self, key: &str) -> Result<Option<JiraIssue>> {
        let url = format!("{}/rest/api/3/issue/{}", self.base_url, key);
        debug!(url = %url, "GET");
        let mut req = self.client.get(&url);
        if let Some((user, token)) = &self.credentials {
            req = req.basic_auth(user, Some(token));
        }
        let resp = req.send().await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let resp = resp.error_for_status()?;
        let issue: ApiIssue = resp.json().await?;
        let story_field = self.get_story_point_field().await?;
        Ok(Some(Self::convert_issue(issue, story_field.as_deref())))
    }

    /// Default project key supplied at construction.
    pub fn project_key(&self) -> &str {
        &self.project_key
    }

    /// Search JIRA issues by JQL, paginating in `SEARCH_PAGE_SIZE` chunks.
    ///
    /// Why: many JIRA workflows (sprint rollups, ticket-id enrichment for
    /// commit messages) need bulk reads; single-issue fetches would be O(N)
    /// HTTP round-trips.
    /// What: `POST /rest/api/3/search` with `{ jql, startAt, maxResults }`,
    /// loops until either `max_results` issues are collected or the server
    /// reports no more pages.
    /// Test: covered by `jira_search_response_deserializes` (wire shape).
    ///
    /// # Errors
    ///
    /// - [`CollectError::Http`] on transport / non-success HTTP responses.
    /// - [`CollectError::Json`] on payload parse failures.
    pub async fn search_issues(&self, jql: &str, max_results: usize) -> Result<Vec<JiraIssue>> {
        let url = format!("{}/rest/api/3/search", self.base_url);
        let story_field = self.get_story_point_field().await?;
        // Request the story-point field explicitly when we know its key so
        // JIRA includes it; otherwise rely on `*all` to get every field.
        let fields: Vec<String> = match &story_field {
            Some(key) => vec![
                "summary".into(),
                "status".into(),
                "issuetype".into(),
                key.clone(),
            ],
            None => vec!["*all".into()],
        };

        let mut out: Vec<JiraIssue> = Vec::new();
        let mut start_at = 0u64;
        loop {
            let remaining = max_results.saturating_sub(out.len());
            if remaining == 0 {
                break;
            }
            let page_size = remaining.min(SEARCH_PAGE_SIZE);
            let body = json!({
                "jql": jql,
                "startAt": start_at,
                "maxResults": page_size,
                "fields": fields,
            });
            debug!(url = %url, %jql, start_at, "POST");
            let mut req = self.client.post(&url).json(&body);
            if let Some((user, token)) = &self.credentials {
                req = req.basic_auth(user, Some(token));
            }
            let resp = req.send().await?.error_for_status()?;
            let parsed: SearchResponse = resp.json().await?;
            let n = parsed.issues.len();
            for issue in parsed.issues {
                out.push(Self::convert_issue(issue, story_field.as_deref()));
                if out.len() >= max_results {
                    break;
                }
            }
            if n < page_size {
                break;
            }
            start_at += n as u64;
            if start_at >= parsed.total {
                break;
            }
        }
        Ok(out)
    }

    /// Discover the JIRA custom-field key for "Story Points", cached for
    /// the lifetime of the client.
    ///
    /// Why: the field id (e.g. `customfield_10016`) is per-instance, so we
    /// must look it up at runtime rather than hard-coding.
    /// What: `GET /rest/api/3/field`, scans for a field whose `name`
    /// matches `"Story Points"` or `"Story point estimate"` (case-insensitive).
    /// Test: deserialization shape covered by `field_descriptor_deserializes`.
    ///
    /// Returns `Ok(None)` if no matching field exists on the instance.
    ///
    /// # Errors
    ///
    /// - [`CollectError::Http`] on transport / non-success HTTP responses.
    /// - [`CollectError::Json`] on payload parse failures.
    pub async fn get_story_point_field(&self) -> Result<Option<String>> {
        // Fast path: serve from cache.
        {
            let guard = self
                .story_point_field
                .lock()
                .map_err(|e| CollectError::Config(format!("story-point cache poisoned: {e}")))?;
            if let Some(cached) = guard.as_ref() {
                return Ok(cached.clone());
            }
        }

        let url = format!("{}/rest/api/3/field", self.base_url);
        debug!(url = %url, "GET");
        let mut req = self.client.get(&url);
        if let Some((user, token)) = &self.credentials {
            req = req.basic_auth(user, Some(token));
        }
        let resp = req.send().await?.error_for_status()?;
        let fields: Vec<FieldDescriptor> = resp.json().await?;

        let found = fields
            .into_iter()
            .find(|f| {
                let n = f.name.to_ascii_lowercase();
                n == "story points" || n == "story point estimate"
            })
            .map(|f| f.id);

        // Persist (whether hit or miss) so we don't refetch.
        let mut guard = self
            .story_point_field
            .lock()
            .map_err(|e| CollectError::Config(format!("story-point cache poisoned: {e}")))?;
        *guard = Some(found.clone());
        Ok(found)
    }

    /// Convert an `ApiIssue` wire-form into our public [`JiraIssue`], plucking
    /// the story-point custom field when its key is known.
    fn convert_issue(api: ApiIssue, story_field_key: Option<&str>) -> JiraIssue {
        let story_points =
            story_field_key.and_then(|key| api.fields.extra.get(key).and_then(|v| v.as_f64()));
        JiraIssue {
            key: api.key,
            summary: api.fields.summary,
            status: api.fields.status.name,
            issue_type: api.fields.issue_type.name,
            story_points,
        }
    }

    /// Search JIRA issues by JQL, paginating in `SEARCH_PAGE_SIZE` chunks,
    /// with each issue's status-changelog embedded (issue #3966).
    ///
    /// Why: `fact_ticket_transitions` needs every status transition per
    /// ticket. JIRA's `expand=changelog` on the search endpoint returns each
    /// issue's changelog `histories` inline, avoiding an N+1 per-issue
    /// `/changelog` call for the common case.
    ///
    /// What: `POST /rest/api/3/search` with
    /// `{ jql, startAt, maxResults, fields: ["project", "updated"],
    /// expand: ["changelog"] }`, looping until `max_results` issues are
    /// collected or the server reports no more pages. Each returned
    /// [`ChangelogIssue`] carries only the `status`-field changelog items;
    /// all other changelog item kinds (e.g. `assignee`, `priority`) are
    /// dropped since only status transitions are in scope for this fact
    /// table.
    ///
    /// Pagination is **keyset**, not offset: the caller passes the sync
    /// [`SyncScope`] rather than a pre-built JQL string so each page can
    /// re-anchor the `updated >=` window onto the previous page's maximum.
    /// Offset paging over `ORDER BY updated ASC` — paging on the same
    /// mutable field it sorts by — lets a ticket edited mid-walk shift an
    /// unread ticket across the read boundary, permanently. See
    /// [`super::paging`] for the full argument and the residual
    /// intra-minute case. Boundary re-reads are deduplicated by issue key,
    /// so the returned vector holds each ticket at most once.
    ///
    /// Transient failures (429/5xx/timeouts) are retried with backoff; see
    /// [`super::retry`].
    ///
    /// The search-embedded changelog is itself paged, so per issue this
    /// compares JIRA's `changelog.total` against the number of embedded
    /// entries; when the embedded copy is short (its OLDEST entries having
    /// been dropped), [`JiraClient::fetch_changelog`] walks the dedicated
    /// `GET /issue/{key}/changelog` endpoint and REPLACES the truncated
    /// transitions with the full history. The extra round trip is paid only
    /// by the truncated minority — a complete embedded changelog costs
    /// nothing beyond the search itself (issue #4084).
    ///
    /// The fallback runs during conversion, i.e. before the keyset
    /// deduplication above, so a truncated ticket re-read at a window
    /// boundary pays for it twice. That is deliberate: the alternative
    /// threads the truncation verdict through the dedup zip to save a round
    /// trip on the rare intersection of "history longer than one changelog
    /// page" and "sits exactly on a re-anchor boundary", which is not worth
    /// the extra state.
    ///
    /// Test: `search_with_changelog_falls_back_when_embedded_is_truncated`,
    /// `search_with_changelog_skips_fallback_when_embedded_is_complete`,
    /// `search_with_changelog_propagates_fallback_failure`.
    ///
    /// # Errors
    ///
    /// - [`CollectError::Http`] on transport / non-success HTTP responses.
    /// - [`CollectError::Json`] on payload parse failures.
    /// - [`CollectError::IncompleteChangelog`] when a truncated ticket's
    ///   fallback walk cannot retrieve the full history. This aborts the
    ///   search rather than returning a knowingly-short history: writing
    ///   confidently-wrong backfill data is strictly worse than failing.
    pub async fn search_with_changelog(
        &self,
        scope: &SyncScope,
        max_results: usize,
    ) -> Result<ChangelogWalk> {
        let url = format!("{}/rest/api/3/search", self.base_url);
        let fields = vec!["project".to_string(), "updated".to_string()];
        // Every JQL bound this walk emits — the initial scope and every
        // re-anchor — must be rendered in the account's zone, or the window
        // silently lands hours from the instant it encodes. See `jql_time`.
        let tz = self.account_timezone().await?;

        // Slack over the ideal page count absorbs the deduplicated re-reads
        // that window re-anchoring deliberately causes.
        let max_pages = max_results.div_ceil(SEARCH_PAGE_SIZE) * 2 + 8;
        let mut pager = KeysetPager::new(scope.since, max_pages);
        let mut out: Vec<ChangelogIssue> = Vec::new();
        let mut truncated = false;

        loop {
            let remaining = max_results.saturating_sub(out.len());
            if remaining == 0 {
                truncated = true;
                break;
            }
            let page_size = remaining.min(SEARCH_PAGE_SIZE);
            let request = pager.request();
            let jql = build_jql(
                &SyncScope {
                    project_key: scope.project_key.clone(),
                    since: request.since,
                },
                tz,
            )?;
            let body = json!({
                "jql": jql,
                "startAt": request.start_at,
                "maxResults": page_size,
                "fields": fields,
                "expand": ["changelog"],
            });
            debug!(url = %url, %jql, start_at = request.start_at, "POST (with changelog)");
            let parsed: ChangelogSearchResponse =
                with_retry("search_with_changelog", &self.retry, &self.budget, || {
                    self.post(&url, &body)
                })
                .await?;

            // Per-issue conversion, with the truncated-changelog fallback
            // spliced in (issue #4084). This is a loop rather than a `map`
            // because the fallback is an `await`; the truncation verdict is
            // taken from the RAW payload, before `from_api` consumes it.
            let mut issues: Vec<ChangelogIssue> = Vec::with_capacity(parsed.issues.len());
            for api_issue in parsed.issues {
                let truncated = embedded_changelog_is_truncated(&api_issue);
                let key = api_issue.key.clone();
                let mut issue = ChangelogIssue::from_api(api_issue);
                if truncated {
                    issue.transitions = self.fetch_changelog(&key).await?;
                }
                issues.push(issue);
            }
            let items: Vec<PagedItem> = issues.iter().map(|i| (i.key.clone(), i.updated)).collect();
            let step = pager.record_page(&items, page_size);

            for (issue, is_new) in issues.into_iter().zip(step.is_new) {
                if !is_new {
                    continue;
                }
                out.push(issue);
                if out.len() >= max_results {
                    break;
                }
            }
            if !step.more {
                break;
            }
        }
        Ok(ChangelogWalk {
            issues: out,
            offset_paged_minute: pager.offset_paged_minute(),
            truncated,
        })
    }

    /// `POST` a JSON body, carrying this client's credentials. See
    /// [`super::http::post_json`] — the retry wrapper re-invokes this on
    /// every attempt because a `RequestBuilder` is single-use.
    async fn post<T: serde::de::DeserializeOwned>(&self, url: &str, body: &Value) -> Result<T> {
        post_json(&self.client, self.credentials.as_ref(), url, body).await
    }

    /// `GET` and decode, carrying this client's credentials. See
    /// [`super::http::get_json`].
    async fn get<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T> {
        get_json(&self.client, self.credentials.as_ref(), url).await
    }

    /// Fetch every comment on a JIRA issue, paginating in
    /// `COMMENT_PAGE_SIZE` chunks (issue #3966).
    ///
    /// Why: `fact_jira_comment_detail` needs the full comment history per
    /// ticket, not just the first page JIRA embeds inline on `fields.comment`
    /// during a search — the dedicated `/comment` endpoint is the only way
    /// to get all of them.
    ///
    /// What: `GET /rest/api/3/issue/{key}/comment?startAt=&maxResults=`,
    /// looping until a page comes back SHORT of [`COMMENT_PAGE_SIZE`] — the
    /// server's `total` is deliberately not consulted, see
    /// [`super::model::CommentSearchResponse`]. `body_len` is
    /// computed from the raw JSON `body` field:
    /// - plain string body (JIRA Server/DC, or classic-editor Cloud
    ///   comments): UTF-8 byte length of the string.
    /// - Atlassian Document Format object (JIRA Cloud v3 default): byte
    ///   length of the body's JSON-serialized form. This over-counts
    ///   relative to rendered plain text (ADF markup adds bytes), which is a
    ///   known, documented approximation for this first slice — full ADF
    ///   plain-text extraction would need a dedicated renderer.
    ///
    /// Transient failures (429/5xx/timeouts) are retried with backoff (see
    /// [`super::retry`]). This matters more here than anywhere else in the
    /// client: this is the one request issued *per ticket*, so it is the
    /// request a rate limiter throttles first, and a failure here is what
    /// holds the whole run's incremental cursor back.
    ///
    /// Test: `fetch_comments_pages_every_comment_when_total_is_absent`,
    /// `fetch_comments_stops_after_an_empty_page_on_an_exact_multiple`.
    ///
    /// # Errors
    ///
    /// - [`CollectError::Http`] on transport / non-success HTTP responses,
    ///   after the retry budget is exhausted.
    /// - [`CollectError::Json`] on payload parse failures.
    pub async fn fetch_comments(&self, key: &str) -> Result<Vec<JiraComment>> {
        let mut out = Vec::new();
        let mut start_at = 0u64;
        loop {
            let url = format!(
                "{}/rest/api/3/issue/{}/comment?startAt={}&maxResults={}",
                self.base_url, key, start_at, COMMENT_PAGE_SIZE
            );
            debug!(url = %url, "GET");
            let parsed: CommentSearchResponse =
                with_retry("fetch_comments", &self.retry, &self.budget, || {
                    self.get(&url)
                })
                .await?;
            let n = parsed.comments.len();
            for c in parsed.comments {
                if let Some(comment) = JiraComment::from_api(c) {
                    out.push(comment);
                }
            }
            // Terminate on a SHORT PAGE, never on the server's `total`. A
            // response omitting `total` used to read as `0` under
            // `#[serde(default)]` and end the walk after page 1, silently
            // ingesting a prefix of the ticket's comments — and since the
            // fetch still returned `Ok`, the failed-ticket cursor clamp did
            // not protect it: the cursor advanced and the loss was
            // permanent (PR #4067 review round 3). A full page always costs
            // one extra request to discover the end; that is the price of
            // not trusting a field the server may not send.
            if n < COMMENT_PAGE_SIZE {
                break;
            }
            start_at += n as u64;
        }
        Ok(out)
    }
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
