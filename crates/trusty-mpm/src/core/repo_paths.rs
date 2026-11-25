//! Managed repos root and base-clone path resolution (#1706, #1803).
//!
//! Why: these are pure path helpers (env, config, `$HOME`), but they lived in
//! `daemon::managed_routes::inproject`, which the `daemon` feature gates, while
//! the always-on `project::resolver` calls [`base_clone_path`] — so a plain
//! `--no-default-features` build failed to compile (#9776). `inproject`
//! re-exports every public item here, so each existing path still resolves.
//! What: [`repos_root_from`], [`repos_root`], [`base_clone_path`], their
//! fail-closed `try_` variants for writers (#11821), and the
//! [`REPOS_ROOT_ENV`] / [`DEFAULT_REPOS_DIR`] constants they read.
//! Test: `repos_root_default_ends_with_canonical_segment`,
//! `base_clone_path_resolves_to_canonical_root` (in `inproject`'s tests).

use std::path::{Path, PathBuf};

/// Environment variable that overrides the managed repos root.
///
/// Why: operators need an escape hatch (tests, non-standard layouts) that wins
/// over config without touching the config file.
/// What: `"TRUSTY_MPM_REPOS_ROOT"`.
/// Test: indirectly by integration tests that set this env var.
pub const REPOS_ROOT_ENV: &str = "TRUSTY_MPM_REPOS_ROOT";

/// Default repos root directory name under `$HOME`.
///
/// Why: mirrors the canonical trusty-mpm-projects directory that `workspace_root()`
/// also resolves to, so both the guided-default path and `tm launch` converge on
/// the same base clone location (#1803).
/// What: `"trusty-mpm-projects"`.
/// Test: `repos_root_default_ends_with_canonical_segment`.
pub const DEFAULT_REPOS_DIR: &str = "trusty-mpm-projects";

/// Resolve the absolute root for base clones, with an optional direct override.
///
/// Why: tests that control the repos root must not mutate process-wide env vars
/// (which races other threads). Exposing the override as a parameter lets callers
/// supply the value directly without `set_var` / `remove_var`.
/// What: `env_override` wins over the `TRUSTY_MPM_REPOS_ROOT` env var, which
/// wins over `workspace_root()`. Falls back to `/tmp/trusty-mpm-projects` only
/// when the home directory is unresolvable AND nothing absolute was supplied.
/// Test: `repos_root_default_ends_with_canonical_segment`; `base_clone_path_respects_repos_root_override` (integration).
pub fn repos_root_from(env_override: Option<&str>) -> PathBuf {
    // Delegate to the canonical workspace root resolver (TRUSTY_MPM_WORKSPACE_ROOT
    // > config template > ~/trusty-mpm-projects).
    repos_root_override(env_override).unwrap_or_else(|| {
        let config = crate::core::trusty_tools_config::TrustyToolsConfig::load();
        crate::core::trusty_tools_config::workspace_root(&config)
    })
}

/// [`repos_root`] that fails closed on a broken `install.toml`.
///
/// Why: df-D1t (#11821): a base clone must not land under the default root
/// because the operator's install file is broken.
/// What: `TRUSTY_MPM_REPOS_ROOT` wins as before; otherwise
/// [`crate::core::trusty_tools_config::try_workspace_root`], whose error is
/// returned unchanged.
/// Test: `broken_install_file_fails_the_workspace_root_closed` (env_serial).
pub fn try_repos_root() -> Result<PathBuf, trusty_common::install_paths::InstallPathsError> {
    match repos_root_override(None) {
        Some(root) => Ok(root),
        None => crate::core::trusty_tools_config::try_workspace_root(
            &crate::core::trusty_tools_config::TrustyToolsConfig::load(),
        ),
    }
}

/// The in-process override and `TRUSTY_MPM_REPOS_ROOT` rungs, `~`-expanded;
/// `None` when neither is set.
fn repos_root_override(env_override: Option<&str>) -> Option<PathBuf> {
    // Direct in-process override (highest priority — used by tests to avoid env
    // mutation), then TRUSTY_MPM_REPOS_ROOT (backward compat, wins over
    // workspace_root).
    let from_env = std::env::var(REPOS_ROOT_ENV).ok();
    let raw = env_override
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| from_env.as_deref().map(str::trim).filter(|s| !s.is_empty()))?;
    Some(match dirs::home_dir() {
        Some(h) => expand_tilde_repos(raw, &h),
        None => PathBuf::from(raw),
    })
}

/// Resolve the absolute root for base clones.
///
/// Why: the base-clone path needs ONE answer for "where do managed base clones
/// live?", with precedence TRUSTY_MPM_REPOS_ROOT env > TRUSTY_MPM_WORKSPACE_ROOT
/// env > config template > built-in `~/trusty-mpm-projects` (#1803).
/// What: delegates to [`repos_root_from`] with no in-process override, reading
/// `TRUSTY_MPM_REPOS_ROOT` from the environment as usual.
/// Falls back to `/tmp/trusty-mpm-projects` only when the home directory is
/// unresolvable AND nothing absolute was supplied.
/// Test: `repos_root_default_ends_with_canonical_segment`.
pub fn repos_root() -> PathBuf {
    repos_root_from(None)
}

/// Expand a leading `~` in a path string to `home`.
///
/// Why: env values are often home-relative; normalizing them once here keeps
/// caller code free of `~` handling.
/// What: replaces a leading `~/` or bare `~` with `home`; other paths pass through.
/// Test: covered indirectly by `repos_root` env-override tests.
fn expand_tilde_repos(template: &str, home: &Path) -> PathBuf {
    if let Some(rest) = template.strip_prefix("~/") {
        home.join(rest)
    } else if template == "~" {
        home.to_path_buf()
    } else {
        PathBuf::from(template)
    }
}

/// Compute the base clone path for a GitHub `owner/repo`.
///
/// Why: the base clone directory must be stable and deterministic so multiple
/// sessions against the same repo all share one protected clone, whatever case
/// or punctuation the caller spelled the identity with (#10604).
/// What: returns `<repos_root>/<owner>/<repo>`, each segment slugified exactly
/// as [`trusty_common::github_path::parse_github_path`] does (lowercase, other
/// runs to `-`), so `BobMatNYC/Trusty_Tools` and `bobmatnyc/trusty-tools` name
/// one directory.
/// Readers only: a broken `install.toml` is logged and defaulted here; a
/// caller that clones uses [`try_base_clone_path`] (#11821).
/// Test: `base_clone_path_is_canonical_for_a_mixed_case_identity_10604`;
/// `base_clone_path_resolves_to_canonical_root`; wiring via integration tests.
pub fn base_clone_path(owner: &str, repo: &str) -> PathBuf {
    base_clone_path_in(&repos_root(), owner, repo)
}

/// [`base_clone_path`] under an explicit `repos_root` (#11828).
///
/// Why: a test or a caller that already holds the root must not read the env
/// or the config again.
/// Test: `base_clone_path_is_canonical_for_a_mixed_case_identity_10604`.
pub fn base_clone_path_in(repos_root: &Path, owner: &str, repo: &str) -> PathBuf {
    // #10604: canonical here, not by caller convention.
    repos_root
        .join(canonical_segment(owner))
        .join(canonical_segment(repo))
}

/// [`base_clone_path`] that fails closed on a broken `install.toml`.
///
/// Why: df-D1t (#11821): every path that clones into or reuses a base clone
/// must refuse rather than clone under the default root.
/// What: [`try_repos_root`] joined with the same canonical segments.
/// Test: `broken_install_file_fails_the_workspace_root_closed`,
/// `cold_start_refuses_on_a_broken_install_file` (env_serial).
pub fn try_base_clone_path(
    owner: &str,
    repo: &str,
) -> Result<PathBuf, trusty_common::install_paths::InstallPathsError> {
    Ok(base_clone_path_in(&try_repos_root()?, owner, repo))
}

/// One `owner` or `repo` path segment in the slug form tm names clones with.
///
/// What: the slug [`trusty_common::github_path::parse_owner_repo`] gives a
/// single segment; ASCII-lowercase when that parse refuses (an `@` in it) or
/// leaves nothing.
fn canonical_segment(segment: &str) -> String {
    trusty_common::github_path::parse_owner_repo(segment)
        .map(|gh| gh.repo)
        .unwrap_or_else(|| segment.trim().to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #10604: a mixed-case, underscored identity lands on the lowercase slug
    /// path every slugified caller already uses.
    #[test]
    fn base_clone_path_is_canonical_for_a_mixed_case_identity_10604() {
        // #10604: assert on the tail, not on three `repos_root()` reads, so a
        // concurrent test that changes HOME or the root env cannot flake this.
        for (owner, repo) in [
            ("bobmatnyc", "trusty-tools"),
            ("BobMatNYC", "Trusty_Tools"),
            ("BOBMATNYC", "Trusty-Tools.git"),
        ] {
            let path = base_clone_path(owner, repo);
            assert!(path.ends_with("bobmatnyc/trusty-tools"), "{path:?}");
            let root = Path::new("/r");
            assert_eq!(
                base_clone_path_in(root, owner, repo),
                root.join("bobmatnyc/trusty-tools")
            );
        }
    }
}
