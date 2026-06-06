//! GitHub identity resolution for multi-account ticketing (#243).
//!
//! Why: Users frequently work across multiple GitHub accounts (personal,
//! work, OSS) — pinning a single `GITHUB_TOKEN`/`GITHUB_REPO` pair forces
//! constant env juggling. Named identities in `~/.trusty-agents/config.toml` let
//! the harness pick the right credentials per-project.
//! What: `GitHubIdentity` declares a named identity by referencing env vars
//! that hold the token + repo. `to_ticketing_config()` materializes a
//! `TicketingConfig` only when both env vars are set, so consumers can
//! gracefully degrade when credentials are unavailable.
//! Test: `tests::*` cover env resolution, missing env handling, and
//! `to_ticketing_config()` round-trip.

use serde::{Deserialize, Serialize};

use super::TicketingConfig;

/// A named GitHub identity referencing env vars for its credentials.
///
/// Why: Storing tokens directly in TOML would be a credential-leak risk.
/// Indirecting through env var names keeps secrets out of the config file
/// while still allowing per-identity routing.
/// What: `name` is the identity label (e.g. "personal", "work"); `token_env`
/// is the env var holding the GitHub PAT; `repo_env` is the env var holding
/// the default `owner/repo` for that identity.
/// Test: `identity_resolves_token_from_env`, `identity_to_config_requires_both`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
pub struct GitHubIdentity {
    pub name: String,
    pub token_env: String,
    pub repo_env: String,
    /// When `true`, force the `gh` CLI backend even when a token is present
    /// (#245).
    ///
    /// Why: Some users prefer `gh` even after configuring a PAT (e.g. for
    /// SSO-protected orgs where `gh` handles the auth dance automatically).
    /// What: Defaults to `false`. When `true`, `build_client()` skips the
    /// REST path and uses `GhCliClient` if `gh` is available.
    /// Test: Covered indirectly by `build_client_force_gh_cli` in mod.rs.
    #[serde(default)]
    pub use_gh_cli: bool,
}

impl GitHubIdentity {
    /// Resolve the token from `self.token_env`.
    pub fn token(&self) -> Option<String> {
        std::env::var(&self.token_env)
            .ok()
            .filter(|s| !s.is_empty())
    }

    /// Resolve the repo (`owner/repo`) from `self.repo_env`.
    pub fn repo(&self) -> Option<String> {
        std::env::var(&self.repo_env).ok().filter(|s| !s.is_empty())
    }

    /// Materialize a `TicketingConfig` for this identity, if env is set.
    ///
    /// Why: Returning `None` when either env var is missing lets the caller
    /// silently skip ticketing wiring instead of producing a broken client
    /// that 401s on every call.
    /// What: Returns `Some(TicketingConfig { provider: "github", ... })` iff
    /// both `token_env` and `repo_env` resolve to non-empty strings.
    /// Test: `identity_to_config_requires_both`.
    pub fn to_ticketing_config(&self) -> Option<TicketingConfig> {
        let token = self.token()?;
        let repo = self.repo()?;
        Some(TicketingConfig {
            provider: "github".to_string(),
            github_token: Some(token),
            github_repo: Some(repo),
            force_gh_cli: self.use_gh_cli,
            ..Default::default()
        })
    }
}

/// `[github]` section of `~/.trusty-agents/config.toml` (#243).
///
/// Why: Holds the multi-identity registry plus the default-identity pointer
/// so `GlobalConfig::github_identity(None)` can resolve to "the right one".
/// What: `default_identity` is the name to use when none is requested;
/// `identities` is the list of named entries.
/// Test: `tests::default_identity_lookup`, `tests::named_identity_lookup`.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct GitHubSection {
    #[serde(default)]
    pub default_identity: Option<String>,
    #[serde(default, rename = "identities")]
    pub identities: Vec<GitHubIdentity>,
}

impl GitHubSection {
    /// Lookup an identity by name, or fall back to `default_identity`.
    ///
    /// Why: Most callers don't know or care which identity to use — they want
    /// "whatever the user configured as default". Passing `Some(name)` lets
    /// per-project overrides bypass the default.
    /// What: If `name` is `Some`, returns the matching identity (if any).
    /// Otherwise returns the identity matching `self.default_identity`, or
    /// the first identity when no default is set.
    /// Test: `tests::default_identity_lookup`.
    pub fn identity(&self, name: Option<&str>) -> Option<&GitHubIdentity> {
        if let Some(n) = name {
            return self.identities.iter().find(|i| i.name == n);
        }
        if let Some(default_name) = &self.default_identity
            && let Some(found) = self.identities.iter().find(|i| &i.name == default_name)
        {
            return Some(found);
        }
        self.identities.first()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_var(prefix: &str) -> String {
        format!("TAGENT_TEST_{}_{}", prefix, uuid::Uuid::new_v4().simple())
    }

    #[test]
    fn identity_resolves_token_from_env() {
        let var = unique_var("TOK");
        unsafe {
            std::env::set_var(&var, "ghp_xyz");
        }
        let id = GitHubIdentity {
            name: "test".into(),
            token_env: var.clone(),
            repo_env: "DOES_NOT_EXIST".into(),
            ..Default::default()
        };
        assert_eq!(id.token().as_deref(), Some("ghp_xyz"));
        unsafe {
            std::env::remove_var(&var);
        }
    }

    #[test]
    fn identity_repo_returns_none_when_missing() {
        let id = GitHubIdentity {
            name: "test".into(),
            token_env: "DOES_NOT_EXIST_TOK_X".into(),
            repo_env: "DOES_NOT_EXIST_REPO_X".into(),
            ..Default::default()
        };
        unsafe {
            std::env::remove_var("DOES_NOT_EXIST_TOK_X");
            std::env::remove_var("DOES_NOT_EXIST_REPO_X");
        }
        assert!(id.repo().is_none());
    }

    #[test]
    fn identity_to_config_requires_both() {
        let tok_var = unique_var("TOK_REQ");
        let repo_var = unique_var("REPO_REQ");
        // Only token set: should return None.
        unsafe {
            std::env::set_var(&tok_var, "t");
        }
        let id = GitHubIdentity {
            name: "p".into(),
            token_env: tok_var.clone(),
            repo_env: repo_var.clone(),
            ..Default::default()
        };
        assert!(id.to_ticketing_config().is_none());
        // Both set: returns Some.
        unsafe {
            std::env::set_var(&repo_var, "owner/repo");
        }
        let cfg = id.to_ticketing_config().expect("both vars set");
        assert_eq!(cfg.provider, "github");
        assert_eq!(cfg.github_token.as_deref(), Some("t"));
        assert_eq!(cfg.github_repo.as_deref(), Some("owner/repo"));
        unsafe {
            std::env::remove_var(&tok_var);
            std::env::remove_var(&repo_var);
        }
    }

    #[test]
    fn default_identity_lookup() {
        let section = GitHubSection {
            default_identity: Some("personal".into()),
            identities: vec![
                GitHubIdentity {
                    name: "work".into(),
                    token_env: "X".into(),
                    repo_env: "Y".into(),
                    ..Default::default()
                },
                GitHubIdentity {
                    name: "personal".into(),
                    token_env: "A".into(),
                    repo_env: "B".into(),
                    ..Default::default()
                },
            ],
        };
        let chosen = section.identity(None).expect("default resolves");
        assert_eq!(chosen.name, "personal");
        let work = section.identity(Some("work")).expect("named");
        assert_eq!(work.name, "work");
        assert!(section.identity(Some("ghost")).is_none());
    }

    #[test]
    fn identity_falls_back_to_first_when_no_default() {
        let section = GitHubSection {
            default_identity: None,
            identities: vec![GitHubIdentity {
                name: "only".into(),
                token_env: "A".into(),
                repo_env: "B".into(),
                ..Default::default()
            }],
        };
        assert_eq!(
            section.identity(None).map(|i| i.name.as_str()),
            Some("only")
        );
    }

    #[test]
    fn empty_section_returns_none() {
        let section = GitHubSection::default();
        assert!(section.identity(None).is_none());
        assert!(section.identity(Some("x")).is_none());
    }
}
