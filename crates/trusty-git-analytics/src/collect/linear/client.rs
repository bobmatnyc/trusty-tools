//! Linear GraphQL API client for issue enrichment.
//!
//! Uses the Linear GraphQL API (<https://api.linear.app/graphql>).
//! Authentication: `Authorization: <api_key>` header (no "Bearer" prefix).
//! The key comes from `linear.api_key` in config, or — when config is silent —
//! from `trusty_common::credentials::resolve_key` (#5983). There is no Linear
//! CLI on this path.
//!
//! Issue identifiers are matched against commit messages with the pattern
//! `[A-Z][A-Z0-9]{0,9}-\d+` (e.g. `ENG-123`, `FE-456`).

use std::collections::HashSet;

use reqwest::Client;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use trusty_common::credentials::scrub_secrets;

use crate::collect::errors::{CollectError, Result};
use crate::core::config::LinearConfig;
use crate::core::db::Database;

/// HTTP `User-Agent` string sent on every request.
const USER_AGENT_VALUE: &str = "trusty-git-analytics/0.1";

/// Linear GraphQL endpoint.
const LINEAR_GRAPHQL_URL: &str = "https://api.linear.app/graphql";

/// Characters of a Linear-authored payload carried into operator-visible text.
///
/// Linear's auth rejection is under 300 bytes; the cap only stops a large
/// HTML error page from being pasted into `stats.errors` or a warn log.
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
        LinearClient {
            client: Client::new(),
            api_key: key.to_string(),
            endpoint: PROBE_ENDPOINT.to_string(),
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
}
