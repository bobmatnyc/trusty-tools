//! `$HOME/.trusty-tools/install.toml`: the install file's shape, reader and
//! writer (#11821).
//!
//! Why: the install file is the persistent rung of the [`super::InstallPaths`]
//! chain; P2's `tm paths set` and the console write it through
//! [`write_install_file`], so validation lives in one place.
//! What: [`InstallFile`] rejects unknown keys; [`read_install_file`] maps an
//! absent file to `None`; [`write_install_file`] expands `~` once, refuses
//! empty or relative values, and replaces the file atomically.
//! Test: `install_paths::tests` (`src/install_paths/tests.rs`).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{InstallPathsError, PathSource, TOOLS_DIR_NAME, validate};
use crate::workspace_layout::expand_tilde;

/// File name of the install file under `$HOME/.trusty-tools`.
pub const INSTALL_FILE_NAME: &str = "install.toml";

/// The keys `install.toml` may carry; every one is optional.
///
/// Why: a typo such as `project_root` must fail loudly, not leave the key at
/// its default (the Fail-Open Check), hence `deny_unknown_fields`.
/// What: four optional absolute paths, read literally (no `~` expansion).
/// Test: `unknown_key_is_rejected`, `writer_round_trips_and_expands_tilde`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallFile {
    /// Root under which managed projects are cloned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projects_root: Option<PathBuf>,
    /// Directory holding `.trusty-tools` and `.trusty-mpm`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_dir: Option<PathBuf>,
    /// Claude Code's user-tier config dir.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_config_dir: Option<PathBuf>,
    /// Codex's home dir.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_home: Option<PathBuf>,
}

/// `<home>/.trusty-tools/install.toml`, located from `$HOME` only.
///
/// Why: the config dir cannot name itself, so the file that relocates it
/// must sit at a fixed anchor.
/// What: joins `home`, [`TOOLS_DIR_NAME`] and [`INSTALL_FILE_NAME`].
/// Test: `install_file_is_anchored_at_home_not_config_dir`.
pub fn install_file_path(home: &Path) -> PathBuf {
    home.join(TOOLS_DIR_NAME).join(INSTALL_FILE_NAME)
}

/// Read `<home>/.trusty-tools/install.toml`.
///
/// Why: the resolver needs "absent" (no file rung) apart from "broken".
/// What: `Ok(None)` when the file does not exist; `Ok(Some(_))` on a parse;
/// `Read` on any other I/O error; `Parse` on bad TOML or an unknown key.
/// Values are not validated here; the resolver validates the winning ones.
/// Test: `missing_file_means_default`, `unknown_key_is_rejected`.
pub fn read_install_file(home: &Path) -> Result<Option<InstallFile>, InstallPathsError> {
    let path = install_file_path(home);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(InstallPathsError::Read { path, source }),
    };
    toml::from_str(&raw)
        .map(Some)
        .map_err(|e| InstallPathsError::Parse {
            path,
            message: e.message().to_string(),
        })
}

/// Write `<home>/.trusty-tools/install.toml`, replacing any existing file.
///
/// Why: the single writer P2's CLI and the console call, so a stored value is
/// always absolute and the resolver never sees a `~`.
/// What: expands a leading `~` in each value against `home` once, refuses an
/// empty or relative value (`Empty` / `Relative`, origin `InstallFile`),
/// creates `<home>/.trusty-tools` if needed, and writes through a temp file
/// plus rename. Returns the written path and the values as stored.
/// Test: `writer_round_trips_and_expands_tilde`, `writer_refuses_relative_paths`.
pub fn write_install_file(
    home: &Path,
    values: &InstallFile,
) -> Result<(PathBuf, InstallFile), InstallPathsError> {
    let stored = InstallFile {
        projects_root: normalise("projects_root", values.projects_root.as_ref(), home)?,
        config_dir: normalise("config_dir", values.config_dir.as_ref(), home)?,
        claude_config_dir: normalise("claude_config_dir", values.claude_config_dir.as_ref(), home)?,
        codex_home: normalise("codex_home", values.codex_home.as_ref(), home)?,
    };
    let body = toml::to_string(&stored).map_err(|e| InstallPathsError::Serialize {
        message: e.to_string(),
    })?;

    let path = install_file_path(home);
    let write_err = |source| InstallPathsError::Write {
        path: path.clone(),
        source,
    };
    let dir = path.parent().unwrap_or(home);
    std::fs::create_dir_all(dir).map_err(write_err)?;
    let tmp = dir.join(format!(".{INSTALL_FILE_NAME}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, body).map_err(write_err)?;
    if let Err(source) = std::fs::rename(&tmp, &path) {
        // Best effort: the rename error is the one worth reporting.
        let _ = std::fs::remove_file(&tmp);
        return Err(write_err(source));
    }
    Ok((path, stored))
}

/// Expand `~` once and validate one writer value.
fn normalise(
    key: &'static str,
    value: Option<&PathBuf>,
    home: &Path,
) -> Result<Option<PathBuf>, InstallPathsError> {
    let Some(raw) = value else {
        return Ok(None);
    };
    let expanded = match raw.to_str() {
        Some(s) => expand_tilde(s.trim(), home),
        None => raw.clone(),
    };
    validate(key, PathSource::InstallFile, &expanded)?;
    Ok(Some(expanded))
}
