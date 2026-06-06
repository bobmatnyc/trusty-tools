//! `web_search` and `fetch_url` tools backed by Brave Search + reqwest.
//!
//! Why: Research agents need to gather external context. Brave Search has a
//! simple REST API and generous free tier; `fetch_url` complements it by
//! letting the agent read a specific URL's text content.
//! What:
//!   - `BraveSearchProvider` implements `SearchProvider` against the Brave
//!     Search JSON endpoint.
//!   - `BraveSearchTool` implements `ToolExecutor` by wrapping a
//!     `SearchProvider` (so tests can substitute a stub).
//!   - `FetchUrlTool` fetches a URL, strips HTML tags via `scraper`, and
//!     returns a truncated text body.
//!
//! If `BRAVE_API_KEY` is absent, `web_search` returns a graceful error string
//! describing how to configure it rather than panicking.
//! Test: Unit tests below exercise `BraveSearchTool` with a stub provider
//! and verify tag/formatting of the output string.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use scraper::Html;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::tools::traits::{SearchProvider, SearchResult, ToolExecutor, ToolResult};

const BRAVE_SEARCH_URL: &str = "https://api.search.brave.com/res/v1/web/search";
const FETCH_MAX_CHARS: usize = 8000;

/// Brave Search API-backed `SearchProvider`.
pub struct BraveSearchProvider {
    api_key: String,
    client: reqwest::Client,
}

impl BraveSearchProvider {
    /// Construct from an API key.
    ///
    /// Why: Explicit injection lets callers load the key from env or config
    /// themselves, keeping this type free of env-var lookups.
    /// What: Stores the key and builds a reqwest client with a 15s timeout.
    /// Test: Instantiate with a dummy key; `search()` is exercised via
    /// integration tests (not unit).
    pub fn new(api_key: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .expect("failed to build reqwest client");
        Self { api_key, client }
    }
}

#[derive(Debug, Deserialize)]
struct BraveResponse {
    web: Option<BraveWeb>,
}

#[derive(Debug, Deserialize)]
struct BraveWeb {
    results: Vec<BraveResultRaw>,
}

#[derive(Debug, Deserialize)]
struct BraveResultRaw {
    title: String,
    url: String,
    description: String,
}

#[async_trait]
impl SearchProvider for BraveSearchProvider {
    async fn search(&self, query: &str, n: usize) -> Result<Vec<SearchResult>> {
        let resp = self
            .client
            .get(BRAVE_SEARCH_URL)
            .header("X-Subscription-Token", &self.api_key)
            .header("Accept", "application/json")
            .query(&[("q", query), ("count", &n.to_string())])
            .send()
            .await
            .context("Brave search request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Brave search returned HTTP {status}: {body}");
        }

        let parsed: BraveResponse = resp
            .json()
            .await
            .context("failed to parse Brave search response body as JSON")?;

        let results = parsed
            .web
            .map(|w| w.results)
            .unwrap_or_default()
            .into_iter()
            .take(n)
            .map(|r| SearchResult {
                title: r.title,
                url: r.url,
                snippet: strip_html(&r.description),
            })
            .collect();
        Ok(results)
    }
}

/// The `web_search` tool.
///
/// Holds an `Arc<dyn SearchProvider>` so tests can inject a stub.
pub struct BraveSearchTool {
    provider: Option<Arc<dyn SearchProvider>>,
}

impl BraveSearchTool {
    /// Construct with an explicit provider (used in tests).
    #[allow(dead_code)]
    pub fn with_provider(provider: Arc<dyn SearchProvider>) -> Self {
        Self {
            provider: Some(provider),
        }
    }

    /// Construct from env (`BRAVE_API_KEY`). If absent, the tool will return
    /// a graceful error message at dispatch time rather than failing here.
    ///
    /// Why: We want the tool to be registerable even without a key so the
    /// rest of the registry still works; the error is deferred until call.
    /// What: Reads `BRAVE_API_KEY`; if set, builds a `BraveSearchProvider`.
    /// Test: Unset var -> `execute()` returns an error string naming the var.
    pub fn from_env() -> Self {
        match std::env::var("BRAVE_API_KEY") {
            Ok(key) if !key.is_empty() => Self {
                provider: Some(Arc::new(BraveSearchProvider::new(key)) as Arc<dyn SearchProvider>),
            },
            _ => Self { provider: None },
        }
    }
}

#[async_trait]
impl ToolExecutor for BraveSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "web_search",
                "description": "Search the web via Brave Search. Returns a list of result titles, URLs, and snippets.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "The search query."
                        },
                        "n": {
                            "type": "integer",
                            "description": "Maximum number of results (default 5, max 10).",
                            "minimum": 1,
                            "maximum": 10
                        }
                    },
                    "required": ["query"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some(provider) = &self.provider else {
            return ToolResult::err(
                "web_search unavailable: BRAVE_API_KEY env var is not set. Configure it in .env.local.",
            );
        };

        let Some(query) = args.get("query").and_then(Value::as_str) else {
            return ToolResult::err("web_search: missing 'query'");
        };
        let n = args
            .get("n")
            .and_then(Value::as_u64)
            .map(|v| v as usize)
            .unwrap_or(5)
            .min(10);

        match provider.search(query, n).await {
            Ok(results) => ToolResult::ok(format_results(&results)),
            Err(e) => ToolResult::err(format!("web_search failed: {e:#}")),
        }
    }
}

/// `fetch_url` — GET a URL and return its text (HTML stripped, truncated).
pub struct FetchUrlTool {
    client: reqwest::Client,
}

impl FetchUrlTool {
    /// Build with a default reqwest client (15s timeout).
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .user_agent("trusty-agents/0.1 (research-agent)")
                .build()
                .expect("failed to build reqwest client"),
        }
    }
}

impl Default for FetchUrlTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolExecutor for FetchUrlTool {
    fn name(&self) -> &str {
        "fetch_url"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "fetch_url",
                "description": "Fetch a URL and return its text content (HTML tags stripped, truncated to ~8000 chars).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "url": {
                            "type": "string",
                            "description": "The absolute URL to fetch."
                        }
                    },
                    "required": ["url"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some(url) = args.get("url").and_then(Value::as_str) else {
            return ToolResult::err("fetch_url: missing 'url'");
        };
        let resp = match self.client.get(url).send().await {
            Ok(r) => r,
            Err(e) => return ToolResult::err(format!("failed to fetch {url}: {e:#}")),
        };
        if !resp.status().is_success() {
            return ToolResult::err(format!("HTTP {} while fetching {url}", resp.status()));
        }
        match resp.text().await {
            Ok(body) => {
                let stripped = strip_html(&body);
                ToolResult::ok(truncate(&stripped, FETCH_MAX_CHARS))
            }
            Err(e) => ToolResult::err(format!("failed to read response body: {e:#}")),
        }
    }
}

/// Strip HTML tags using `scraper`, falling back to the raw string on parse
/// failure. Collapses whitespace.
fn strip_html(input: &str) -> String {
    let doc = Html::parse_document(input);
    let text: String = doc.root_element().text().collect::<Vec<_>>().join(" ");
    // Collapse whitespace runs.
    let mut out = String::with_capacity(text.len());
    let mut last_ws = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !last_ws {
                out.push(' ');
            }
            last_ws = true;
        } else {
            out.push(ch);
            last_ws = false;
        }
    }
    out.trim().to_string()
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let truncated: String = s.chars().take(max).collect();
    format!(
        "{truncated}\n...[truncated, {} chars total]",
        s.chars().count()
    )
}

fn format_results(results: &[SearchResult]) -> String {
    if results.is_empty() {
        return "No results.".to_string();
    }
    let mut out = String::new();
    for (i, r) in results.iter().enumerate() {
        out.push_str(&format!(
            "{}. {}\n   {}\n   {}\n",
            i + 1,
            r.title,
            r.url,
            r.snippet
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubProvider;

    #[async_trait]
    impl SearchProvider for StubProvider {
        async fn search(&self, _q: &str, _n: usize) -> Result<Vec<SearchResult>> {
            Ok(vec![SearchResult {
                title: "Stub Title".into(),
                url: "https://example.com".into(),
                snippet: "Example snippet.".into(),
            }])
        }
    }

    #[tokio::test]
    async fn brave_search_tool_formats_results() {
        let tool = BraveSearchTool::with_provider(Arc::new(StubProvider));
        let out = tool.execute(json!({"query": "rust async"})).await;
        assert!(!out.is_error());
        assert!(out.content().contains("Stub Title"));
        assert!(out.content().contains("https://example.com"));
    }

    #[tokio::test]
    async fn web_search_without_key_returns_graceful_message() {
        // Simulate missing provider.
        let tool = BraveSearchTool { provider: None };
        let out = tool.execute(json!({"query": "x"})).await;
        assert!(out.is_error());
        assert!(out.content().contains("BRAVE_API_KEY"));
    }

    #[test]
    fn strip_html_basic() {
        let s = strip_html("<p>hello <b>world</b></p>");
        assert!(s.contains("hello"));
        assert!(s.contains("world"));
        assert!(!s.contains('<'));
    }

    #[test]
    fn truncate_respects_limit() {
        let s = "a".repeat(100);
        let out = truncate(&s, 10);
        assert!(out.starts_with("aaaaaaaaaa"));
        assert!(out.contains("truncated"));
    }
}
