//! Small CTRL helpers shared across `ctrl/*` submodules.
//!
//! Why: Centralises tiny, broadly-reused functions (slot draining, self-project
//! detection, the PM messaging audit log) so the larger turn/handler modules
//! can stay focused on their main flow.
//! What: `drain_slot`, `detect_self_project`, `append_pm_message`, and
//! `pm_messages_path`.
//! Test: Each is covered indirectly by the ctrl integration tests; pure helpers
//! are also exercised by unit tests in `mod tests` of the parent module.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::bus::BusEnvelope;

/// Take the value out of an `Arc<Mutex<Option<T>>>` slot, treating a poisoned
/// lock as "nothing there".
///
/// Why: `ctrl_chat_turn` drains several queued side-effect slots (pending
/// connect, self-task, stop) that all share the same lock-then-take pattern.
/// Centralising it removes three near-identical match blocks and ensures
/// poisoning behaviour stays consistent.
/// What: Returns `Some(value)` when the lock acquired and the slot held a
/// value; `None` when the lock was poisoned OR the slot was empty.
/// Test: Exercised indirectly by `ctrl_chat_turn` integration tests; the
/// happy path is the common case (no poisoning).
pub(crate) fn drain_slot<T>(slot: &Arc<Mutex<Option<T>>>) -> Option<T> {
    slot.lock().ok()?.take()
}

/// Detect open-mpm's own project root. (#182)
///
/// Why: When CTRL runs from a checkout/build of open-mpm itself, we want to
/// expose self-development tools (status, dispatch a task on ourselves).
/// Detection has to work whether the user runs `cargo run` (cwd = repo) or
/// invokes a release binary from elsewhere on the filesystem (cwd =
/// somewhere unrelated; current_exe = `…/target/release/open-mpm`).
/// What: Tries three strategies in order:
///   1. `OPEN_MPM_PROJECT_DIR` env var (explicit override).
///   2. Walk up from `current_exe()` looking for `.open-mpm/agents/pm.toml`.
///   3. Use `current_dir()` if it contains the same marker.
/// Returns the first match, or `None` when no strategy succeeds.
/// Test: `detect_self_project_finds_repo_via_cwd` (in tests below).
pub fn detect_self_project() -> Option<PathBuf> {
    fn looks_like_self(p: &Path) -> bool {
        p.join(".open-mpm").join("agents").join("pm.toml").is_file()
    }
    fn walk_up(start: &Path) -> Option<PathBuf> {
        let mut cur = Some(start.to_path_buf());
        while let Some(p) = cur {
            if looks_like_self(&p) {
                return Some(p);
            }
            cur = p.parent().map(Path::to_path_buf);
        }
        None
    }

    if let Ok(p) = std::env::var("OPEN_MPM_PROJECT_DIR")
        && let Ok(canon) = PathBuf::from(&p).canonicalize()
        && looks_like_self(&canon)
    {
        return Some(canon);
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
        && let Some(found) = walk_up(parent)
    {
        return Some(found);
    }
    if let Ok(cwd) = std::env::current_dir()
        && let Some(found) = walk_up(&cwd)
    {
        return Some(found);
    }
    None
}

/// JSONL record persisted to `~/.open-mpm/sessions/pm-messages.jsonl` for
/// every PM-to-PM (or external) bus envelope that CTRL relays.
///
/// Why: The bus is in-memory broadcast; once the program exits the trail
/// disappears. A grep-friendly JSONL audit log lets users reconstruct
/// who-told-whom-what across runs.
/// What: ISO-8601 timestamp, source/target project basenames, raw content
/// string, and a uuid `message_id` for correlation across logs.
#[derive(Debug, Serialize, Deserialize)]
pub struct PmMessageRecord {
    pub timestamp: String,
    pub from_project: String,
    pub to_project: String,
    pub content: String,
    pub message_id: String,
}

/// Append one bus envelope to `~/.open-mpm/sessions/pm-messages.jsonl`.
///
/// Why: Mirrors `session_record::append_run_record`; gives CTRL a durable
/// audit trail for inter-project messaging without a database.
/// What: Best-effort; creates parent dirs, opens append-mode, writes one
/// JSON line. The "content" field projects whatever string we can find on
/// the inner message (`task.text` if shaped as `{type:"task", text:"..."}`
/// or the raw JSON otherwise).
/// Test: covered indirectly by ctrl bus relay integration; unit-tested via
/// `append_pm_message_writes_jsonl_line`.
pub fn append_pm_message(env: &BusEnvelope) -> anyhow::Result<()> {
    let path = pm_messages_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = env
        .message
        .get("text")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| env.message.to_string());
    let record = PmMessageRecord {
        timestamp: chrono::Utc::now().to_rfc3339(),
        from_project: env.source_project.clone(),
        to_project: env.target_project.clone().unwrap_or_default(),
        content,
        message_id: uuid::Uuid::new_v4().to_string(),
    };
    let line = serde_json::to_string(&record)?;
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    f.write_all(line.as_bytes())?;
    f.write_all(b"\n")?;
    Ok(())
}

/// Resolve `~/.open-mpm/sessions/pm-messages.jsonl`.
pub(crate) fn pm_messages_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("no home dir")?;
    Ok(home
        .join(".open-mpm")
        .join("sessions")
        .join("pm-messages.jsonl"))
}
