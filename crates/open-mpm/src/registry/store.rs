//! `ProjectRegistry` — the on-disk store backed by `~/.open-mpm/projects.json`.
//!
//! Why: Isolating the file-backed CRUD/lifecycle methods from the data types
//! and pure helpers keeps both files focused and under the 500-line cap.
//! What: `ProjectRegistry` with load/save plus the register / pm-start /
//! remove / deregister / touch / list / summary operations.
//! Test: Covered by `registry::tests` (round-trips with a tempdir `$HOME`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::Utc;
use tokio::fs;

use super::{ProjectEntry, ProjectStatus, derive_project_name, extract_github_repo};

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
