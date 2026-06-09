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

use super::atlassian::{AtlassianCreds, AtlassianProduct};
use super::confluence_parse::parse_section;
use super::{
    ContextSection, ContextSource, ContextSourceError, RetrievalMode, ReviewSubject, TransportErr,
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
    /// What: builds a reqwest client.  Returns `Err(ContextSourceError::Transport)`
    /// if the TLS backend cannot be initialised — surfaces the failure to
    /// `ConfluenceSource::from_config` instead of panicking at startup (closes
    /// #953).
    /// Test: covered transitively by `ConfluenceSource::from_config`.
    pub fn new() -> Result<Self, ContextSourceError> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|e| ContextSourceError::Transport {
                src: "confluence",
                err: super::TransportErr(format!("failed to build HTTP client: {e}")),
            })?;
        Ok(Self { http })
    }
}

impl Default for ReqwestConfluenceTransport {
    /// Construct with default settings; panics only on TLS-backend init failure.
    ///
    /// Why: `Default` cannot return `Result`; kept for compatibility.
    /// Production callers inside `from_config` use `Self::new()` and handle the
    /// error gracefully instead.
    /// What: delegates to `Self::new().expect(…)`.
    /// Test: covered by `ConfluenceSource::from_config`.
    fn default() -> Self {
        Self::new().expect("reqwest::Client::build failed — TLS backend unavailable")
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
            // `expand=body.view` (Fix 2, #599) returns the rendered page HTML so
            // we can embed a stripped excerpt as the snippet body.
            .query(&[
                ("cql", cql),
                ("limit", &limit.to_string()),
                ("expand", "body.view"),
            ])
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

// ─── Disabled transport (fallback on TLS-init failure) ──────────────────────

/// A no-op `ConfluenceTransport` used when TLS backend init fails.
///
/// Why: `from_config` must never panic; when `ReqwestConfluenceTransport::new()`
/// fails the source is constructed with this sentinel so it is permanently
/// disabled and `gather` never calls it (guarded by `is_enabled`).
/// What: always returns an `Api` error — but since `is_enabled()` is false the
/// orchestrator skips `gather` entirely and this code is never reached.
/// Test: covered implicitly by the `from_config` TLS-failure path.
struct DisabledTransport;

#[async_trait]
impl ConfluenceTransport for DisabledTransport {
    async fn search_cql(
        &self,
        _creds: &AtlassianCreds,
        _cql: &str,
        _limit: u32,
    ) -> Result<String, ContextSourceError> {
        Err(ContextSourceError::Transport {
            src: SOURCE_NAME,
            err: super::TransportErr("HTTP transport unavailable (TLS init failed)".to_string()),
        })
    }
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
    /// `effective_enabled`, and attaches the production transport.  If the
    /// reqwest TLS backend cannot be initialised the source is constructed in a
    /// permanently-disabled state (enabled=false) so it degrades gracefully
    /// instead of panicking at startup (closes #953).
    /// Test: `disabled_when_no_creds`.
    pub fn from_config(cfg: &super::SourceConfig) -> Self {
        let creds = AtlassianCreds::from_env_for(AtlassianProduct::Confluence);
        let transport = match ReqwestConfluenceTransport::new() {
            Ok(t) => Box::new(t) as Box<dyn ConfluenceTransport>,
            Err(e) => {
                tracing::error!(
                    "confluence: failed to build HTTP transport (source disabled): {e}"
                );
                return Self {
                    enabled: false,
                    mode: cfg.mode,
                    creds: None,
                    transport: Box::new(DisabledTransport),
                };
            }
        };
        let enabled = cfg.effective_enabled(creds.is_some());
        Self {
            enabled,
            mode: cfg.mode,
            creds,
            transport,
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
    ///
    /// Relevance note: the live REST path orders by recency
    /// (`lastmodified DESC`) only as a tiebreaker; Confluence's CQL has no native
    /// relevance score for `text ~`.  True semantic relevance ranking for
    /// Confluence arrives via the indexed/semantic mode in PR-B (the APEX /
    /// atlassian vector index), which is the incumbent's primary Confluence path.
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
