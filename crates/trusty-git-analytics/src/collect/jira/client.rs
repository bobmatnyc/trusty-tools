//! Minimal JIRA REST client for fetching individual issues.

use std::sync::Mutex;

use chrono::{DateTime, Utc};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, USER_AGENT};
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::{debug, warn};

use crate::collect::env_expand::expand_env_var;
use crate::collect::errors::{CollectError, Result};
use crate::core::config::JiraConfig;

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
    credentials: Option<(String, String)>,
    /// Default project key for filtered queries.
    project_key: String,
    /// Cached story-point custom field key (e.g. `customfield_10016`).
    /// `None` = uncached; `Some(None)` = discovered to be absent;
    /// `Some(Some(_))` = discovered key.
    story_point_field: Mutex<Option<Option<String>>>,
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

/// Wire shape of a JQL search response.
#[derive(Debug, Deserialize)]
struct SearchResponse {
    issues: Vec<ApiIssue>,
    #[serde(default)]
    total: u64,
    #[serde(rename = "startAt", default)]
    start_at: u64,
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
            (Some(u), Some(t)) => Some((expand_env_var(u), expand_env_var(t))),
            _ => None,
        };

        Ok(Self {
            client,
            base_url: base,
            credentials,
            project_key: config.project_key.clone().unwrap_or_default(),
            story_point_field: Mutex::new(None),
        })
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
            start_at = parsed.start_at + n as u64;
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
    /// KNOWN LIMITATION: JIRA's search-embedded changelog is itself paged
    /// (`changelog.total` vs `changelog.histories.len()`); a ticket with a
    /// history longer than one changelog page will have its oldest
    /// transitions truncated on this path. A full per-issue
    /// `GET /issue/{key}/changelog` walk would close this gap but is
    /// deferred — see issue #3966 tracking comment.
    ///
    /// # Errors
    ///
    /// - [`CollectError::Http`] on transport / non-success HTTP responses.
    /// - [`CollectError::Json`] on payload parse failures.
    pub async fn search_with_changelog(
        &self,
        jql: &str,
        max_results: usize,
    ) -> Result<Vec<ChangelogIssue>> {
        let url = format!("{}/rest/api/3/search", self.base_url);
        let fields = vec!["project".to_string(), "updated".to_string()];

        let mut out: Vec<ChangelogIssue> = Vec::new();
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
                "expand": ["changelog"],
            });
            debug!(url = %url, %jql, start_at, "POST (with changelog)");
            let mut req = self.client.post(&url).json(&body);
            if let Some((user, token)) = &self.credentials {
                req = req.basic_auth(user, Some(token));
            }
            let resp = req.send().await?.error_for_status()?;
            let parsed: ChangelogSearchResponse = resp.json().await?;
            let n = parsed.issues.len();
            for issue in parsed.issues {
                out.push(ChangelogIssue::from_api(issue));
                if out.len() >= max_results {
                    break;
                }
            }
            if n < page_size {
                break;
            }
            start_at = parsed.start_at + n as u64;
            if start_at >= parsed.total {
                break;
            }
        }
        Ok(out)
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
    /// looping until the server reports no more pages. `body_len` is
    /// computed from the raw JSON `body` field:
    /// - plain string body (JIRA Server/DC, or classic-editor Cloud
    ///   comments): UTF-8 byte length of the string.
    /// - Atlassian Document Format object (JIRA Cloud v3 default): byte
    ///   length of the body's JSON-serialized form. This over-counts
    ///   relative to rendered plain text (ADF markup adds bytes), which is a
    ///   known, documented approximation for this first slice — full ADF
    ///   plain-text extraction would need a dedicated renderer.
    ///
    /// # Errors
    ///
    /// - [`CollectError::Http`] on transport / non-success HTTP responses.
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
            let mut req = self.client.get(&url);
            if let Some((user, token)) = &self.credentials {
                req = req.basic_auth(user, Some(token));
            }
            let resp = req.send().await?.error_for_status()?;
            let parsed: CommentSearchResponse = resp.json().await?;
            let n = parsed.comments.len();
            for c in parsed.comments {
                if let Some(comment) = JiraComment::from_api(c) {
                    out.push(comment);
                }
            }
            if n == 0 {
                break;
            }
            start_at += n as u64;
            if start_at >= parsed.total {
                break;
            }
        }
        Ok(out)
    }
}

/// One parsed status transition from a JIRA changelog history entry
/// (issue #3966).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JiraTransition {
    /// Status before the transition. `None` when this is the first
    /// changelog entry touching the `status` field on the ticket.
    pub from_status: Option<String>,
    /// Status after the transition.
    pub to_status: String,
    /// Display name of the author who made the transition, when JIRA
    /// reports one (some automation-triggered transitions omit an author).
    pub author: Option<String>,
    /// Timestamp of the transition.
    pub created: DateTime<Utc>,
}

/// A JIRA issue plus its parsed status-transition changelog (issue #3966).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangelogIssue {
    /// Issue key, e.g. `PROJ-123`.
    pub key: String,
    /// JIRA project key. Falls back to the `key` prefix (everything before
    /// the last `-`) when the `project` field is absent from the response,
    /// which should not happen in practice but keeps this infallible.
    pub project_key: String,
    /// The issue's `updated` timestamp, when present. Drives the
    /// incremental-sync cursor (see `collect::jira::sync::next_cursor`).
    pub updated: Option<DateTime<Utc>>,
    /// `status`-field transitions extracted from the changelog, oldest
    /// first (JIRA returns histories in chronological order).
    pub transitions: Vec<JiraTransition>,
}

impl ChangelogIssue {
    fn from_api(api: ChangelogApiIssue) -> Self {
        let project_key = api
            .fields
            .project
            .map(|p| p.key)
            .unwrap_or_else(|| project_key_from_issue_key(&api.key));
        let updated = api.fields.updated.as_deref().and_then(parse_jira_datetime);
        let mut transitions = Vec::new();
        if let Some(changelog) = api.changelog {
            for history in changelog.histories {
                let author = history.author.map(|a| a.display_name);
                let Some(created) = parse_jira_datetime(&history.created) else {
                    warn!(
                        key = %api.key,
                        created = %history.created,
                        "unparseable changelog history timestamp; skipping this history entry"
                    );
                    continue;
                };
                for item in history.items {
                    if item.field != "status" {
                        continue;
                    }
                    let Some(to_status) = item.to_string else {
                        continue;
                    };
                    transitions.push(JiraTransition {
                        from_status: item.from_string,
                        to_status,
                        author: author.clone(),
                        created,
                    });
                }
            }
        }
        Self {
            key: api.key,
            project_key,
            updated,
            transitions,
        }
    }
}

/// Fallback project-key extraction from an issue key (`PROJ-123` -> `PROJ`).
///
/// Used only when a search response omits the `project` field, which is not
/// expected in normal operation but keeps [`ChangelogIssue::from_api`]
/// infallible rather than panicking on a malformed response.
fn project_key_from_issue_key(key: &str) -> String {
    key.rsplit_once('-')
        .map(|(prefix, _)| prefix.to_string())
        .unwrap_or_else(|| key.to_string())
}

/// Parse a JIRA-flavoured ISO8601 timestamp.
///
/// JIRA Cloud emits offsets without a colon (e.g.
/// `2026-01-01T00:00:00.000+0000`); strict RFC3339 parsers reject these. Try
/// chrono's `%+` (RFC3339) first, then fall back to JIRA's
/// `%Y-%m-%dT%H:%M:%S%.3f%z` shape.
///
/// Duplicated (rather than shared) from the equivalent helper in
/// `commands/incidents/mod.rs` — that copy is private to its module and
/// this crate does not currently have a shared date-parsing utility module;
/// see issue #3966 for context. A future cleanup could hoist both into
/// `collect::jira` or a new `collect::datetime` module.
fn parse_jira_datetime(s: &str) -> Option<DateTime<Utc>> {
    if let Ok(d) = DateTime::parse_from_rfc3339(s) {
        return Some(d.with_timezone(&Utc));
    }
    chrono::DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.3f%z")
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

/// One JIRA comment, reduced to what `fact_jira_comment_detail` needs
/// (issue #3966).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JiraComment {
    /// Comment ID (unique per-instance).
    pub id: String,
    /// Display name of the comment's author, when JIRA reports one.
    pub author: Option<String>,
    /// Comment creation timestamp.
    pub created: DateTime<Utc>,
    /// Length of the comment body — see [`JiraClient::fetch_comments`] doc
    /// comment for the ADF-vs-plain-text caveat.
    pub body_len: i64,
}

impl JiraComment {
    /// Convert one wire-form comment, or `None` if its `created` timestamp
    /// is unparseable (mirrors `ChangelogIssue::from_api`'s per-entry skip
    /// behavior — one malformed record must not abort the whole batch, nor
    /// fabricate a fake timestamp that would corrupt freshness/ordering
    /// queries downstream).
    fn from_api(api: ApiComment) -> Option<Self> {
        let created = match parse_jira_datetime(&api.created) {
            Some(c) => c,
            None => {
                warn!(
                    comment_id = %api.id,
                    created = %api.created,
                    "unparseable comment timestamp; skipping this comment"
                );
                return None;
            }
        };
        let body_len = match &api.body {
            Value::String(s) => s.len() as i64,
            other => serde_json::to_string(other).map(|s| s.len()).unwrap_or(0) as i64,
        };
        Some(Self {
            id: api.id,
            author: api.author.map(|a| a.display_name),
            created,
            body_len,
        })
    }
}

/// Wire shape of `POST /rest/api/3/search` with `expand=changelog`.
#[derive(Debug, Deserialize)]
struct ChangelogSearchResponse {
    issues: Vec<ChangelogApiIssue>,
    #[serde(default)]
    total: u64,
    #[serde(rename = "startAt", default)]
    start_at: u64,
}

#[derive(Debug, Deserialize)]
struct ChangelogApiIssue {
    key: String,
    fields: ChangelogApiFields,
    #[serde(default)]
    changelog: Option<ApiChangelog>,
}

#[derive(Debug, Deserialize, Default)]
struct ChangelogApiFields {
    #[serde(default)]
    project: Option<ApiProjectRef>,
    #[serde(default)]
    updated: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApiProjectRef {
    key: String,
}

#[derive(Debug, Deserialize)]
struct ApiChangelog {
    histories: Vec<ApiChangelogHistory>,
}

#[derive(Debug, Deserialize)]
struct ApiChangelogHistory {
    #[serde(default)]
    author: Option<ApiAuthor>,
    created: String,
    items: Vec<ApiChangelogItem>,
}

#[derive(Debug, Deserialize)]
struct ApiAuthor {
    #[serde(rename = "displayName")]
    display_name: String,
}

#[derive(Debug, Deserialize)]
struct ApiChangelogItem {
    field: String,
    #[serde(rename = "fromString", default)]
    from_string: Option<String>,
    #[serde(rename = "toString", default)]
    to_string: Option<String>,
}

/// Wire shape of `GET /rest/api/3/issue/{key}/comment`.
#[derive(Debug, Deserialize)]
struct CommentSearchResponse {
    comments: Vec<ApiComment>,
    #[serde(default)]
    total: u64,
}

#[derive(Debug, Deserialize)]
struct ApiComment {
    id: String,
    #[serde(default)]
    author: Option<ApiAuthor>,
    created: String,
    #[serde(default = "default_comment_body")]
    body: Value,
}

/// Default `body` value when JIRA omits it entirely (should not happen in
/// practice, but keeps deserialization infallible).
fn default_comment_body() -> Value {
    Value::String(String::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Confirm a JQL search response shape parses end-to-end.
    ///
    /// Why: pagination logic depends on `total` and `startAt` fields; if
    /// JIRA renames either, our loop terminates incorrectly.
    /// What: parse a representative search payload with one issue.
    /// Test: assert `total`, `startAt`, and inner issue fields all populate.
    #[test]
    fn jira_search_response_deserializes() {
        let json = r#"{
            "startAt": 0,
            "total": 1,
            "issues": [
                {
                    "key": "PROJ-1",
                    "fields": {
                        "summary": "Fix bug",
                        "status": {"name": "Done"},
                        "issuetype": {"name": "Bug"},
                        "customfield_10016": 5.0
                    }
                }
            ]
        }"#;
        let resp: SearchResponse = serde_json::from_str(json).expect("parses");
        assert_eq!(resp.total, 1);
        assert_eq!(resp.start_at, 0);
        assert_eq!(resp.issues.len(), 1);
        let issue = JiraClient::convert_issue(
            resp.issues.into_iter().next().expect("one"),
            Some("customfield_10016"),
        );
        assert_eq!(issue.key, "PROJ-1");
        assert_eq!(issue.summary, "Fix bug");
        assert_eq!(issue.status, "Done");
        assert_eq!(issue.issue_type, "Bug");
        assert_eq!(issue.story_points, Some(5.0));
    }

    /// Confirm field descriptor wire shape deserializes.
    ///
    /// Why: cache discovery hinges on this exact shape.
    /// What: parse a representative `/rest/api/3/field` element.
    /// Test: assert both fields extract.
    #[test]
    fn field_descriptor_deserializes() {
        let json = r#"[
            {"id": "customfield_10016", "name": "Story Points"},
            {"id": "summary", "name": "Summary"}
        ]"#;
        let fields: Vec<FieldDescriptor> = serde_json::from_str(json).expect("parses");
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].id, "customfield_10016");
        assert_eq!(fields[0].name, "Story Points");
    }

    /// Story points should be `None` when the custom field is absent.
    ///
    /// Why: not every JIRA instance has a configured story-point field;
    /// missing fields must degrade gracefully.
    /// What: convert an issue payload that omits the custom field.
    /// Test: assert `story_points` is `None`.
    #[test]
    fn convert_issue_returns_none_when_field_missing() {
        let json = r#"{
            "key": "PROJ-2",
            "fields": {
                "summary": "x",
                "status": {"name": "Open"},
                "issuetype": {"name": "Task"}
            }
        }"#;
        let api: ApiIssue = serde_json::from_str(json).expect("parses");
        let issue = JiraClient::convert_issue(api, Some("customfield_10016"));
        assert!(issue.story_points.is_none());
    }

    // ---- Changelog / comment tests (issue #3966) ----------------------

    /// A changelog search response with one status transition parses into a
    /// single `JiraTransition` with the expected `from`/`to`/author/created.
    #[test]
    fn changelog_search_response_parses_status_transition() {
        let json = r#"{
            "startAt": 0,
            "total": 1,
            "issues": [
                {
                    "key": "PROJ-1",
                    "fields": {"project": {"key": "PROJ"}},
                    "changelog": {
                        "histories": [
                            {
                                "author": {"displayName": "Jane Doe"},
                                "created": "2026-01-01T10:00:00.000+0000",
                                "items": [
                                    {"field": "status", "fromString": "To Do", "toString": "In Progress"}
                                ]
                            }
                        ]
                    }
                }
            ]
        }"#;
        let resp: ChangelogSearchResponse = serde_json::from_str(json).expect("parses");
        assert_eq!(resp.issues.len(), 1);
        let issue = ChangelogIssue::from_api(resp.issues.into_iter().next().expect("one"));
        assert_eq!(issue.key, "PROJ-1");
        assert_eq!(issue.project_key, "PROJ");
        assert_eq!(issue.transitions.len(), 1);
        let t = &issue.transitions[0];
        assert_eq!(t.from_status.as_deref(), Some("To Do"));
        assert_eq!(t.to_status, "In Progress");
        assert_eq!(t.author.as_deref(), Some("Jane Doe"));
    }

    /// Non-`status` changelog items (e.g. `assignee`) must be dropped —
    /// only status transitions belong in `fact_ticket_transitions`.
    #[test]
    fn changelog_ignores_non_status_fields() {
        let json = r#"{
            "key": "PROJ-2",
            "fields": {"project": {"key": "PROJ"}},
            "changelog": {
                "histories": [
                    {
                        "created": "2026-01-01T10:00:00.000+0000",
                        "items": [
                            {"field": "assignee", "fromString": "Alice", "toString": "Bob"},
                            {"field": "status", "fromString": "Open", "toString": "Closed"}
                        ]
                    }
                ]
            }
        }"#;
        let api: ChangelogApiIssue = serde_json::from_str(json).expect("parses");
        let issue = ChangelogIssue::from_api(api);
        assert_eq!(
            issue.transitions.len(),
            1,
            "assignee changes must be filtered out"
        );
        assert_eq!(issue.transitions[0].to_status, "Closed");
    }

    /// The very first status-touching history entry has no `fromString`
    /// (the ticket's creation state) — this must surface as `from_status:
    /// None`, not be dropped or error.
    #[test]
    fn changelog_initial_transition_has_no_from_status() {
        let json = r#"{
            "key": "PROJ-3",
            "fields": {"project": {"key": "PROJ"}},
            "changelog": {
                "histories": [
                    {
                        "created": "2026-01-01T10:00:00.000+0000",
                        "items": [
                            {"field": "status", "toString": "Open"}
                        ]
                    }
                ]
            }
        }"#;
        let api: ChangelogApiIssue = serde_json::from_str(json).expect("parses");
        let issue = ChangelogIssue::from_api(api);
        assert_eq!(issue.transitions.len(), 1);
        assert!(issue.transitions[0].from_status.is_none());
        assert_eq!(issue.transitions[0].to_status, "Open");
    }

    /// When the `project` field is absent, the project key falls back to
    /// the issue key's prefix rather than panicking.
    #[test]
    fn changelog_falls_back_to_key_prefix_when_project_missing() {
        let json = r#"{
            "key": "INFRA-42",
            "fields": {},
            "changelog": {"histories": []}
        }"#;
        let api: ChangelogApiIssue = serde_json::from_str(json).expect("parses");
        let issue = ChangelogIssue::from_api(api);
        assert_eq!(issue.project_key, "INFRA");
    }

    /// An unparseable changelog `created` timestamp must skip that history
    /// entry (no transitions extracted from it) rather than panicking or
    /// erroring the whole batch.
    #[test]
    fn changelog_skips_unparseable_timestamp() {
        let json = r#"{
            "key": "PROJ-4",
            "fields": {"project": {"key": "PROJ"}},
            "changelog": {
                "histories": [
                    {"created": "not-a-date", "items": [{"field": "status", "toString": "Done"}]}
                ]
            }
        }"#;
        let api: ChangelogApiIssue = serde_json::from_str(json).expect("parses");
        let issue = ChangelogIssue::from_api(api);
        assert!(issue.transitions.is_empty());
    }

    /// A plain-string comment body's length is measured directly in bytes.
    #[test]
    fn comment_body_len_for_plain_string() {
        let json = r#"{"id": "1001", "author": {"displayName": "Jane Doe"}, "created": "2026-01-01T10:00:00.000+0000", "body": "hello world"}"#;
        let api: ApiComment = serde_json::from_str(json).expect("parses");
        let comment = JiraComment::from_api(api).expect("valid timestamp parses");
        assert_eq!(comment.id, "1001");
        assert_eq!(comment.author.as_deref(), Some("Jane Doe"));
        assert_eq!(comment.body_len, "hello world".len() as i64);
    }

    /// An Atlassian Document Format (object) comment body is measured as
    /// the byte length of its JSON-serialized form (documented
    /// approximation — see `JiraClient::fetch_comments` doc comment).
    #[test]
    fn comment_body_len_for_adf_object() {
        let json = r#"{
            "id": "1002",
            "created": "2026-01-01T10:00:00.000+0000",
            "body": {"type": "doc", "version": 1, "content": []}
        }"#;
        let api: ApiComment = serde_json::from_str(json).expect("parses");
        let comment = JiraComment::from_api(api).expect("valid timestamp parses");
        assert!(comment.author.is_none());
        let expected_len =
            serde_json::to_string(&json!({"type": "doc", "version": 1, "content": []}))
                .unwrap()
                .len() as i64;
        assert_eq!(comment.body_len, expected_len);
    }

    /// A comment with an unparseable `created` timestamp must be skipped
    /// (returns `None`) rather than fabricating a fallback timestamp that
    /// would corrupt downstream ordering/freshness queries.
    #[test]
    fn comment_with_unparseable_timestamp_is_skipped() {
        let json = r#"{"id": "1003", "created": "not-a-date", "body": "x"}"#;
        let api: ApiComment = serde_json::from_str(json).expect("parses");
        assert!(JiraComment::from_api(api).is_none());
    }

    /// `GET /issue/{key}/comment` response shape parses end-to-end,
    /// including `total` for pagination termination.
    #[test]
    fn comment_search_response_deserializes() {
        let json = r#"{
            "startAt": 0,
            "maxResults": 100,
            "total": 2,
            "comments": [
                {"id": "1", "created": "2026-01-01T00:00:00.000+0000", "body": "a"},
                {"id": "2", "created": "2026-01-02T00:00:00.000+0000", "body": "b"}
            ]
        }"#;
        let resp: CommentSearchResponse = serde_json::from_str(json).expect("parses");
        assert_eq!(resp.total, 2);
        assert_eq!(resp.comments.len(), 2);
    }

    /// `parse_jira_datetime` accepts both strict RFC3339 and JIRA's
    /// colonless-offset flavour.
    #[test]
    fn parse_jira_datetime_accepts_both_shapes() {
        assert!(parse_jira_datetime("2026-01-01T00:00:00Z").is_some());
        assert!(parse_jira_datetime("2026-01-01T00:00:00.000+0000").is_some());
        assert!(parse_jira_datetime("garbage").is_none());
    }
}
