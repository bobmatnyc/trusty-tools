//! Process-env entry points of the [`super::InstallPaths`] chain (#11821).
//!
//! Why: legacy wrappers (`crate_config_dir`, `resolve_workspace_root`) and
//! trusty-mpm's framework layout resolve ONE key from the live process env
//! and `$HOME`. They must apply the same rule as [`super::InstallPaths::resolve`],
//! so they call the same rung functions rather than a second chain.
//! What: one-key resolution over the process env, a fallible form per caller
//! class, and the logged-default forms that readers keep.
//! Test: `install_paths::tests` (`src/install_paths/tests.rs`).

use std::path::{Path, PathBuf};

use super::{
    InstallPathsError, Key, PathOverrides, ResolvedPath, read_install_file, required,
    resolve_early, resolve_late,
};
use crate::workspace_layout::DEFAULT_WORKSPACE_DIR;

/// Resolve ONE key against an injected env lookup and an optional home.
///
/// Why: a bad value under another key must not change this key's answer, and
/// "no home" must be an error rather than a `/tmp` guess (#11821).
/// What: `NoHome` when `home` is `None`; else flag, env, install file,
/// template, default, with env checked before the file is read.
/// Test: `try_projects_root_fails_closed_without_a_home`.
fn resolve_key_with(
    key: Key,
    env: impl Fn(&str) -> Option<String>,
    home: Option<&Path>,
    overrides: &PathOverrides,
) -> Result<Option<ResolvedPath>, InstallPathsError> {
    let home = home.ok_or(InstallPathsError::NoHome)?;
    if let Some(hit) = resolve_early(key, &env, home, overrides)? {
        return Ok(Some(hit));
    }
    let file = read_install_file(home)?;
    resolve_late(key, file.as_ref(), overrides, home)
}

/// [`resolve_key_with`] over the process env and `dirs::home_dir()`.
fn resolve_process_key(
    key: Key,
    overrides: &PathOverrides,
) -> Result<Option<ResolvedPath>, InstallPathsError> {
    let home = dirs::home_dir();
    resolve_key_with(key, |n| std::env::var(n).ok(), home.as_deref(), overrides)
}

/// The process-env config dir, failing closed.
///
/// Why: trusty-mpm's framework layout (`.trusty-mpm`, `.trusty-tools`) must
/// honour the configured config dir and must not land under `$HOME` when the
/// install file is broken (#11821 P3).
/// What: `TRUSTY_MPM_CONFIG_DIR` env, then `install.toml` `config_dir`, then
/// `$HOME`; `NoHome`, `Read`, `Parse`, `Empty` or `Relative` on failure.
/// Test: `try_config_dir_follows_env_file_and_fails_closed`.
pub fn try_config_dir() -> Result<PathBuf, InstallPathsError> {
    let hit = resolve_process_key(Key::ConfigDir, &PathOverrides::default())?;
    required(Key::ConfigDir, hit).map(|r| r.path)
}

/// [`try_config_dir`] for an explicit home: `<home>/.trusty-tools/install.toml`,
/// then `home`; `TRUSTY_MPM_CONFIG_DIR` first only when `home` is the
/// process's own home.
///
/// Why: callers that take `home` as a parameter (so tests stay hermetic)
/// must reach the same answer as [`try_config_dir`] for the real home
/// (#11821 P3). The env var configures this process's install, so a scratch
/// home passed by a test is not redirected by it.
/// What: the one-key chain with `home` in place of `dirs::home_dir()`.
/// Test: `try_config_dir_under_reads_the_install_file_of_that_home`.
pub fn try_config_dir_under(home: &Path) -> Result<PathBuf, InstallPathsError> {
    let own_home = dirs::home_dir().is_some_and(|h| h == home);
    let env = |n: &str| own_home.then(|| std::env::var(n).ok()).flatten();
    let hit = resolve_key_with(Key::ConfigDir, env, Some(home), &PathOverrides::default())?;
    required(Key::ConfigDir, hit).map(|r| r.path)
}

/// The process-env config dir for `crate_config`'s writers and `load`.
///
/// Why: a broken install file must fail a write closed, never redirect it
/// under `$HOME` (#11821).
/// What: `Ok(Some(dir))` when resolved; `Ok(None)` when home is unknown (the
/// pre-#11821 "no home" case); `Err` for a broken or invalid source that no
/// higher rung overrides.
/// Test: `save_fails_closed_on_broken_install_file`.
#[cfg(feature = "crate-config")]
pub(crate) fn config_dir_checked() -> Result<Option<PathBuf>, InstallPathsError> {
    match try_config_dir() {
        Ok(dir) => Ok(Some(dir)),
        Err(InstallPathsError::NoHome) => Ok(None),
        Err(err) => Err(err),
    }
}

/// The process-env config dir for the `Option`-returning `crate_config_dir`.
///
/// Why: `crate_config_dir` cannot surface an error without breaking callers
/// in other crates, and some of them write under the path it returns.
/// What: [`config_dir_checked`], with an error logged at `warn` and mapped
/// to `None`, the "no config dir" answer every caller already handles. It
/// never falls back to `$HOME` (#11821).
/// Test: `process_env_wrappers_match_today`, `save_fails_closed_on_broken_install_file`.
#[cfg(feature = "crate-config")]
pub(crate) fn config_dir_for_legacy_wrapper() -> Option<PathBuf> {
    config_dir_checked().unwrap_or_else(|err| {
        tracing::warn!(error = %err, "install paths unresolved; no config dir (fail closed)");
        None
    })
}

/// The projects root against an injected env and home; the pure core of
/// [`projects_root_checked`].
///
/// Why: the no-home arm cannot be reached through the process env on hosts
/// whose `dirs::home_dir()` falls back to the password database.
/// What: [`resolve_key_with`] for `projects_root` with `template` as the
/// template rung.
/// Test: `try_projects_root_fails_closed_without_a_home`.
pub(super) fn projects_root_with(
    env: impl Fn(&str) -> Option<String>,
    home: Option<&Path>,
    template: Option<&str>,
) -> Result<PathBuf, InstallPathsError> {
    let overrides = PathOverrides {
        projects_root_template: template.map(str::to_owned),
        ..PathOverrides::default()
    };
    let hit = resolve_key_with(Key::ProjectsRoot, env, home, &overrides)?;
    required(Key::ProjectsRoot, hit).map(|r| r.path)
}

/// The process-env projects root for
/// `workspace_layout::try_resolve_workspace_root`.
///
/// Why: a writer must not clone under a root the operator did not choose: a
/// broken install file, a relative value or an unknown home all fail closed
/// (df-D1t, #11821).
/// What: [`projects_root_with`] over the process env and `dirs::home_dir()`:
/// env, install file, `template`, default. `NoHome` is an error, never `/tmp`.
/// Test: `try_resolve_workspace_root_fails_closed_on_broken_install_file`,
/// `try_resolve_workspace_root_refuses_relative_env_and_template`,
/// `try_projects_root_fails_closed_without_a_home`.
pub(crate) fn projects_root_checked(template: Option<&str>) -> Result<PathBuf, InstallPathsError> {
    // #11821: no `/tmp` fallback; the caller refuses instead.
    let home = dirs::home_dir();
    projects_root_with(|n| std::env::var(n).ok(), home.as_deref(), template)
}

/// The process-env projects root for the legacy `resolve_workspace_root`
/// reader.
///
/// What: [`projects_root_checked`], with an error logged at `warn` and
/// mapped to the pre-#11821 default. Writers use the `try_` variant instead
/// (#11821).
/// Test: `process_env_wrappers_match_today`.
pub(crate) fn projects_root_for_legacy_wrapper(template: Option<&str>) -> PathBuf {
    projects_root_checked(template).unwrap_or_else(|err| {
        tracing::warn!(error = %err, "install paths unresolved; using the default projects root");
        legacy_projects_root()
    })
}

/// `$HOME/trusty-mpm-projects`, or under `/tmp` when home is unknown; the
/// reader-only fallback.
fn legacy_projects_root() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(DEFAULT_WORKSPACE_DIR)
}
