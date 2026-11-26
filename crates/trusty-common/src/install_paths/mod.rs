//! The one resolver for a trusty-tools install's host paths (#11821).
//!
//! Why: the projects root (`~/trusty-mpm-projects`) and the config dir (the
//! directory holding `.trusty-tools` and `.trusty-mpm`) were each derived from
//! `$HOME` at many call sites, with no install-level way to move them. A single
//! resolver, in the crate every consumer already depends on, gives one
//! precedence chain and one record of which source won.
//!
//! What: [`InstallPaths::resolve`] applies **flag > env > install file >
//! default** per key and returns absolute paths plus the winning
//! [`PathSource`]. For `projects_root` only, the legacy `config.yaml`
//! `workspace_root_template` sits between the install file and the default;
//! `workspace_layout`'s workspace-root resolvers are thin wrappers over this
//! one chain (#11821). The install file is always `$HOME/.trusty-tools/install.toml`
//! ([`install_file_path`]), never under the configured config dir, because the
//! config dir cannot name itself. Every value must be absolute: a relative or
//! empty value is an [`InstallPathsError`], never a silent default.
//! [`write_install_file`] is the one writer. The module is feature-free and has
//! no YAML dependency.
//!
//! Test: `install_paths::tests` (`src/install_paths/tests.rs`).
//!
//! [`InstallPaths::resolve`]: crate::install_paths::InstallPaths::resolve
//! [`PathSource`]: crate::install_paths::PathSource
//! [`install_file_path`]: crate::install_paths::install_file_path
//! [`InstallPathsError`]: crate::install_paths::InstallPathsError
//! [`write_install_file`]: crate::install_paths::write_install_file

mod file;
mod process;

use std::path::{Path, PathBuf};

pub use file::{InstallFile, install_file_path, read_install_file, write_install_file};
#[cfg(feature = "crate-config")]
pub(crate) use process::{config_dir_checked, config_dir_for_legacy_wrapper};
pub(crate) use process::{projects_root_checked, projects_root_for_legacy_wrapper};
pub use process::{try_config_dir, try_config_dir_under};

use crate::workspace_layout::{DEFAULT_WORKSPACE_DIR, WORKSPACE_ROOT_ENV, expand_tilde};

/// Directory under the config dir that holds every crate's config tree.
pub const TOOLS_DIR_NAME: &str = ".trusty-tools";

/// Directory under the config dir that holds trusty-mpm's framework state.
pub const MPM_DIR_NAME: &str = ".trusty-mpm";

/// Environment variable that relocates the config dir (#11821).
pub const CONFIG_DIR_ENV: &str = "TRUSTY_MPM_CONFIG_DIR";

/// Environment variable Claude Code reads for its user-tier config dir.
pub const CLAUDE_CONFIG_DIR_ENV: &str = "CLAUDE_CONFIG_DIR";

/// Environment variable Codex reads for its home dir.
pub const CODEX_HOME_ENV: &str = "CODEX_HOME";

/// Where a resolved path came from.
///
/// Why: `tm paths show` and `tm doctor` (P2) report the winning source per key,
/// so an operator can tell a stray env var from the install file.
/// What: one variant per rung of the precedence chain, highest first.
/// Test: `flag_beats_env_file_and_default`, `env_beats_file_and_default`,
/// `file_beats_default`, `missing_file_means_default`,
/// `file_beats_template_and_template_beats_default`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathSource {
    /// An explicit override passed in by the caller (a CLI flag).
    Flag,
    /// A process environment variable.
    Env,
    /// `$HOME/.trusty-tools/install.toml`.
    InstallFile,
    /// trusty-mpm's `config.yaml` `workspace_root_template` (`projects_root`
    /// only), below the install file (#11821).
    Template,
    /// The built-in default.
    Default,
}

impl std::fmt::Display for PathSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Flag => "flag",
            Self::Env => "env",
            Self::InstallFile => "install-file",
            Self::Template => "config-template",
            Self::Default => "default",
        })
    }
}

/// An absolute path plus the source that supplied it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPath {
    /// The absolute path.
    pub path: PathBuf,
    /// The precedence rung that supplied `path`.
    pub source: PathSource,
}

/// Caller-supplied inputs: the flag rung (CLI flags in P2) plus the legacy
/// `config.yaml` template for `projects_root`.
///
/// A `None` flag field defers to env, then the install file, then the
/// default. `projects_root_template` is NOT a flag: it sits below the install
/// file (#11821).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PathOverrides {
    /// Override for the projects root.
    pub projects_root: Option<PathBuf>,
    /// Override for the config dir.
    pub config_dir: Option<PathBuf>,
    /// Override for Claude Code's config dir.
    pub claude_config_dir: Option<PathBuf>,
    /// Override for the Codex home.
    pub codex_home: Option<PathBuf>,
    /// trusty-mpm's `config.yaml` `workspace_root_template`, may lead with
    /// `~`; consulted after the install file and before the default.
    pub projects_root_template: Option<String>,
}

/// Errors from resolving or writing install paths.
///
/// Why: a misconfigured path must surface, not fall back to a default the
/// operator did not choose (the Fail-Open Check).
/// What: home missing, an empty or relative value (with its key and source),
/// and install-file I/O, parse and serialise failures.
/// Test: `relative_file_value_is_rejected`, `empty_file_value_is_rejected`,
/// `relative_env_value_is_rejected`, `unknown_key_is_rejected`,
/// `writer_refuses_relative_paths`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum InstallPathsError {
    /// The home directory could not be determined.
    #[error("the home directory could not be determined")]
    NoHome,
    /// A value was empty.
    #[error("{key} from {origin} is empty; give an absolute path")]
    Empty {
        /// The key, e.g. `projects_root`.
        key: &'static str,
        /// The source that carried the value.
        origin: PathSource,
    },
    /// A value was not an absolute path.
    #[error("{key} from {origin} must be an absolute path, got {value:?}")]
    Relative {
        /// The key, e.g. `projects_root`.
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
    /// The install file is not valid TOML or names an unknown key.
    #[error("cannot parse {path:?}: {message}")]
    Parse {
        /// The install file path.
        path: PathBuf,
        /// The parser's message.
        message: String,
    },
    /// The install file could not be written.
    #[error("cannot write {path:?}: {source}")]
    Write {
        /// The install file path.
        path: PathBuf,
        /// The I/O error.
        source: std::io::Error,
    },
    /// The install file could not be serialised.
    #[error("cannot serialise the install file: {message}")]
    Serialize {
        /// The serialiser's message.
        message: String,
    },
}

/// The resolved host paths of one trusty-tools install.
///
/// Why: one value every crate can ask for the projects root and config dir.
/// What: `projects_root` and `config_dir` always resolve (the default is
/// `~/trusty-mpm-projects` and `~`); `claude_config_dir` and `codex_home` are
/// `None` when no rung names them, meaning "the tool's own default".
/// Test: `defaults_match_today`, `tools_and_mpm_dirs_sit_under_config_dir`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallPaths {
    /// Root under which managed projects are cloned.
    pub projects_root: ResolvedPath,
    /// Directory holding `.trusty-tools` and `.trusty-mpm`.
    pub config_dir: ResolvedPath,
    /// Claude Code's user-tier config dir, when configured.
    pub claude_config_dir: Option<ResolvedPath>,
    /// Codex's home dir, when configured.
    pub codex_home: Option<ResolvedPath>,
}

/// The four keys, shared by the resolver and the install file.
#[derive(Clone, Copy)]
enum Key {
    ProjectsRoot,
    ConfigDir,
    ClaudeConfigDir,
    CodexHome,
}

impl Key {
    fn name(self) -> &'static str {
        match self {
            Self::ProjectsRoot => "projects_root",
            Self::ConfigDir => "config_dir",
            Self::ClaudeConfigDir => "claude_config_dir",
            Self::CodexHome => "codex_home",
        }
    }

    fn env_var(self) -> &'static str {
        match self {
            Self::ProjectsRoot => WORKSPACE_ROOT_ENV,
            Self::ConfigDir => CONFIG_DIR_ENV,
            Self::ClaudeConfigDir => CLAUDE_CONFIG_DIR_ENV,
            Self::CodexHome => CODEX_HOME_ENV,
        }
    }

    fn pick(self, o: &PathOverrides) -> Option<&PathBuf> {
        match self {
            Self::ProjectsRoot => o.projects_root.as_ref(),
            Self::ConfigDir => o.config_dir.as_ref(),
            Self::ClaudeConfigDir => o.claude_config_dir.as_ref(),
            Self::CodexHome => o.codex_home.as_ref(),
        }
    }

    /// The non-blank template rung; `projects_root` is the only templated key.
    fn pick_template(self, o: &PathOverrides) -> Option<&str> {
        match self {
            Self::ProjectsRoot => o
                .projects_root_template
                .as_deref()
                .map(str::trim)
                .filter(|t| !t.is_empty()),
            Self::ConfigDir | Self::ClaudeConfigDir | Self::CodexHome => None,
        }
    }

    fn pick_file(self, f: &InstallFile) -> Option<&PathBuf> {
        match self {
            Self::ProjectsRoot => f.projects_root.as_ref(),
            Self::ConfigDir => f.config_dir.as_ref(),
            Self::ClaudeConfigDir => f.claude_config_dir.as_ref(),
            Self::CodexHome => f.codex_home.as_ref(),
        }
    }

    fn default_for(self, home: &Path) -> Option<PathBuf> {
        match self {
            Self::ProjectsRoot => Some(home.join(DEFAULT_WORKSPACE_DIR)),
            Self::ConfigDir => Some(home.to_path_buf()),
            Self::ClaudeConfigDir | Self::CodexHome => None,
        }
    }
}

impl InstallPaths {
    /// Resolve from the process env and `$HOME`, with no caller overrides.
    ///
    /// Why: the zero-argument entry point for code with no CLI flags to pass.
    /// What: [`Self::resolve_overridden`] with empty [`PathOverrides`].
    /// Test: `defaults_match_today` and the other `resolve_with` tests.
    pub fn resolve() -> Result<Self, InstallPathsError> {
        Self::resolve_overridden(&PathOverrides::default())
    }

    /// Resolve from the process env and `$HOME`, with caller overrides on top.
    ///
    /// Why: P2's `tm` global flags pass their values here.
    /// What: reads `dirs::home_dir()` and `std::env::var`, then
    /// [`Self::resolve_with`]. Errors with `NoHome` when home is unknown.
    /// Test: `flag_beats_env_file_and_default` (via `resolve_with`).
    pub fn resolve_overridden(overrides: &PathOverrides) -> Result<Self, InstallPathsError> {
        let home = dirs::home_dir().ok_or(InstallPathsError::NoHome)?;
        Self::resolve_with(|name| std::env::var(name).ok(), &home, overrides)
    }

    /// Resolve against an injected env lookup and home dir.
    ///
    /// Why: the pure core, so tests never mutate process-global env.
    /// What: per key takes the first of override, env, file, template
    /// (`projects_root` only), default. The
    /// install file is read at most once, and only when some key is not decided
    /// by an override or env (#11821), so a broken file cannot block a valid
    /// env value. Override and env values are trimmed (empty env means unset)
    /// and `~`-expanded; every winning value must be absolute.
    /// Test: `flag_beats_env_file_and_default`, `env_beats_file_and_default`,
    /// `env_wins_over_broken_file`,
    /// `file_beats_default`, `missing_file_means_default`,
    /// `relative_env_value_is_rejected`, `relative_flag_value_is_rejected`.
    pub fn resolve_with(
        env: impl Fn(&str) -> Option<String>,
        home: &Path,
        overrides: &PathOverrides,
    ) -> Result<Self, InstallPathsError> {
        let mut file: Option<Option<InstallFile>> = None;
        let mut one = |key: Key| -> Result<Option<ResolvedPath>, InstallPathsError> {
            if let Some(hit) = resolve_early(key, &env, home, overrides)? {
                return Ok(Some(hit));
            }
            if file.is_none() {
                file = Some(read_install_file(home)?);
            }
            resolve_late(key, file.as_ref().and_then(Option::as_ref), overrides, home)
        };
        let projects_root = required(Key::ProjectsRoot, one(Key::ProjectsRoot)?)?;
        let config_dir = required(Key::ConfigDir, one(Key::ConfigDir)?)?;
        Ok(Self {
            projects_root,
            config_dir,
            claude_config_dir: one(Key::ClaudeConfigDir)?,
            codex_home: one(Key::CodexHome)?,
        })
    }

    /// `<config_dir>/.trusty-tools`.
    pub fn tools_dir(&self) -> PathBuf {
        self.config_dir.path.join(TOOLS_DIR_NAME)
    }

    /// `<config_dir>/.trusty-mpm`.
    pub fn mpm_dir(&self) -> PathBuf {
        self.config_dir.path.join(MPM_DIR_NAME)
    }
}

/// A key that always has a default must have resolved.
fn required(key: Key, hit: Option<ResolvedPath>) -> Result<ResolvedPath, InstallPathsError> {
    hit.ok_or(InstallPathsError::Empty {
        key: key.name(),
        origin: PathSource::Default,
    })
}

/// The override and env rungs; `None` means neither decides the key.
fn resolve_early(
    key: Key,
    env: &impl Fn(&str) -> Option<String>,
    home: &Path,
    overrides: &PathOverrides,
) -> Result<Option<ResolvedPath>, InstallPathsError> {
    if let Some(raw) = key.pick(overrides) {
        let expanded = expand_raw(raw, home);
        return absolute(key, PathSource::Flag, expanded).map(Some);
    }
    if let Some(raw) = env(key.env_var()).filter(|v| !v.trim().is_empty()) {
        let expanded = expand_tilde(raw.trim(), home);
        return absolute(key, PathSource::Env, expanded).map(Some);
    }
    Ok(None)
}

/// The file, template and default rungs; `None` only for a key with no default.
fn resolve_late(
    key: Key,
    file: Option<&InstallFile>,
    overrides: &PathOverrides,
    home: &Path,
) -> Result<Option<ResolvedPath>, InstallPathsError> {
    if let Some(path) = file.and_then(|f| key.pick_file(f)) {
        // install.toml values are taken literally: no `~`, no trimming.
        return absolute(key, PathSource::InstallFile, path.clone()).map(Some);
    }
    // #11821: the template is one rung of this chain, validated like the rest.
    if let Some(raw) = key.pick_template(overrides) {
        return absolute(key, PathSource::Template, expand_tilde(raw, home)).map(Some);
    }
    Ok(key.default_for(home).map(|path| ResolvedPath {
        path,
        source: PathSource::Default,
    }))
}

/// `~`-expand an override that is valid UTF-8; pass any other through.
fn expand_raw(raw: &Path, home: &Path) -> PathBuf {
    match raw.to_str() {
        Some(s) => expand_tilde(s.trim(), home),
        None => raw.to_path_buf(),
    }
}

/// Accept `path` only when it is non-empty and absolute.
fn absolute(
    key: Key,
    origin: PathSource,
    path: PathBuf,
) -> Result<ResolvedPath, InstallPathsError> {
    validate(key.name(), origin, &path)?;
    Ok(ResolvedPath {
        path,
        source: origin,
    })
}

/// The shared empty/relative check for the resolver and the writer.
fn validate(key: &'static str, origin: PathSource, path: &Path) -> Result<(), InstallPathsError> {
    if path.as_os_str().is_empty() {
        return Err(InstallPathsError::Empty { key, origin });
    }
    if !path.is_absolute() {
        return Err(InstallPathsError::Relative {
            key,
            origin,
            value: path.to_path_buf(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests;
