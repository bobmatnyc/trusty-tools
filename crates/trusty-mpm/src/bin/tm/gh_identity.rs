//! CLI-side, project-aware GitHub identity resolution for every `gh`
//! subprocess `tm` spawns (#1265, project-aware since #2184).
//!
//! Why: the precedence engine (`config_dir` > `token_env` > `account`, plus
//! `GH_HOST`) now lives in the library (`trusty_mpm::core::gh_identity`) so
//! the daemon's managed-spawn path can share it (#2184). This module is the
//! remaining CLI-specific glue: it detects the ACTIVE project (by matching
//! the current directory's git origin remote against `config.projects`),
//! applies the #2184 project-over-global precedence via
//! [`trusty_mpm::core::gh_identity::select_github_config`], and folds the
//! typed [`GhIdentityError`] into `anyhow` (binary code uses `anyhow`, per
//! this workspace's error-handling convention).
//!
//! What: [`load_gh_env`] is the one-line entry point every `tm issue`/`tm
//! ticket`/`tm watch` call site already uses; it is now project-aware with NO
//! call-site changes required. [`resolve_project_aware`] is the pure,
//! directly-testable core (config + a pre-detected origin URL in, `GhEnv`
//! out) that `load_gh_env` wraps with real cwd/git detection.
//!
//! Test: `resolve_project_aware_*` in the inline `tests` module exercise the
//! project-match precedence hermetically (no real `gh`/git needed); the
//! underlying precedence chain itself is tested in
//! `trusty_mpm::core::gh_identity`.

use trusty_mpm::core::gh_identity::{GhEnv, GhIdentityError, select_github_config};
use trusty_mpm::core::trusty_tools_config::TrustyToolsConfig;
use trusty_mpm::project::record::repo_url_matches;

// Re-exported for the existing CLI call sites (`clone_url` is used by
// `commands::watch`; `effective_host`/`resolve_gh_env` are exercised via the
// library's own test suite but kept reachable here for any future CLI need).
pub(crate) use trusty_mpm::core::gh_identity::clone_url;

/// Resolve an optional [`trusty_mpm::core::trusty_tools_config::GithubConfig`]
/// into a [`GhEnv`], surfacing the typed [`GhIdentityError`] as `anyhow` for
/// binary call sites.
///
/// Why: production call sites use `anyhow::Result`; folding the typed error
/// into `anyhow` here keeps those sites a one-liner.
/// What: delegates to [`trusty_mpm::core::gh_identity::resolve_gh_env`].
/// Test: covered transitively by `resolve_project_aware_*` and the library's
/// own `resolve_*` tests.
fn resolve_gh_env_anyhow(
    config: Option<&trusty_mpm::core::trusty_tools_config::GithubConfig>,
) -> anyhow::Result<GhEnv> {
    trusty_mpm::core::gh_identity::resolve_gh_env(config)
        .map_err(|e: GhIdentityError| anyhow::anyhow!(e))
}

/// Resolve the [`GhEnv`] for `origin_url` against a loaded [`TrustyToolsConfig`]
/// (#2184) — the pure, hermetically-testable core of [`load_gh_env`].
///
/// Why: separating the "which config wins" decision from the real cwd/git
/// detection lets the project-over-global precedence be asserted without a
/// real git repo on disk.
/// What: when `origin_url` is `Some` and matches (via [`repo_url_matches`]) a
/// `config.projects` entry with its own `github:` binding, that binding wins
/// outright; otherwise falls back to the global `config.github`; with no
/// match (or no `origin_url`) resolution is purely global — matching
/// pre-#2184 behaviour exactly.
/// Test: `resolve_project_aware_project_binding_wins`,
/// `resolve_project_aware_falls_back_to_global`,
/// `resolve_project_aware_no_origin_uses_global`.
pub(crate) fn resolve_project_aware(
    config: &TrustyToolsConfig,
    origin_url: Option<&str>,
) -> anyhow::Result<GhEnv> {
    let project_github = origin_url
        .and_then(|url| {
            config
                .projects
                .iter()
                .find(|p| repo_url_matches(&p.repo_url, url))
        })
        .and_then(|p| p.github.as_ref());
    let selected = select_github_config(project_github, config.github.as_ref());
    resolve_gh_env_anyhow(selected)
}

/// Convenience wrapper: load trusty-mpm config, detect the active project from
/// the current directory's git origin remote, and resolve the active [`GhEnv`].
///
/// Why: every `tm` entry point that spawns `gh` needs the same "load config →
/// detect project → resolve `github:` section → GhEnv" sequence; centralising
/// it keeps the call sites to one line and the behaviour identical across
/// `ticket`/`watch`/`issue`.
/// What: loads [`TrustyToolsConfig`], detects the current directory's git
/// origin remote (best-effort — a non-repo cwd or a read failure simply yields
/// no project match, never an error), and resolves via
/// [`resolve_project_aware`], surfacing the `account`-strategy refusal as an
/// `anyhow` error.
/// Test: `resolve_project_aware_*` cover the resolution logic; this function
/// is thin wiring over real cwd/git detection.
pub(crate) fn load_gh_env() -> anyhow::Result<GhEnv> {
    let config = TrustyToolsConfig::load();
    let origin_url = std::env::current_dir()
        .ok()
        .and_then(|cwd| trusty_mpm::daemon::managed_routes::inproject::get_origin_url(&cwd));
    let env = resolve_project_aware(&config, origin_url.as_deref())?;
    if !env.is_empty() {
        // Names only — never the resolved token VALUE (which `vars()` may hold).
        let names: Vec<&str> = env.vars().iter().map(|(k, _)| k.as_str()).collect();
        tracing::debug!(overrides = ?names, "applying per-project GitHub identity binding to gh calls");
    }
    Ok(env)
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusty_mpm::core::trusty_tools_config::{GithubConfig, ProjectConfig};

    fn project(repo_url: &str, github: Option<GithubConfig>) -> ProjectConfig {
        ProjectConfig {
            name: "p".into(),
            repo_url: repo_url.into(),
            default_branch: None,
            stack_hint: None,
            tags: None,
            description: None,
            gh_user: None,
            gh_account: None,
            github,
            commit_name: None,
            commit_email: None,
            untracked_sync: None,
        }
    }

    fn gh(config_dir: &str) -> GithubConfig {
        GithubConfig {
            config_dir: Some(config_dir.into()),
            token_env: None,
            account: None,
            host: None,
        }
    }

    /// Why: a matched project's own `github:` binding must win over the
    /// global one — the #2184 precedence this module adds.
    /// Test: itself.
    #[test]
    fn resolve_project_aware_project_binding_wins() {
        let config = TrustyToolsConfig {
            github: Some(gh("/cfg/global")),
            projects: vec![project(
                "https://github.com/acme/widget",
                Some(gh("/cfg/project")),
            )],
            ..Default::default()
        };
        let env =
            resolve_project_aware(&config, Some("https://github.com/acme/widget.git")).expect("ok");
        assert_eq!(
            env.vars(),
            &[("GH_CONFIG_DIR".to_string(), "/cfg/project".to_string())]
        );
    }

    /// Why: a project match with no `github:` binding of its own must fall
    /// back to the global tier.
    /// Test: itself.
    #[test]
    fn resolve_project_aware_falls_back_to_global() {
        let config = TrustyToolsConfig {
            github: Some(gh("/cfg/global")),
            projects: vec![project("https://github.com/acme/widget", None)],
            ..Default::default()
        };
        let env =
            resolve_project_aware(&config, Some("https://github.com/acme/widget")).expect("ok");
        assert_eq!(
            env.vars(),
            &[("GH_CONFIG_DIR".to_string(), "/cfg/global".to_string())]
        );
    }

    /// Why: with no detected origin (e.g. `tm` run outside a git repo), and no
    /// declared project therefore matches — resolution must be purely global,
    /// matching pre-#2184 behaviour exactly.
    /// Test: itself.
    #[test]
    fn resolve_project_aware_no_origin_uses_global() {
        let config = TrustyToolsConfig {
            github: Some(gh("/cfg/global")),
            ..Default::default()
        };
        let env = resolve_project_aware(&config, None).expect("ok");
        assert_eq!(
            env.vars(),
            &[("GH_CONFIG_DIR".to_string(), "/cfg/global".to_string())]
        );
    }

    /// Why: a detected origin that matches NO declared project, with no
    /// global binding either, must resolve to a fully ambient (empty) env —
    /// the no-regression guarantee for projects with no #2184/#1265 config.
    /// Test: itself.
    #[test]
    fn resolve_project_aware_no_match_no_global_is_ambient() {
        let config = TrustyToolsConfig::default();
        let env =
            resolve_project_aware(&config, Some("https://github.com/someone/else")).expect("ok");
        assert!(env.is_empty());
    }
}
