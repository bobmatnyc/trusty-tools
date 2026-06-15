//! trusty-mpm's `~/.trusty-tools/trusty-mpm/config.yaml` settings + workspace-root
//! resolution (#1220).
//!
//! Why: #1220 introduces two related conventions for trusty-mpm. (1) the
//! cross-crate config convention `~/.trusty-tools/<crate>/config.yaml` — trusty-mpm
//! reads its slice (`~/.trusty-tools/trusty-mpm/config.yaml`) for the workspace-root
//! template, the auto-resume default, and the default model; (2) the managed-session
//! workspace-root default moves from `~/.trusty-mpm/workspaces/<project>/<id>/` to
//! `~/trusty-mpm-projects/<owner>/<repo>/<id>/`, deriving `<owner>/<repo>` from the
//! target repo's GitHub remote. Keeping the typed config AND the root resolver in one
//! module keeps the precedence rules (env > config > built-in default) in a single
//! tested place and well under the 500-SLOC cap.
//!
//! What: [`TrustyToolsConfig`] is the YAML-deserialised settings struct
//! (`workspace_root_template`, `auto_resume`, `default_model`), loaded via
//! [`TrustyToolsConfig::load`] (delegates to `trusty_common::crate_config`).
//! [`workspace_root`] resolves the absolute root for managed-session workspaces with
//! precedence **`TRUSTY_MPM_WORKSPACE_ROOT` env > config template > built-in default
//! (`~/trusty-mpm-projects`)**, expanding a leading `~`. [`workspace_subpath`] joins
//! the `<owner>/<repo>` identity onto that root.
//!
//! Test: `default_template_is_trusty_mpm_projects`, `env_overrides_config_and_default`,
//! `config_template_used_when_no_env`, `tilde_expansion`, `subpath_nests_owner_repo`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use trusty_common::github_path::GithubPath;

/// Crate name used as the `~/.trusty-tools/<crate>/` directory segment.
///
/// Why: the cross-crate convention keys each crate's config dir by its crate name;
/// a constant keeps trusty-mpm's segment from drifting.
/// What: `"trusty-mpm"`.
/// Test: `config_dir_is_trusty_mpm` indirectly via `crate_config` path tests.
pub const CRATE_NAME: &str = "trusty-mpm";

/// Built-in default workspace-root directory name under `$HOME` (#1220).
///
/// Why: #1220 fixes the new default at `~/trusty-mpm-projects/`. Naming it once
/// keeps the resolver and the migration scan in agreement.
/// What: `"trusty-mpm-projects"`.
/// Test: `default_template_is_trusty_mpm_projects`.
pub const DEFAULT_WORKSPACE_DIR: &str = "trusty-mpm-projects";

/// Environment variable that overrides the resolved workspace root.
///
/// Why: operators (and tests) need an escape hatch that wins over the config file,
/// matching the pre-#1220 behaviour where this env var alone selected the root.
/// What: `"TRUSTY_MPM_WORKSPACE_ROOT"`.
/// Test: `env_overrides_config_and_default`.
pub const WORKSPACE_ROOT_ENV: &str = "TRUSTY_MPM_WORKSPACE_ROOT";

/// trusty-mpm's slice of the `~/.trusty-tools/<crate>/config.yaml` convention.
///
/// Why: gives operators a single declarative file for the settings #1220 calls out
/// — the workspace-root template, the auto-resume default, and the default model —
/// instead of relying solely on env vars or the legacy `~/.trusty-mpm/config.toml`
/// (which stays for its existing agent/model sections; this is the NEW, additive
/// YAML surface).
/// What: every field is optional so an absent file or absent key falls back to the
/// built-in default. `workspace_root_template` may contain a leading `~`.
/// Test: `config_template_used_when_no_env`, the `crate_config` round-trip tests.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustyToolsConfig {
    /// Template directory for managed-session workspace roots.
    ///
    /// `None` → the built-in default `~/trusty-mpm-projects`. A leading `~` is
    /// expanded to the home directory. Sessions nest as
    /// `<this>/<owner>/<repo>/<session-id>/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_root_template: Option<String>,

    /// Default supervisor auto-resume preference surfaced in the console Config UI.
    ///
    /// `None` → unset (the supervisor's own default / persisted `auto_resume` file
    /// wins). This field lets the convention express the preference declaratively.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_resume: Option<bool>,

    /// Default model id (or tier alias) for launched sessions.
    ///
    /// `None` → fall back to the existing `~/.trusty-mpm/config.toml` model
    /// resolution. Present here so the console Config UI can edit it via the
    /// #1220 convention without touching the legacy TOML.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
}

impl TrustyToolsConfig {
    /// Load trusty-mpm's config from `~/.trusty-tools/trusty-mpm/config.yaml`.
    ///
    /// Why: the daemon and CLI read this once at startup; collapsing absent /
    /// home-unknown / malformed to defaults (with a logged warning for malformed)
    /// means a bad file never aborts startup.
    /// What: delegates to [`trusty_common::crate_config::load_or_default`] for
    /// [`CRATE_NAME`].
    /// Test: `config_template_used_when_no_env` (round-trips via `crate_config`).
    pub fn load() -> Self {
        trusty_common::crate_config::load_or_default::<Self>(CRATE_NAME)
    }
}

/// Expand a leading `~` in a path template to the home directory.
///
/// Why: config templates and the built-in default are written home-relative
/// (`~/trusty-mpm-projects`); the resolver must turn them into absolute paths.
/// What: replaces a leading `~` (optionally `~/`) with `home`; other paths pass
/// through unchanged. Absolute paths and templates without `~` are returned as-is.
/// Test: `tilde_expansion`.
fn expand_tilde(template: &str, home: &Path) -> PathBuf {
    if let Some(rest) = template.strip_prefix("~/") {
        home.join(rest)
    } else if template == "~" {
        home.to_path_buf()
    } else {
        PathBuf::from(template)
    }
}

/// Resolve the absolute workspace root for managed sessions.
///
/// Why: the spawn path needs ONE answer for "where do session workspaces live?"
/// with #1220's new default and an env/config override, in a tested place so the
/// HTTP route and the MCP tool cannot diverge.
/// What: applies the precedence **`TRUSTY_MPM_WORKSPACE_ROOT` env > config
/// `workspace_root_template` > built-in `~/trusty-mpm-projects`**, expanding a
/// leading `~`. Falls back to `/tmp/trusty-mpm-projects` only when the home
/// directory is unresolvable AND nothing absolute was supplied.
/// Test: `default_template_is_trusty_mpm_projects`, `env_overrides_config_and_default`,
/// `config_template_used_when_no_env`.
pub fn workspace_root(config: &TrustyToolsConfig) -> PathBuf {
    let home = dirs::home_dir();

    // 1. Env override wins (back-compat with the pre-#1220 behaviour).
    if let Ok(raw) = std::env::var(WORKSPACE_ROOT_ENV) {
        let raw = raw.trim();
        if !raw.is_empty() {
            return match &home {
                Some(h) => expand_tilde(raw, h),
                None => PathBuf::from(raw),
            };
        }
    }

    // 2. Config template (from ~/.trusty-tools/trusty-mpm/config.yaml).
    if let Some(template) = config
        .workspace_root_template
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return match &home {
            Some(h) => expand_tilde(template, h),
            None => PathBuf::from(template),
        };
    }

    // 3. Built-in default: ~/trusty-mpm-projects.
    match home {
        Some(h) => h.join(DEFAULT_WORKSPACE_DIR),
        None => PathBuf::from("/tmp").join(DEFAULT_WORKSPACE_DIR),
    }
}

/// Join a project's `<owner>/<repo>` identity onto the workspace root.
///
/// Why: #1220 nests sessions under `<root>/<owner>/<repo>/` so multiple sessions
/// on the same repo group together and the path mirrors the GitHub identity.
/// What: returns `<root>/<owner>/<repo>` for the given [`GithubPath`]. The session
/// id is appended by the provisioner, not here, so this stays the "project home".
/// Test: `subpath_nests_owner_repo`.
pub fn workspace_subpath(config: &TrustyToolsConfig, gh: &GithubPath) -> PathBuf {
    workspace_root(config).join(&gh.owner).join(&gh.repo)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialise env mutation so the env-reading tests cannot race each other
    /// across the shared test process.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Why: the built-in default (no env, no config) must be the #1220 path.
    /// Test: itself.
    #[test]
    fn default_template_is_trusty_mpm_projects() {
        let _g = env_lock();
        // SAFETY: guarded by env_lock; restored below.
        unsafe { std::env::remove_var(WORKSPACE_ROOT_ENV) };
        let root = workspace_root(&TrustyToolsConfig::default());
        assert!(
            root.ends_with(DEFAULT_WORKSPACE_DIR),
            "expected …/{DEFAULT_WORKSPACE_DIR}, got {}",
            root.display()
        );
    }

    /// Why: the env var must win over both the config template and the default.
    /// Test: itself.
    #[test]
    fn env_overrides_config_and_default() {
        let _g = env_lock();
        let cfg = TrustyToolsConfig {
            workspace_root_template: Some("~/from-config".into()),
            ..Default::default()
        };
        // SAFETY: guarded by env_lock; removed at end.
        unsafe { std::env::set_var(WORKSPACE_ROOT_ENV, "/explicit/env/root") };
        let root = workspace_root(&cfg);
        unsafe { std::env::remove_var(WORKSPACE_ROOT_ENV) };
        assert_eq!(root, PathBuf::from("/explicit/env/root"));
    }

    /// Why: with no env override, the config template must be used (and `~`
    /// expanded), beating the built-in default.
    /// Test: itself.
    #[test]
    fn config_template_used_when_no_env() {
        let _g = env_lock();
        unsafe { std::env::remove_var(WORKSPACE_ROOT_ENV) };
        let cfg = TrustyToolsConfig {
            workspace_root_template: Some("/custom/projects".into()),
            ..Default::default()
        };
        let root = workspace_root(&cfg);
        assert_eq!(root, PathBuf::from("/custom/projects"));
    }

    /// Why: a leading `~` in a template must expand to the home directory.
    /// Test: itself.
    #[test]
    fn tilde_expansion() {
        let home = PathBuf::from("/home/bob");
        assert_eq!(
            expand_tilde("~/trusty-mpm-projects", &home),
            PathBuf::from("/home/bob/trusty-mpm-projects")
        );
        assert_eq!(expand_tilde("~", &home), home);
        assert_eq!(expand_tilde("/abs/path", &home), PathBuf::from("/abs/path"));
    }

    /// Why: the project subpath must nest `<owner>/<repo>` under the root in that
    /// order (the #1220 layout the migration scan also relies on).
    /// Test: itself.
    #[test]
    fn subpath_nests_owner_repo() {
        let _g = env_lock();
        unsafe { std::env::remove_var(WORKSPACE_ROOT_ENV) };
        let cfg = TrustyToolsConfig {
            workspace_root_template: Some("/projects".into()),
            ..Default::default()
        };
        let gh = GithubPath {
            owner: "bobmatnyc".into(),
            repo: "trusty-tools".into(),
        };
        assert_eq!(
            workspace_subpath(&cfg, &gh),
            PathBuf::from("/projects/bobmatnyc/trusty-tools")
        );
    }
}
