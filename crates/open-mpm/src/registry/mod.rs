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

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::fs;

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

/// Global project registry backed by `~/.open-mpm/projects.json`.
///
/// Why: A single file makes it trivial to inspect or back up across machines.
/// Atomic rename on save prevents corruption if the process is killed mid-write.
pub struct ProjectRegistry {
    /// `~/.open-mpm/projects.json`
    registry_path: PathBuf,
}

impl ProjectRegistry {
    /// Create a registry handle pointing at `~/.open-mpm/projects.json`.
    ///
    /// Why: Centralizes home-dir resolution so callers don't scatter it.
    /// What: Returns an error when `$HOME` is unset.
    /// Test: Call with `HOME` set to a tempdir and assert no error.
    pub fn new() -> Result<Self> {
        let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
        Ok(Self {
            registry_path: home.join(".open-mpm").join("projects.json"),
        })
    }

    /// Load all registry entries from disk.
    ///
    /// Why: Callers need the full map to update and re-save it without losing
    /// entries written by other processes or prior sessions.
    /// What: Returns an empty map on first run (file absent) and propagates
    /// other IO or parse errors.
    /// Test: Assert empty map when file does not exist.
    pub async fn load(&self) -> Result<HashMap<String, ProjectEntry>> {
        match fs::read_to_string(&self.registry_path).await {
            Ok(text) => {
                let map: HashMap<String, ProjectEntry> = serde_json::from_str(&text)?;
                Ok(map)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(HashMap::new()),
            Err(e) => Err(e.into()),
        }
    }

    /// Atomically save all registry entries to disk.
    ///
    /// Why: `rename` is atomic on POSIX; crashes between write and rename
    /// leave the old file intact.
    /// What: Writes to `<registry_path>.tmp` then renames over the target.
    /// Test: Save a map, reload, assert round-trip equality.
    pub async fn save(&self, entries: &HashMap<String, ProjectEntry>) -> Result<()> {
        if let Some(parent) = self.registry_path.parent() {
            fs::create_dir_all(parent).await?;
        }
        let tmp = self.registry_path.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(entries)?;
        fs::write(&tmp, &text).await?;
        fs::rename(&tmp, &self.registry_path).await?;
        Ok(())
    }

    /// Register or refresh the current project in the global registry.
    ///
    /// Why: Every startup should record that this project is Active so the
    /// registry stays accurate without manual maintenance.
    /// What: Reads the existing map, upserts an entry for `project_dir`
    /// (preserving `last_run`), sets status to Active, derives the name from
    /// the first `# ` heading in `CLAUDE.md` or falls back to the dir basename,
    /// and saves.
    /// Test: Call twice on the same dir; assert the entry is present once and
    /// name matches the CLAUDE.md heading.
    pub async fn register(&self, project_dir: &Path) -> Result<()> {
        let mut entries = self.load().await.unwrap_or_default();
        let key = project_dir.to_string_lossy().to_string();

        let name = derive_project_name(project_dir).await;
        let existing = entries.get(&key);
        let last_run = existing.and_then(|e| e.last_run);

        let last_connected = existing.and_then(|e| e.last_connected);
        let pm_count = existing.map(|e| e.pm_count).unwrap_or(0);
        let is_self = existing.map(|e| e.is_self).unwrap_or(false);
        let git_origin = existing.and_then(|e| e.git_origin.clone());
        let open_issues_count = existing.and_then(|e| e.open_issues_count);
        let open_prs_count = existing.and_then(|e| e.open_prs_count);
        entries.insert(
            key,
            ProjectEntry {
                path: project_dir.to_path_buf(),
                name,
                last_run,
                status: ProjectStatus::Active,
                last_connected,
                pm_count,
                is_self,
                git_origin,
                open_issues_count,
                open_prs_count,
            },
        );
        self.save(&entries).await?;
        tracing::debug!(path = %project_dir.display(), "project registered in global registry");
        Ok(())
    }

    /// Register the open-mpm self-project, flagging `is_self = true`. (#182)
    ///
    /// Why: CTRL detects its own source tree at startup so the user can
    /// dispatch development tasks against open-mpm itself. We persist the
    /// flag in the same `projects.json` so it survives restarts and is
    /// visible to other tools.
    /// What: Upserts the entry like `register`, but explicitly sets
    /// `is_self = true`.
    /// Test: `register_self_project_sets_is_self_flag`.
    pub async fn register_self_project(&self, project_dir: &Path) -> Result<()> {
        let mut entries = self.load().await.unwrap_or_default();
        let key = project_dir.to_string_lossy().to_string();
        let name = derive_project_name(project_dir).await;
        let existing = entries.get(&key).cloned();
        let last_run = existing.as_ref().and_then(|e| e.last_run);
        let last_connected = existing.as_ref().and_then(|e| e.last_connected);
        let pm_count = existing.as_ref().map(|e| e.pm_count).unwrap_or(0);
        let git_origin = existing.as_ref().and_then(|e| e.git_origin.clone());
        let open_issues_count = existing.as_ref().and_then(|e| e.open_issues_count);
        let open_prs_count = existing.as_ref().and_then(|e| e.open_prs_count);
        entries.insert(
            key,
            ProjectEntry {
                path: project_dir.to_path_buf(),
                name: existing.map(|e| e.name).unwrap_or(name),
                last_run,
                status: ProjectStatus::Active,
                last_connected,
                pm_count,
                is_self: true,
                git_origin,
                open_issues_count,
                open_prs_count,
            },
        );
        self.save(&entries).await?;
        Ok(())
    }

    /// Record that CTRL just spawned a PM for `project_dir`.
    ///
    /// Why: `register` is about "project exists"; `register_pm_start` is
    /// about CTRL-level lifecycle (when did we last connect? how many
    /// sessions so far?). Separating them keeps `last_run` semantics intact
    /// for workflow tooling.
    /// What: Upserts the entry (creating one if absent), sets
    /// `last_connected = now`, and increments `pm_count`.
    /// Test: `register_pm_start_increments_count`.
    pub async fn register_pm_start(&self, project_dir: &Path) -> Result<()> {
        let mut entries = self.load().await.unwrap_or_default();
        let key = project_dir.to_string_lossy().to_string();
        let name = derive_project_name(project_dir).await;
        let existing = entries.get(&key).cloned();
        let last_run = existing.as_ref().and_then(|e| e.last_run);
        let prev_count = existing.as_ref().map(|e| e.pm_count).unwrap_or(0);
        let prev_is_self = existing.as_ref().map(|e| e.is_self).unwrap_or(false);
        let mut git_origin = existing.as_ref().and_then(|e| e.git_origin.clone());
        let mut open_issues_count = existing.as_ref().and_then(|e| e.open_issues_count);
        let mut open_prs_count = existing.as_ref().and_then(|e| e.open_prs_count);

        // #340: Populate git_origin if we don't already have it. Subprocess
        // failures (no git on PATH, not a repo) are non-fatal — log at debug
        // and move on so registration never blocks on an unrelated tool.
        if git_origin.is_none() {
            match tokio::process::Command::new("git")
                .args([
                    "-C",
                    project_dir.to_str().unwrap_or("."),
                    "remote",
                    "get-url",
                    "origin",
                ])
                .output()
                .await
            {
                Ok(output) if output.status.success() => {
                    let origin = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    if !origin.is_empty() {
                        git_origin = Some(origin);
                    }
                }
                Ok(_) => {
                    tracing::debug!(path = %project_dir.display(), "git remote get-url origin: non-zero exit");
                }
                Err(e) => {
                    tracing::debug!(error = %e, "git remote get-url origin: spawn failed");
                }
            }
        }

        // #340: Refresh issue/PR counts via gh CLI when we have a github
        // origin. Errors are swallowed (debug log only) — `gh` may not be
        // installed or authed, and that mustn't break PM startup.
        if let Some(ref origin) = git_origin
            && let Some(repo) = extract_github_repo(origin)
        {
            use crate::ticketing::TicketingClient as _;
            let gh = crate::ticketing::gh_cli::GhCliClient::new(Some(repo.clone()));
            match gh.count_open_issues(&repo).await {
                Ok(n) => open_issues_count = Some(n),
                Err(e) => tracing::debug!(error = %e, repo = %repo, "gh count_open_issues failed"),
            }
            match gh.count_open_prs(&repo).await {
                Ok(n) => open_prs_count = Some(n),
                Err(e) => tracing::debug!(error = %e, repo = %repo, "gh count_open_prs failed"),
            }
        }

        entries.insert(
            key,
            ProjectEntry {
                path: project_dir.to_path_buf(),
                name: existing.map(|e| e.name).unwrap_or(name),
                last_run,
                status: ProjectStatus::Active,
                last_connected: Some(Utc::now()),
                pm_count: prev_count + 1,
                is_self: prev_is_self,
                git_origin,
                open_issues_count,
                open_prs_count,
            },
        );
        self.save(&entries).await?;
        Ok(())
    }

    /// Remove a project entry by absolute path.
    ///
    /// Why: The CTRL `remove_project` tool needs a way to drop a project from
    /// `~/.open-mpm/projects.json` without waiting for `deregister_missing`
    /// (which only acts on already-deleted directories). This is a hard
    /// remove — caller is responsible for stopping any running PM separately.
    /// What: Loads the map, removes the canonical-string key matching `path`,
    /// saves. Returns true when an entry was removed.
    /// Test: `remove_drops_existing_entry` (in registry tests).
    pub async fn remove(&self, path: &Path) -> Result<bool> {
        let mut entries = self.load().await.unwrap_or_default();
        let key = path.to_string_lossy().to_string();
        let removed = entries.remove(&key).is_some();
        if removed {
            self.save(&entries).await?;
        }
        Ok(removed)
    }

    /// Mark projects whose directories no longer exist as Removed.
    ///
    /// Why: Projects get deleted or moved; keeping stale Active entries
    /// causes confusing status output and bus routing errors.
    /// What: Iterates all entries, sets status to Removed for those where
    /// `entry.path` does not exist on disk, saves the updated map, and
    /// returns the list of removed paths.
    /// Test: Register a path pointing to a non-existent dir, call this,
    /// assert the returned vec contains that path.
    pub async fn deregister_missing(&self) -> Result<Vec<PathBuf>> {
        let mut entries = self.load().await.unwrap_or_default();
        let mut removed = Vec::new();
        for entry in entries.values_mut() {
            if !entry.path.exists() && entry.status != ProjectStatus::Removed {
                entry.status = ProjectStatus::Removed;
                removed.push(entry.path.clone());
            }
        }
        if !removed.is_empty() {
            self.save(&entries).await?;
            for p in &removed {
                tracing::debug!(path = %p.display(), "project directory gone; marked Removed");
            }
        }
        Ok(removed)
    }

    /// Update the `last_run` timestamp for `project_dir` to now.
    ///
    /// Why: Status summaries and idle-detection depend on knowing when the
    /// project was last active, which `register` alone doesn't update on
    /// repeated startups.
    /// What: Loads, mutates the matching entry's `last_run`, saves.
    /// Test: Call `touch`, reload, assert `last_run` is within 1s of now.
    pub async fn touch(&self, project_dir: &Path) -> Result<()> {
        let mut entries = self.load().await.unwrap_or_default();
        let key = project_dir.to_string_lossy().to_string();
        if let Some(entry) = entries.get_mut(&key) {
            entry.last_run = Some(Utc::now());
            self.save(&entries).await?;
        }
        Ok(())
    }

    /// Return all entries with status Active.
    ///
    /// Why: Most consumers (status display, bus listing) care only about live projects.
    /// What: Filters the full map and returns a Vec sorted by last_run descending.
    /// Test: Register two paths, mark one Removed, assert list returns only one.
    pub async fn list_active(&self) -> Result<Vec<ProjectEntry>> {
        let entries = self.load().await.unwrap_or_default();
        let mut active: Vec<ProjectEntry> = entries
            .into_values()
            .filter(|e| e.status == ProjectStatus::Active)
            .collect();
        active.sort_by_key(|b| std::cmp::Reverse(b.last_run));
        Ok(active)
    }

    /// Render a human-readable status summary for the `/status` command.
    ///
    /// Why: A single formatted string is easier for callers to log or display
    /// than iterating the Vec themselves.
    /// What: Returns a Markdown table of active projects with name, path, and
    /// last_run. Returns a short "no projects" message when the list is empty.
    /// Test: Register one project, call `status_summary`, assert the project
    /// name appears in the output.
    pub async fn status_summary(&self) -> Result<String> {
        let active = self.list_active().await?;
        if active.is_empty() {
            return Ok("No active projects registered.".to_string());
        }
        let mut out = String::from("## Registered Projects\n\n");
        out.push_str("| Name | Path | Last Run |\n");
        out.push_str("|------|------|----------|\n");
        for entry in &active {
            let last = entry
                .last_run
                .map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string())
                .unwrap_or_else(|| "never".to_string());
            out.push_str(&format!(
                "| {} | `{}` | {} |\n",
                entry.name,
                entry.path.display(),
                last
            ));
        }
        Ok(out)
    }
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
async fn derive_project_name(project_dir: &Path) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn entry_with_times(
        path: &str,
        last_run: Option<DateTime<Utc>>,
        last_connected: Option<DateTime<Utc>>,
    ) -> ProjectEntry {
        ProjectEntry {
            path: PathBuf::from(path),
            name: path.into(),
            last_run,
            status: ProjectStatus::Active,
            last_connected,
            pm_count: 0,
            is_self: false,
            git_origin: None,
            open_issues_count: None,
            open_prs_count: None,
        }
    }

    #[test]
    fn last_active_picks_max() {
        let early = Utc::now() - chrono::Duration::days(5);
        let late = Utc::now() - chrono::Duration::days(1);

        // Both set: returns the later one.
        let e = entry_with_times("/a", Some(early), Some(late));
        assert_eq!(e.last_active(), Some(late));

        // Only last_run.
        let e = entry_with_times("/a", Some(early), None);
        assert_eq!(e.last_active(), Some(early));

        // Only last_connected.
        let e = entry_with_times("/a", None, Some(late));
        assert_eq!(e.last_active(), Some(late));

        // Neither.
        let e = entry_with_times("/a", None, None);
        assert_eq!(e.last_active(), None);
    }

    #[test]
    fn extract_github_repo_https_form() {
        assert_eq!(
            extract_github_repo("https://github.com/bobmatnyc/open-mpm.git"),
            Some("bobmatnyc/open-mpm".into())
        );
        assert_eq!(
            extract_github_repo("https://github.com/bobmatnyc/open-mpm"),
            Some("bobmatnyc/open-mpm".into())
        );
    }

    #[test]
    fn extract_github_repo_ssh_form() {
        assert_eq!(
            extract_github_repo("git@github.com:bobmatnyc/open-mpm.git"),
            Some("bobmatnyc/open-mpm".into())
        );
        assert_eq!(
            extract_github_repo("git@github.com:duettoresearch/duetto"),
            Some("duettoresearch/duetto".into())
        );
    }

    #[test]
    fn extract_github_repo_returns_none_for_non_github() {
        assert!(extract_github_repo("https://gitlab.com/o/r.git").is_none());
        assert!(extract_github_repo("git@bitbucket.org:o/r.git").is_none());
        assert!(extract_github_repo("").is_none());
        // github.com prefix but no repo path is invalid.
        assert!(extract_github_repo("https://github.com/").is_none());
    }

    #[test]
    fn discover_active_projects_returns_recent_and_session_owned() {
        let now = Utc::now();
        let recent = entry_with_times("/recent", Some(now - chrono::Duration::days(2)), None);
        let stale = entry_with_times("/stale", Some(now - chrono::Duration::days(60)), None);
        let session_owned = entry_with_times(
            "/session-owned",
            Some(now - chrono::Duration::days(60)),
            None,
        );

        let entries = vec![recent.clone(), stale.clone(), session_owned.clone()];
        let session_paths = vec![PathBuf::from("/session-owned")];
        let window = chrono::Duration::days(14);

        let active = discover_active_projects(&entries, &session_paths, window);
        let paths: Vec<&PathBuf> = active.iter().map(|e| &e.path).collect();

        // recent is included (within 14 days).
        assert!(paths.iter().any(|p| p.to_string_lossy() == "/recent"));
        // session_owned is included (has a TM session despite stale activity).
        assert!(
            paths
                .iter()
                .any(|p| p.to_string_lossy() == "/session-owned")
        );
        // stale is excluded.
        assert!(!paths.iter().any(|p| p.to_string_lossy() == "/stale"));
    }

    fn make_entry(path: &str) -> ProjectEntry {
        ProjectEntry {
            path: PathBuf::from(path),
            name: path.into(),
            last_run: None,
            status: ProjectStatus::Active,
            last_connected: None,
            pm_count: 0,
            is_self: false,
            git_origin: None,
            open_issues_count: None,
            open_prs_count: None,
        }
    }

    #[test]
    fn is_real_project_rejects_temp_dirs() {
        // macOS temp dir under /var/folders
        let e = make_entry("/private/var/folders/l1/abc123/T/.tmptcuMXm");
        assert!(
            !e.is_real_project(),
            "macOS /var/folders temp should be excluded"
        );

        // basename starting with .tmp
        let e = make_entry("/private/var/folders/l1/abc123/T/.tmpXe19Vm");
        assert!(
            !e.is_real_project(),
            ".tmp-prefixed basename should be excluded"
        );

        // /tmp prefix
        let e = make_entry("/tmp/myworkdir");
        assert!(!e.is_real_project(), "/tmp/ prefix should be excluded");

        // /private/tmp prefix
        let e = make_entry("/private/tmp/workdir");
        assert!(
            !e.is_real_project(),
            "/private/tmp/ prefix should be excluded"
        );

        // /private/var prefix (covers broader macOS system paths)
        let e = make_entry("/private/var/something/project");
        assert!(
            !e.is_real_project(),
            "/private/var/ prefix should be excluded"
        );
    }

    #[test]
    fn is_real_project_accepts_normal_dirs() {
        // Normal home-directory project
        let e = make_entry("/Users/masa/Projects/open-mpm");
        assert!(
            e.is_real_project(),
            "normal home project should be accepted"
        );

        // /var/www style server path (not macOS /var/folders)
        let e = make_entry("/var/www/myapp");
        assert!(
            e.is_real_project(),
            "/var/www should be accepted (not /var/folders)"
        );

        // Project whose name happens to contain "tmp" but not as a prefix
        let e = make_entry("/Users/masa/projects/dumptruck");
        assert!(
            e.is_real_project(),
            "name containing tmp (not prefix) should be accepted"
        );
    }

    #[test]
    fn discover_active_projects_excludes_temp_dirs() {
        let now = Utc::now();
        // A temp-dir entry that is recent enough it would normally pass the window.
        let temp_entry = ProjectEntry {
            path: PathBuf::from("/private/var/folders/l1/abc/T/.tmptcuMXm"),
            name: ".tmptcuMXm".into(),
            last_run: Some(now - chrono::Duration::days(1)),
            status: ProjectStatus::Active,
            last_connected: None,
            pm_count: 0,
            is_self: false,
            git_origin: None,
            open_issues_count: None,
            open_prs_count: None,
        };
        let real_entry = entry_with_times(
            "/Users/masa/Projects/myapp",
            Some(now - chrono::Duration::days(1)),
            None,
        );
        let entries = vec![temp_entry, real_entry];
        let active = discover_active_projects(&entries, &[], chrono::Duration::days(14));
        let paths: Vec<&PathBuf> = active.iter().map(|e| &e.path).collect();
        assert!(
            !paths.iter().any(|p| p.to_string_lossy().contains(".tmp")),
            "temp dir should be filtered out of discover_active_projects"
        );
        assert!(
            paths.iter().any(|p| p.to_string_lossy().contains("myapp")),
            "real project should remain in discover_active_projects"
        );
    }

    #[test]
    fn project_entry_old_json_deserializes_without_new_fields() {
        // Why: existing users have projects.json without the new fields;
        // serde defaults must keep them loadable.
        let json = r#"{
            "path": "/p",
            "name": "p",
            "last_run": null,
            "status": "active"
        }"#;
        let e: ProjectEntry = serde_json::from_str(json).expect("deserialize");
        assert_eq!(e.git_origin, None);
        assert_eq!(e.open_issues_count, None);
        assert_eq!(e.open_prs_count, None);
        assert_eq!(e.pm_count, 0);
        assert!(!e.is_self);
    }
}
