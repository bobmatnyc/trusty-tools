//! Linear GraphQL API client for issue enrichment.
//!
//! Uses the Linear GraphQL API (<https://api.linear.app/graphql>).
//! Authentication: `Authorization: <api_key>` header (no "Bearer" prefix).
//!
//! Issue identifiers are matched against commit messages with the pattern
//! `[A-Z][A-Z0-9]{0,9}-\d+` (e.g. `ENG-123`, `FE-456`).

use std::collections::HashSet;

use reqwest::Client;
use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::collect::errors::{CollectError, Result};
use crate::core::config::LinearConfig;
use crate::core::db::Database;

/// HTTP `User-Agent` string sent on every request.
const USER_AGENT_VALUE: &str = "trusty-git-analytics/0.1";

/// Linear GraphQL endpoint.
const LINEAR_GRAPHQL_URL: &str = "https://api.linear.app/graphql";

/// Characters of a non-success response body carried into the error message.
///
/// Linear's auth rejection is under 300 bytes; the cap only stops a large
/// HTML error page from being pasted into `stats.errors`.
const MAX_ERROR_BODY_CHARS: usize = 500;

/// A Linear issue fetched from the API.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
}

/// Async Linear GraphQL client.
#[derive(Debug)]
pub struct LinearClient {
    client: Client,
    api_key: String,
    /// GraphQL endpoint every request is sent to.
    ///
    /// Always [`LINEAR_GRAPHQL_URL`] in production. Tests override it via
    /// [`LinearClient::with_endpoint`] so a mock server can answer, which is
    /// what makes the #5665 auth-failure arm assertable without a live key.
    endpoint: String,
}

impl LinearClient {
    /// Create a new Linear client from config.
    ///
    /// Resolves `${LINEAR_API_KEY}` env var substitution in the `api_key` field.
    ///
    /// # Errors
    ///
    /// - [`CollectError::Config`] if `api_key` is missing or resolves to empty.
    /// - [`CollectError::Http`] if the HTTP client cannot be built.
    pub fn new(config: &LinearConfig) -> Result<Self> {
        let raw_key = config.api_key.as_deref().unwrap_or("");
        let api_key = expand_env_var(raw_key);
        if api_key.is_empty() {
            return Err(CollectError::Config("Linear api_key is required".into()));
        }
        let client = Client::builder()
            .user_agent(USER_AGENT_VALUE)
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(CollectError::Http)?;
        Ok(Self {
            client,
            api_key,
            endpoint: LINEAR_GRAPHQL_URL.to_string(),
        })
    }

    /// Build a client that talks to `endpoint` instead of Linear itself.
    ///
    /// Test-only seam (#5665): the HTTP status arm of [`Self::fetch_issue`] is
    /// only reachable through a server that answers non-2xx, and asserting it
    /// against the live API would need a revoked key in CI.
    #[cfg(test)]
    fn with_endpoint(config: &LinearConfig, endpoint: impl Into<String>) -> Result<Self> {
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
    /// `fetch_issue_returns_none_for_absent_issue`.
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
                message: truncate_body(&body),
            });
        }

        let json: serde_json::Value = resp.json().await.map_err(CollectError::Http)?;

        // GraphQL errors are returned with 200 OK; check for errors array.
        if let Some(errors) = json.get("errors").and_then(|v| v.as_array()) {
            if !errors.is_empty() {
                tracing::warn!(
                    identifier = %identifier,
                    errors = ?errors,
                    "Linear GraphQL errors"
                );
                return Ok(None);
            }
        }

        let issue_val = &json["data"]["issue"];
        if issue_val.is_null() {
            return Ok(None);
        }

        Ok(Some(LinearIssue {
            identifier: issue_val["identifier"]
                .as_str()
                .unwrap_or(identifier)
                .to_string(),
            title: issue_val["title"].as_str().unwrap_or("").to_string(),
            state: issue_val["state"]["name"]
                .as_str()
                .unwrap_or("Unknown")
                .to_string(),
            team: issue_val["team"]["name"]
                .as_str()
                .unwrap_or("Unknown")
                .to_string(),
            assignee: issue_val["assignee"]["displayName"]
                .as_str()
                .map(String::from),
            priority: issue_val["priority"].as_u64().unwrap_or(0) as u8,
            url: issue_val["url"].as_str().unwrap_or("").to_string(),
        }))
    }

    /// Extract Linear issue identifiers from a commit message.
    ///
    /// Matches patterns like `ENG-123`, `FE-456`, `PROJ-789`.
    /// Returns a deduplicated list of identifiers found (order preserved).
    pub fn extract_issue_ids(message: &str) -> Vec<String> {
        let re = regex::Regex::new(r"\b([A-Z][A-Z0-9]{0,9}-\d+)\b").expect("valid regex");
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for cap in re.captures_iter(message) {
            let id = cap[1].to_string();
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
}

/// Persist Linear issues to the database (free function for reuse from tests
/// and contexts where no [`LinearClient`] instance is available).
///
/// # Errors
///
/// Propagates [`crate::core::TgaError::DbError`] on SQL failures.
pub fn store_linear_issues(db: &Database, issues: &[LinearIssue]) -> crate::core::Result<usize> {
    let conn = db.connection();
    let fetched_at = chrono::Utc::now().to_rfc3339();
    let mut count = 0usize;
    for issue in issues {
        let team_key = issue.identifier.split('-').next().unwrap_or("").to_string();
        conn.execute(
            "INSERT OR REPLACE INTO linear_issues \
             (identifier, title, state, team, team_key, assignee, priority, url, fetched_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
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
            ],
        )?;
        count += 1;
    }
    Ok(count)
}

/// Clip a response body to [`MAX_ERROR_BODY_CHARS`] on a character boundary.
///
/// Test: `truncate_body_clips_long_input`, `truncate_body_keeps_short_input`.
fn truncate_body(body: &str) -> String {
    let trimmed = body.trim();
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

    #[test]
    fn new_rejects_missing_api_key() {
        let cfg = LinearConfig::default();
        let err = LinearClient::new(&cfg).expect_err("should reject empty key");
        match err {
            CollectError::Config(msg) => assert!(msg.contains("api_key")),
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

    #[test]
    fn truncate_body_keeps_short_input() {
        assert_eq!(truncate_body("  {\"errors\":[]}  "), "{\"errors\":[]}");
    }

    #[test]
    fn truncate_body_clips_long_input() {
        let out = truncate_body(&"x".repeat(MAX_ERROR_BODY_CHARS + 50));
        assert_eq!(out.chars().count(), MAX_ERROR_BODY_CHARS + 1);
        assert!(out.ends_with('…'));
    }

    /// Build a client pointed at `endpoint` with a syntactically valid key.
    fn mock_client(endpoint: &str) -> LinearClient {
        let cfg = LinearConfig {
            api_key: Some("lin_api_test".into()),
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
}
