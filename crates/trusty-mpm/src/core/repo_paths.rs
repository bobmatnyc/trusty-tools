//! Managed repos root and base-clone path resolution (#1706, #1803).
//!
//! Why: these are pure path helpers (env, config, `$HOME`), but they lived in
//! `daemon::managed_routes::inproject`, which the `daemon` feature gates, while
//! the always-on `project::resolver` calls [`base_clone_path`] — so a plain
//! `--no-default-features` build failed to compile (#9776). `inproject`
//! re-exports every public item here, so each existing path still resolves.
//! What: [`repos_root_from`], [`repos_root`], [`base_clone_path`], and the
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
    let home = dirs::home_dir();

    // Direct in-process override (highest priority — used by tests to avoid env mutation).
    if let Some(raw) = env_override {
        let raw = raw.trim();
        if !raw.is_empty() {
            return match &home {
                Some(h) => expand_tilde_repos(raw, h),
                None => PathBuf::from(raw),
            };
        }
    }

    // TRUSTY_MPM_REPOS_ROOT env override (backward compat, wins over workspace_root).
    if let Ok(raw) = std::env::var(REPOS_ROOT_ENV) {
        let raw = raw.trim();
        if !raw.is_empty() {
            return match &home {
                Some(h) => expand_tilde_repos(raw, h),
                None => PathBuf::from(raw),
            };
        }
    }

    // Delegate to the canonical workspace root resolver (TRUSTY_MPM_WORKSPACE_ROOT
    // > config template > ~/trusty-mpm-projects).
    let config = crate::core::trusty_tools_config::TrustyToolsConfig::load();
    crate::core::trusty_tools_config::workspace_root(&config)
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
/// sessions against the same repo all share one protected clone.
/// What: returns `<repos_root>/<owner>/<repo>`.
/// Test: `base_clone_path_resolves_to_canonical_root` (unit); wiring via integration tests.
pub fn base_clone_path(owner: &str, repo: &str) -> PathBuf {
    repos_root().join(owner).join(repo)
}
