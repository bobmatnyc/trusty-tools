//! The Secrets page's project picker, read from the tm project registry
//! (#9067, DOC-74 §15.6).
//!
//! Why: the page must pick a project from the registry tm keeps, never a
//! typed path and never the console's own working directory (Architect
//! ruling). A secrets call needs a checkout directory, and tm's
//! `project-paths.json` is the registry that records one per project.
//! What: [`default_registry_file`] locates that file as tm does;
//! [`read_registry`] turns it into [`PickerProject`] rows, and
//! [`registry_lists`] tells a bridged call whether its project is a path
//! the registry lists (#9067 PR B1). The console reads
//! the file itself, as trusty-search's allowlist does, so no crate gains a
//! dependency edge for it (owner ruling Df0).
//! Test: `projects_route_lists_the_registry_and_never_dials`,
//! `projects_route_answers_an_empty_list_for_an_absent_or_empty_registry`,
//! `projects_route_refuses_a_malformed_or_unknown_registry`,
//! `registry_file_follows_the_tm_root`,
//! `an_unlisted_project_is_refused_on_every_route_without_a_dial`.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The environment variable tm reads for its root directory.
pub const MPM_ROOT_ENV: &str = "TRUSTY_MPM_ROOT";
/// The registry's file name under tm's root.
pub const REGISTRY_FILE: &str = "project-paths.json";

/// One row of `project-paths.json`, as tm's `ProjectAliasEntry` writes it.
///
/// What: unknown fields are ignored, so tm can grow the record.
#[derive(Deserialize)]
struct RegistryRow {
    alias: String,
    path: PathBuf,
}

/// One project the page can pick.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PickerProject {
    /// tm's alias for the project.
    pub name: String,
    /// The checkout's absolute path, the `project` field of a secrets call.
    pub path: String,
}

/// Why the registry could not be read. The page sees fixed text only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryError {
    /// The file does not exist (#9067 PR B1: the picker reads it as empty).
    Missing,
    /// The file exists but could not be read.
    Unreadable,
    /// The file is not a JSON array of registry rows.
    Malformed,
}

/// `$TRUSTY_MPM_ROOT/project-paths.json`, else `~/.trusty-mpm/project-paths.json`.
///
/// What: `None` when neither is known, so the console never falls back to a
/// path relative to its own working directory.
/// Test: `registry_file_follows_the_tm_root`.
pub fn default_registry_file() -> Option<PathBuf> {
    registry_file_for(std::env::var_os(MPM_ROOT_ENV), dirs::home_dir())
}

/// [`default_registry_file`] for a given tm root and home directory.
pub(crate) fn registry_file_for(root: Option<OsString>, home: Option<PathBuf>) -> Option<PathBuf> {
    match root {
        Some(root) if !root.is_empty() => Some(PathBuf::from(root).join(REGISTRY_FILE)),
        _ => home.map(|home| home.join(".trusty-mpm").join(REGISTRY_FILE)),
    }
}

/// The projects registered in `file`, in file order.
///
/// What: an absent file is an empty list. A row is kept only when its path
/// is absolute, valid UTF-8 and an existing directory, since the page can
/// only bind a checkout that exists here.
///
/// # Errors
///
/// [`RegistryError`]; the I/O or serde message is dropped, never shown.
pub fn read_registry(file: &Path) -> Result<Vec<PickerProject>, RegistryError> {
    let rows = match load_rows(file) {
        Ok(rows) => rows,
        Err(RegistryError::Missing) => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    Ok(rows
        .into_iter()
        .filter(|row| row.path.is_absolute() && row.path.is_dir())
        .filter_map(|row| {
            let path = row.path.to_str()?.to_owned();
            Some(PickerProject {
                name: row.alias,
                path,
            })
        })
        .collect())
}

/// Whether the registry in `file` lists `project`, byte for byte.
///
/// Why: #9067 PR B1, Architect ruling — a bridged call may name only a
/// project the tm registry lists, so a page or script cannot point the
/// secrets server at an arbitrary checkout.
/// What: reads the file fresh on each call and compares `project` with each
/// row's `path` as raw bytes. There is no prefix, case-fold, normalisation
/// or substring match; `Path` equality is avoided because it ignores a
/// trailing slash and repeated separators. Every row counts, not only the
/// rows the picker shows.
/// Test: `an_unlisted_project_is_refused_on_every_route_without_a_dial`,
/// `near_miss_project_names_are_refused_without_a_dial`,
/// `a_registry_edit_between_calls_is_seen_by_the_next_call`.
///
/// # Errors
///
/// [`RegistryError`], including [`RegistryError::Missing`]: an absent
/// registry lists nothing and the caller refuses the call.
pub fn registry_lists(file: &Path, project: &str) -> Result<bool, RegistryError> {
    let wanted = OsStr::new(project);
    Ok(load_rows(file)?
        .iter()
        .any(|row| row.path.as_os_str() == wanted))
}

/// Every row of `file`; the I/O or serde message is dropped, never shown.
fn load_rows(file: &Path) -> Result<Vec<RegistryRow>, RegistryError> {
    let raw = match std::fs::read_to_string(file) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(RegistryError::Missing),
        Err(_) => return Err(RegistryError::Unreadable),
    };
    serde_json::from_str(&raw).map_err(|_| RegistryError::Malformed)
}
