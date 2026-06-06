//! Unified ticketing abstraction across GitHub Issues, JIRA, and Linear.
//!
//! Why: We want agents to create/update/close tickets without caring which
//! provider the project uses. Mirrors `mcp-ticketer`'s Python ABC
//! (`BaseAdapter`) ported to a Rust trait with async methods.
//! What: `TicketingClient` trait + three adapters (`github`, `jira`,
//! `linear`) + `TicketingConfig` for instantiation.
//! Test: See `tests` submodule at end of this file for config/build tests.

#![allow(dead_code)]

pub mod actions;
pub mod gh_cli;
pub mod github;
pub mod identity;
pub mod jira;
pub mod linear;
pub mod types;

use anyhow::Result;
use async_trait::async_trait;

pub use identity::{GitHubIdentity, GitHubSection};
pub use types::*;

/// Unified ticketing client trait — mirrors mcp-ticketer's BaseAdapter ABC.
///
/// Why: Swapping providers (GitHub → Linear → JIRA) shouldn't touch tool
/// call sites. All adapters implement the same surface.
/// What: Async CRUD + list + comment. Adapters translate provider-native
/// payloads to/from the canonical `Ticket` / `CreateTicketReq` shapes.
/// Test: Each adapter has construction tests asserting error on missing
/// credentials; end-to-end tests are left to integration harnesses.
#[async_trait]
pub trait TicketingClient: Send + Sync {
    /// Provider name ("github", "jira", "linear") for logging/display.
    fn provider_name(&self) -> &str;

    async fn create_ticket(&self, req: CreateTicketReq) -> Result<Ticket>;
    async fn get_ticket(&self, id: &str) -> Result<Ticket>;
    async fn update_ticket(&self, id: &str, req: UpdateTicketReq) -> Result<Ticket>;
    async fn close_ticket(&self, id: &str) -> Result<()>;
    async fn list_tickets(&self, filter: TicketFilter) -> Result<Vec<Ticket>>;
    async fn add_comment(&self, id: &str, body: &str) -> Result<()>;

    // ----- #246: capabilities, tagging, ownership, search, transitions -----

    /// Report which optional features this adapter supports.
    ///
    /// Why: Lets callers introspect (and the agent UI hide) operations
    /// that aren't backed by the underlying provider.
    /// What: Default returns all-false; concrete adapters override.
    /// Test: `capabilities_returns_correct_flags_for_github`.
    fn capabilities(&self) -> TicketingCapabilities {
        TicketingCapabilities::default()
    }

    /// Add tags/labels to a ticket without replacing the existing set.
    ///
    /// Why: `update_ticket(labels=...)` overwrites; agents often want to
    /// merely add a `triaged` label without losing existing labels.
    /// What: Default returns "not supported" error so adapters can opt in.
    /// Test: GitHub adapter override is exercised in integration tests;
    /// default path returns Err.
    async fn add_tags(&self, _id: &str, _tags: &[String]) -> Result<Ticket> {
        Err(anyhow::anyhow!(
            "tagging not supported by {}",
            self.provider_name()
        ))
    }

    /// Remove specific tags/labels from a ticket without touching the rest.
    ///
    /// Why: Mirror of `add_tags` — needed for transitions like removing
    /// a `wip` label after a PR merges.
    /// What: Default returns "not supported" error.
    /// Test: Default path returns Err for adapters that don't override.
    async fn remove_tags(&self, _id: &str, _tags: &[String]) -> Result<Ticket> {
        Err(anyhow::anyhow!(
            "tagging not supported by {}",
            self.provider_name()
        ))
    }

    /// List all tags/labels available in the project/repo.
    ///
    /// Why: Lets the LLM pick a real label rather than inventing one.
    /// What: Default returns an empty list (safe — caller can fall back
    /// to free-form labels).
    /// Test: GitHub adapter returns labels from REST API.
    async fn list_available_tags(&self) -> Result<Vec<Tag>> {
        Ok(vec![])
    }

    /// Assign a ticket to a user.
    ///
    /// Why: Ownership transfer is a common workflow step (triage → owner).
    /// What: Default returns "not supported" error.
    /// Test: Default path returns Err.
    async fn assign(&self, _id: &str, _assignee: &str) -> Result<Ticket> {
        Err(anyhow::anyhow!(
            "assignment not supported by {}",
            self.provider_name()
        ))
    }

    /// Remove the current assignee from a ticket.
    ///
    /// Why: Pairs with `assign` — needed when the owner is reassigned to
    /// the backlog.
    /// What: Default returns "not supported" error.
    /// Test: Default path returns Err.
    async fn unassign(&self, _id: &str) -> Result<Ticket> {
        Err(anyhow::anyhow!(
            "assignment not supported by {}",
            self.provider_name()
        ))
    }

    /// Full-text search across tickets.
    ///
    /// Why: `list_tickets` only filters; search lets agents find tickets
    /// by phrase ("cors bug").
    /// What: Default returns "not supported" error so adapters opt in.
    /// Test: Default path returns Err.
    async fn search(&self, _query: &str, _filter: TicketFilter) -> Result<Vec<Ticket>> {
        Err(anyhow::anyhow!(
            "search not supported by {}",
            self.provider_name()
        ))
    }

    /// Report which target statuses the ticket can transition to.
    ///
    /// Why: JIRA workflows restrict transitions; surfacing this lets the
    /// agent avoid invalid moves.
    /// What: Default returns `[Open, Closed]` — the GitHub-like set.
    /// Test: Default path is exercised by adapters that don't override.
    async fn available_transitions(&self, _id: &str) -> Result<Vec<TicketStatus>> {
        Ok(vec![TicketStatus::Open, TicketStatus::Closed])
    }

    /// Count open issues for a repository (#342).
    ///
    /// Why: Project-discovery UIs (e.g. `/projects`) want a lightweight
    /// "this repo has N open tickets" signal without paginating through
    /// the full issue list.
    /// What: Default returns 0 — adapters that can answer cheaply (e.g.
    /// `gh issue list --json number`) override.
    /// Test: `GhCliClient` override is exercised when `gh` is available;
    /// default path is no-op.
    async fn count_open_issues(&self, _repo: &str) -> Result<u32> {
        Ok(0)
    }

    /// Count open pull requests for a repository (#342).
    ///
    /// Why: Same reasoning as `count_open_issues` — project status UIs
    /// want a quick PR count without fetching every PR.
    /// What: Default returns 0 — adapters override when they can.
    /// Test: `GhCliClient` override.
    async fn count_open_prs(&self, _repo: &str) -> Result<u32> {
        Ok(0)
    }

    /// Move a ticket to the given status.
    ///
    /// Why: A higher-level operation than `update_ticket(status=...)`
    /// — providers like JIRA require named transitions, not a status
    /// field write.
    /// What: Default delegates to `close_ticket` for terminal states and
    /// `update_ticket` for `Open`. Other targets return an error.
    /// Test: Default path is covered by GitHub adapter overrides.
    async fn transition(&self, id: &str, to: TicketStatus) -> Result<Ticket> {
        match to {
            TicketStatus::Closed | TicketStatus::Done | TicketStatus::Cancelled => {
                self.close_ticket(id).await?;
                self.get_ticket(id).await
            }
            TicketStatus::Open => {
                self.update_ticket(
                    id,
                    UpdateTicketReq {
                        status: Some(TicketStatus::Open),
                        ..Default::default()
                    },
                )
                .await
            }
            other => Err(anyhow::anyhow!(
                "transition to {:?} not supported by {}",
                other,
                self.provider_name()
            )),
        }
    }
}

/// Config for the ticketing provider.
///
/// Why: Adapters need credentials + routing info but we want one parseable
/// struct (TOML / env) rather than a tagged enum per provider. Fields not
/// relevant to the selected `provider` are simply unused.
/// What: Provider name + credentials + project/repo/team IDs per provider.
/// Build from TOML or `from_env()`.
/// Test: `from_env_reads_github_token` etc. in `tests` below.
#[derive(Debug, Clone, Default)]
pub struct TicketingConfig {
    pub provider: String,
    pub github_token: Option<String>,
    pub github_repo: Option<String>, // "owner/repo"
    pub jira_url: Option<String>,
    pub jira_email: Option<String>,
    pub jira_token: Option<String>,
    pub jira_project: Option<String>,
    pub linear_api_key: Option<String>,
    pub linear_team_id: Option<String>,
    /// When `true`, `build_client()` skips the REST path and uses the
    /// `gh` CLI backend even if a token+repo pair is present (#245).
    ///
    /// Why: Lets a user with both a configured PAT *and* `gh` installed
    /// explicitly prefer `gh` (e.g. for SSO orgs where `gh` handles the
    /// auth dance more gracefully than a static PAT).
    /// What: Default `false`. Plumbed through from
    /// `GitHubIdentity::use_gh_cli`.
    pub force_gh_cli: bool,
}

impl TicketingConfig {
    /// Build a config from environment variables.
    ///
    /// Why: Lets `trusty-agents` pick up tokens without requiring a full TOML
    /// section — convenient for CI and local dev.
    /// What: Reads `TICKETING_PROVIDER`, `GITHUB_TOKEN`, `GITHUB_REPO`,
    /// `JIRA_URL`, `JIRA_EMAIL`, `JIRA_TOKEN`, `JIRA_PROJECT`,
    /// `LINEAR_API_KEY`, `LINEAR_TEAM_ID`.
    /// Test: `from_env_reads_github_token` in `tests`.
    pub fn from_env() -> Self {
        let provider = std::env::var("TICKETING_PROVIDER").unwrap_or_else(|_| "github".to_string());
        Self {
            provider,
            github_token: std::env::var("GITHUB_TOKEN").ok(),
            github_repo: std::env::var("GITHUB_REPO").ok(),
            jira_url: std::env::var("JIRA_URL").ok(),
            jira_email: std::env::var("JIRA_EMAIL").ok(),
            jira_token: std::env::var("JIRA_TOKEN").ok(),
            jira_project: std::env::var("JIRA_PROJECT").ok(),
            linear_api_key: std::env::var("LINEAR_API_KEY").ok(),
            linear_team_id: std::env::var("LINEAR_TEAM_ID").ok(),
            force_gh_cli: false,
        }
    }

    /// Instantiate the appropriate adapter based on `self.provider`.
    ///
    /// Why: Single choice point for provider dispatch; everywhere else in
    /// the codebase holds a `Box<dyn TicketingClient>` and never sees a
    /// concrete type. For the GitHub provider, prefers the REST client when
    /// a token + repo are available, and falls back to the `gh` CLI client
    /// when `gh` is on PATH and authenticated (#245).
    /// What: Returns `Box<dyn TicketingClient>` for one of github/jira/linear,
    /// or an error for an unknown provider or no usable GitHub backend.
    /// Async because the gh-availability probe is a subprocess call.
    /// Test: `build_client_rejects_unknown_provider`,
    /// `build_client_force_gh_cli_falls_back_to_cli`.
    pub async fn build_client(&self) -> Result<Box<dyn TicketingClient>> {
        match self.provider.as_str() {
            "github" => self.build_github_client().await,
            "jira" => Ok(Box::new(jira::JiraClient::new(self)?)),
            "linear" => Ok(Box::new(linear::LinearClient::new(self)?)),
            p => anyhow::bail!("unknown ticketing provider: {p}"),
        }
    }

    /// Build the GitHub-specific client with REST→gh-CLI fallback.
    ///
    /// Why: Two valid paths exist (PAT-based REST or `gh` CLI). Centralizing
    /// the decision means callers don't have to repeat the precedence rules.
    /// What: 1) If token+repo are present and `force_gh_cli` is false, use
    /// `GitHubClient`. 2) Else if `gh` is available, use `GhCliClient`.
    /// 3) Else return an error.
    /// Test: `build_client_force_gh_cli_falls_back_to_cli`.
    async fn build_github_client(&self) -> Result<Box<dyn TicketingClient>> {
        let token_present = self
            .github_token
            .as_deref()
            .filter(|s| !s.is_empty())
            .is_some()
            || std::env::var("GITHUB_TOKEN")
                .ok()
                .filter(|s| !s.is_empty())
                .is_some();
        let repo_present = self
            .github_repo
            .as_deref()
            .filter(|s| !s.is_empty())
            .is_some();

        if !self.force_gh_cli && token_present && repo_present {
            return Ok(Box::new(github::GitHubClient::new(self)?));
        }
        if gh_cli::gh_available().await {
            return Ok(Box::new(gh_cli::GhCliClient::new(self.github_repo.clone())));
        }
        if !token_present {
            anyhow::bail!(
                "no GitHub backend available: neither GITHUB_TOKEN/github_token nor `gh` CLI auth is configured"
            );
        }
        // Token present but repo missing and no gh fallback.
        Ok(Box::new(github::GitHubClient::new(self)?))
    }
}

#[cfg(test)]
mod tests;
