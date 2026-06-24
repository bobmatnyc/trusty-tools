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
//! caching and fail-open semantics.  The blocking bridge spawns a dedicated
//! thread with its own `current_thread` Tokio runtime so it works regardless
//! of the ambient runtime flavor — no `block_in_place` panic on single-threaded
//! runtimes.
//!
//! Test: `external_spec` tests module below (9 unit tests, no network).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use base64::Engine as _;
use serde::Deserialize;
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
/// What: wraps an `Arc<dyn ExternalFetch>` (shared so it can be cloned into
/// the blocking thread), an optional `spec_path_prefix` prepended to every
/// file name, and a `Mutex<HashMap>` cache.  `load` checks the cache first;
/// on a miss it calls `blocking_fetch` and stores the result.  All errors are
/// logged and swallowed — fail-open is the contract (AC-11).
///
/// ## Runtime-flavor safety
///
/// `SpecLookup::load` is a synchronous function.  To run the async fetch
/// without calling `block_in_place` (which panics on a `current_thread`
/// runtime), `blocking_fetch` spawns a dedicated OS thread and builds its own
/// `current_thread` Tokio runtime there.  This is safe on every ambient
/// runtime flavor.
///
/// Test: 9 unit tests in `tests` submodule; no network.
pub struct ExternalRepoSpecLookup {
    spec_path_prefix: Option<String>,
    /// Arc so the fetcher can be cloned cheaply into the blocking-fetch thread.
    fetcher: Arc<dyn ExternalFetch>,
    cache: Mutex<HashMap<String, Option<String>>>,
}

impl ExternalRepoSpecLookup {
    /// Construct with an explicit fetcher (used by tests and `from_parts`).
    ///
    /// Why: allows test code to inject a mock fetcher without going through the
    /// full `from_parts` GitHub client path.
    /// What: stores the prefix and fetcher (wrapped in `Arc`); initialises an
    /// empty cache.
    /// Test: all unit tests in this module use this constructor.
    pub fn new(spec_path_prefix: Option<String>, fetcher: Arc<dyn ExternalFetch>) -> Self {
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
        Some(Self::new(spec_path_prefix, Arc::new(fetcher)))
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

    /// Blocking wrapper around `fetcher.fetch`, safe on any Tokio runtime flavor.
    ///
    /// Why: `SpecLookup::load` is synchronous but `ExternalFetch::fetch` is
    /// async.  `tokio::task::block_in_place` would panic on a `current_thread`
    /// runtime (used by `#[tokio::test]` and the MCP stdio serve path).
    /// Instead we spawn a dedicated OS thread that owns its own
    /// `current_thread` Tokio runtime — this never panics regardless of the
    /// caller's ambient runtime flavor.
    /// What: clones the `Arc<dyn ExternalFetch>` into the thread; the thread
    /// builds a runtime, drives `fetcher.fetch(path)` to completion, and sends
    /// the result back over a channel.  The calling thread blocks on `recv()`.
    /// On any error (runtime build, channel, fetch) returns `None` (fail-open).
    /// Test: exercised by every `load`-path test; see also
    /// `fetch_fails_open_on_error` and `fetch_fail_open_on_not_found`.
    fn blocking_fetch(&self, path: &str) -> Option<String> {
        let fetcher = Arc::clone(&self.fetcher);
        let path_owned = path.to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map(|rt| rt.block_on(fetcher.fetch(&path_owned)));
            let _ = tx.send(result);
        });
        match rx.recv() {
            Ok(Ok(Ok(content))) => content,
            Ok(Ok(Err(e))) => {
                debug!("external spec fetch error for {path:?}: {e}");
                None
            }
            Ok(Err(e)) => {
                debug!("external spec: tokio runtime build failed for {path:?}: {e}");
                None
            }
            Err(_) => {
                debug!("external spec: worker thread dropped sender for {path:?}");
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
    ) -> (Arc<AtomicUsize>, Arc<dyn ExternalFetch>) {
        let call_count = Arc::new(AtomicUsize::new(0));
        let mock = Arc::new(MockExternalFetch {
            result,
            call_count: Arc::clone(&call_count),
        });
        (call_count, mock)
    }

    // ── Happy path ────────────────────────────────────────────────────────────

    /// Mock returns content → `load` returns the same text.
    ///
    /// Why: primary AC — a configured external lookup surfaces the spec.
    /// What: mock returns `Ok(Some("spec content"))`, load returns it.
    /// Test: this test; also verifies no panic on the default test runtime.
    #[tokio::test]
    async fn fetch_succeeds_with_mock() {
        let (_, fetcher) = make_mock(Ok(Some("spec content".to_string())));
        let lookup = ExternalRepoSpecLookup::new(None, fetcher);
        let result = lookup.load("SPEC-001.md");
        assert_eq!(result, Some("spec content".to_string()));
    }

    // ── Fail-open: 404 / None ─────────────────────────────────────────────────

    /// Mock returns `Ok(None)` (404) → `load` returns `None` without panicking.
    ///
    /// Why: a missing spec in the external repo must never block a review.
    /// What: mock returns `Ok(None)`, load returns `None`.
    /// Test: this test.
    #[tokio::test]
    async fn fetch_fail_open_on_not_found() {
        let (_, fetcher) = make_mock(Ok(None));
        let lookup = ExternalRepoSpecLookup::new(None, fetcher);
        assert!(lookup.load("SPEC-001.md").is_none());
    }

    // ── Fail-open: transport / API error ─────────────────────────────────────

    /// Mock returns `Err` → `load` returns `None` (fail-open, no panic).
    ///
    /// Why: network or API failure must degrade gracefully.
    /// What: mock returns `Err("network failure")`, load returns `None`.
    /// Test: this test.
    #[tokio::test]
    async fn fetch_fails_open_on_error() {
        let (_, fetcher) = make_mock(Err("network failure".to_string()));
        let lookup = ExternalRepoSpecLookup::new(None, fetcher);
        assert!(lookup.load("SPEC-001.md").is_none());
    }

    // ── Cache avoids second fetch ─────────────────────────────────────────────

    /// Second `load` for the same path does not call the fetcher again.
    ///
    /// Why: the ISR may reference the same spec multiple times per run.
    /// What: load called twice, `call_count` stays at 1.
    /// Test: this test.
    #[tokio::test]
    async fn cache_avoids_second_fetch() {
        let (count, fetcher) = make_mock(Ok(Some("cached".to_string())));
        let lookup = ExternalRepoSpecLookup::new(None, fetcher);
        lookup.load("SPEC-001.md");
        lookup.load("SPEC-001.md");
        assert_eq!(count.load(Ordering::SeqCst), 1, "fetcher called only once");
    }

    // ── Cache stores None (fail-open) ─────────────────────────────────────────

    /// `None` results are also cached — repeated missing-spec lookups don't
    /// re-trigger a failing fetch.
    ///
    /// Why: a flaky or absent spec should not hammer the GitHub API.
    /// What: mock returns `Ok(None)`; two `load` calls produce one fetch.
    /// Test: this test.
    #[tokio::test]
    async fn cache_stores_none_result() {
        let (count, fetcher) = make_mock(Ok(None));
        let lookup = ExternalRepoSpecLookup::new(None, fetcher);
        lookup.load("SPEC-001.md");
        lookup.load("SPEC-001.md");
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "None result must be cached"
        );
    }

    // ── Path prefix combination ───────────────────────────────────────────────

    /// `spec_path_prefix` is prepended with a `/` separator.
    ///
    /// Why: operators set a prefix so bare filenames resolve to the correct
    /// repo path.
    /// What: `"docs/specs"` + `"SPEC-001.md"` → `"docs/specs/SPEC-001.md"`.
    /// Test: this test.
    #[test]
    fn path_prefix_combined_correctly() {
        let (_, fetcher) = make_mock(Ok(None));
        let lookup = ExternalRepoSpecLookup::new(Some("docs/specs".to_string()), fetcher);
        assert_eq!(lookup.resolve_path("SPEC-001.md"), "docs/specs/SPEC-001.md");
    }

    /// Trailing slash on prefix does not produce a double separator.
    ///
    /// Why: `"docs/specs/"` is a natural form; must yield one `/`, not two.
    /// What: `"docs/specs/"` + `"SPEC-001.md"` → `"docs/specs/SPEC-001.md"`.
    /// Test: this test.
    #[test]
    fn path_prefix_with_trailing_slash_no_double_sep() {
        let (_, fetcher) = make_mock(Ok(None));
        let lookup = ExternalRepoSpecLookup::new(Some("docs/specs/".to_string()), fetcher);
        assert_eq!(lookup.resolve_path("SPEC-001.md"), "docs/specs/SPEC-001.md");
    }

    /// Without a prefix, `resolve_path` returns `spec_file` unchanged.
    ///
    /// Why: bare paths must pass through unchanged when no prefix is set.
    /// What: `resolve_path("docs/specs/SPEC-001.md")` is the same string.
    /// Test: this test.
    #[test]
    fn path_no_prefix_unchanged() {
        let (_, fetcher) = make_mock(Ok(None));
        let lookup = ExternalRepoSpecLookup::new(None, fetcher);
        assert_eq!(
            lookup.resolve_path("docs/specs/SPEC-001.md"),
            "docs/specs/SPEC-001.md"
        );
    }

    /// `from_parts` returns `None` for a malformed `owner_repo` string.
    ///
    /// Why: a config value without a `/` must not panic.
    /// What: `from_parts("noslash", …)` returns `None`.
    /// Test: this test (no network — parse-failure short-circuits).
    #[test]
    fn from_parts_rejects_malformed_owner_repo() {
        let result = ExternalRepoSpecLookup::from_parts("noslash", None, "tok".to_string());
        assert!(result.is_none(), "malformed owner_repo must return None");
    }
}
