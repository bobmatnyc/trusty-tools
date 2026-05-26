//! Config loading for trusty-tickets.
//!
//! Why: Backends need credentials; the same MCP server needs to support
//! multiple backends with sensible env-var fallbacks for CI / one-off use.
//! What: TOML config + env-var overrides. Legacy mcp-ticketer JSON config
//! is read for migration compatibility.
//! Test: `tests/config.rs` walks the env-var path; this module has
//! unit tests for the parsing layer.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

/// Top-level config.
///
/// Why: One file declares which backends are wired up and which is
/// preferred by default.
/// What: Map of `name -> BackendConfig`.
/// Test: `tests::parse_minimal_toml`.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub default_backend: Option<String>,
    #[serde(default)]
    pub backends: HashMap<String, BackendConfig>,
}

/// Per-backend config wrapper.
///
/// Why: Three backend shapes share one TOML table.
/// What: Tagged enum keyed on `backend = "github"|"jira"|"linear"`.
/// Test: `tests::parse_minimal_toml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "backend")]
#[serde(rename_all = "snake_case")]
pub enum BackendConfig {
    Github(GithubConfig),
    Jira(JiraConfig),
    Linear(LinearConfig),
}

/// GitHub PAT-based config.
///
/// Why: GitHub auth is a PAT or `gh auth token`.
/// What: All fields optional — env vars fill the gaps.
/// Test: `tests::github_from_env`.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct GithubConfig {
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub gh_cli_user: Option<String>,
    #[serde(default)]
    pub gh_cli_host: Option<String>,
}

/// JIRA Cloud config.
///
/// Why: Basic auth with email+API token.
/// What: Server URL + project key + creds.
/// Test: `tests::jira_from_env`.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct JiraConfig {
    #[serde(default)]
    pub server: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub api_token: Option<String>,
    #[serde(default)]
    pub project_key: Option<String>,
}

/// Linear API key + team selection.
///
/// Why: Linear scoping is per-team; either key or id works.
/// What: Optional fields; env vars take precedence on missing.
/// Test: `tests::linear_from_env`.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct LinearConfig {
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub team_key: Option<String>,
    #[serde(default)]
    pub team_id: Option<String>,
}

impl GithubConfig {
    /// Merge env-var fallbacks onto this config.
    ///
    /// Why: Many users will set only env vars; we don't want a TOML file
    /// to be mandatory.
    /// What: Fills empty fields from `GITHUB_TOKEN` / `GITHUB_OWNER` / `GITHUB_REPO`.
    /// Test: `tests::github_from_env`.
    pub fn with_env(mut self) -> Self {
        self.token = self.token.or_else(|| std::env::var("GITHUB_TOKEN").ok());
        self.owner = self.owner.or_else(|| std::env::var("GITHUB_OWNER").ok());
        self.repo = self.repo.or_else(|| std::env::var("GITHUB_REPO").ok());
        self
    }
}

impl JiraConfig {
    /// Why: Same fallback pattern as `GithubConfig`.
    /// What: Reads JIRA_SERVER / JIRA_EMAIL / JIRA_API_TOKEN / JIRA_PROJECT_KEY.
    /// Test: `tests::jira_from_env`.
    pub fn with_env(mut self) -> Self {
        self.server = self.server.or_else(|| std::env::var("JIRA_SERVER").ok());
        self.email = self.email.or_else(|| std::env::var("JIRA_EMAIL").ok());
        self.api_token = self
            .api_token
            .or_else(|| std::env::var("JIRA_API_TOKEN").ok());
        self.project_key = self
            .project_key
            .or_else(|| std::env::var("JIRA_PROJECT_KEY").ok());
        self
    }
}

impl LinearConfig {
    /// Why: Linear API key may be supplied purely from env in CI.
    /// What: Reads LINEAR_API_KEY / LINEAR_TEAM_KEY / LINEAR_TEAM_ID.
    /// Test: `tests::linear_from_env`.
    pub fn with_env(mut self) -> Self {
        self.api_key = self
            .api_key
            .or_else(|| std::env::var("LINEAR_API_KEY").ok());
        self.team_key = self
            .team_key
            .or_else(|| std::env::var("LINEAR_TEAM_KEY").ok());
        self.team_id = self
            .team_id
            .or_else(|| std::env::var("LINEAR_TEAM_ID").ok());
        self
    }
}

impl Config {
    /// Load config from disk + env vars.
    ///
    /// Why: Called once at startup; never re-read at runtime.
    /// What: Searches: `./.trusty-tickets/config.toml`, legacy
    /// `./.mcp-ticketer/config.json`, `~/.trusty-tickets/config.toml`.
    /// If no file is found, returns an empty config — backends can still
    /// be configured via env vars (auto-detected on use).
    /// `TICKETS_BACKEND` env var sets `default_backend` if unset.
    /// Test: `tests::load_returns_default_when_no_file` (env-isolated).
    pub fn load() -> Result<Self> {
        let mut cfg = Self::load_from_disk()?;
        // Apply env overlays to every configured backend
        for v in cfg.backends.values_mut() {
            match v {
                BackendConfig::Github(g) => *g = std::mem::take(g).with_env(),
                BackendConfig::Jira(j) => *j = std::mem::take(j).with_env(),
                BackendConfig::Linear(l) => *l = std::mem::take(l).with_env(),
            }
        }
        // Auto-register backends from env vars if not present
        cfg.auto_register_from_env();
        if cfg.default_backend.is_none() {
            cfg.default_backend = std::env::var("TICKETS_BACKEND").ok();
        }
        Ok(cfg)
    }

    fn load_from_disk() -> Result<Self> {
        let cwd_toml = PathBuf::from(".trusty-tickets/config.toml");
        if cwd_toml.exists() {
            let s = std::fs::read_to_string(&cwd_toml)
                .with_context(|| format!("read {}", cwd_toml.display()))?;
            return toml::from_str(&s).with_context(|| format!("parse {}", cwd_toml.display()));
        }
        let legacy = PathBuf::from(".mcp-ticketer/config.json");
        if legacy.exists() {
            let s = std::fs::read_to_string(&legacy)
                .with_context(|| format!("read {}", legacy.display()))?;
            return serde_json::from_str(&s)
                .with_context(|| format!("parse legacy {}", legacy.display()));
        }
        if let Some(home) = dirs::home_dir() {
            let home_toml = home.join(".trusty-tickets/config.toml");
            if home_toml.exists() {
                let s = std::fs::read_to_string(&home_toml)
                    .with_context(|| format!("read {}", home_toml.display()))?;
                return toml::from_str(&s)
                    .with_context(|| format!("parse {}", home_toml.display()));
            }
        }
        Ok(Self::default())
    }

    /// Register backends whose env vars are set but no config entry exists.
    ///
    /// Why: A user with `GITHUB_TOKEN` set should get a working `github`
    /// backend without writing a TOML file.
    /// What: Adds entries for github/jira/linear if minimal env vars present.
    /// Test: `tests::auto_register_github_from_env`.
    fn auto_register_from_env(&mut self) {
        if !self.backends.contains_key("github")
            && std::env::var("GITHUB_TOKEN").is_ok()
            && std::env::var("GITHUB_OWNER").is_ok()
            && std::env::var("GITHUB_REPO").is_ok()
        {
            self.backends.insert(
                "github".into(),
                BackendConfig::Github(GithubConfig::default().with_env()),
            );
        }
        if !self.backends.contains_key("jira")
            && std::env::var("JIRA_SERVER").is_ok()
            && std::env::var("JIRA_API_TOKEN").is_ok()
        {
            self.backends.insert(
                "jira".into(),
                BackendConfig::Jira(JiraConfig::default().with_env()),
            );
        }
        if !self.backends.contains_key("linear") && std::env::var("LINEAR_API_KEY").is_ok() {
            self.backends.insert(
                "linear".into(),
                BackendConfig::Linear(LinearConfig::default().with_env()),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_toml() {
        let toml_src = r#"
            default_backend = "github"

            [backends.github]
            backend = "github"
            owner = "octocat"
            repo = "hello"
        "#;
        let cfg: Config = toml::from_str(toml_src).expect("parse");
        assert_eq!(cfg.default_backend.as_deref(), Some("github"));
        match cfg.backends.get("github").unwrap() {
            BackendConfig::Github(g) => assert_eq!(g.owner.as_deref(), Some("octocat")),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn parse_linear_toml() {
        let toml_src = r#"
            [backends.linear]
            backend = "linear"
            api_key = "lin_api_..." # pragma: allowlist secret
            team_key = "ENG"
        "#;
        let cfg: Config = toml::from_str(toml_src).expect("parse");
        match cfg.backends.get("linear").unwrap() {
            BackendConfig::Linear(l) => assert_eq!(l.team_key.as_deref(), Some("ENG")),
            _ => panic!("wrong variant"),
        }
    }
}
