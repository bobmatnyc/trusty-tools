//! Lightweight JSON-backed session registry at `.trusty-agents/state/sessions.json`.
//!
//! Why: The redb-backed `memory::SessionRegistry` is precise but opaque to
//! human inspection and to non-Rust tooling. A flat JSON registry sitting next
//! to other state files lets operators (and a future `trusty-agents memories clean
//! --older-than 7d` CLI) eyeball, diff, and prune sessions without spinning up
//! the embedded store. Adding it now keeps session_id tagging end-to-end:
//! every memory carries a session_id; every session_id is recorded here.
//! What: `SessionEntry` records `{id, started_at, workflow, status,
//! ended_at?}`; `SessionsRegistry` loads from / saves to a single JSON file
//! with helpers `record_start`, `record_end`, and `list`.
//! Test: `record_start_appends_entry`, `record_end_updates_status`,
//! `list_returns_all_entries` in the `tests` module below.
//!
//! NOTE: scaffolded ahead of `main.rs` wiring — `#[allow(dead_code)]` on the
//! whole module keeps the build clean while the hookup lands incrementally.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Default file name within the state dir.
const SESSIONS_FILE: &str = "sessions.json";

/// One row in the session registry.
///
/// Why: Captures just enough to drive cleanup ("older than 7d") and selective
/// export ("all sessions for workflow=prescriptive") without bloating the
/// JSON with per-call telemetry. Status is a free-form string ("running",
/// "completed", "failed") so we can grow vocabulary without a schema bump.
/// What: Plain serde struct. Optional `ended_at` is `None` until `record_end`.
/// Test: `record_start_appends_entry`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionEntry {
    pub id: String,
    pub started_at: DateTime<Utc>,
    pub workflow: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub ended_at: Option<DateTime<Utc>>,
}

/// On-disk JSON shape: `{"sessions": [...]}`.
///
/// Why: Wrapping the array in a top-level object leaves room for future
/// metadata (schema_version, last_cleaned_at) without breaking parsing.
/// What: Owned `Vec<SessionEntry>` plus the file path it was loaded from.
/// Test: Round-trip in `record_start_appends_entry`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct SessionsFile {
    #[serde(default)]
    sessions: Vec<SessionEntry>,
}

/// JSON-backed session registry.
///
/// Why: A thin wrapper that owns the file path and offers operations to
/// callers (PM startup / shutdown). Stays sync — file is small and writes
/// happen at session boundaries, not per-message.
/// What: Holds the path to the JSON file; reads on each call so concurrent
/// processes share state via the file.
/// Test: All `tests` below.
pub struct SessionsRegistry {
    path: PathBuf,
}

impl SessionsRegistry {
    /// Open (or create) a registry rooted at `state_dir/sessions.json`.
    ///
    /// Why: Centralize path construction so callers only pass the state dir.
    /// What: Ensures the parent dir exists; does not create the file until a
    /// write happens.
    /// Test: `record_start_appends_entry`.
    pub fn open(state_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(state_dir)
            .with_context(|| format!("creating {}", state_dir.display()))?;
        Ok(Self {
            path: state_dir.join(SESSIONS_FILE),
        })
    }

    /// Path of the backing file (for diagnostics / tests).
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append a session entry with `status = "running"`. Idempotent — if an
    /// entry with the same id already exists, leaves it alone.
    pub fn record_start(&self, id: &str, workflow: &str) -> Result<()> {
        let mut file = self.read()?;
        if file.sessions.iter().any(|s| s.id == id) {
            return Ok(());
        }
        file.sessions.push(SessionEntry {
            id: id.to_string(),
            started_at: Utc::now(),
            workflow: workflow.to_string(),
            status: "running".to_string(),
            ended_at: None,
        });
        self.write(&file)
    }

    /// Mark a session terminal. `status` is typically "completed" or "failed".
    /// No-op if the id isn't found (the registry stays append-only-ish).
    pub fn record_end(&self, id: &str, status: &str) -> Result<()> {
        let mut file = self.read()?;
        let mut changed = false;
        for s in file.sessions.iter_mut() {
            if s.id == id {
                s.status = status.to_string();
                s.ended_at = Some(Utc::now());
                changed = true;
                break;
            }
        }
        if changed { self.write(&file) } else { Ok(()) }
    }

    /// Return all known sessions in insertion order.
    pub fn list(&self) -> Result<Vec<SessionEntry>> {
        Ok(self.read()?.sessions)
    }

    fn read(&self) -> Result<SessionsFile> {
        match std::fs::read(&self.path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes).unwrap_or_default()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(SessionsFile::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", self.path.display())),
        }
    }

    fn write(&self, file: &SessionsFile) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(file)?;
        std::fs::write(&self.path, &bytes)
            .with_context(|| format!("writing {}", self.path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn record_start_appends_entry() {
        let tmp = tempdir().unwrap();
        let reg = SessionsRegistry::open(tmp.path()).unwrap();
        reg.record_start("sess-1", "prescriptive").unwrap();
        let list = reg.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "sess-1");
        assert_eq!(list[0].status, "running");
        assert_eq!(list[0].workflow, "prescriptive");
        assert!(list[0].ended_at.is_none());
        assert!(reg.path().exists());
    }

    #[test]
    fn record_start_is_idempotent() {
        let tmp = tempdir().unwrap();
        let reg = SessionsRegistry::open(tmp.path()).unwrap();
        reg.record_start("sess-1", "wf-a").unwrap();
        reg.record_start("sess-1", "wf-b").unwrap(); // should not duplicate
        let list = reg.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].workflow, "wf-a"); // first wins
    }

    #[test]
    fn record_end_updates_status() {
        let tmp = tempdir().unwrap();
        let reg = SessionsRegistry::open(tmp.path()).unwrap();
        reg.record_start("sess-1", "prescriptive").unwrap();
        reg.record_end("sess-1", "completed").unwrap();
        let list = reg.list().unwrap();
        assert_eq!(list[0].status, "completed");
        assert!(list[0].ended_at.is_some());
    }

    #[test]
    fn record_end_is_noop_for_unknown_id() {
        let tmp = tempdir().unwrap();
        let reg = SessionsRegistry::open(tmp.path()).unwrap();
        reg.record_end("ghost", "completed").unwrap();
        assert!(reg.list().unwrap().is_empty());
    }

    #[test]
    fn list_returns_all_entries_in_order() {
        let tmp = tempdir().unwrap();
        let reg = SessionsRegistry::open(tmp.path()).unwrap();
        reg.record_start("a", "w").unwrap();
        reg.record_start("b", "w").unwrap();
        reg.record_start("c", "w").unwrap();
        let ids: Vec<String> = reg.list().unwrap().into_iter().map(|s| s.id).collect();
        assert_eq!(ids, vec!["a", "b", "c"]);
    }

    #[test]
    fn missing_file_yields_empty_list() {
        let tmp = tempdir().unwrap();
        let reg = SessionsRegistry::open(tmp.path()).unwrap();
        assert!(reg.list().unwrap().is_empty());
    }
}
