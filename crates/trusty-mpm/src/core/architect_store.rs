//! The Architect directory and its dashboard store root, resolved without
//! the caller naming them (#10944).
//!
//! Why: `tm tui` and other socket clients cannot know where the Architect's
//! `local/owner-queue` lives; the Architect ruled (Q2, 2026-11-10) that the
//! daemon resolves it. `tm fleet` already resolves the Architect directory,
//! so the rule lives here once and `fleet::resolve_dir` delegates (Q5).
//! What: [`pick_architect_dir`] and [`recorded_architects`] are the shared
//! Architect-directory rule; [`resolve_store_root`] adds the env overrides,
//! a strict config read and the existence check; [`normalize_params`] fills
//! `store.root` in a dashboard request that omits it.
//! Test: `core/architect_store_tests.rs`,
//! `omitted_store_lists_and_tallies_the_resolved_store`.

use std::fmt;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};
use trusty_architect::dashboard::DashError;

use crate::core::config::MpmConfig;
use crate::core::session_profile;

#[cfg(test)]
#[path = "architect_store_tests.rs"]
mod tests;

/// The Architect's directory under the user home (ruling Q4 of #8436).
pub const DEFAULT_ARCHITECT_DIR: &str = "trusty-mpm-projects/architect";

/// The dashboard store under an Architect directory.
pub const STORE_SUBDIR: &str = "local/owner-queue";

/// Env naming the store root itself (the name `tm architect dashboard` reads).
pub const ENV_DASHBOARD_ROOT: &str = "DASHBOARD_ROOT";

/// Env naming the Architect directory.
pub const ENV_ARCHITECT_PROJECT_DIR: &str = "ARCHITECT_PROJECT_DIR";

/// `~/.trusty-mpm/config.toml` under `home`.
pub fn user_config_path(home: &Path) -> PathBuf {
    home.join(".trusty-mpm").join("config.toml")
}

/// Every absolute `[architect]` (legacy `[supervisor]`) `projects` entry
/// whose `.trusty-mpm.toml` requests the Architect profile: the Architects
/// `tm fleet init` recorded.
///
/// Test: `recorded_architect_beats_the_default`,
/// `status_without_dir_checks_the_recorded_architect`.
pub fn recorded_architects(config: &MpmConfig) -> Vec<PathBuf> {
    config
        .architect_grant()
        .projects
        .iter()
        .filter(|entry| entry.is_absolute() && session_profile::requested(entry).is_architect())
        .cloned()
        .collect()
}

/// Which Architect directory `recorded` names: the one entry, else
/// [`DEFAULT_ARCHITECT_DIR`] under `home`.
///
/// # Errors
/// More than one entry: the entries, so the caller names them.
///
/// Test: `two_recorded_architects_are_refused`, `two_recorded_architects_ask_for_dir`.
pub fn pick_architect_dir(recorded: &[PathBuf], home: &Path) -> Result<PathBuf, Vec<PathBuf>> {
    match recorded {
        [] => Ok(home.join(DEFAULT_ARCHITECT_DIR)),
        [one] => Ok(one.clone()),
        many => Err(many.to_vec()),
    }
}

/// Where a resolved store root came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootSource {
    /// Env [`ENV_DASHBOARD_ROOT`].
    EnvDashboardRoot,
    /// Env [`ENV_ARCHITECT_PROJECT_DIR`].
    EnvArchitectDir,
    /// The one Architect recorded in this user config file.
    Config(PathBuf),
    /// [`DEFAULT_ARCHITECT_DIR`]: nothing set, nothing recorded.
    Default,
}

impl fmt::Display for RootSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EnvDashboardRoot => write!(f, "env {ENV_DASHBOARD_ROOT}"),
            Self::EnvArchitectDir => write!(f, "env {ENV_ARCHITECT_PROJECT_DIR}"),
            Self::Config(path) => write!(f, "the Architect recorded in {}", path.display()),
            Self::Default => write!(f, "the default ~/{DEFAULT_ARCHITECT_DIR}"),
        }
    }
}

/// A store root the daemon chose for a caller that named none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRoot {
    /// The store root, absolute and an existing directory.
    pub path: PathBuf,
    /// Where it came from.
    pub source: RootSource,
}

/// The fix every resolution error names.
const FIX: &str = "run `tm fleet init`, or pass `store.root` (`tm tui --architect-store <dir>`)";

fn unavailable(what: impl fmt::Display) -> DashError {
    DashError::Unavailable(format!(
        "cannot resolve the Architect dashboard store: {what}; {FIX}"
    ))
}

/// The dashboard store root for a caller that gave none.
///
/// Why: #10944 — the daemon resolves the root (Architect ruling Q2); a
/// missing store must never read as an empty dashboard, because
/// `Store::list` answers `[]` for a missing directory (Q6).
/// What: the first set of: `env(DASHBOARD_ROOT)`; `env(ARCHITECT_PROJECT_DIR)`
/// joined with [`STORE_SUBDIR`]; the one Architect in `home`'s user config
/// (strict parse, never the defaulting loader); [`DEFAULT_ARCHITECT_DIR`].
/// The result must be absolute and an existing directory.
///
/// # Errors
/// [`DashError::Unavailable`] (exit 3) naming the path tried, its source and
/// the fix: no usable home, an unreadable or malformed config, more than one
/// recorded Architect, recorded entries none of which requests the Architect
/// profile, a relative path, a path that is not a directory.
///
/// Test: `env_dashboard_root_wins_over_config`, `recorded_architect_beats_the_default`,
/// `default_dir_is_used_when_no_config`, `missing_owner_queue_dir_is_an_error_not_empty`,
/// `malformed_config_is_refused_not_defaulted`, `no_home_is_refused`,
/// `root_that_is_a_file_is_refused`, `recorded_dir_without_profile_is_refused_not_defaulted`.
pub fn resolve_store_root(
    env: &dyn Fn(&str) -> Option<String>,
    home: Option<&Path>,
) -> Result<ResolvedRoot, DashError> {
    let set = |name: &str| env(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    let (path, source) = if let Some(root) = set(ENV_DASHBOARD_ROOT) {
        (root, RootSource::EnvDashboardRoot)
    } else if let Some(dir) = set(ENV_ARCHITECT_PROJECT_DIR) {
        (dir.join(STORE_SUBDIR), RootSource::EnvArchitectDir)
    } else {
        let home = home
            .filter(|h| h.is_absolute())
            .ok_or_else(|| unavailable("the daemon has no home directory"))?;
        architect_dir_from_config(home)?
    };
    // #10944 Q6: the existence check that keeps a missing store from listing as `[]`.
    if !path.is_absolute() {
        return Err(unavailable(format_args!(
            "{} (from {source}) is not an absolute path",
            path.display()
        )));
    }
    if !path.is_dir() {
        return Err(unavailable(format_args!(
            "{} (from {source}) is not a directory",
            path.display()
        )));
    }
    Ok(ResolvedRoot { path, source })
}

/// The store under the Architect `home`'s user config records, else under
/// the default directory.
fn architect_dir_from_config(home: &Path) -> Result<(PathBuf, RootSource), DashError> {
    let config_path = user_config_path(home);
    let config = match std::fs::read_to_string(&config_path) {
        Ok(raw) => toml::from_str::<MpmConfig>(&raw).map_err(|e| {
            unavailable(format_args!("{} is malformed: {e}", config_path.display()))
        })?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => MpmConfig::default(),
        Err(e) => {
            return Err(unavailable(format_args!(
                "cannot read {}: {e}",
                config_path.display()
            )));
        }
    };
    let recorded = recorded_architects(&config);
    // #10944 critic HIGH: a recorded entry that lost its Architect profile must
    // not fall through to the default store (the #8995 shape).
    let listed: Vec<PathBuf> = config
        .architect_grant()
        .projects
        .iter()
        .filter(|entry| entry.is_absolute())
        .cloned()
        .collect();
    if recorded.is_empty() && !listed.is_empty() {
        return Err(unavailable(format_args!(
            "{} records {} but none has a `.trusty-mpm.toml` requesting the Architect profile",
            config_path.display(),
            join_paths(&listed)
        )));
    }
    let source = if recorded.is_empty() {
        RootSource::Default
    } else {
        RootSource::Config(config_path.clone())
    };
    let dir = pick_architect_dir(&recorded, home).map_err(|many| {
        unavailable(format_args!(
            "{} records more than one Architect ({})",
            config_path.display(),
            join_paths(&many)
        ))
    })?;
    Ok((dir.join(STORE_SUBDIR), source))
}

/// `a, b` of the paths' display forms.
fn join_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// `params` with `store.root` filled in when the caller omitted it.
///
/// Why: #10944 Q4 — the resolution lives on the daemon path only, before
/// decode, so `StoreSel.root` stays required for the CLI and the MCP tool and
/// a request carrying `store.root` reaches the verb byte-for-byte unchanged.
/// What: `null` params count as `{}`. An object without `store` gets
/// `store: {"root": <resolved>}`; a `store` object without `root` gets `root`
/// and keeps its other keys. Anything else is returned untouched, so decode
/// still refuses it (unknown keys included). `resolve` runs only in the two
/// filling cases; the reply's second half is the root it chose.
///
/// # Errors
/// `resolve`'s error.
///
/// Test: `explicit_store_root_is_never_resolved`,
/// `missing_store_gets_the_resolved_root_and_dir_format`,
/// `store_without_root_keeps_format_actor_and_prune_log`.
pub fn normalize_params(
    params: Value,
    resolve: impl FnOnce() -> Result<ResolvedRoot, DashError>,
) -> Result<(Value, Option<ResolvedRoot>), DashError> {
    let mut object = match params {
        Value::Null => Map::new(),
        Value::Object(object) => object,
        other => return Ok((other, None)),
    };
    let needs_root = match object.get("store") {
        None => true,
        Some(Value::Object(store)) => !store.contains_key("root"),
        Some(_) => false,
    };
    if !needs_root {
        return Ok((Value::Object(object), None));
    }
    let resolved = resolve()?;
    let root = Value::String(resolved.path.to_string_lossy().into_owned());
    match object.get_mut("store") {
        Some(Value::Object(store)) => {
            store.insert("root".to_owned(), root);
        }
        _ => {
            object.insert(
                "store".to_owned(),
                Value::Object(Map::from_iter([("root".to_owned(), root)])),
            );
        }
    }
    Ok((Value::Object(object), Some(resolved)))
}
