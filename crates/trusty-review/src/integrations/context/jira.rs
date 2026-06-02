//! LIVE JIRA context source (Phase 6, #550).
//!
//! Why: when a PR implements a JIRA ticket, surfacing that ticket's summary +
//! status in the reviewer prompt gives the model the *intent* behind the change
//! — turning "is this code correct?" into "does this code do what the ticket
//! asked?".  This is the same Stage-5 retrieval code-intelligence performs.
//!
//! What: `JiraSource` implements `ContextSource` in `Live` mode.  It first scans
//! the PR title + body for JIRA ticket keys (`PROJ-123`); when any are found it
//! does an EXACT `issueKey in (...)` lookup (parity with the incumbent's primary
//! path, `pr_review_service.py:4068`), otherwise it falls back to a JQL
//! `text ~ "<keywords>"` search.  Either way it queries `{base}/rest/api/3/search/jql`
//! using the shared `AtlassianCreds` basic-auth header, then maps each issue to a
//! `## Related JIRA tickets` bullet (key, summary, status, description excerpt,
//! browse link).  The HTTP call goes through an injectable `JiraTransport` trait
//! so the query + parse logic is unit-tested against canned JSON with no network;
//! the ticket-ID + ADF/parse helpers live in the sibling `jira_parse` module.
//!
//! Fail-open: missing creds → `NotConfigured` (skip, logged once); any transport
//! / API / parse error → the orchestrator logs and drops the section.  A JIRA
//! outage NEVER blocks the review (#550 supplementary-vs-required distinction).
//!
//! Test: `query_builds_jql_keyword`, `query_builds_jql_ticket_ids`,
//! `disabled_when_no_creds`, `semantic_mode_errors`, `gather_with_fake_transport`
//! in this module; parsing in `jira_parse`.

use async_trait::async_trait;

use super::atlassian::{AtlassianCreds, AtlassianProduct};
use super::jira_parse::{extract_ticket_ids, parse_section};
use super::{
    ContextSection, ContextSource, ContextSourceError, RetrievalMode, ReviewSubject, TransportErr,
};

/// Source identifier used in logs, config keys, and error messages.
const SOURCE_NAME: &str = "jira";

/// Max JIRA issues to embed in the section (keeps the prompt bounded).
const MAX_RESULTS: u32 = 5;

/// Max diff identifiers folded into the keyword query.
const MAX_QUERY_IDENTIFIERS: usize = 6;

// ─── Transport seam ─────────────────────────────────────────────────────────

/// Injectable HTTP transport for the JIRA search call.
///
/// Why: the source's value is its query-construction + response-parsing logic,
/// which must be unit-testable without a live JIRA.  Hiding the single network
/// call behind this trait lets tests inject canned JSON.
/// What: one async method that performs the JQL search and returns the raw JSON
/// body (or a `TransportErr`/`Api` failure).  The real impl uses reqwest.
/// Test: implemented by `ReqwestJiraTransport` (prod) and fakes in tests.
#[async_trait]
pub trait JiraTransport: Send + Sync {
    /// POST a JQL search and return the raw response body on 2xx.
    async fn search_jql(
        &self,
        creds: &AtlassianCreds,
        jql: &str,
        max_results: u32,
    ) -> Result<String, ContextSourceError>;
}

/// Production `JiraTransport` over reqwest.
///
/// Why: the default transport for real reviews; isolates the only network call.
/// What: POSTs `{base}/rest/api/3/search/jql` with basic auth and a JSON body
/// `{ jql, maxResults, fields }`, mapping non-2xx to `Api` and transport
/// failures to `Transport`.
/// Test: not unit-tested (requires network); exercised via the fake in
/// `gather_with_fake_transport`.
pub struct ReqwestJiraTransport {
    /// Shared reqwest client (connection pool reuse).
    http: reqwest::Client,
}

impl ReqwestJiraTransport {
    /// Construct with a default 15s-timeout client.
    ///
    /// Why: external enrichment must not hang a review; a tight timeout bounds
    /// the worst case (the orchestrator also wraps each source in a timeout).
    /// What: builds a reqwest client; panics only on TLS-backend init failure
    /// (a programmer/environment error, never a runtime condition).
    /// Test: covered transitively by `JiraSource::with_default_transport`.
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("reqwest::Client::build failed — TLS backend unavailable");
        Self { http }
    }
}

impl Default for ReqwestJiraTransport {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl JiraTransport for ReqwestJiraTransport {
    async fn search_jql(
        &self,
        creds: &AtlassianCreds,
        jql: &str,
        max_results: u32,
    ) -> Result<String, ContextSourceError> {
        let url = format!("{}/rest/api/3/search/jql", creds.base_url);
        let body = serde_json::json!({
            "jql": jql,
            "maxResults": max_results,
            // `description` (Fix 2, #599) is embedded as the snippet body so the
            // reviewer sees the ticket's intent, not just its one-line summary.
            "fields": ["summary", "status", "description"],
        });
        let resp = self
            .http
            .post(&url)
            .header("Authorization", creds.basic_auth_header())
            .header("Accept", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| ContextSourceError::Transport {
                src: SOURCE_NAME,
                err: TransportErr(format!("POST {url}: {e}")),
            })?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| ContextSourceError::Transport {
                src: SOURCE_NAME,
                err: TransportErr(format!("read body of {url}: {e}")),
            })?;
        if !status.is_success() {
            return Err(ContextSourceError::Api {
                src: SOURCE_NAME,
                status: status.as_u16(),
                body: text,
            });
        }
        Ok(text)
    }
}

// ─── The source ─────────────────────────────────────────────────────────────

/// LIVE JIRA context source.
///
/// Why: implements the `ContextSource` seam for JIRA; constructed by the runner
/// when the JIRA source is enabled (config + Atlassian creds present).
/// What: holds the resolved `enabled` flag, the retrieval `mode`, the optional
/// `AtlassianCreds`, and the injected transport.  `enabled` is false when creds
/// are absent (auto-disable), so `gather` returns `NotConfigured` only if forced
/// on without creds.
/// Test: `disabled_when_no_creds`, `gather_with_fake_transport`.
pub struct JiraSource {
    enabled: bool,
    mode: RetrievalMode,
    creds: Option<AtlassianCreds>,
    transport: Box<dyn JiraTransport>,
}

impl JiraSource {
    /// Build from resolved config using the canonical + JIRA-scoped env creds.
    ///
    /// Why: the runner wires the source from `ContextSourcesConfig` without
    /// knowing the credential mechanics; this resolves them and computes the
    /// auto-disable (no creds → disabled unless explicitly enabled).
    /// What: resolves `AtlassianCreds::from_env_for(Jira)`, computes
    /// `effective_enabled(creds_present)`, and attaches the production transport.
    /// Test: `disabled_when_no_creds`.
    pub fn from_config(cfg: &super::SourceConfig) -> Self {
        let creds = AtlassianCreds::from_env_for(AtlassianProduct::Jira);
        let enabled = cfg.effective_enabled(creds.is_some());
        Self {
            enabled,
            mode: cfg.mode,
            creds,
            transport: Box::new(ReqwestJiraTransport::new()),
        }
    }

    /// Construct directly (used by tests to inject a fake transport / creds).
    ///
    /// Why: tests need to drive `gather` without env vars or a network.
    /// What: stores the provided enabled/mode/creds/transport verbatim.
    /// Test: used by `gather_with_fake_transport`, `semantic_mode_errors`.
    pub fn new(
        enabled: bool,
        mode: RetrievalMode,
        creds: Option<AtlassianCreds>,
        transport: Box<dyn JiraTransport>,
    ) -> Self {
        Self {
            enabled,
            mode,
            creds,
            transport,
        }
    }

    /// Build the JQL string for the subject, preferring exact ticket-ID lookup.
    ///
    /// Why: Duetto PR titles/descriptions conventionally name the ticket key, so
    /// an exact `issueKey in (...)` lookup is both more precise and cheaper than
    /// full-text guessing — this is the incumbent's PRIMARY JIRA path
    /// (`pr_review_service.py:4068`, `:4076`).  Only when no key is present do we
    /// fall back to the keyword `text ~ "..."` search.
    /// What: scans `title + "\n" + body` for ticket keys (deduped, first-seen);
    /// if any, returns `issueKey in (PROJ-1, PROJ-2) ORDER BY updated DESC`
    /// (capped at `MAX_RESULTS` keys); otherwise builds the keyword JQL
    /// `text ~ "<keywords>" ORDER BY updated DESC` (double-quotes stripped).
    /// Returns `None` only when there is neither a ticket key nor a keyword
    /// signal (caller skips the call).
    ///
    /// Relevance note: the live REST path orders by recency (`updated DESC`) as a
    /// tiebreaker; true semantic relevance ranking for JIRA arrives via the
    /// indexed/semantic mode in PR-B (the APEX/atlassian vector index).
    /// Test: `query_builds_jql_keyword`, `query_builds_jql_ticket_ids`,
    /// `query_ticket_ids_beat_keywords`, `query_none_without_signal`.
    fn build_jql(subject: &ReviewSubject) -> Option<String> {
        // Fix 1: ticket-ID priority path. Scan title AND body for keys.
        let scan = format!("{}\n{}", subject.title, subject.body);
        let ids = extract_ticket_ids(&scan);
        if !ids.is_empty() {
            let keys = ids
                .iter()
                .take(MAX_RESULTS as usize)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            return Some(format!("issueKey in ({keys}) ORDER BY updated DESC"));
        }

        // Keyword fallback.
        let keywords = subject.keyword_query(MAX_QUERY_IDENTIFIERS);
        let keywords = keywords.replace('"', " ");
        let keywords = keywords.trim();
        if keywords.is_empty() {
            return None;
        }
        Some(format!("text ~ \"{keywords}\" ORDER BY updated DESC"))
    }
}

#[async_trait]
impl ContextSource for JiraSource {
    fn name(&self) -> &'static str {
        SOURCE_NAME
    }

    fn is_enabled(&self) -> bool {
        self.enabled
    }

    fn mode(&self) -> RetrievalMode {
        self.mode
    }

    async fn gather(&self, subject: &ReviewSubject) -> Result<ContextSection, ContextSourceError> {
        // PR-A implements only the Live backend.
        if self.mode == RetrievalMode::Semantic {
            return Err(ContextSourceError::SemanticNotImplemented { src: SOURCE_NAME });
        }
        let creds = self
            .creds
            .as_ref()
            .ok_or(ContextSourceError::NotConfigured {
                src: SOURCE_NAME,
                reason: "ATLASSIAN_API_TOKEN / ATLASSIAN_EMAIL / ATLASSIAN_URL not set".to_string(),
            })?;
        let Some(jql) = Self::build_jql(subject) else {
            // No keyword signal (e.g. local-diff with empty title) — nothing to
            // search.  Empty section, not an error.
            return Ok(ContextSection {
                heading: "Related JIRA tickets".to_string(),
                snippets: Vec::new(),
            });
        };
        let body = self.transport.search_jql(creds, &jql, MAX_RESULTS).await?;
        parse_section(&body, &creds.base_url)
    }
}

// ─── Unit tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn creds() -> AtlassianCreds {
        AtlassianCreds {
            email: "bob@acme.com".to_string(),
            token: "tok".to_string(), // pragma: allowlist secret
            base_url: "https://acme.atlassian.net".to_string(),
        }
    }

    /// A `JiraTransport` returning a fixed body (or error) without network.
    struct FakeJira {
        body: Result<String, ()>,
    }

    #[async_trait]
    impl JiraTransport for FakeJira {
        async fn search_jql(
            &self,
            _creds: &AtlassianCreds,
            _jql: &str,
            _max: u32,
        ) -> Result<String, ContextSourceError> {
            self.body.clone().map_err(|_| ContextSourceError::Api {
                src: SOURCE_NAME,
                status: 500,
                body: "boom".to_string(),
            })
        }
    }

    fn subject() -> ReviewSubject {
        ReviewSubject {
            owner: "acme".to_string(),
            repo: "backend".to_string(),
            title: "Add token refresh".to_string(),
            identifiers: vec!["TokenStore".to_string()],
            ..Default::default()
        }
    }

    #[test]
    fn query_builds_jql_keyword() {
        // No ticket key in title/body → keyword fallback path.
        let jql = JiraSource::build_jql(&subject()).expect("has signal");
        assert!(jql.contains("text ~ \"Add token refresh TokenStore\""));
        assert!(jql.contains("ORDER BY updated DESC"));
    }

    #[test]
    fn query_builds_jql_ticket_ids() {
        // Fix 1: a ticket key in the title → exact issueKey lookup, NOT keyword.
        let subj = ReviewSubject {
            title: "PROJ-42 add token refresh".to_string(),
            identifiers: vec!["TokenStore".to_string()],
            ..Default::default()
        };
        let jql = JiraSource::build_jql(&subj).expect("has signal");
        assert_eq!(jql, "issueKey in (PROJ-42) ORDER BY updated DESC");
        assert!(!jql.contains("text ~"));
    }

    #[test]
    fn query_ticket_ids_scan_body_too() {
        // A key only in the PR body is still found (title has no key).
        let subj = ReviewSubject {
            title: "Add token refresh".to_string(),
            body: "Implements PROJ-7 and PROJ-8.".to_string(),
            ..Default::default()
        };
        let jql = JiraSource::build_jql(&subj).expect("has signal");
        assert_eq!(jql, "issueKey in (PROJ-7, PROJ-8) ORDER BY updated DESC");
    }

    #[test]
    fn query_ticket_ids_dedup_and_capped() {
        // Duplicates collapse; the list is capped at MAX_RESULTS keys.
        let ids: Vec<String> = (1..=10).map(|n| format!("PROJ-{n}")).collect();
        let subj = ReviewSubject {
            title: format!("{} PROJ-1", ids.join(" ")),
            ..Default::default()
        };
        let jql = JiraSource::build_jql(&subj).expect("has signal");
        // Exactly MAX_RESULTS (5) keys, comma-separated.
        let inner = jql
            .trim_start_matches("issueKey in (")
            .split(')')
            .next()
            .unwrap();
        assert_eq!(inner.split(", ").count(), MAX_RESULTS as usize);
    }

    #[test]
    fn query_strips_quotes() {
        let subj = ReviewSubject {
            title: "Add \"quoted\" thing".to_string(),
            ..Default::default()
        };
        let jql = JiraSource::build_jql(&subj).unwrap();
        // No raw double-quotes inside the keyword payload would break the JQL.
        assert!(!jql.contains("\"quoted\""));
    }

    #[test]
    fn query_none_without_signal() {
        let subj = ReviewSubject::default();
        assert!(JiraSource::build_jql(&subj).is_none());
    }

    #[tokio::test]
    async fn disabled_when_no_creds() {
        // Forced-on but no creds → NotConfigured (fail-open at orchestrator).
        let src = JiraSource::new(
            true,
            RetrievalMode::Live,
            None,
            Box::new(FakeJira {
                body: Ok("{}".into()),
            }),
        );
        let r = src.gather(&subject()).await;
        assert!(matches!(r, Err(ContextSourceError::NotConfigured { .. })));
    }

    #[tokio::test]
    async fn semantic_mode_errors() {
        let src = JiraSource::new(
            true,
            RetrievalMode::Semantic,
            Some(creds()),
            Box::new(FakeJira {
                body: Ok("{}".into()),
            }),
        );
        let r = src.gather(&subject()).await;
        assert!(matches!(
            r,
            Err(ContextSourceError::SemanticNotImplemented { src: "jira" })
        ));
    }

    #[tokio::test]
    async fn gather_with_fake_transport() {
        let body =
            r#"{"issues":[{"key":"PROJ-7","fields":{"summary":"Fix","status":{"name":"Open"}}}]}"#;
        let src = JiraSource::new(
            true,
            RetrievalMode::Live,
            Some(creds()),
            Box::new(FakeJira {
                body: Ok(body.to_string()),
            }),
        );
        let section = src.gather(&subject()).await.expect("ok");
        assert_eq!(section.snippets.len(), 1);
        assert_eq!(section.snippets[0].title, "PROJ-7 — Fix");
    }

    #[tokio::test]
    async fn gather_propagates_api_error_for_logging() {
        let src = JiraSource::new(
            true,
            RetrievalMode::Live,
            Some(creds()),
            Box::new(FakeJira { body: Err(()) }),
        );
        let r = src.gather(&subject()).await;
        assert!(matches!(
            r,
            Err(ContextSourceError::Api { status: 500, .. })
        ));
    }
}
