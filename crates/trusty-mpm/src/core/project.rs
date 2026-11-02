//! Project registry model.
//!
//! Why: trusty-mpm manages Claude Code sessions grouped by *project* — a
//! registered working directory. The daemon, CLI, and dashboard all need a
//! shared notion of what a project is so sessions can be filtered and the
//! `project` subcommands have a stable wire type.
//! What: defines [`ProjectInfo`], the snapshot of a registered project
//! exchanged over the daemon's HTTP API, and [`RegisterProjectResponse`], the
//! `POST /projects` body that adds the record init kept (#10586).
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

/// Response of `POST /projects` and `mpm.projects.register` (#10586).
///
/// Why: init writes a registry-B record unless one already covers the
/// directory's name or `owner/repo`. That match can be a different repo (the
/// key drops the host), so a bare `200` hid that nothing was written.
/// What: the [`ProjectInfo`] fields, flattened so the wire stays a superset of
/// the old body, plus `kept_existing`. The field is ABSENT when the record was
/// written, and present when an existing record was kept instead.
/// Test: `project_init_reports_the_kept_record_for_a_different_origin_10586`,
/// `project_init_kept_record_url_carries_no_userinfo_10586`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RegisterProjectResponse {
    /// The announced project.
    #[serde(flatten)]
    pub info: ProjectInfo,
    /// The registry record kept in place of a write. Absent (never `null`)
    /// when the record was written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub kept_existing: Option<KeptRegistryRecord>,
}

/// The registry-B record `tm project init` kept rather than wrote (#10586).
///
/// What: the record's name and its `repo_url` with any userinfo removed
/// before it leaves the daemon.
/// Test: `project_init_kept_record_url_carries_no_userinfo_10586`,
/// `kept_record_notice_names_the_record_without_userinfo_10586`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct KeptRegistryRecord {
    /// The kept record's registry name.
    pub name: String,
    /// The kept record's `repo_url`, userinfo removed.
    pub repo_url: String,
}

impl KeptRegistryRecord {
    /// The record as the daemon may show it: `repo_url` without userinfo.
    ///
    /// Test: `project_init_kept_record_url_carries_no_userinfo_10586`.
    pub fn redacted(name: &str, repo_url: &str) -> Self {
        Self {
            name: name.to_string(),
            repo_url: crate::core::remote_url_redact::url_without_userinfo(repo_url),
        }
    }

    /// The one line the CLI and Telegram print when init kept this record.
    ///
    /// What: re-strips the URL, so a daemon that sent userinfo still never
    /// puts it on screen. Plain text; Telegram HTML-escapes it.
    /// Test: `kept_record_notice_names_the_record_without_userinfo_10586`.
    pub fn notice(&self) -> String {
        format!(
            "already registered as {} ({}); nothing written",
            self.name,
            crate::core::remote_url_redact::url_without_userinfo(&self.repo_url)
        )
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

    /// Why: the notice is what the CLI prints; even a userinfo-bearing URL
    /// from the wire must not reach the screen (#10586).
    /// Test: this test.
    #[test]
    fn kept_record_notice_names_the_record_without_userinfo_10586() {
        let token = "ghp_10586SyntheticNotARealToken";
        let raw = format!("https://octo:{token}@github.com/acme/gizmo");
        for kept in [
            KeptRegistryRecord::redacted("gizmo", &raw),
            KeptRegistryRecord {
                name: "gizmo".into(),
                repo_url: raw.clone(),
            },
        ] {
            let line = kept.notice();
            assert_eq!(
                line,
                "already registered as gizmo (https://github.com/acme/gizmo); nothing written"
            );
            assert!(!line.contains(token) && !line.contains("octo") && !line.contains('@'));
        }
    }

    /// Why: a written record must serialize with NO `kept_existing` key, and
    /// the `ProjectInfo` fields stay top-level (#10586).
    /// Test: this test.
    #[test]
    fn register_response_omits_kept_existing_when_written_10586() {
        let resp = RegisterProjectResponse {
            info: ProjectInfo::new("/work/demo"),
            kept_existing: None,
        };
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["name"], "demo");
        assert!(v.get("kept_existing").is_none(), "{v}");
        let back: RegisterProjectResponse = serde_json::from_value(v).unwrap();
        assert_eq!(back, resp);
    }
}
