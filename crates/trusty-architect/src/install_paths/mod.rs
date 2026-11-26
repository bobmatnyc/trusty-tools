//! The Architect's local reader of an install's host paths (#11821 P4-f2).
//!
//! Why: tm resolves the config dir and projects root through
//! `trusty_common::install_paths`, but this crate is a leaf (#9171 A1) and may
//! not depend on trusty-common. Architect ruling D1af(1), option A: a local
//! reader of `install.toml`, held to the trusty-common resolver by a parity
//! test, so the poller reads the claude-config dir tm writes to.
//! What: [`config_dir`] and [`projects_root`] apply trusty-common's one-key
//! chain, **env > install file > template (`projects_root` only) > default**
//! (Architect D1ae). Env values are trimmed (blank means unset) and `~`-expanded;
//! install-file values are taken literally; every winning value must be
//! absolute. The install file is read only when env does not decide the key,
//! so a broken file cannot block a valid env value; otherwise a broken file is
//! an error, never a `$HOME` fallback (as P3-f1/f2). The parity test in
//! trusty-crate-contracts runs this reader and trusty-common's over one
//! fixture set.
//! Test: `install_paths::tests` (`src/install_paths/tests.rs`).

mod file;

use std::path::{Path, PathBuf};

pub use file::{
    INSTALL_FILE_NAME, InstallFile, TOOLS_DIR_NAME, install_file_path, read_install_file,
};

/// Environment variable that relocates the config dir (trusty-common's
/// `CONFIG_DIR_ENV`).
pub const CONFIG_DIR_ENV: &str = "TRUSTY_MPM_CONFIG_DIR";

/// Environment variable that relocates the projects root (trusty-common's
/// `WORKSPACE_ROOT_ENV`).
pub const PROJECTS_ROOT_ENV: &str = "TRUSTY_MPM_WORKSPACE_ROOT";

/// The projects root's default directory under home.
pub const DEFAULT_PROJECTS_DIR: &str = "trusty-mpm-projects";

/// Where a resolved path came from; `Display` matches trusty-common's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathSource {
    /// A process environment variable.
    Env,
    /// `$HOME/.trusty-tools/install.toml`.
    InstallFile,
    /// trusty-mpm's `config.yaml` `workspace_root_template` (`projects_root`).
    Template,
    /// The built-in default.
    Default,
}

impl std::fmt::Display for PathSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Env => "env",
            Self::InstallFile => "install-file",
            Self::Template => "config-template",
            Self::Default => "default",
        })
    }
}

/// An absolute path plus the rung that supplied it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPath {
    /// The absolute path.
    pub path: PathBuf,
    /// The precedence rung that supplied `path`.
    pub source: PathSource,
}

/// Why an install path did not resolve; the arms mirror trusty-common's.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum InstallPathsError {
    /// A value was empty.
    #[error("{key} from {origin} is empty; give an absolute path")]
    Empty {
        /// The key, e.g. `config_dir`.
        key: &'static str,
        /// The source that carried the value.
        origin: PathSource,
    },
    /// A value was not an absolute path.
    #[error("{key} from {origin} must be an absolute path, got {value:?}")]
    Relative {
        /// The key, e.g. `config_dir`.
        key: &'static str,
        /// The source that carried the value.
        origin: PathSource,
        /// The rejected value.
        value: PathBuf,
    },
    /// The install file exists but could not be read.
    #[error("cannot read {path:?}: {source}")]
    Read {
        /// The install file path.
        path: PathBuf,
        /// The I/O error.
        source: std::io::Error,
    },
    /// The install file is outside the supported shape or names an unknown key.
    #[error("cannot parse {path:?}: {message}")]
    Parse {
        /// The install file path.
        path: PathBuf,
        /// The parser's message.
        message: String,
    },
}

/// The config dir: `TRUSTY_MPM_CONFIG_DIR`, then `install.toml` `config_dir`,
/// then `home`.
///
/// Why: tm's claude-config dir is `<config dir>/.trusty-tools/trusty-mpm/
/// claude-config`; the poller must read the same one (#11821 P4-f2).
/// What: the one-key chain over `get` and `home`; see the module doc.
/// Test: `config_dir_env_beats_file_beats_home`,
/// `broken_file_fails_closed_unless_env_decides`.
pub fn config_dir(
    home: &Path,
    get: &dyn Fn(&str) -> Option<String>,
) -> Result<ResolvedPath, InstallPathsError> {
    resolve("config_dir", CONFIG_DIR_ENV, home, get, None, |f| {
        f.config_dir.as_ref()
    })
    .map(|hit| hit.unwrap_or_else(|| default(home.to_path_buf())))
}

/// The projects root: `TRUSTY_MPM_WORKSPACE_ROOT`, then `install.toml`
/// `projects_root`, then `template`, then `<home>/trusty-mpm-projects`.
///
/// Why: the same chain trusty-common's `projects_root` key applies, including
/// its template rung (D1ae).
/// What: the one-key chain; `template` is trimmed, blank means absent, and
/// `~`-expanded.
/// Test: `projects_root_template_sits_below_the_file`.
pub fn projects_root(
    home: &Path,
    get: &dyn Fn(&str) -> Option<String>,
    template: Option<&str>,
) -> Result<ResolvedPath, InstallPathsError> {
    let key = "projects_root";
    let hit = resolve(key, PROJECTS_ROOT_ENV, home, get, template, |f| {
        f.projects_root.as_ref()
    })?;
    Ok(hit.unwrap_or_else(|| default(home.join(DEFAULT_PROJECTS_DIR))))
}

/// Env, install file and template rungs; `None` means the default applies.
fn resolve(
    key: &'static str,
    env_var: &str,
    home: &Path,
    get: &dyn Fn(&str) -> Option<String>,
    template: Option<&str>,
    pick: impl Fn(&InstallFile) -> Option<&PathBuf>,
) -> Result<Option<ResolvedPath>, InstallPathsError> {
    if let Some(raw) = get(env_var).filter(|v| !v.trim().is_empty()) {
        return absolute(key, PathSource::Env, expand_tilde(raw.trim(), home)).map(Some);
    }
    // #11821: a broken file is an error here, never a fall back to `home`.
    let file = read_install_file(home)?;
    if let Some(path) = file.as_ref().and_then(pick) {
        return absolute(key, PathSource::InstallFile, path.clone()).map(Some);
    }
    if let Some(raw) = template.map(str::trim).filter(|t| !t.is_empty()) {
        return absolute(key, PathSource::Template, expand_tilde(raw, home)).map(Some);
    }
    Ok(None)
}

fn default(path: PathBuf) -> ResolvedPath {
    ResolvedPath {
        path,
        source: PathSource::Default,
    }
}

/// trusty-common's `expand_tilde`: a leading `~/` or a bare `~` is `home`.
fn expand_tilde(raw: &str, home: &Path) -> PathBuf {
    match raw.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None if raw == "~" => home.to_path_buf(),
        None => PathBuf::from(raw),
    }
}

/// Accept `path` only when it is non-empty and absolute.
fn absolute(
    key: &'static str,
    origin: PathSource,
    path: PathBuf,
) -> Result<ResolvedPath, InstallPathsError> {
    if path.as_os_str().is_empty() {
        return Err(InstallPathsError::Empty { key, origin });
    }
    if !path.is_absolute() {
        return Err(InstallPathsError::Relative {
            key,
            origin,
            value: path,
        });
    }
    Ok(ResolvedPath {
        path,
        source: origin,
    })
}

#[cfg(test)]
mod tests;
