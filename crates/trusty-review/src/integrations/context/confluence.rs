//! LIVE Confluence context source (Phase 6, #550).
//!
//! Why: product/architecture/decision docs in Confluence often constrain how a
//! change *should* be made.  Surfacing the matching pages lets the reviewer
//! check the diff against documented design intent — the same Stage-5 retrieval
//! code-intelligence does.  Confluence is LIVE (RESOLVED on #550), querying the
//! API at review time rather than pre-indexing.
//!
//! What: `ConfluenceSource` implements `ContextSource` in `Live` mode by issuing
//! a CQL `text ~ "<keywords>"` search against `{base}/wiki/rest/api/content/search`
//! using the shared `AtlassianCreds` basic-auth header (same site root as JIRA,
//! with the `/wiki` REST prefix), then mapping each page to a
//! `## Related Confluence docs` bullet (title, space, web link).  The HTTP call
//! goes through an injectable `ConfluenceTransport` for network-free testing.
//!
//! Fail-open: missing creds → `NotConfigured` (skip, logged once); transport /
//! API / parse error → orchestrator logs and drops the section.  Never blocks
//! the review.
//!
//! Test: `query_builds_cql`, `parse_pages_to_section`, `disabled_when_no_creds`,
//! `semantic_mode_errors`, `gather_with_fake_transport` in this module.

use async_trait::async_trait;
use serde::Deserialize;

use super::atlassian::{AtlassianCreds, AtlassianProduct};
use super::{
    ContextSection, ContextSnippet, ContextSource, ContextSourceError, RetrievalMode,
    ReviewSubject, TransportErr,
};

/// Source identifier used in logs, config keys, and error messages.
const SOURCE_NAME: &str = "confluence";

/// Max Confluence pages to embed in the section.
const MAX_RESULTS: u32 = 5;

/// Max diff identifiers folded into the keyword query.
const MAX_QUERY_IDENTIFIERS: usize = 6;

// ─── Transport seam ─────────────────────────────────────────────────────────

/// Injectable HTTP transport for the Confluence CQL search call.
///
/// Why: same rationale as `JiraTransport` — the query + parse logic must be
/// testable without a live Confluence.
/// What: one async method performing the CQL search, returning the raw JSON body
/// (or a typed failure).
/// Test: implemented by `ReqwestConfluenceTransport` (prod) and a fake in tests.
#[async_trait]
pub trait ConfluenceTransport: Send + Sync {
    /// GET a CQL search and return the raw response body on 2xx.
    async fn search_cql(
        &self,
        creds: &AtlassianCreds,
        cql: &str,
        limit: u32,
    ) -> Result<String, ContextSourceError>;
}

/// Production `ConfluenceTransport` over reqwest.
///
/// Why: the default transport for real reviews.
/// What: GETs `{base}/wiki/rest/api/content/search?cql=...&limit=...` with basic
/// auth, mapping non-2xx to `Api` and transport failures to `Transport`.
/// Test: exercised via the fake in `gather_with_fake_transport`.
pub struct ReqwestConfluenceTransport {
    http: reqwest::Client,
}

impl ReqwestConfluenceTransport {
    /// Construct with a default 15s-timeout client.
    ///
    /// Why: bound the worst-case latency of an enrichment call.
    /// What: builds a reqwest client; panics only on TLS-backend init failure.
    /// Test: covered transitively by `ConfluenceSource::from_config`.
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("reqwest::Client::build failed — TLS backend unavailable");
        Self { http }
    }
}

impl Default for ReqwestConfluenceTransport {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ConfluenceTransport for ReqwestConfluenceTransport {
    async fn search_cql(
        &self,
        creds: &AtlassianCreds,
        cql: &str,
        limit: u32,
    ) -> Result<String, ContextSourceError> {
        let url = format!("{}/wiki/rest/api/content/search", creds.base_url);
        let resp = self
            .http
            .get(&url)
            .query(&[("cql", cql), ("limit", &limit.to_string())])
            .header("Authorization", creds.basic_auth_header())
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| ContextSourceError::Transport {
                src: SOURCE_NAME,
                err: TransportErr(format!("GET {url}: {e}")),
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

// ─── JSON shapes ────────────────────────────────────────────────────────────

/// Top-level Confluence content-search response.
#[derive(Debug, Deserialize)]
struct ConfluenceSearchResponse {
    #[serde(default)]
    results: Vec<ConfluencePage>,
}

/// One Confluence page (content) result.
#[derive(Debug, Deserialize)]
struct ConfluencePage {
    #[serde(default)]
    title: String,
    #[serde(default)]
    space: Option<ConfluenceSpace>,
    #[serde(default, rename = "_links")]
    links: Option<ConfluenceLinks>,
}

/// The page's space (we render its name/key).
#[derive(Debug, Deserialize)]
struct ConfluenceSpace {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    key: Option<String>,
}

/// The page's `_links` block (we use `webui` to build the canonical URL).
#[derive(Debug, Deserialize)]
struct ConfluenceLinks {
    #[serde(default)]
    webui: Option<String>,
}

// ─── The source ─────────────────────────────────────────────────────────────

/// LIVE Confluence context source.
///
/// Why: implements the `ContextSource` seam for Confluence; constructed by the
/// runner when enabled (config + Atlassian creds present).
/// What: holds `enabled`, `mode`, optional `AtlassianCreds`, and the injected
/// transport.  Auto-disabled when creds are absent.
/// Test: `disabled_when_no_creds`, `gather_with_fake_transport`.
pub struct ConfluenceSource {
    enabled: bool,
    mode: RetrievalMode,
    creds: Option<AtlassianCreds>,
    transport: Box<dyn ConfluenceTransport>,
}

impl ConfluenceSource {
    /// Build from resolved config using canonical + Confluence-scoped env creds.
    ///
    /// Why: the runner wires the source without knowing credential mechanics.
    /// What: resolves `AtlassianCreds::from_env_for(Confluence)`, computes
    /// `effective_enabled`, and attaches the production transport.
    /// Test: `disabled_when_no_creds`.
    pub fn from_config(cfg: &super::SourceConfig) -> Self {
        let creds = AtlassianCreds::from_env_for(AtlassianProduct::Confluence);
        let enabled = cfg.effective_enabled(creds.is_some());
        Self {
            enabled,
            mode: cfg.mode,
            creds,
            transport: Box::new(ReqwestConfluenceTransport::new()),
        }
    }

    /// Construct directly (tests inject a fake transport / creds).
    ///
    /// Why: drive `gather` without env or network.
    /// What: stores the provided fields verbatim.
    /// Test: `gather_with_fake_transport`, `semantic_mode_errors`.
    pub fn new(
        enabled: bool,
        mode: RetrievalMode,
        creds: Option<AtlassianCreds>,
        transport: Box<dyn ConfluenceTransport>,
    ) -> Self {
        Self {
            enabled,
            mode,
            creds,
            transport,
        }
    }

    /// Build the CQL string from the subject's keyword query.
    ///
    /// Why: Confluence full-text search uses `text ~ "..."`; one builder keeps
    /// quoting consistent and testable, and scopes results to pages.
    /// What: returns `type=page AND text ~ "<keywords>" ORDER BY lastmodified DESC`
    /// (double-quotes stripped from keywords).  `None` when no keyword signal.
    /// Test: `query_builds_cql`.
    fn build_cql(subject: &ReviewSubject) -> Option<String> {
        let keywords = subject.keyword_query(MAX_QUERY_IDENTIFIERS);
        let keywords = keywords.replace('"', " ");
        let keywords = keywords.trim();
        if keywords.is_empty() {
            return None;
        }
        Some(format!(
            "type=page AND text ~ \"{keywords}\" ORDER BY lastmodified DESC"
        ))
    }

    /// Parse a Confluence search body into a `ContextSection`.
    ///
    /// Why: separate parsing from the network call for unit-testability.
    /// What: maps each page to a `ContextSnippet` (title, subtitle = space name
    /// or key, link = `{base}/wiki{webui}`), wrapped in a `Related Confluence
    /// docs` section.
    /// Test: `parse_pages_to_section`.
    fn parse_section(body: &str, base_url: &str) -> Result<ContextSection, ContextSourceError> {
        let resp: ConfluenceSearchResponse =
            serde_json::from_str(body).map_err(|e| ContextSourceError::Parse {
                src: SOURCE_NAME,
                detail: e.to_string(),
            })?;
        let snippets = resp
            .results
            .into_iter()
            .map(|page| {
                let subtitle = page
                    .space
                    .and_then(|s| s.name.or(s.key))
                    .map(|s| format!("space: {s}"));
                let link = page
                    .links
                    .and_then(|l| l.webui)
                    .map(|webui| format!("{base_url}/wiki{webui}"));
                ContextSnippet {
                    title: page.title,
                    subtitle,
                    body: None,
                    link,
                }
            })
            .collect();
        Ok(ContextSection {
            heading: "Related Confluence docs".to_string(),
            snippets,
        })
    }
}

#[async_trait]
impl ContextSource for ConfluenceSource {
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
        let Some(cql) = Self::build_cql(subject) else {
            return Ok(ContextSection {
                heading: "Related Confluence docs".to_string(),
                snippets: Vec::new(),
            });
        };
        let body = self.transport.search_cql(creds, &cql, MAX_RESULTS).await?;
        Self::parse_section(&body, &creds.base_url)
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

    struct FakeConfluence {
        body: Result<String, ()>,
    }

    #[async_trait]
    impl ConfluenceTransport for FakeConfluence {
        async fn search_cql(
            &self,
            _creds: &AtlassianCreds,
            _cql: &str,
            _limit: u32,
        ) -> Result<String, ContextSourceError> {
            self.body.clone().map_err(|_| ContextSourceError::Api {
                src: SOURCE_NAME,
                status: 502,
                body: "down".to_string(),
            })
        }
    }

    fn subject() -> ReviewSubject {
        ReviewSubject {
            owner: "acme".to_string(),
            repo: "backend".to_string(),
            title: "Auth design".to_string(),
            identifiers: vec!["Session".to_string()],
            ..Default::default()
        }
    }

    #[test]
    fn query_builds_cql() {
        let cql = ConfluenceSource::build_cql(&subject()).expect("has signal");
        assert!(cql.contains("type=page"));
        assert!(cql.contains("text ~ \"Auth design Session\""));
        assert!(cql.contains("ORDER BY lastmodified DESC"));
    }

    #[test]
    fn query_none_without_signal() {
        assert!(ConfluenceSource::build_cql(&ReviewSubject::default()).is_none());
    }

    #[test]
    fn parse_pages_to_section() {
        let body = r#"{
            "results": [
                {"title": "Auth Architecture", "space": {"name": "Engineering"},
                 "_links": {"webui": "/spaces/ENG/pages/123/Auth"}},
                {"title": "Sessions", "space": {"key": "ENG"}}
            ]
        }"#;
        let section = ConfluenceSource::parse_section(body, "https://acme.atlassian.net").unwrap();
        assert_eq!(section.heading, "Related Confluence docs");
        assert_eq!(section.snippets.len(), 2);
        assert_eq!(section.snippets[0].title, "Auth Architecture");
        assert_eq!(
            section.snippets[0].subtitle.as_deref(),
            Some("space: Engineering")
        );
        assert_eq!(
            section.snippets[0].link.as_deref(),
            Some("https://acme.atlassian.net/wiki/spaces/ENG/pages/123/Auth")
        );
        // Second page has only a space key and no link.
        assert_eq!(section.snippets[1].subtitle.as_deref(), Some("space: ENG"));
        assert!(section.snippets[1].link.is_none());
    }

    #[test]
    fn parse_error_on_garbage() {
        let r = ConfluenceSource::parse_section("xx", "https://acme.atlassian.net");
        assert!(matches!(r, Err(ContextSourceError::Parse { .. })));
    }

    #[tokio::test]
    async fn disabled_when_no_creds() {
        let src = ConfluenceSource::new(
            true,
            RetrievalMode::Live,
            None,
            Box::new(FakeConfluence {
                body: Ok("{}".into()),
            }),
        );
        let r = src.gather(&subject()).await;
        assert!(matches!(r, Err(ContextSourceError::NotConfigured { .. })));
    }

    #[tokio::test]
    async fn semantic_mode_errors() {
        let src = ConfluenceSource::new(
            true,
            RetrievalMode::Semantic,
            Some(creds()),
            Box::new(FakeConfluence {
                body: Ok("{}".into()),
            }),
        );
        let r = src.gather(&subject()).await;
        assert!(matches!(
            r,
            Err(ContextSourceError::SemanticNotImplemented { src: "confluence" })
        ));
    }

    #[tokio::test]
    async fn gather_with_fake_transport() {
        let body = r#"{"results":[{"title":"Design Doc","space":{"name":"Eng"}}]}"#;
        let src = ConfluenceSource::new(
            true,
            RetrievalMode::Live,
            Some(creds()),
            Box::new(FakeConfluence {
                body: Ok(body.to_string()),
            }),
        );
        let section = src.gather(&subject()).await.expect("ok");
        assert_eq!(section.snippets.len(), 1);
        assert_eq!(section.snippets[0].title, "Design Doc");
    }
}
