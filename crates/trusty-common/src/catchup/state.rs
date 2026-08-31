//! Watermark state for the incremental catch-up system (DOC-28, #1762).
//!
//! Why: the catch-up is incremental — each run surfaces only activity SINCE the
//! previous run; on the first run it surfaces all available history. This module
//! persists the watermark so consecutive native sessions get focused, not
//! repetitive, catch-ups.
//! What: [`CatchupState`] is the persisted state; [`load_catchup_state`] reads it
//! fail-open (None on missing or parse error); [`save_catchup_state`] writes it,
//! creating parent directories as needed. Both take a `state_root` override so a
//! test can point the framework root at a temp dir (#4323).
//! Test: `state_save_load_roundtrip`, `state_missing_file_returns_none`,
//! `state_parse_failure_returns_none`, `catchup_dir_honours_state_root`,
//! `catchup_dir_falls_back_to_home`.
//!
// CUTOVER BRIDGE — remove post-migration (#1762)

use std::path::{Path, PathBuf};

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
/// same location. #4323: the watermark write was the last catch-up write still
/// resolving `dirs::home_dir()` with no override, so every test that ran
/// catch-up with `advance_watermark = true` left a directory in the operator's
/// real `~/.trusty-mpm/projects/` — 39,749 of them, named after the tempdir the
/// palace id was derived from.
/// What: when `state_root` is `Some`, uses it as the `.trusty-mpm` framework
/// root; otherwise derives `~/.trusty-mpm` from the home directory. Returns
/// `<root>/projects/<palace-id>/`.
/// Test: `catchup_dir_honours_state_root`, `catchup_dir_falls_back_to_home`.
fn catchup_dir(palace_id: &str, state_root: Option<&Path>) -> Option<PathBuf> {
    let root = match state_root {
        Some(root) => root.to_path_buf(),
        None => dirs::home_dir()?.join(".trusty-mpm"),
    };
    Some(root.join("projects").join(palace_id))
}

/// Resolve the path to the catch-up state JSON file for a palace.
///
/// Why: callers need the file path for read and write; centralizing avoids drift.
/// What: `<state_root|~/.trusty-mpm>/projects/<palace-id>/catchup-state.json`.
/// Test: covered indirectly by `state_save_load_roundtrip`.
fn state_path(palace_id: &str, state_root: Option<&Path>) -> Option<PathBuf> {
    Some(catchup_dir(palace_id, state_root)?.join("catchup-state.json"))
}

/// Load the catch-up watermark state for a palace, fail-open.
///
/// Why: if the state file is absent (first run) or corrupt, we want full
/// catch-up history — returning None signals "no watermark, use full history".
/// What: reads `<state_root|~/.trusty-mpm>/projects/<palace-id>/catchup-state.json`;
/// returns None if the file is missing, unreadable, or JSON-invalid. Never
/// panics. `state_root` overrides the framework root (#4323); `None` is the
/// production home-relative default.
/// Test: `state_missing_file_returns_none`, `state_parse_failure_returns_none`,
/// `state_save_load_roundtrip`.
pub fn load_catchup_state(palace_id: &str, state_root: Option<&Path>) -> Option<CatchupState> {
    let path = state_path(palace_id, state_root)?;
    let bytes = std::fs::read(&path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Persist the catch-up watermark state for a palace.
///
/// Why: advancing the watermark after a successful catch-up ensures the next
/// run only surfaces incremental activity.
/// What: creates parent directories if needed, then writes the state as JSON.
/// `state_root` overrides the framework root (#4323); `None` is the production
/// home-relative default.
/// Test: `state_save_load_roundtrip`, `save_writes_under_the_state_root`.
pub fn save_catchup_state(
    palace_id: &str,
    state: &CatchupState,
    state_root: Option<&Path>,
) -> anyhow::Result<()> {
    let dir = catchup_dir(palace_id, state_root)
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
    use tempfile::TempDir;

    fn sample(palace_id: &str) -> CatchupState {
        CatchupState {
            last_catchup_at: "2026-06-27T10:00:00Z".parse::<DateTime<Utc>>().unwrap(),
            palace_id: palace_id.to_string(),
            last_git_sha: Some("abc1234".to_string()),
        }
    }

    /// Why: the old roundtrip test reimplemented the path join in the test body,
    /// so it proved nothing about `save_catchup_state` / `load_catchup_state`
    /// and could not have caught them writing to the real home (#4323).
    /// What: saves and loads through the real functions with a temp state root.
    /// Test: itself.
    #[test]
    fn state_save_load_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let state = sample("test-palace");
        save_catchup_state("test-palace", &state, Some(tmp.path())).unwrap();
        let loaded =
            load_catchup_state("test-palace", Some(tmp.path())).expect("state should load back");
        assert_eq!(loaded.palace_id, "test-palace");
        assert_eq!(loaded.last_git_sha.as_deref(), Some("abc1234"));
    }

    /// #4323: the write must land under the supplied root, never under `$HOME`.
    #[test]
    fn save_writes_under_the_state_root() {
        let tmp = TempDir::new().unwrap();
        save_catchup_state("pinned-palace", &sample("pinned-palace"), Some(tmp.path())).unwrap();
        assert!(
            tmp.path()
                .join("projects")
                .join("pinned-palace")
                .join("catchup-state.json")
                .is_file(),
            "the watermark must be written under the state root"
        );
    }

    /// #4323: `None` keeps the production layout — `~/.trusty-mpm/projects/…`.
    /// Path derivation only; nothing is written.
    #[test]
    fn catchup_dir_falls_back_to_home() {
        let Some(home) = dirs::home_dir() else {
            return; // No home dir resolvable: the `None` arm is unreachable here.
        };
        assert_eq!(
            catchup_dir("p", None),
            Some(home.join(".trusty-mpm").join("projects").join("p"))
        );
    }

    /// #4323: `Some(root)` replaces the whole `.trusty-mpm` root, not just home.
    #[test]
    fn catchup_dir_honours_state_root() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(
            catchup_dir("p", Some(tmp.path())),
            Some(tmp.path().join("projects").join("p"))
        );
    }

    #[test]
    fn state_missing_file_returns_none() {
        let tmp = TempDir::new().unwrap();
        assert!(load_catchup_state("never-written", Some(tmp.path())).is_none());
    }

    #[test]
    fn state_parse_failure_returns_none() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("projects").join("test-palace");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("catchup-state.json"), b"not valid json {{").unwrap();
        assert!(
            load_catchup_state("test-palace", Some(tmp.path())).is_none(),
            "invalid JSON should yield None"
        );
    }
}
