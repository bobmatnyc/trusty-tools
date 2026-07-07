//! Resolved git-operation identity: `gh`-auth env overrides plus an optional
//! commit-author override, applied to the managed/provisioner git subprocesses
//! (#2184).
//!
//! Why: [`crate::core::gh_identity`] resolves a `GithubConfig` into env
//! overrides for `gh`/git-credential-helper auth, but the managed spawn path
//! (`daemon::managed_routes::lifecycle::spawn_managed`) and the workspace
//! provisioner (`provisioner::workspace::RealGitBackend`) need TWO things
//! bundled together for every git subprocess they run: those same env
//! overrides (so `git clone`/`fetch` over HTTPS authenticate as the right
//! identity when a credential helper consults them) AND an optional commit
//! author override (`-c user.name=`/`-c user.email=`) that #2184 introduces as
//! a NET-NEW per-project setting — `tm` previously never set a commit identity
//! on any git invocation it made. [`GitIdentity`] is that bundle;
//! [`resolve_git_identity`] builds one from the resolved `GithubConfig` plus
//! the project's own commit-identity fields; [`resolve_for_config`] is the
//! convenience entry point that also finds the matching project (by
//! `repo_url`) in a loaded [`TrustyToolsConfig`].
//!
//! What: `GitIdentity::env` are the `(name, value)` pairs to apply via
//! `Command::envs`; `GitIdentity::commit_config_args` renders the `-c
//! user.name=…`/`-c user.email=…` args to prepend to a `git` invocation (empty
//! when neither is set, so an unconfigured project's git commands are
//! byte-for-byte identical to pre-#2184 behaviour).
//!
//! Test: `resolve_git_identity_*`, `resolve_for_config_*`,
//! `commit_config_args_*` in the inline `tests` module.

use crate::core::gh_identity::{GhIdentityError, resolve_gh_env, select_github_config};
use crate::core::trusty_tools_config::{GithubConfig, TrustyToolsConfig};
use crate::project::record::repo_url_matches;

/// A resolved git-operation identity: auth env overrides plus an optional
/// commit-author override.
///
/// Why: bundling both concerns lets `RealGitBackend` apply a SINGLE resolved
/// value to every git subprocess it runs, rather than threading two separate
/// values through the `GitBackend` trait.
/// What: `env` — ordered `(name, value)` overrides (mirrors
/// [`crate::core::gh_identity::GhEnv::vars`]); `commit_name`/`commit_email` —
/// optional git commit author overrides. All empty/`None` (the `Default`) is
/// the pre-#2184 ambient behaviour: no env overrides, no commit identity
/// applied.
/// Test: `commit_config_args_empty_when_unset`,
/// `commit_config_args_both_set`, `commit_config_args_name_only`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitIdentity {
    /// Env overrides to apply to the git subprocess (e.g. `GH_CONFIG_DIR`,
    /// `GH_TOKEN`, `GH_HOST`) so an HTTPS credential helper picks the right
    /// identity.
    pub env: Vec<(String, String)>,
    /// Git commit author name override, applied as `-c user.name=<name>`.
    pub commit_name: Option<String>,
    /// Git commit author email override, applied as `-c user.email=<email>`.
    pub commit_email: Option<String>,
}

impl GitIdentity {
    /// Whether this identity carries no overrides at all (fully ambient).
    ///
    /// Why: lets callers skip building a command wrapper entirely when there
    /// is nothing to apply.
    /// What: true when `env` is empty and both commit fields are `None`.
    /// Test: covered by `resolve_git_identity_none_when_nothing_configured`.
    pub fn is_empty(&self) -> bool {
        self.env.is_empty() && self.commit_name.is_none() && self.commit_email.is_none()
    }

    /// Render the `-c user.name=…`/`-c user.email=…` args for a `git` command.
    ///
    /// Why: git accepts repeated `-c key=value` overrides BEFORE the
    /// subcommand; centralising the rendering means every git subprocess site
    /// builds the identical arg shape.
    /// What: returns `[]` when neither field is set; otherwise one `-c
    /// user.name=<name>` pair and/or one `-c user.email=<email>` pair, in that
    /// order, only for the fields that are `Some`.
    /// Test: `commit_config_args_empty_when_unset`,
    /// `commit_config_args_both_set`, `commit_config_args_name_only`.
    pub fn commit_config_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        if let Some(name) = &self.commit_name {
            args.push("-c".to_string());
            args.push(format!("user.name={name}"));
        }
        if let Some(email) = &self.commit_email {
            args.push("-c".to_string());
            args.push(format!("user.email={email}"));
        }
        args
    }
}

/// Build a [`GitIdentity`] from already-selected `GithubConfig` tiers plus
/// commit-identity fields.
///
/// Why: the pure combination step — apply [`select_github_config`]'s
/// project-over-global precedence, resolve it into env overrides via
/// [`resolve_gh_env`], and attach the (project-scoped-only; #2184 does not
/// define a global commit identity tier) commit author override — kept
/// separate from any config/registry lookup so it is unit-testable without a
/// loaded [`TrustyToolsConfig`].
/// What: returns `Err` only when [`resolve_gh_env`] refuses an `account`-only
/// binding (see `core::gh_identity` module docs).
/// Test: `resolve_git_identity_project_overrides_global`,
/// `resolve_git_identity_falls_back_to_global`,
/// `resolve_git_identity_none_when_nothing_configured`,
/// `resolve_git_identity_commit_identity_independent_of_github`.
pub fn resolve_git_identity(
    project_github: Option<&GithubConfig>,
    global_github: Option<&GithubConfig>,
    commit_name: Option<&str>,
    commit_email: Option<&str>,
) -> Result<GitIdentity, GhIdentityError> {
    let selected = select_github_config(project_github, global_github);
    let env = resolve_gh_env(selected)?;
    Ok(GitIdentity {
        env: env.vars().to_vec(),
        commit_name: commit_name.map(str::to_string),
        commit_email: commit_email.map(str::to_string),
    })
}

/// Resolve the [`GitIdentity`] for `repo_url` from a loaded [`TrustyToolsConfig`].
///
/// Why: `spawn_managed`'s clone-based and local-redirect provisioning paths
/// already load `TrustyToolsConfig` once per spawn; this is the one place that
/// turns that config plus the target `repo_url` into the identity
/// `RealGitBackend` should use, so the daemon's two call sites cannot diverge.
/// What: finds the first `config.projects` entry whose `repo_url` matches (via
/// [`repo_url_matches`], which compares parsed `owner/repo` when possible) and
/// uses ITS `github`/`commit_name`/`commit_email` fields; falls back to the
/// global `config.github` tier when no project matches or the matched project
/// has no `github` binding of its own (commit identity is project-scoped only
/// — no global fallback tier). No match at all → fully ambient `GitIdentity`
/// (`is_empty()` — no regression).
/// Test: `resolve_for_config_matches_project_by_repo_url`,
/// `resolve_for_config_falls_back_to_global_github`,
/// `resolve_for_config_ambient_when_no_match`.
pub fn resolve_for_config(
    config: &TrustyToolsConfig,
    repo_url: &str,
) -> Result<GitIdentity, GhIdentityError> {
    let matched = config
        .projects
        .iter()
        .find(|p| repo_url_matches(&p.repo_url, repo_url));

    let project_github = matched.and_then(|p| p.github.as_ref());
    let commit_name = matched.and_then(|p| p.commit_name.as_deref());
    let commit_email = matched.and_then(|p| p.commit_email.as_deref());

    resolve_git_identity(
        project_github,
        config.github.as_ref(),
        commit_name,
        commit_email,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gh(account: &str) -> GithubConfig {
        GithubConfig {
            account: None,
            config_dir: Some(std::path::PathBuf::from(format!("/cfg/{account}"))),
            token_env: None,
            host: None,
        }
    }

    // ── resolve_git_identity ──────────────────────────────────────────────

    /// Why: a project-scoped `github:` binding must win over the global one.
    /// Test: itself.
    #[test]
    fn resolve_git_identity_project_overrides_global() {
        let project = gh("project");
        let global = gh("global");
        let identity = resolve_git_identity(Some(&project), Some(&global), None, None).expect("ok");
        assert_eq!(
            identity.env,
            vec![("GH_CONFIG_DIR".to_string(), "/cfg/project".to_string())]
        );
    }

    /// Why: with no project binding, the global tier must be used.
    /// Test: itself.
    #[test]
    fn resolve_git_identity_falls_back_to_global() {
        let global = gh("global");
        let identity = resolve_git_identity(None, Some(&global), None, None).expect("ok");
        assert_eq!(
            identity.env,
            vec![("GH_CONFIG_DIR".to_string(), "/cfg/global".to_string())]
        );
    }

    /// Why: with nothing configured anywhere, the identity must be fully
    /// ambient — no regression for projects with no #2184 binding.
    /// Test: itself.
    #[test]
    fn resolve_git_identity_none_when_nothing_configured() {
        let identity = resolve_git_identity(None, None, None, None).expect("ok");
        assert!(identity.is_empty());
    }

    /// Why: commit identity is an independent axis from the `gh` env
    /// overrides — it must apply even when no `github:` binding exists at
    /// either tier.
    /// Test: itself.
    #[test]
    fn resolve_git_identity_commit_identity_independent_of_github() {
        let identity =
            resolve_git_identity(None, None, Some("Bot"), Some("bot@example.com")).expect("ok");
        assert!(identity.env.is_empty());
        assert_eq!(identity.commit_name.as_deref(), Some("Bot"));
        assert_eq!(identity.commit_email.as_deref(), Some("bot@example.com"));
    }

    // ── resolve_for_config ────────────────────────────────────────────────

    fn project_config(
        repo_url: &str,
        github: Option<GithubConfig>,
    ) -> crate::core::trusty_tools_config::ProjectConfig {
        crate::core::trusty_tools_config::ProjectConfig {
            name: "p".into(),
            repo_url: repo_url.into(),
            default_branch: None,
            stack_hint: None,
            tags: None,
            description: None,
            gh_user: None,
            github,
            commit_name: Some("Project Bot".into()),
            commit_email: Some("bot@project.example.com".into()),
            untracked_sync: None,
        }
    }

    /// Why: `resolve_for_config` must find the project by matching `repo_url`
    /// (tolerating `.git`/scheme differences via `repo_url_matches`) and use
    /// its `github`/commit-identity fields.
    /// Test: itself.
    #[test]
    fn resolve_for_config_matches_project_by_repo_url() {
        let config = TrustyToolsConfig {
            projects: vec![project_config(
                "https://github.com/acme/widget.git",
                Some(gh("project")),
            )],
            ..Default::default()
        };
        let identity = resolve_for_config(&config, "https://github.com/acme/widget").expect("ok");
        assert_eq!(
            identity.env,
            vec![("GH_CONFIG_DIR".to_string(), "/cfg/project".to_string())]
        );
        assert_eq!(identity.commit_name.as_deref(), Some("Project Bot"));
        assert_eq!(
            identity.commit_email.as_deref(),
            Some("bot@project.example.com")
        );
    }

    /// Why: a project match with NO `github:` binding of its own must fall
    /// back to the global tier (project-over-global, not project-only).
    /// Test: itself.
    #[test]
    fn resolve_for_config_falls_back_to_global_github() {
        let config = TrustyToolsConfig {
            github: Some(gh("global")),
            projects: vec![project_config("https://github.com/acme/widget", None)],
            ..Default::default()
        };
        let identity = resolve_for_config(&config, "https://github.com/acme/widget").expect("ok");
        assert_eq!(
            identity.env,
            vec![("GH_CONFIG_DIR".to_string(), "/cfg/global".to_string())]
        );
    }

    /// Why: a `repo_url` matching NO declared project must resolve to a fully
    /// ambient identity — the no-regression guarantee for repos `tm` clones
    /// that were never declared in `config.projects`.
    /// Test: itself.
    #[test]
    fn resolve_for_config_ambient_when_no_match() {
        let config = TrustyToolsConfig::default();
        let identity = resolve_for_config(&config, "https://github.com/someone/else").expect("ok");
        assert!(identity.is_empty());
    }

    // ── commit_config_args ────────────────────────────────────────────────

    /// Why: an unconfigured identity must render NO `-c` args, so a git
    /// invocation for a project with no commit-identity override is
    /// byte-for-byte identical to pre-#2184 behaviour.
    /// Test: itself.
    #[test]
    fn commit_config_args_empty_when_unset() {
        assert!(GitIdentity::default().commit_config_args().is_empty());
    }

    /// Why: both fields set must render both `-c` pairs, name before email.
    /// Test: itself.
    #[test]
    fn commit_config_args_both_set() {
        let identity = GitIdentity {
            commit_name: Some("Bot".into()),
            commit_email: Some("bot@example.com".into()),
            ..Default::default()
        };
        assert_eq!(
            identity.commit_config_args(),
            vec![
                "-c".to_string(),
                "user.name=Bot".to_string(),
                "-c".to_string(),
                "user.email=bot@example.com".to_string(),
            ]
        );
    }

    /// Why: only the set field must render — no spurious empty override.
    /// Test: itself.
    #[test]
    fn commit_config_args_name_only() {
        let identity = GitIdentity {
            commit_name: Some("Bot".into()),
            ..Default::default()
        };
        assert_eq!(
            identity.commit_config_args(),
            vec!["-c".to_string(), "user.name=Bot".to_string()]
        );
    }
}
