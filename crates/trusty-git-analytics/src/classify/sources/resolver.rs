//! Per-run external source resolver with in-memory caching.
//!
//! Why: a single `tga classify` run over 15k commits may reference hundreds
//! of unique JIRA/GitHub tickets. Re-fetching the same ticket for every commit
//! that mentions it would flood the external APIs with duplicate requests and
//! be slow. The resolver caches every external lookup for the lifetime of one
//! classification run (in-memory; never persisted to disk).
//!
//! What: [`ExternalSourceResolver`] wraps a `reqwest::Client` and a per-source
//! cache. [`ExternalSourceResolver::resolve`] accepts a commit message, extracts
//! ticket keys, checks the cache, fetches only misses, and returns the
//! highest-priority [`super::ExternalSignal`] found across all configured sources.
//!
//! Test: covered by `tests::resolver_uses_cached_result_on_second_call` and the
//! mock-HTTP integration tests.

use std::collections::HashMap;
use std::sync::Mutex;

use tracing::debug;

use super::{
    confluence, datadog,
    github_issues::{self, GitHubRef},
    jira, linear, shortcut, ExternalSignal, SourceConfig,
};

/// In-memory cache type alias (key → Option<ExternalSignal>).
type Cache = HashMap<String, Option<ExternalSignal>>;

/// Per-source internal state.
enum SourceState {
    Jira {
        config: super::JiraSourceConfig,
        cache: Mutex<Cache>,
        /// Test-only base URL override (points at a wiremock server).
        base_url_override: Option<String>,
    },
    GithubIssues {
        config: super::GithubIssuesSourceConfig,
        cache: Mutex<Cache>,
        /// Test-only API base URL override.
        api_base_override: Option<String>,
    },
    Linear {
        config: super::LinearSourceConfig,
        cache: Mutex<Cache>,
        /// Test-only GraphQL base URL override.
        api_base_override: Option<String>,
    },
    Shortcut {
        config: super::ShortcutSourceConfig,
        cache: Mutex<Cache>,
        /// Test-only REST base URL override.
        api_base_override: Option<String>,
    },
    Confluence {
        config: super::ConfluenceSourceConfig,
        cache: Mutex<Cache>,
        /// Test-only base URL override.
        api_base_override: Option<String>,
    },
    Datadog {
        config: super::DatadogSourceConfig,
        cache: Mutex<Cache>,
        /// Test-only API base URL override.
        api_base_override: Option<String>,
    },
}

/// Per-run resolver that dispatches commit messages to configured external
/// sources and caches the results.
///
/// Why: concentrating the dispatch, caching, and priority logic here keeps the
/// pipeline free of HTTP concerns and makes the resolver trivially testable
/// via the override setters.
/// What: holds one [`SourceState`] per configured source, a shared
/// `reqwest::Client`, and exposes [`Self::resolve`] which returns the first
/// non-`None` signal across all sources (JIRA sources checked before GitHub
/// sources, following the priority model in issue #260).
/// Test: see `tests::*` in this module; end-to-end integration is in
/// `pipeline::tests`.
pub struct ExternalSourceResolver {
    client: reqwest::Client,
    sources: Vec<SourceState>,
}

impl ExternalSourceResolver {
    /// Build a resolver from a slice of [`SourceConfig`]s.
    ///
    /// Why: the pipeline constructs the resolver once per run from the config;
    /// construction is cheap (no HTTP calls).
    /// What: builds one [`SourceState`] per config entry, sharing a single
    /// `reqwest::Client` across all sources.
    /// Test: see `tests::resolver_builds_from_empty_sources`.
    pub fn new(sources: &[SourceConfig]) -> Self {
        let client = reqwest::Client::new();
        let states = sources
            .iter()
            .map(|cfg| match cfg {
                SourceConfig::Jira(j) => SourceState::Jira {
                    config: j.clone(),
                    cache: Mutex::new(HashMap::new()),
                    base_url_override: None,
                },
                SourceConfig::GithubIssues(g) => SourceState::GithubIssues {
                    config: g.clone(),
                    cache: Mutex::new(HashMap::new()),
                    api_base_override: None,
                },
                SourceConfig::Linear(l) => SourceState::Linear {
                    config: l.clone(),
                    cache: Mutex::new(HashMap::new()),
                    api_base_override: None,
                },
                SourceConfig::Shortcut(s) => SourceState::Shortcut {
                    config: s.clone(),
                    cache: Mutex::new(HashMap::new()),
                    api_base_override: None,
                },
                SourceConfig::Confluence(c) => SourceState::Confluence {
                    config: c.clone(),
                    cache: Mutex::new(HashMap::new()),
                    api_base_override: None,
                },
                SourceConfig::Datadog(d) => SourceState::Datadog {
                    config: d.clone(),
                    cache: Mutex::new(HashMap::new()),
                    api_base_override: None,
                },
            })
            .collect();
        Self {
            client,
            sources: states,
        }
    }

    /// Resolve a commit message against all configured sources.
    ///
    /// Why: the pipeline calls this once per commit; having a single entry
    /// point that walks all sources in priority order avoids duplicating the
    /// dispatch logic.
    /// What: extracts JIRA keys and GitHub refs from `message`; for each
    /// source in order (JIRA before GitHub), checks the cache and fetches
    /// misses; returns the first non-`None` signal found. Returns `None` if
    /// no source matched.
    /// Test: covered by `tests::resolver_returns_jira_signal_when_configured`
    /// and `tests::resolver_returns_none_when_no_keys`.
    pub async fn resolve(&self, message: &str) -> Option<ExternalSignal> {
        for state in &self.sources {
            if let Some(signal) = self.resolve_source(message, state).await {
                return Some(signal);
            }
        }
        None
    }

    async fn resolve_source(&self, message: &str, state: &SourceState) -> Option<ExternalSignal> {
        match state {
            SourceState::Jira {
                config,
                cache,
                base_url_override,
            } => {
                let keys = jira::extract_jira_keys(message);
                if keys.is_empty() {
                    return None;
                }
                // Filter by project_keys if configured.
                let filtered: Vec<String> = if config.project_keys.is_empty() {
                    keys
                } else {
                    keys.into_iter()
                        .filter(|k| {
                            config
                                .project_keys
                                .iter()
                                .any(|pk| k.starts_with(&format!("{pk}-")))
                        })
                        .collect()
                };
                if filtered.is_empty() {
                    return None;
                }

                // Separate cached from uncached.
                let (cached_hits, misses): (Vec<_>, Vec<_>) = {
                    let guard = cache.lock().expect("jira cache lock");
                    filtered
                        .iter()
                        .partition(|k| guard.contains_key(k.as_str()))
                };

                // Return immediately if a cached hit has a signal.
                {
                    let guard = cache.lock().expect("jira cache lock");
                    for k in &cached_hits {
                        if let Some(Some(sig)) = guard.get(k.as_str()) {
                            debug!(key = k.as_str(), "jira cache hit");
                            return Some(sig.clone());
                        }
                    }
                }

                // Fetch misses.
                let fetched = jira::fetch_issues_batch(
                    &self.client,
                    config,
                    &misses.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                    base_url_override.as_deref(),
                )
                .await;

                // Populate cache.
                {
                    let mut guard = cache.lock().expect("jira cache lock");
                    for (k, sig) in &fetched {
                        guard.insert(k.clone(), sig.clone());
                    }
                }

                // Return first hit from freshly-fetched results.
                for k in &misses {
                    if let Some(Some(sig)) = fetched.get(k.as_str()) {
                        return Some(sig.clone());
                    }
                }
                None
            }

            SourceState::GithubIssues {
                config,
                cache,
                api_base_override,
            } => {
                let refs: Vec<GitHubRef> = github_issues::extract_github_refs(message);
                if refs.is_empty() {
                    return None;
                }

                // Check cache first.
                {
                    let guard = cache.lock().expect("github cache lock");
                    for gh_ref in &refs {
                        let repo = gh_ref.repo.as_deref().unwrap_or(&config.repo);
                        let key = format!("{repo}#{}", gh_ref.number);
                        if let Some(Some(sig)) = guard.get(&key) {
                            debug!(cache_key = %key, "github cache hit");
                            return Some(sig.clone());
                        }
                    }
                }

                // Fetch misses.
                let fetched = github_issues::fetch_issues_batch(
                    &self.client,
                    config,
                    &refs,
                    api_base_override.as_deref(),
                )
                .await;

                // Populate cache.
                {
                    let mut guard = cache.lock().expect("github cache lock");
                    for (k, sig) in &fetched {
                        guard.insert(k.clone(), sig.clone());
                    }
                }

                // Return first hit.
                for gh_ref in &refs {
                    let repo = gh_ref.repo.as_deref().unwrap_or(&config.repo);
                    let key = format!("{repo}#{}", gh_ref.number);
                    if let Some(Some(sig)) = fetched.get(&key) {
                        return Some(sig.clone());
                    }
                }
                None
            }

            SourceState::Linear {
                config,
                cache,
                api_base_override,
            } => {
                let keys = linear::extract_linear_keys(message);
                if keys.is_empty() {
                    return None;
                }
                // Filter by team_keys if configured.
                let filtered: Vec<String> = keys
                    .into_iter()
                    .filter(|k| linear::matches_team_key(k, &config.team_keys))
                    .collect();
                if filtered.is_empty() {
                    return None;
                }

                // Cache check.
                {
                    let guard = cache.lock().expect("linear cache lock");
                    for k in &filtered {
                        if let Some(Some(sig)) = guard.get(k.as_str()) {
                            debug!(key = k.as_str(), "linear cache hit");
                            return Some(sig.clone());
                        }
                    }
                }

                // Fetch misses.
                let fetched = linear::fetch_issues_batch(
                    &self.client,
                    config,
                    &filtered,
                    api_base_override.as_deref(),
                )
                .await;

                {
                    let mut guard = cache.lock().expect("linear cache lock");
                    for (k, sig) in &fetched {
                        guard.insert(k.clone(), sig.clone());
                    }
                }

                for k in &filtered {
                    if let Some(Some(sig)) = fetched.get(k.as_str()) {
                        return Some(sig.clone());
                    }
                }
                None
            }

            SourceState::Shortcut {
                config,
                cache,
                api_base_override,
            } => {
                let ids = shortcut::extract_shortcut_ids(message);
                if ids.is_empty() {
                    return None;
                }

                // Cache check.
                {
                    let guard = cache.lock().expect("shortcut cache lock");
                    for id in &ids {
                        let k = id.to_string();
                        if let Some(Some(sig)) = guard.get(&k) {
                            debug!(story_id = id, "shortcut cache hit");
                            return Some(sig.clone());
                        }
                    }
                }

                // Fetch misses.
                let fetched = shortcut::fetch_stories_batch(
                    &self.client,
                    config,
                    &ids,
                    api_base_override.as_deref(),
                )
                .await;

                {
                    let mut guard = cache.lock().expect("shortcut cache lock");
                    for (k, sig) in &fetched {
                        guard.insert(k.clone(), sig.clone());
                    }
                }

                for id in &ids {
                    let k = id.to_string();
                    if let Some(Some(sig)) = fetched.get(&k) {
                        return Some(sig.clone());
                    }
                }
                None
            }

            SourceState::Confluence {
                config,
                cache,
                api_base_override,
            } => {
                let ids = confluence::extract_confluence_ids(message);
                if ids.is_empty() {
                    return None;
                }

                // Cache check.
                {
                    let guard = cache.lock().expect("confluence cache lock");
                    for id in &ids {
                        let k = id.to_string();
                        if let Some(Some(sig)) = guard.get(&k) {
                            debug!(page_id = id, "confluence cache hit");
                            return Some(sig.clone());
                        }
                    }
                }

                // Fetch misses.
                let fetched = confluence::fetch_pages_batch(
                    &self.client,
                    config,
                    &ids,
                    api_base_override.as_deref(),
                )
                .await;

                {
                    let mut guard = cache.lock().expect("confluence cache lock");
                    for (k, sig) in &fetched {
                        guard.insert(k.clone(), sig.clone());
                    }
                }

                for id in &ids {
                    let k = id.to_string();
                    if let Some(Some(sig)) = fetched.get(&k) {
                        return Some(sig.clone());
                    }
                }
                None
            }

            SourceState::Datadog {
                config,
                cache,
                api_base_override,
            } => {
                let shas = datadog::extract_commit_shas(message);
                if shas.is_empty() {
                    return None;
                }

                // Cache check.
                {
                    let guard = cache.lock().expect("datadog cache lock");
                    for sha in &shas {
                        if let Some(Some(sig)) = guard.get(sha.as_str()) {
                            debug!(sha = sha.as_str(), "datadog cache hit");
                            return Some(sig.clone());
                        }
                    }
                }

                // Fetch misses.
                let fetched = datadog::check_shas_batch(
                    &self.client,
                    config,
                    &shas,
                    api_base_override.as_deref(),
                )
                .await;

                {
                    let mut guard = cache.lock().expect("datadog cache lock");
                    for (k, sig) in &fetched {
                        guard.insert(k.clone(), sig.clone());
                    }
                }

                for sha in &shas {
                    if let Some(Some(sig)) = fetched.get(sha.as_str()) {
                        return Some(sig.clone());
                    }
                }
                None
            }
        }
    }

    /// Override the JIRA base URL for a source at index `idx`.
    ///
    /// Why: integration tests use wiremock servers that listen on random ports;
    /// this seam lets tests inject the mock server URL without modifying the
    /// config struct.
    /// What: replaces `base_url_override` for the source at `idx` (0-based).
    /// Test: used by all JIRA integration tests.
    #[cfg(test)]
    pub fn with_jira_base_url(mut self, idx: usize, url: String) -> Self {
        if let Some(SourceState::Jira {
            ref mut base_url_override,
            ..
        }) = self.sources.get_mut(idx)
        {
            *base_url_override = Some(url);
        }
        self
    }

    /// Override the GitHub API base URL for a source at index `idx`.
    ///
    /// Why: same as `with_jira_base_url` but for GitHub Integration tests.
    /// What: replaces `api_base_override` for the source at `idx`.
    /// Test: used by all GitHub integration tests.
    #[cfg(test)]
    pub fn with_github_api_base(mut self, idx: usize, url: String) -> Self {
        if let Some(SourceState::GithubIssues {
            ref mut api_base_override,
            ..
        }) = self.sources.get_mut(idx)
        {
            *api_base_override = Some(url);
        }
        self
    }

    /// Override the Linear GraphQL base URL for a source at index `idx`.
    ///
    /// Why: integration tests use wiremock servers on random ports; this seam
    /// lets tests inject the mock URL without touching the config struct.
    /// What: replaces `api_base_override` for the Linear source at `idx`.
    /// Test: used by Linear integration tests.
    #[cfg(test)]
    pub fn with_linear_api_base(mut self, idx: usize, url: String) -> Self {
        if let Some(SourceState::Linear {
            ref mut api_base_override,
            ..
        }) = self.sources.get_mut(idx)
        {
            *api_base_override = Some(url);
        }
        self
    }

    /// Override the Shortcut REST base URL for a source at index `idx`.
    ///
    /// Why: same as `with_jira_base_url` but for Shortcut integration tests.
    /// What: replaces `api_base_override` for the Shortcut source at `idx`.
    /// Test: used by Shortcut integration tests.
    #[cfg(test)]
    pub fn with_shortcut_api_base(mut self, idx: usize, url: String) -> Self {
        if let Some(SourceState::Shortcut {
            ref mut api_base_override,
            ..
        }) = self.sources.get_mut(idx)
        {
            *api_base_override = Some(url);
        }
        self
    }

    /// Override the Confluence base URL for a source at index `idx`.
    ///
    /// Why: same as `with_jira_base_url` but for Confluence integration tests.
    /// What: replaces `api_base_override` for the Confluence source at `idx`.
    /// Test: used by Confluence integration tests.
    #[cfg(test)]
    pub fn with_confluence_base_url(mut self, idx: usize, url: String) -> Self {
        if let Some(SourceState::Confluence {
            ref mut api_base_override,
            ..
        }) = self.sources.get_mut(idx)
        {
            *api_base_override = Some(url);
        }
        self
    }

    /// Override the Datadog API base URL for a source at index `idx`.
    ///
    /// Why: same as `with_jira_base_url` but for Datadog integration tests.
    /// What: replaces `api_base_override` for the Datadog source at `idx`.
    /// Test: used by Datadog integration tests.
    #[cfg(test)]
    pub fn with_datadog_api_base(mut self, idx: usize, url: String) -> Self {
        if let Some(SourceState::Datadog {
            ref mut api_base_override,
            ..
        }) = self.sources.get_mut(idx)
        {
            *api_base_override = Some(url);
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::classify::sources::{
        GithubIssuesSourceConfig, JiraFieldMappings, JiraSourceConfig, SourceConfig,
    };

    /// Why: the resolver must work with no sources configured (e.g. no
    /// `sources:` block in the rules file) and return `None` without
    /// panicking.
    /// What: assert `resolve` on an empty resolver returns `None`.
    /// Test: no HTTP, pure unit.
    #[tokio::test]
    async fn resolver_builds_from_empty_sources() {
        let resolver = ExternalSourceResolver::new(&[]);
        assert!(resolver.resolve("feat: add login").await.is_none());
    }

    /// Why: commits with no ticket keys must not trigger any HTTP calls and
    /// must return `None` cleanly.
    /// What: configure a JIRA source and resolve a message without a ticket key.
    /// Test: no HTTP (no keys → no fetch).
    #[tokio::test]
    async fn resolver_returns_none_for_messages_without_keys() {
        let config = JiraSourceConfig {
            base_url: "https://acme.atlassian.net".to_string(),
            token_env: "JIRA_API_TOKEN".to_string(),
            username: None,
            email_env: None,
            project_keys: vec!["PROJ".to_string()],
            field_mappings: JiraFieldMappings::default(),
        };
        let resolver = ExternalSourceResolver::new(&[SourceConfig::Jira(config)]);
        let result = resolver.resolve("feat: add login flow").await;
        assert!(result.is_none(), "no keys → no signal");
    }

    /// Why: the resolver must correctly route JIRA keys to the JIRA source
    /// and return the mapped category.
    /// What: stand up a wiremock server that returns a JIRA `Bug` issue type,
    /// configure a mapping `Bug → bug_fix`, and assert the signal comes back.
    /// Test: requires `wiremock` dev-dep; mocks one JIRA HTTP call.
    #[tokio::test]
    async fn resolver_returns_jira_signal_for_bug_issue_type() {
        let server = MockServer::start().await;

        let body = serde_json::json!({
            "key": "PROJ-1234",
            "fields": {
                "issuetype": {"name": "Bug"},
                "labels": [],
                "components": []
            }
        });

        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/PROJ-1234"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        // Set token env var for this test.
        unsafe { std::env::set_var("JIRA_API_TOKEN_TEST_BUG", "test-token") };

        let mut issue_type_map = HashMap::new();
        issue_type_map.insert("Bug".to_string(), "bug_fix".to_string());

        let config = JiraSourceConfig {
            base_url: server.uri(),
            token_env: "JIRA_API_TOKEN_TEST_BUG".to_string(),
            username: None,
            email_env: None,
            project_keys: vec![],
            field_mappings: JiraFieldMappings {
                issue_type: issue_type_map,
                labels: HashMap::new(),
                components: HashMap::new(),
            },
        };
        let resolver = ExternalSourceResolver::new(&[SourceConfig::Jira(config)])
            .with_jira_base_url(0, server.uri());

        let signal = resolver
            .resolve("PROJ-1234 fix null pointer")
            .await
            .expect("should have signal");
        assert_eq!(signal.category, "bug_fix");
        assert!(signal.source.contains("issue_type"));

        unsafe { std::env::remove_var("JIRA_API_TOKEN_TEST_BUG") };
    }

    /// Why: the cache must prevent duplicate HTTP calls for the same ticket
    /// key on multiple commits.
    /// What: mount a JIRA mock that expects exactly one call, then resolve
    /// the same key twice. If the second call hits the server, the test fails
    /// because wiremock will see 2 calls.
    /// Test: wiremock with `expect(1)`.
    #[tokio::test]
    async fn resolver_caches_jira_result_across_calls() {
        let server = MockServer::start().await;

        let body = serde_json::json!({
            "key": "PROJ-99",
            "fields": {
                "issuetype": {"name": "Story"},
                "labels": [],
                "components": []
            }
        });

        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/PROJ-99"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            // Exactly one HTTP call allowed — second call must be cache hit.
            .expect(1)
            .mount(&server)
            .await;

        unsafe { std::env::set_var("JIRA_API_TOKEN_CACHE_TEST", "test-token") };

        let mut issue_type_map = HashMap::new();
        issue_type_map.insert("Story".to_string(), "new_feature".to_string());

        let config = JiraSourceConfig {
            base_url: server.uri(),
            token_env: "JIRA_API_TOKEN_CACHE_TEST".to_string(),
            username: None,
            email_env: None,
            project_keys: vec![],
            field_mappings: JiraFieldMappings {
                issue_type: issue_type_map,
                labels: HashMap::new(),
                components: HashMap::new(),
            },
        };
        let resolver = ExternalSourceResolver::new(&[SourceConfig::Jira(config)])
            .with_jira_base_url(0, server.uri());

        // First call — should fetch from mock.
        let s1 = resolver.resolve("PROJ-99 add widget").await;
        assert!(s1.is_some());

        // Second call — must use the cache (wiremock will fail if it sees a
        // second request).
        let s2 = resolver.resolve("PROJ-99 related commit").await;
        assert_eq!(s1, s2);

        unsafe { std::env::remove_var("JIRA_API_TOKEN_CACHE_TEST") };
    }

    /// Why: when the JIRA token env var is unset the resolver must return
    /// `None` rather than panicking or making unauthenticated requests.
    /// What: configure a JIRA source with a token env var that is definitely
    /// not set, resolve a message with a matching key, assert `None`.
    /// Test: no HTTP expected (token check happens before fetch).
    #[tokio::test]
    async fn resolver_skips_jira_when_token_unset() {
        // Guarantee the env var is absent for this test.
        unsafe { std::env::remove_var("JIRA_TOKEN_DEFINITELY_NOT_SET_XYZ") };

        let config = JiraSourceConfig {
            base_url: "https://acme.atlassian.net".to_string(),
            token_env: "JIRA_TOKEN_DEFINITELY_NOT_SET_XYZ".to_string(),
            username: None,
            email_env: None,
            project_keys: vec![],
            field_mappings: JiraFieldMappings::default(),
        };
        let resolver = ExternalSourceResolver::new(&[SourceConfig::Jira(config)]);
        let result = resolver.resolve("PROJ-1234 update").await;
        assert!(result.is_none(), "missing token must yield None, not panic");
    }

    /// Why: the GitHub Issues resolver must correctly map labels to categories
    /// via wiremock.
    /// What: stand up a GitHub mock returning a `bug`-labelled issue and
    /// assert the resolver returns `bug_fix`.
    /// Test: wiremock mock of GitHub Issues REST v3.
    #[tokio::test]
    async fn resolver_returns_github_signal_for_bug_label() {
        let server = MockServer::start().await;

        let body = serde_json::json!({
            "number": 42,
            "labels": [{"name": "bug"}, {"name": "help wanted"}]
        });

        Mock::given(method("GET"))
            .and(path("/repos/acme/widgets/issues/42"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        unsafe { std::env::set_var("GITHUB_TOKEN_TEST_BUG", "test-token") };

        let mut label_map = HashMap::new();
        label_map.insert("bug".to_string(), "bug_fix".to_string());

        let config = GithubIssuesSourceConfig {
            repo: "acme/widgets".to_string(),
            token_env: "GITHUB_TOKEN_TEST_BUG".to_string(),
            label_mappings: label_map,
        };

        let resolver = ExternalSourceResolver::new(&[SourceConfig::GithubIssues(config)])
            .with_github_api_base(0, server.uri());

        let signal = resolver
            .resolve("fix: closes #42")
            .await
            .expect("should have signal");
        assert_eq!(signal.category, "bug_fix");
        assert!(signal.source.contains("bug"));

        unsafe { std::env::remove_var("GITHUB_TOKEN_TEST_BUG") };
    }
}
