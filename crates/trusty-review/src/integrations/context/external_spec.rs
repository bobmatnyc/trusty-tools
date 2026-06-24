//! External-repo spec lookup via the GitHub Contents API (#1419, PR-B).
//!
//! Why: the conformance source needs to load spec files from a *different*
//! repository (e.g. an org-level `apex-specs` repo) rather than only from
//! `docs/specs/` in the reviewed repo.  Fetching externally lets teams
//! maintain a single canonical spec repo without embedding it in every project.
//!
//! What: `ExternalFetch` (async, injectable for tests), `GithubContentsFetch`
//! (production impl hitting `GET /repos/{owner}/{repo}/contents/{path}`),
//! `ExternalRepoSpecLookup` implementing `SpecLookup` with per-run in-memory
//! caching and fail-open semantics.
//!
//! Test: `external_spec` tests module below (9 unit tests, no network).

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use base64::Engine as _;
use serde::Deserialize;
use tokio::runtime::Handle;
use tracing::debug;
use trusty_common::intent_source::SpecLookup;

use crate::integrations::github::GithubError;

// ─── Injectable async fetch seam ─────────────────────────────────────────────

/// Async fetcher for a single file path from an external repository.
///
/// Why: the production impl hits the GitHub Contents API; the seam lets tests
/// inject a mock without any network access (#1419).
/// What: one method `fetch` returning `Ok(Some(content))` for a found file,
/// `Ok(None)` for a 404, or `Err(GithubError)` for any other failure.
/// Test: `MockExternalFetch` in this module's `tests`.
#[async_trait]
pub trait ExternalFetch: Send + Sync {
    /// Fetch `path` from the external repository.
    ///
    /// Why: callers need to distinguish "not found" (skip) from "error" (log
    /// and fail-open).
    /// What: returns `Ok(Some(text))` when found, `Ok(None)` for 404,
    /// `Err(GithubError)` for transport / auth / API errors.
    /// Test: mocked in the unit tests; production impl in `GithubContentsFetch`.
    async fn fetch(&self, path: &str) -> Result<Option<String>, GithubError>;
}

// ─── Production fetcher ───────────────────────────────────────────────────────

/// GitHub Contents API fetcher for a fixed `owner/repo`.
///
/// Why: the production path for external spec lookup hits
/// `GET /repos/{owner}/{repo}/contents/{path}` and base64-decodes the
/// `content` field returned by the API.
/// What: holds the owner, repo name, a bearer token, and a `reqwest::Client`
/// built once at construction.  `fetch` issues the GET, maps 404 → `Ok(None)`,
/// decodes the content, and maps all other errors to `GithubError`.
/// Test: not unit-tested (hits real network); the seam is exercised via the
/// mock in `tests`.
pub struct GithubContentsFetch {
    owner: String,
    repo: String,
    token: String,
    client: reqwest::Client,
}

impl GithubContentsFetch {
    /// Construct for `owner/repo` with the given bearer token.
    ///
    /// Why: callers provide the parsed owner + repo and a pre-resolved token.
    /// What: builds a `reqwest::Client` with a 30-second timeout.  Returns
    /// `Err(GithubError::Transport)` when the TLS backend cannot be
    /// initialised (rare; surfaced so the caller can fail-open gracefully).
    /// Test: constructed indirectly via `ExternalRepoSpecLookup::from_parts`.
    pub fn new(owner: String, repo: String, token: String) -> Result<Self, GithubError> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| GithubError::Transport(format!("failed to build HTTP client: {e}")))?;
        Ok(Self {
            owner,
            repo,
            token,
            client,
        })
    }
}

/// GitHub Contents API response (only the fields we need).
#[derive(Deserialize)]
struct ContentsResponse {
    content: String,
}

#[async_trait]
impl ExternalFetch for GithubContentsFetch {
    async fn fetch(&self, path: &str) -> Result<Option<String>, GithubError> {
        let url = format!(
            "https://api.github.com/repos/{}/{}/contents/{}",
            self.owner, self.repo, path
        );
        let response = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "trusty-review")
            .send()
            .await
            .map_err(|e| GithubError::Transport(e.to_string()))?;

        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            return Err(GithubError::Api { status, body });
        }

        let parsed: ContentsResponse = response
            .json()
            .await
            .map_err(|e| GithubError::Transport(format!("JSON parse error: {e}")))?;

        // The API returns base64 with embedded newlines — strip all whitespace first.
        let stripped: String = parsed
            .content
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(stripped.as_bytes())
            .map_err(|e| GithubError::Transport(format!("base64 decode error: {e}")))?;
        let text = String::from_utf8(decoded)
            .map_err(|e| GithubError::Transport(format!("UTF-8 decode error: {e}")))?;

        Ok(Some(text))
    }
}

// ─── ExternalRepoSpecLookup ───────────────────────────────────────────────────

/// `SpecLookup` that fetches spec files from an external GitHub repository.
///
/// Why: teams maintaining a canonical spec repo separate from their reviewed
/// repo need a lookup that crosses repo boundaries while remaining fail-open
/// and fast (per-run in-memory cache avoids redundant API calls, #1419 PR-B).
/// What: wraps a `Box<dyn ExternalFetch>`, an optional `spec_path_prefix`
/// prepended to every file name, and a `Mutex<HashMap>` cache.  `load` checks
/// the cache first; on a miss it calls `blocking_fetch` (which uses
/// `block_in_place` so it works from a sync `SpecLookup::load` call inside a
/// Tokio multi-thread runtime) and stores the result.  All errors are logged
/// and swallowed — fail-open is the contract (AC-11).
/// Test: 9 unit tests in `tests` submodule; no network.
pub struct ExternalRepoSpecLookup {
    spec_path_prefix: Option<String>,
    fetcher: Box<dyn ExternalFetch>,
    cache: Mutex<HashMap<String, Option<String>>>,
}

impl ExternalRepoSpecLookup {
    /// Construct with an explicit fetcher (used by tests and `from_parts`).
    ///
    /// Why: allows test code to inject a mock fetcher without going through the
    /// full `from_parts` GitHub client path.
    /// What: stores the prefix and fetcher; initialises an empty cache.
    /// Test: all unit tests in this module use this constructor.
    pub fn new(spec_path_prefix: Option<String>, fetcher: Box<dyn ExternalFetch>) -> Self {
        Self {
            spec_path_prefix,
            fetcher,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Build from `owner/repo`, an optional prefix, and a bearer token.
    ///
    /// Why: the production wiring path — `from_config` calls this to construct
    /// the lookup without knowing the internal types.
    /// What: splits `owner_repo` on `'/'`; returns `None` when the string is
    /// malformed (no slash or owner/repo empty).  On a valid split, builds a
    /// `GithubContentsFetch` and wraps it in an `ExternalRepoSpecLookup`.
    /// Returns `None` when the HTTP client cannot be initialised (rare TLS
    /// failure — fail-open contract).
    /// Test: `from_parts_rejects_malformed_owner_repo`.
    pub fn from_parts(
        owner_repo: &str,
        spec_path_prefix: Option<String>,
        token: String,
    ) -> Option<Self> {
        let (owner, repo) = owner_repo.split_once('/')?;
        if owner.is_empty() || repo.is_empty() {
            return None;
        }
        let fetcher = GithubContentsFetch::new(owner.to_string(), repo.to_string(), token)
            .map_err(|e| debug!("external spec: failed to build fetcher: {e}"))
            .ok()?;
        Some(Self::new(spec_path_prefix, Box::new(fetcher)))
    }

    /// Resolve the full path (prefix + file), normalising separators.
    ///
    /// Why: a trailing slash on the prefix must not produce a double separator
    /// (e.g. `"docs/specs/" + "foo.md"` -> `"docs/specs/foo.md"`, not
    /// `"docs/specs//foo.md"`).
    /// What: when no prefix is set returns `spec_file` unchanged; otherwise
    /// joins with `'/'`, ensuring exactly one separator between them.
    /// Test: `path_prefix_combined_correctly`,
    /// `path_prefix_with_trailing_slash_no_double_sep`, `path_no_prefix_unchanged`.
    fn resolve_path(&self, spec_file: &str) -> String {
        match &self.spec_path_prefix {
            None => spec_file.to_string(),
            Some(prefix) => {
                let p = prefix.trim_end_matches('/');
                format!("{p}/{spec_file}")
            }
        }
    }

    /// Blocking wrapper around `fetcher.fetch` for use in a sync context.
    ///
    /// Why: `SpecLookup::load` is synchronous; `block_in_place` lets it call
    /// async `fetcher.fetch` without spawning a new thread, provided the caller
    /// is inside a Tokio multi-thread runtime (which the review pipeline always
    /// provides).
    /// What: calls `Handle::current().block_on(self.fetcher.fetch(path))`;
    /// on any error logs at DEBUG and returns `None` (fail-open).
    /// Test: exercised transitively by every `load`-path test.
    fn blocking_fetch(&self, path: &str) -> Option<String> {
        let result =
            tokio::task::block_in_place(|| Handle::current().block_on(self.fetcher.fetch(path)));
        match result {
            Ok(content) => content,
            Err(e) => {
                debug!("external spec fetch error for {path:?}: {e}");
                None
            }
        }
    }
}

impl SpecLookup for ExternalRepoSpecLookup {
    /// Load a spec file, using the in-memory cache to avoid redundant API calls.
    ///
    /// Why: spec files are fetched at most once per review run; caching avoids
    /// repeated HTTP round-trips for the same spec file when it is referenced by
    /// multiple changed files in the diff.
    /// What: checks the cache; on a miss calls `blocking_fetch`, stores the
    /// result (including `None`), and returns it.
    /// Test: `fetch_succeeds_with_mock`, `cache_avoids_second_fetch`,
    /// `cache_stores_none_result`.
    fn load(&self, spec_file: &str) -> Option<String> {
        let path = self.resolve_path(spec_file);
        {
            let guard = self.cache.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(cached) = guard.get(&path) {
                return cached.clone();
            }
        }
        let result = self.blocking_fetch(&path);
        let mut guard = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        guard.insert(path, result.clone());
        result
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    struct MockExternalFetch {
        result: Result<Option<String>, String>,
        call_count: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ExternalFetch for MockExternalFetch {
        async fn fetch(&self, _path: &str) -> Result<Option<String>, GithubError> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            self.result.clone().map_err(GithubError::Transport)
        }
    }

    fn make_mock(
        result: Result<Option<String>, String>,
    ) -> (Arc<AtomicUsize>, Box<dyn ExternalFetch>) {
        let call_count = Arc::new(AtomicUsize::new(0));
        let mock = MockExternalFetch {
            result,
            call_count: Arc::clone(&call_count),
        };
        (call_count, Box::new(mock))
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fetch_succeeds_with_mock() {
        let (_, fetcher) = make_mock(Ok(Some("spec content".to_string())));
        let lookup = ExternalRepoSpecLookup::new(None, fetcher);
        let result = lookup.load("SPEC-001.md");
        assert_eq!(result, Some("spec content".to_string()));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fetch_fail_open_on_not_found() {
        let (_, fetcher) = make_mock(Ok(None));
        let lookup = ExternalRepoSpecLookup::new(None, fetcher);
        assert!(lookup.load("SPEC-001.md").is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fetch_fails_open_on_error() {
        let (_, fetcher) = make_mock(Err("network failure".to_string()));
        let lookup = ExternalRepoSpecLookup::new(None, fetcher);
        assert!(lookup.load("SPEC-001.md").is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cache_avoids_second_fetch() {
        let (count, fetcher) = make_mock(Ok(Some("cached".to_string())));
        let lookup = ExternalRepoSpecLookup::new(None, fetcher);
        lookup.load("SPEC-001.md");
        lookup.load("SPEC-001.md");
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "fetcher must be called only once"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cache_stores_none_result() {
        let (count, fetcher) = make_mock(Ok(None));
        let lookup = ExternalRepoSpecLookup::new(None, fetcher);
        lookup.load("SPEC-001.md");
        lookup.load("SPEC-001.md");
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "None result must also be cached"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn path_prefix_combined_correctly() {
        let (_, fetcher) = make_mock(Ok(None));
        let lookup = ExternalRepoSpecLookup::new(Some("docs/specs".to_string()), fetcher);
        let path = lookup.resolve_path("SPEC-001.md");
        assert_eq!(path, "docs/specs/SPEC-001.md");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn path_prefix_with_trailing_slash_no_double_sep() {
        let (_, fetcher) = make_mock(Ok(None));
        let lookup = ExternalRepoSpecLookup::new(Some("docs/specs/".to_string()), fetcher);
        let path = lookup.resolve_path("SPEC-001.md");
        assert_eq!(path, "docs/specs/SPEC-001.md");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn path_no_prefix_unchanged() {
        let (_, fetcher) = make_mock(Ok(None));
        let lookup = ExternalRepoSpecLookup::new(None, fetcher);
        let path = lookup.resolve_path("SPEC-001.md");
        assert_eq!(path, "SPEC-001.md");
    }

    #[test]
    fn from_parts_rejects_malformed_owner_repo() {
        let result = ExternalRepoSpecLookup::from_parts("noslash", None, "tok".to_string());
        assert!(result.is_none(), "malformed owner_repo must return None");
    }
}
