//! Subagent process tracking (#130) — prevents orphaned child processes.
//!
//! Why: Long-running workflows spawn many sub-agent subprocesses. If the PM or
//! workflow engine crashes, those children can become orphans that keep files
//! open, hold ports, or continue burning tokens. A tracker file gives us a
//! durable record of who we spawned so we can GC them on the next startup
//! (and SIGTERM/SIGKILL them on a graceful shutdown).
//! What: `ProcessTracker` persists `ProcessEntry` records to
//! `<agent_dir>/processes.json` atomically (tmp + rename). The entry points
//! are `register` (on spawn), `mark_completed` (on exit), `cleanup_stale`
//! (on startup — drops entries whose PIDs no longer exist), and `shutdown_all`
//! (graceful termination of all running children).
//! Test: Unit tests exercise register/load round-trip, mark_completed state
//! transitions, and cleanup_stale removing fake (dead) PIDs.

#![allow(dead_code)]

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::fs;

/// A single tracked subprocess.
///
/// Why: Provides enough context (agent name + task id) to debug orphans
/// after the fact, and `started_at` lets us age out very old entries.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct ProcessEntry {
    pub pid: u32,
    pub agent_name: String,
    pub task_id: String,
    pub started_at: DateTime<Utc>,
    pub status: ProcessStatus,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProcessStatus {
    Running,
    Completed,
    Killed,
}

/// Tracker persisting child PIDs to `<agent_dir>/processes.json`.
pub struct ProcessTracker {
    tracker_path: PathBuf,
}

impl ProcessTracker {
    /// Construct a tracker for `<agent_dir>/processes.json`.
    ///
    /// Why: Takes the trusty-agents state dir rather than the full file path so
    /// callers can treat `.trusty-agents/state/` as a single runtime root.
    /// What: Concatenates `processes.json` and stores the result.
    /// Test: See `test_register_and_load`.
    pub fn new(agent_dir: &std::path::Path) -> Self {
        Self {
            tracker_path: agent_dir.join("processes.json"),
        }
    }

    /// Load all tracked entries from disk.
    ///
    /// Why: Callers read-modify-write; returning an empty map when the file
    /// is absent makes first-run and steady-state paths uniform.
    /// What: Reads and parses the JSON. Missing file → empty map. Malformed
    /// JSON returns an error.
    /// Test: `test_register_and_load` — registers a PID, reloads, asserts present.
    pub async fn load(&self) -> Result<HashMap<u32, ProcessEntry>> {
        if !fs::try_exists(&self.tracker_path).await.unwrap_or(false) {
            return Ok(HashMap::new());
        }
        let bytes = fs::read(&self.tracker_path)
            .await
            .with_context(|| format!("reading {}", self.tracker_path.display()))?;
        if bytes.is_empty() {
            return Ok(HashMap::new());
        }
        let entries: HashMap<u32, ProcessEntry> = serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing {}", self.tracker_path.display()))?;
        Ok(entries)
    }

    /// Atomically persist the entries map.
    ///
    /// Why: A partial write (e.g., kill -9 mid-flush) would leave the tracker
    /// file corrupt, hiding live PIDs from cleanup. tmp + rename gives us
    /// crash-consistency.
    /// What: Writes to `processes.json.tmp` then renames over `processes.json`.
    /// Test: Indirectly via all mutating operations; corruption behavior
    /// covered by `test_load_missing_file`.
    pub async fn save(&self, entries: &HashMap<u32, ProcessEntry>) -> Result<()> {
        if let Some(parent) = self.tracker_path.parent() {
            fs::create_dir_all(parent).await.ok();
        }
        let tmp = self.tracker_path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(entries).context("serialize process tracker")?;
        fs::write(&tmp, &bytes)
            .await
            .with_context(|| format!("writing {}", tmp.display()))?;
        fs::rename(&tmp, &self.tracker_path)
            .await
            .with_context(|| format!("renaming {}", tmp.display()))?;
        Ok(())
    }

    /// Register a newly-spawned sub-agent PID.
    ///
    /// Why: Called by the subprocess runner right after `Command::spawn`
    /// succeeds; the entry must be on disk before we yield control so a
    /// simultaneous crash can't orphan the child.
    /// What: Inserts a `Running` `ProcessEntry` with the current timestamp.
    /// Test: `test_register_and_load`.
    pub async fn register(&self, pid: u32, agent_name: &str, task_id: &str) -> Result<()> {
        let mut entries = self.load().await.unwrap_or_default();
        entries.insert(
            pid,
            ProcessEntry {
                pid,
                agent_name: agent_name.to_string(),
                task_id: task_id.to_string(),
                started_at: Utc::now(),
                status: ProcessStatus::Running,
            },
        );
        self.save(&entries).await
    }

    /// Mark a tracked PID as `Completed`.
    ///
    /// Why: Completed entries are kept briefly for observability (e.g., so
    /// `--check-orphans` can show the difference between running and dead)
    /// but will not be re-killed or reported as orphans.
    /// What: Updates `status` to `Completed` if the PID is present; no-op
    /// otherwise.
    /// Test: `test_mark_completed`.
    pub async fn mark_completed(&self, pid: u32) -> Result<()> {
        let mut entries = self.load().await.unwrap_or_default();
        if let Some(entry) = entries.get_mut(&pid) {
            entry.status = ProcessStatus::Completed;
            self.save(&entries).await?;
        }
        Ok(())
    }

    /// Remove `Running` entries whose PID is no longer alive.
    ///
    /// Why: Called at startup so a prior crashed run doesn't keep reporting
    /// phantom orphans. Entries whose PIDs are still alive are left alone
    /// (they'll be cleaned up by the owning process).
    /// What: Walks the map, checks `kill -0 <pid>`, and drops any `Running`
    /// entry whose PID is dead. Returns the number removed.
    /// Test: `test_cleanup_stale_removes_dead_pids`.
    pub async fn cleanup_stale(&self) -> Result<usize> {
        let mut entries = self.load().await.unwrap_or_default();
        let before = entries.len();
        entries.retain(|pid, entry| {
            if entry.status != ProcessStatus::Running {
                return true;
            }
            is_pid_alive(*pid)
        });
        let removed = before - entries.len();
        if removed > 0 {
            self.save(&entries).await?;
        }
        Ok(removed)
    }

    /// SIGTERM every `Running` entry; SIGKILL any that don't exit within 5s.
    ///
    /// Why: Graceful shutdown on Ctrl-C or workflow completion. Gives children
    /// a chance to flush logs / close files before being force-killed.
    /// What: Sends SIGTERM via `kill -TERM <pid>`, waits up to 5 seconds for
    /// the process to exit, then SIGKILL (`kill -KILL <pid>`) the stragglers.
    /// Updates each entry's status to `Killed`.
    /// Test: Not unit-tested — requires live children; covered by manual
    /// Ctrl-C during workflow runs.
    pub async fn shutdown_all(&self) -> Result<()> {
        let mut entries = self.load().await.unwrap_or_default();
        let running: Vec<u32> = entries
            .iter()
            .filter(|(_, e)| e.status == ProcessStatus::Running)
            .map(|(pid, _)| *pid)
            .collect();

        for pid in &running {
            let _ = send_signal(*pid, "TERM");
        }

        // Poll up to 5s for each process to exit.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let any_alive = running.iter().any(|pid| is_pid_alive(*pid));
            if !any_alive || std::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        for pid in &running {
            if is_pid_alive(*pid) {
                let _ = send_signal(*pid, "KILL");
            }
            if let Some(entry) = entries.get_mut(pid) {
                entry.status = ProcessStatus::Killed;
            }
        }
        self.save(&entries).await
    }

    /// Path to the backing `processes.json` (for logs / debugging).
    pub fn path(&self) -> &std::path::Path {
        &self.tracker_path
    }
}

/// Return `true` if `kill -0 <pid>` succeeds (process exists and we can signal it).
///
/// Why: Checking liveness via subprocess avoids pulling in `nix`/`libc` for a
/// single syscall. `kill -0` is POSIX and present on both macOS and Linux.
/// What: Runs `kill -0 <pid>`; exit code 0 = alive, non-zero = dead/unreachable.
/// Test: `test_cleanup_stale_removes_dead_pids` exercises the false path with
/// a very high PID that's guaranteed not to exist.
fn is_pid_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Send a POSIX signal by name (e.g., `"TERM"`, `"KILL"`) to a PID.
fn send_signal(pid: u32, signal: &str) -> std::io::Result<()> {
    std::process::Command::new("kill")
        .args([&format!("-{signal}"), &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .status()
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_tracker(tmp: &TempDir) -> ProcessTracker {
        ProcessTracker::new(tmp.path())
    }

    #[tokio::test]
    async fn test_load_missing_file() {
        let tmp = TempDir::new().unwrap();
        let tracker = make_tracker(&tmp);
        let entries = tracker.load().await.unwrap();
        assert!(entries.is_empty());
    }

    #[tokio::test]
    async fn test_register_and_load() {
        let tmp = TempDir::new().unwrap();
        let tracker = make_tracker(&tmp);
        tracker
            .register(12345, "python-engineer", "task-1")
            .await
            .unwrap();

        let entries = tracker.load().await.unwrap();
        assert_eq!(entries.len(), 1);
        let entry = entries.get(&12345).unwrap();
        assert_eq!(entry.agent_name, "python-engineer");
        assert_eq!(entry.task_id, "task-1");
        assert_eq!(entry.status, ProcessStatus::Running);
    }

    #[tokio::test]
    async fn test_mark_completed() {
        let tmp = TempDir::new().unwrap();
        let tracker = make_tracker(&tmp);
        tracker.register(54321, "qa-agent", "task-9").await.unwrap();
        tracker.mark_completed(54321).await.unwrap();

        let entries = tracker.load().await.unwrap();
        assert_eq!(
            entries.get(&54321).unwrap().status,
            ProcessStatus::Completed
        );
    }

    #[tokio::test]
    async fn test_mark_completed_missing_pid_is_noop() {
        let tmp = TempDir::new().unwrap();
        let tracker = make_tracker(&tmp);
        // No registration — mark_completed must not error.
        tracker.mark_completed(99999).await.unwrap();
        assert!(tracker.load().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_cleanup_stale_removes_dead_pids() {
        let tmp = TempDir::new().unwrap();
        let tracker = make_tracker(&tmp);
        // 9999999 is almost certainly not a live process.
        tracker
            .register(9_999_999, "research-agent", "ghost")
            .await
            .unwrap();

        let removed = tracker.cleanup_stale().await.unwrap();
        assert_eq!(removed, 1);
        assert!(tracker.load().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_cleanup_stale_keeps_live_pid() {
        let tmp = TempDir::new().unwrap();
        let tracker = make_tracker(&tmp);
        // Our own process is definitely alive.
        let self_pid = std::process::id();
        tracker.register(self_pid, "self", "t").await.unwrap();
        let removed = tracker.cleanup_stale().await.unwrap();
        assert_eq!(removed, 0);
        assert_eq!(tracker.load().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_cleanup_stale_ignores_non_running() {
        let tmp = TempDir::new().unwrap();
        let tracker = make_tracker(&tmp);
        // Register + mark completed → cleanup should NOT drop it even though
        // the PID isn't alive.
        tracker.register(9_999_998, "x", "t").await.unwrap();
        tracker.mark_completed(9_999_998).await.unwrap();
        let removed = tracker.cleanup_stale().await.unwrap();
        assert_eq!(removed, 0);
        assert_eq!(tracker.load().await.unwrap().len(), 1);
    }
}
