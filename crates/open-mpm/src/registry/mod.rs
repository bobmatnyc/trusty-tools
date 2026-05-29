//! Global project registry stored at `~/.open-mpm/projects.json`.
//!
//! Why: When multiple projects use open-mpm, tracking them centrally enables
//! cross-project features (bus routing, status summaries, stale-project cleanup)
//! without requiring each project to declare itself. The registry is the single
//! source of truth for "what projects does this user have?".
//! What: `ProjectRegistry` reads/writes a JSON map of `ProjectEntry` records
//! keyed by the canonical project path string. Atomic writes (tmp + rename)
//! keep the file consistent on crashes. `register` upserts on startup;
//! `deregister_missing` marks entries where the directory no longer exists.
//! Test: Construct with a tempdir as home, call `register` with a real path,
//! reload, assert the entry is present with status Active.
//!
//! Module layout (see #366 split):
//! - `mod.rs` — `ProjectStatus`/`ProjectEntry` types + pure helpers
//! - `store.rs` — the `ProjectRegistry` file-backed store
//! - `tests.rs` — unit tests for the types + pure helpers

mod store;

#[cfg(test)]
mod tests;

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::fs;

pub use store::ProjectRegistry;

/// Lifecycle status of a tracked project.
///
/// Why: Distinguishes projects that are actively used from those whose
/// directories have been deleted, so the registry can be garbage-collected
/// without permanently forgetting projects that were just temporarily offline.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ProjectStatus {
    Active,
    Idle,
    Removed,
}

/// A single project entry in the global registry.
///
/// Why: Captures the minimum metadata needed for status display and
/// cross-project routing (path, human name, last activity, lifecycle state).
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ProjectEntry {
    /// Absolute path to the project root directory.
    pub path: PathBuf,
    /// Human-readable project name (from CLAUDE.md first heading or dir name).
    pub name: String,
    /// When open-mpm last ran in this project.
    pub last_run: Option<DateTime<Utc>>,
    /// Lifecycle status.
    pub status: ProjectStatus,
    /// When CTRL last connected a PM session to this project.
    ///
    /// Why: Distinct from `last_run` (workflow execution): a user may
    /// connect from CTRL many times without launching a workflow, and
    /// search/UX wants the most-recently-touched project ordering.
    /// What: Set by `register_pm_start` each time CTRL spawns a PM.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_connected: Option<DateTime<Utc>>,
    /// Number of times CTRL has spawned a PM for this project.
    ///
    /// Why: A lightweight activity signal for `list_projects` ranking.
    /// What: Incremented on every `register_pm_start` call.
    #[serde(default)]
    pub pm_count: u64,
    /// True when this entry represents open-mpm's own source tree. (#182)
    ///
    /// Why: CTRL has a self-awareness mode that lets the user dispatch
    /// development tasks against open-mpm itself; we mark the self-project
    /// so UIs and tools can distinguish it without re-detecting.
    /// What: Set by `register_self_project`; defaults to false on existing
    /// entries to keep the registry file backwards compatible.
    /// Test: `register_self_project_sets_is_self_flag`.
    #[serde(default)]
    pub is_self: bool,
    /// Git origin URL (HTTPS or SSH form), e.g. `git@github.com:o/r.git`.
    ///
    /// Why: Lets project-discovery UIs render the upstream repo name and
    /// resolve issue/PR counts via `extract_github_repo`.
    /// What: Populated lazily during `register_pm_start` by shelling out to
    /// `git -C <path> remote get-url origin`. None when the project is not
    /// a git repo or `git` is unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_origin: Option<String>,
    /// Cached count of open GitHub issues for this project (#340).
    ///
    /// Why: Avoids re-querying `gh` on every `/projects` render. Refreshed
    /// each `register_pm_start`.
    /// What: None when not a GitHub project or when `gh` is unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_issues_count: Option<u32>,
    /// Cached count of open GitHub pull requests for this project (#340).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_prs_count: Option<u32>,
}

impl ProjectEntry {
    /// Most-recent activity timestamp across `last_run` and `last_connected`.
    ///
    /// Why: `last_run` tracks workflow execution and `last_connected` tracks
    /// CTRL PM spawns; either one counts as "the user touched this project".
    /// `discover_active_projects` and the enhanced `/projects` view need a
    /// single timestamp to rank against.
    /// What: Returns the larger of the two when both are present, the
    /// non-None value when one is set, or None when neither has fired.
    /// Test: `last_active_picks_max` covers all four arms.
    pub fn last_active(&self) -> Option<DateTime<Utc>> {
        match (self.last_run, self.last_connected) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        }
    }

    /// Returns true if this entry looks like a real project directory,
    /// not a macOS/Linux temp dir or system path.
    ///
    /// Why: macOS and Linux temp directories (e.g. those under
    /// `/var/folders/`, `/tmp/`, or paths whose basename starts with `.tmp`)
    /// occasionally get registered when tools like `mktemp` create a
    /// temporary working directory and open-mpm is invoked there. These
    /// entries pollute the `/projects` list and confuse navigation.
    /// What: Checks both the path prefix (known temp-dir roots) and the
    /// final path component (`.tmp`-prefixed names). Returns false for any
    /// match so callers can filter with `.filter(|e| e.is_real_project())`.
    /// Test: `is_real_project_rejects_temp_dirs` and
    /// `is_real_project_accepts_normal_dirs`.
    pub fn is_real_project(&self) -> bool {
        let path = &self.path;
        let path_str = path.to_string_lossy();
        // Exclude macOS/Linux temp directories.
        if path_str.contains("/var/folders/")
            || path_str.starts_with("/tmp/")
            || path_str.starts_with("/private/tmp/")
            || path_str.starts_with("/private/var/")
        {
            return false;
        }
        // Exclude entries whose final component starts with `.tmp`.
        if let Some(name) = path.file_name()
            && name.to_string_lossy().starts_with(".tmp")
        {
            return false;
        }
        true
    }
}

/// Parse a git origin URL into `"owner/repo"` form for github.com URLs.
///
/// Why: Both HTTPS (`https://github.com/o/r.git`) and SSH
/// (`git@github.com:o/r.git`) forms appear in the wild. The ticketing layer
/// wants the canonical `owner/repo` slug.
/// What: Strips known github.com prefixes and a trailing `.git`. Returns
/// None for non-github origins.
/// Test: `extract_github_repo_*` tests cover https, ssh, no-suffix, and
/// non-github forms.
pub fn extract_github_repo(origin: &str) -> Option<String> {
    let trimmed = origin.trim();
    let body = if let Some(rest) = trimmed.strip_prefix("https://github.com/") {
        rest
    } else if let Some(rest) = trimmed.strip_prefix("git@github.com:") {
        rest
    } else {
        trimmed.strip_prefix("ssh://git@github.com/")?
    };
    let body = body.strip_suffix(".git").unwrap_or(body);
    let body = body.trim_end_matches('/');
    if body.is_empty() || !body.contains('/') {
        return None;
    }
    Some(body.to_string())
}

/// Discover "active" projects across the global registry and the TM session
/// registry (#340).
///
/// Why: The enhanced `/projects` command wants to surface anything the user
/// has touched recently OR has live tmux sessions for, without requiring
/// the user to remember which projects they registered. A project is
/// considered active if either signal fires.
/// What: Returns all registry entries whose `last_active()` is within
/// `window` (e.g. 14 days) OR whose `path` matches the `project_path` of
/// any session in the TM registry. Sorted by `last_active()` descending.
/// Test: `discover_active_projects_returns_recent_and_session_owned`.
pub fn discover_active_projects<'a>(
    entries: &'a [ProjectEntry],
    tm_session_paths: &[PathBuf],
    window: chrono::Duration,
) -> Vec<&'a ProjectEntry> {
    let cutoff = Utc::now() - window;
    let mut out: Vec<&ProjectEntry> = entries
        .iter()
        .filter(|e| e.is_real_project())
        .filter(|e| {
            let recent = e.last_active().map(|t| t >= cutoff).unwrap_or(false);
            let has_session = tm_session_paths.iter().any(|p| p == &e.path);
            recent || has_session
        })
        .collect();
    out.sort_by_key(|b| std::cmp::Reverse(b.last_active()));
    out
}

/// Derive a human-readable project name for `project_dir`.
///
/// Why: The `# ` first-heading convention from CLAUDE.md is the most reliable
/// signal; falling back to the directory basename keeps things working when
/// CLAUDE.md is absent or has no heading.
/// What: Reads `project_dir/CLAUDE.md`, scans for the first line starting with
/// `# `, strips the prefix. Falls back to `dir_name` on any error.
/// Test: Write a CLAUDE.md with `# my-project\n`, assert returns "my-project".
pub(super) async fn derive_project_name(project_dir: &Path) -> String {
    let claude_md = project_dir.join("CLAUDE.md");
    if let Ok(text) = fs::read_to_string(&claude_md).await {
        for line in text.lines() {
            if let Some(title) = line.strip_prefix("# ") {
                let name = title.trim().to_string();
                if !name.is_empty() {
                    return name;
                }
            }
        }
    }
    project_dir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string()
}
