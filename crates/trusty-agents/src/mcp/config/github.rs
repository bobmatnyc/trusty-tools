//! GitHub identity resolution for multi-account ticketing (#243).
//!
//! Why: Users frequently work across multiple GitHub accounts (personal,
//! work, OSS) — pinning a single `GITHUB_TOKEN`/`GITHUB_REPO` pair forces
//! constant env juggling. Named identities in `~/.trusty-agents/config.toml` let
//! the harness pick the right credentials per-project.
//! What: `GitHubIdentity` declares a named identity by referencing env vars
//! that hold the token + repo. #11632: moved here from the cut `ticketing`
//! module; `GlobalConfig`'s `[github]` section still parses into it.
//! Test: `tests::*` cover env resolution and missing env handling.

use serde::{Deserialize, Serialize};

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
