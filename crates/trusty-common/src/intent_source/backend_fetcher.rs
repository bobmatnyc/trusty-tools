//! Default ticket fetcher + spec loader (production wiring).
//!
//! Why: the resolver's `TicketFetcher`/`SpecLookup` seams need a real default
//! that reuses the in-crate `tickets` GitHub backend and the local
//! `docs/specs/` tree — without coupling `resolve` to either (spec §6.6, §7.4).
//! What: `BackendTicketFetcher` (wraps `tickets::Backend::get_issue` behind a
//! pluggable `IntentTokenResolver`) and `FsSpecLookup` (reads `docs/specs/*.md`
//! from a configured repo root).
//! Test: `super::tests::fs_spec_lookup_*`; the GitHub fetch path is
//! network-bound so it is exercised by mocks in unit tests and live only in
//! integration.

use std::path::PathBuf;

use async_trait::async_trait;

use crate::tickets::api::backends::Backend;
use crate::tickets::api::backends::github::GitHubBackend;
use crate::tickets::api::config::GithubConfig;

use super::resolve::{IntentTokenResolver, SpecLookup, TicketData, TicketFetcher};
use super::types::IsrError;

/// Default `TicketFetcher` over the in-crate GitHub `tickets` backend.
///
/// Why: the spec says reuse `Backend::get_issue` rather than build a new GitHub
/// client (spec §6.5, §6.6). This wraps it, deriving the bearer token through
/// the pluggable `IntentTokenResolver` so App/JWT auth works in serve mode
/// (spec §7.4) — it does NOT hard-wire a PAT.
/// What: holds a boxed token resolver; `fetch` builds a `GitHubBackend` for the
/// owner/repo with the resolved token and calls `get_issue`, mapping the
/// `Issue` into a backend-agnostic `TicketData`.
/// Test: `super::tests` mock the `TicketFetcher` trait directly; this impl's
/// auth wiring is covered by `super::tests::backend_fetcher_token_*`.
pub struct BackendTicketFetcher {
    token_resolver: Box<dyn IntentTokenResolver>,
}

impl BackendTicketFetcher {
    /// Construct a fetcher with a custom token resolver.
    ///
    /// Why: review's serve mode supplies an App/JWT resolver; the CLI supplies
    /// the env default. This constructor keeps that choice at the call site
    /// (spec §7.4).
    /// What: stores the boxed resolver for use in `fetch`.
    /// Test: `super::tests::backend_fetcher_token_*`.
    #[must_use]
    pub fn new(token_resolver: Box<dyn IntentTokenResolver>) -> Self {
        Self { token_resolver }
    }
}

#[async_trait]
impl TicketFetcher for BackendTicketFetcher {
    async fn fetch(
        &self,
        owner: &str,
        repo: &str,
        ticket_id: &str,
    ) -> Result<TicketData, IsrError> {
        let token = self.token_resolver.token(owner, repo).await?;
        let cfg = GithubConfig {
            token: Some(token),
            owner: Some(owner.to_string()),
            repo: Some(repo.to_string()),
            gh_cli_user: None,
            gh_cli_host: None,
        };
        let backend = GitHubBackend::new(cfg).map_err(|e| IsrError::TicketFetch(e.to_string()))?;
        // GitHub issue ids are numeric; strip a leading `#` if present.
        let id = ticket_id.trim_start_matches('#');
        let issue = backend
            .get_issue(id)
            .await
            .map_err(|e| IsrError::TicketFetch(e.to_string()))?;
        Ok(TicketData {
            id: format!("#{}", issue.id),
            title: issue.title,
            body: issue.description.unwrap_or_default(),
            url: issue.url,
            backend: issue.backend,
        })
    }
}

/// Default `SpecLookup` that reads `docs/specs/*.md` from a repo root.
///
/// Why: the common case resolves spec markdown straight off disk relative to
/// the checkout; review's serve mode can supply a different loader (spec §6.4).
/// What: holds a repo-root path; `load` joins the `docs/specs/*.md` relative
/// path and reads the file, returning `None` on any read error (a gap, never
/// an error — the ISR reads links, it does not enforce them).
/// Test: `super::tests::fs_spec_lookup_*`.
pub struct FsSpecLookup {
    repo_root: PathBuf,
}

impl FsSpecLookup {
    /// Construct a loader rooted at `repo_root`.
    ///
    /// Why: spec paths in SLD refs are repo-relative (`docs/specs/…`); the
    /// loader needs the root to resolve them (spec §6.4).
    /// What: stores the root path.
    /// Test: `super::tests::fs_spec_lookup_*`.
    #[must_use]
    pub fn new(repo_root: impl Into<PathBuf>) -> Self {
        Self {
            repo_root: repo_root.into(),
        }
    }
}

impl SpecLookup for FsSpecLookup {
    fn load(&self, spec_file: &str) -> Option<String> {
        let path = self.repo_root.join(spec_file);
        std::fs::read_to_string(path).ok()
    }
}
