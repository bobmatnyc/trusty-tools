//! The base directory trusty-mpm's config trees nest under (#11821 P3).
//!
//! Why: `~/.trusty-mpm` and `~/.trusty-tools` were each derived from `$HOME`
//! at dozens of call sites, so an install-level config dir
//! (`TRUSTY_MPM_CONFIG_DIR`, `install.toml` `config_dir`) moved only the
//! paths `trusty_common::crate_config` builds. These accessors put every
//! trusty-mpm derivation on the one `trusty_common::install_paths` chain.
//! What: [`try_config_base`] fails closed; [`config_base_dir`] is the
//! `Option` form for callers that already handle "no home"; [`config_base`]
//! is the infallible reader form. [`framework_root_dir`] and
//! [`tools_root_dir`] join the two tree names.
//! Test: `config_base_follows_the_install_paths_chain` and
//! `framework_and_tools_dirs_nest_under_the_config_base` (env_serial).

use std::path::{Path, PathBuf};

use trusty_common::install_paths::InstallPathsError;

/// The configured config dir, failing closed.
///
/// Why: a writer must not land under `$HOME` when `install.toml` is broken.
/// What: `trusty_common::install_paths::try_config_dir` — env, install file,
/// `$HOME`; `NoHome` or the install-file error otherwise.
/// Test: `config_base_follows_the_install_paths_chain`.
pub fn try_config_base() -> Result<PathBuf, InstallPathsError> {
    trusty_common::install_paths::try_config_dir()
}

/// [`try_config_base`] as an `Option`, for a caller whose "no home" arm
/// already skips or refuses.
///
/// What: `None` on any error, logged at `warn`; never `$HOME` on a broken
/// install file (fail closed, #11821).
/// Test: `config_base_follows_the_install_paths_chain`.
pub fn config_base_dir() -> Option<PathBuf> {
    try_config_base()
        .inspect_err(|err| {
            tracing::warn!(error = %err, "install paths unresolved; no config dir (fail closed)");
        })
        .ok()
}

/// [`try_config_base`] for an infallible reader.
///
/// Why: `FrameworkPaths::default()` and its readers cannot carry an error.
/// The launch refuses on a broken install file (df-D1w), so a write through
/// this path never starts a session.
/// What: the configured config dir; on an error, logged at `warn`, the
/// pre-#11821 base (`$HOME`, or `.` with no home).
/// Test: `config_base_follows_the_install_paths_chain`.
pub fn config_base() -> PathBuf {
    try_config_base().unwrap_or_else(|err| {
        tracing::warn!(error = %err, "install paths unresolved; using $HOME as the config dir");
        crate::core::paths::FrameworkPaths::home_base()
    })
}

/// The config dir of the install whose home is `home`.
///
/// Why: many tm paths take `home` as a parameter so tests stay hermetic; the
/// config trees must still follow that home's `install.toml` (#11821 P3).
/// What: `trusty_common::install_paths::try_config_dir_under(home)`.
/// Test: `framework_and_tools_dirs_nest_under_the_config_base`.
pub fn try_config_base_under(home: &Path) -> Result<PathBuf, InstallPathsError> {
    trusty_common::install_paths::try_config_dir_under(home)
}

/// [`try_config_base_under`] for an infallible reader: on an error, logged
/// at `warn`, `home` itself (the pre-#11821 base).
///
/// Test: `framework_and_tools_dirs_nest_under_the_config_base`.
pub fn config_base_under(home: &Path) -> PathBuf {
    try_config_base_under(home).unwrap_or_else(|err| {
        tracing::warn!(error = %err, "install paths unresolved; using home as the config dir");
        home.to_path_buf()
    })
}

/// `<config dir of home>/.trusty-mpm`, the framework root for `home`.
///
/// Test: `framework_and_tools_dirs_nest_under_the_config_base`.
pub fn framework_root_under(home: &Path) -> PathBuf {
    config_base_under(home).join(crate::core::paths::FRAMEWORK_DIR_NAME)
}

/// `<config dir of home>/.trusty-tools`.
///
/// Test: `framework_and_tools_dirs_nest_under_the_config_base`.
pub fn tools_root_under(home: &Path) -> PathBuf {
    config_base_under(home).join(trusty_common::crate_config::TRUSTY_TOOLS_DIR)
}

/// `<config dir>/.trusty-mpm`, failing closed as an `Option`.
///
/// What: [`config_base_dir`] joined with
/// [`FRAMEWORK_DIR_NAME`](crate::core::paths::FRAMEWORK_DIR_NAME).
/// Test: `framework_and_tools_dirs_nest_under_the_config_base`.
pub fn framework_root_dir() -> Option<PathBuf> {
    config_base_dir().map(|base| base.join(crate::core::paths::FRAMEWORK_DIR_NAME))
}

/// `<config dir>/.trusty-tools`, failing closed as an `Option`.
///
/// What: [`config_base_dir`] joined with
/// [`TRUSTY_TOOLS_DIR`](trusty_common::crate_config::TRUSTY_TOOLS_DIR).
/// Test: `framework_and_tools_dirs_nest_under_the_config_base`.
pub fn tools_root_dir() -> Option<PathBuf> {
    config_base_dir().map(|base| base.join(trusty_common::crate_config::TRUSTY_TOOLS_DIR))
}
