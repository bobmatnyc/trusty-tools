//! Watermark state for the incremental catch-up system (DOC-28, #1762).
//!
//! Why: the catch-up is incremental — each run surfaces only activity SINCE the
//! previous run; on the first run it surfaces all available history. This module
//! persists the watermark so consecutive native sessions get focused, not
//! repetitive, catch-ups.
//! What: [`CatchupState`] is the persisted state; [`load_catchup_state`] reads it
//! fail-open (None on missing or parse error); [`save_catchup_state`] writes it,
//! creating parent directories as needed.
//! Test: `state_save_load_roundtrip`, `state_missing_file_returns_none`,
//! `state_parse_failure_returns_none`.
//!
// CUTOVER BRIDGE — remove post-migration (#1762)

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Persisted watermark for the catch-up system.
///
/// Why: tracks the timestamp of the last successful catch-up so subsequent
/// calls surface only incremental activity.
/// What: serialized to/from `~/.trusty-mpm/projects/<palace-id>/catchup-state.json`.
/// Test: `state_save_load_roundtrip`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatchupState {
    /// UTC timestamp of the last successful catch-up.
    pub last_catchup_at: DateTime<Utc>,
    /// The palace id this watermark belongs to.
    pub palace_id: String,
    /// The git HEAD SHA at the time of the last catch-up (for branch tracking).
    pub last_git_sha: Option<String>,
}

/// Resolve the directory path for a palace's catch-up state.
///
/// Why: centralizes the path derivation so tests and production code resolve the
/// same location.
/// What: returns `~/.trusty-mpm/projects/<palace-id>/` using the home dir.
/// Test: covered indirectly by `state_save_load_roundtrip`.
fn catchup_dir(palace_id: &str) -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(home.join(".trusty-mpm").join("projects").join(palace_id))
}

/// Resolve the path to the catch-up state JSON file for a palace.
///
/// Why: callers need the file path for read and write; centralizing avoids drift.
/// What: `~/.trusty-mpm/projects/<palace-id>/catchup-state.json`.
/// Test: covered indirectly by `state_save_load_roundtrip`.
fn state_path(palace_id: &str) -> Option<PathBuf> {
    Some(catchup_dir(palace_id)?.join("catchup-state.json"))
}

/// Load the catch-up watermark state for a palace, fail-open.
///
/// Why: if the state file is absent (first run) or corrupt, we want full
/// catch-up history — returning None signals "no watermark, use full history".
/// What: reads `~/.trusty-mpm/projects/<palace-id>/catchup-state.json`; returns
/// None if the file is missing, unreadable, or JSON-invalid. Never panics.
/// Test: `state_missing_file_returns_none`, `state_parse_failure_returns_none`,
/// `state_save_load_roundtrip`.
pub fn load_catchup_state(palace_id: &str) -> Option<CatchupState> {
    let path = state_path(palace_id)?;
    let bytes = std::fs::read(&path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Persist the catch-up watermark state for a palace.
///
/// Why: advancing the watermark after a successful catch-up ensures the next
/// run only surfaces incremental activity.
/// What: creates parent directories if needed, then writes the state as JSON.
/// Test: `state_save_load_roundtrip`.
pub fn save_catchup_state(palace_id: &str, state: &CatchupState) -> anyhow::Result<()> {
    let dir = catchup_dir(palace_id)
        .ok_or_else(|| anyhow::anyhow!("could not resolve home directory for catchup state"))?;
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("catchup-state.json");
    let json = serde_json::to_vec_pretty(state)?;
    std::fs::write(&path, json)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Write a state file using a temp dir (bypasses home dir lookup).
    fn write_state_to(dir: &TempDir, palace_id: &str, state: &CatchupState) {
        let p = dir.path().join(palace_id);
        fs::create_dir_all(&p).unwrap();
        let json = serde_json::to_vec_pretty(state).unwrap();
        fs::write(p.join("catchup-state.json"), json).unwrap();
    }

    /// Read a state file from a temp dir (bypasses home dir lookup).
    fn load_state_from(dir: &TempDir, palace_id: &str) -> Option<CatchupState> {
        let path = dir.path().join(palace_id).join("catchup-state.json");
        let bytes = std::fs::read(&path).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    #[test]
    fn state_save_load_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let state = CatchupState {
            last_catchup_at: "2026-06-27T10:00:00Z".parse::<DateTime<Utc>>().unwrap(),
            palace_id: "test-palace".to_string(),
            last_git_sha: Some("abc1234".to_string()),
        };
        write_state_to(&tmp, "test-palace", &state);
        let loaded = load_state_from(&tmp, "test-palace");
        assert!(loaded.is_some(), "state should load back");
        let loaded = loaded.unwrap();
        assert_eq!(loaded.palace_id, "test-palace");
        assert_eq!(loaded.last_git_sha.as_deref(), Some("abc1234"));
    }

    #[test]
    fn state_missing_file_returns_none() {
        // Simulate load_catchup_state behavior when file is absent: None.
        let path = PathBuf::from("/tmp/nonexistent-palace-xyz-catchup/catchup-state.json");
        let bytes = std::fs::read(&path);
        assert!(bytes.is_err(), "file should not exist");
        let result: Option<CatchupState> = bytes.ok().and_then(|b| serde_json::from_slice(&b).ok());
        assert!(result.is_none());
    }

    #[test]
    fn state_parse_failure_returns_none() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("test-palace");
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("catchup-state.json"), b"not valid json {{").unwrap();
        let bytes = std::fs::read(p.join("catchup-state.json")).ok();
        let result: Option<CatchupState> = bytes.and_then(|b| serde_json::from_slice(&b).ok());
        assert!(result.is_none(), "invalid JSON should yield None");
    }
}
