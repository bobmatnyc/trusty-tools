//! Project registry model.
//!
//! Why: trusty-mpm manages Claude Code sessions grouped by *project* — a
//! registered working directory. The daemon, CLI, and dashboard all need a
//! shared notion of what a project is so sessions can be filtered and the
//! `project` subcommands have a stable wire type.
//! What: defines [`ProjectInfo`], the snapshot of a registered project
//! exchanged over the daemon's HTTP API.
//! Test: `cargo test -p trusty-mpm-core` round-trips a `ProjectInfo` through
//! JSON and checks name derivation from a path.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

/// A registered trusty-mpm project: a working directory under management.
///
/// Why: every project subcommand (`init`, `list`, `info`) and every
/// session-to-project association needs the same typed snapshot.
/// What: the absolute project path, a human name derived from the directory,
/// and the registration timestamp.
/// Test: `name_from_path_uses_dir_name`, `project_json_roundtrip`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ProjectInfo {
    /// Absolute path to the project's working directory.
    #[schema(value_type = String)]
    pub path: PathBuf,
    /// Human-readable name, derived from the directory name.
    pub name: String,
    /// When the project was first registered with the daemon.
    #[schema(value_type = String, format = "date-time")]
    pub registered_at: SystemTime,
}

impl ProjectInfo {
    /// Build a freshly-registered project from its directory path.
    ///
    /// Why: every call site that registers a project needs the same
    /// derivation rule — name from the final path component, timestamp now —
    /// so centralizing it prevents drift.
    /// What: derives `name` via [`name_from_path`] and stamps `registered_at`
    /// to the current time.
    /// Test: `new_derives_name_and_stamps_time`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let name = name_from_path(&path);
        Self {
            path,
            name,
            registered_at: SystemTime::now(),
        }
    }
}

/// Derive a human project name from a directory path.
///
/// Why: a project's name defaults to its directory name; this is the single
/// rule used by both `ProjectInfo::new` and the `project init` config skeleton.
/// What: returns the final path component, falling back to `"project"` for a
/// path with no usable component (e.g. `/`).
/// Test: `name_from_path_uses_dir_name`, `name_from_path_falls_back`.
pub fn name_from_path(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "project".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_from_path_uses_dir_name() {
        assert_eq!(name_from_path(Path::new("/work/my-app")), "my-app");
        assert_eq!(name_from_path(Path::new("/work/my-app/")), "my-app");
    }

    #[test]
    fn name_from_path_falls_back() {
        // A path with no final component yields the placeholder name.
        assert_eq!(name_from_path(Path::new("/")), "project");
    }

    #[test]
    fn new_derives_name_and_stamps_time() {
        let before = SystemTime::now();
        let info = ProjectInfo::new("/work/demo");
        assert_eq!(info.name, "demo");
        assert_eq!(info.path, PathBuf::from("/work/demo"));
        assert!(info.registered_at >= before);
    }

    #[test]
    fn project_json_roundtrip() {
        let info = ProjectInfo::new("/work/round-trip");
        let json = serde_json::to_string(&info).unwrap();
        let back: ProjectInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back, info);
    }
}
