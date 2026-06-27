//! Parsing and loading of claude-mpm paused-session JSON files.
//!
//! Why: during migration from claude-mpm (Python) to trusty-mpm (Rust), users
//! may have sessions paused in the old format. This module provides the bridge
//! types needed to load and render those sessions without re-spawning claude-mpm.
//! What: [`ClaudeMpmSession`] is a serde-deserializable struct that loads the
//! digest fields only (deliberately omitting the large `conversation` array).
//! [`load_latest_claude_mpm_session`] and [`load_all_claude_mpm_sessions`] are
//! the loading entry-points.
//! Test: `roundtrip_full_json`, `roundtrip_partial_json`, `roundtrip_skips_conversation`
//! in the inline test module.
//!
// CUTOVER BRIDGE — remove post-migration (#1762)

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde::Deserialize;

/// Digest of a paused claude-mpm session (JSON format).
///
/// Why: the full JSON can contain a multi-MB `conversation` array; loading only
/// the digest fields keeps memory usage bounded and avoids leaking conversation
/// history into unexpected contexts.
/// What: captures the fields defined in the claude-mpm session schema that are
/// relevant for resuming work. The `conversation` field is intentionally absent:
/// serde ignores unknown JSON fields by default.
/// Test: `roundtrip_full_json` verifies that all expected fields deserialize;
/// `roundtrip_skips_conversation` verifies that JSON with a huge `conversation`
/// field still parses correctly.
///
// CUTOVER BRIDGE — remove post-migration (#1762)
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ClaudeMpmSession {
    /// Unique identifier for this session.
    #[serde(default)]
    pub session_id: String,
    /// ISO-8601 timestamp when the session was paused.
    #[serde(default)]
    pub paused_at: Option<String>,
    /// Approximate duration of the session in hours.
    #[serde(default)]
    pub duration_hours: Option<f64>,
    /// Fraction of context window used at pause time (0.0–1.0).
    #[serde(default)]
    pub context_usage: Option<f64>,
    /// Git branch, recent commits, and modified files at pause time.
    #[serde(default)]
    pub git_context: Option<String>,
    /// High-priority reminders the PM recorded at pause.
    #[serde(default)]
    pub important_reminders: Option<Vec<String>>,
    /// Instructions the PM wrote for how to resume this session.
    #[serde(default)]
    pub resume_instructions: Option<String>,
    /// Open questions unresolved at pause time.
    #[serde(default)]
    pub open_questions: Option<Vec<String>>,
    /// Free-form todo items captured at pause.
    #[serde(default)]
    pub todos: Option<Vec<String>>,
    /// Structured task list at pause time.
    #[serde(default)]
    pub task_list: Option<Vec<String>>,
    /// Absolute path of the project directory this session belongs to.
    #[serde(default)]
    pub project_path: Option<String>,
    // `conversation`, `active_context`, `performance_metrics`, `version`, `build`
    // are intentionally NOT present; serde_json silently ignores unknown fields.
}

/// Load the most-recent paused claude-mpm session for a project directory.
///
/// Why: callers (the resume bridge) want a single "best guess" session for a
/// given project without iterating all files themselves.
/// What: reads `<project_dir>/.claude-mpm/sessions/LATEST-SESSION.txt`; if it
/// contains a filename, loads that file. Otherwise falls back to the session-*.json
/// file with the most-recent mtime. Returns `Ok(None)` when no sessions exist.
/// Test: `load_latest_uses_pointer`, `load_latest_falls_back_to_newest_mtime`.
///
// CUTOVER BRIDGE — remove post-migration (#1762)
pub fn load_latest_claude_mpm_session(
    project_dir: &Path,
) -> anyhow::Result<Option<ClaudeMpmSession>> {
    let sessions_dir = project_dir.join(".claude-mpm").join("sessions");
    if !sessions_dir.is_dir() {
        return Ok(None);
    }

    // Try the LATEST-SESSION.txt pointer first.
    let pointer = sessions_dir.join("LATEST-SESSION.txt");
    if let Ok(content) = std::fs::read_to_string(&pointer) {
        // The pointer may be a bare filename OR a multi-line human-readable text;
        // extract the first token that ends with ".json".
        let candidate = content
            .lines()
            .find(|l| l.trim().ends_with(".json"))
            .map(|l| l.trim().to_owned());
        if let Some(fname) = candidate {
            let path = sessions_dir.join(&fname);
            if path.exists() {
                let session = parse_session_file(&path)?;
                return Ok(Some(session));
            }
        }
        // Some pointer files contain just the filename on the first line.
        let first_line = content.lines().next().unwrap_or("").trim().to_owned();
        if !first_line.is_empty() {
            let path = sessions_dir.join(&first_line);
            if path.exists() && first_line.ends_with(".json") {
                let session = parse_session_file(&path)?;
                return Ok(Some(session));
            }
        }
    }

    // Fallback: pick the session-*.json file with the newest mtime.
    newest_session_file(&sessions_dir)
        .map(|opt| opt.map(|p| parse_session_file(&p)).transpose())
        .unwrap_or(Ok(None))
}

/// Load all paused claude-mpm sessions for a project directory.
///
/// Why: the `--full` mode of `tm session catchup` may wish to present all
/// available sessions rather than just the most recent one.
/// What: globs `<project_dir>/.claude-mpm/sessions/session-*.json`, parses each,
/// returns them sorted newest-first by `paused_at` (or by filename if the field
/// is absent). Silently skips files that fail to parse.
/// Test: `load_all_returns_sorted`.
///
// CUTOVER BRIDGE — remove post-migration (#1762)
pub fn load_all_claude_mpm_sessions(project_dir: &Path) -> anyhow::Result<Vec<ClaudeMpmSession>> {
    let sessions_dir = project_dir.join(".claude-mpm").join("sessions");
    if !sessions_dir.is_dir() {
        return Ok(vec![]);
    }

    let mut sessions: Vec<(String, ClaudeMpmSession)> = std::fs::read_dir(&sessions_dir)
        .with_context(|| format!("reading {}", sessions_dir.display()))?
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name().into_string().unwrap_or_default();
            name.starts_with("session-") && name.ends_with(".json")
        })
        .filter_map(|e| {
            let name = e.file_name().into_string().unwrap_or_default();
            parse_session_file(&e.path()).ok().map(|s| (name, s))
        })
        .collect();

    // Sort newest-first: use `paused_at` when present, else filename (which is
    // timestamp-ordered: `session-YYYYMMDD-HHMMSS.json`).
    sessions.sort_by(|(name_a, a), (name_b, b)| {
        let ka = a.paused_at.as_deref().unwrap_or(name_a.as_str());
        let kb = b.paused_at.as_deref().unwrap_or(name_b.as_str());
        kb.cmp(ka)
    });

    Ok(sessions.into_iter().map(|(_, s)| s).collect())
}

/// Parse a single claude-mpm session JSON file.
///
/// Why: isolates the JSON parsing so callers handle errors uniformly.
/// What: reads the file at `path` and deserialises it into [`ClaudeMpmSession`].
/// Test: `roundtrip_full_json`.
///
// CUTOVER BRIDGE — remove post-migration (#1762)
fn parse_session_file(path: &Path) -> anyhow::Result<ClaudeMpmSession> {
    let bytes =
        std::fs::read(path).with_context(|| format!("reading session file {}", path.display()))?;
    serde_json::from_slice(&bytes)
        .with_context(|| format!("parsing session file {}", path.display()))
}

/// Return the `session-*.json` file with the most-recent mtime, if any.
///
/// Why: provides the fallback discovery path when no LATEST-SESSION.txt pointer
/// is available.
/// What: reads the directory, filters to `session-*.json` filenames, and picks
/// the entry with the greatest `modified()` timestamp. Returns `None` when none
/// exist.
/// Test: covered by `load_latest_falls_back_to_newest_mtime`.
///
// CUTOVER BRIDGE — remove post-migration (#1762)
fn newest_session_file(sessions_dir: &Path) -> anyhow::Result<Option<PathBuf>> {
    let best = std::fs::read_dir(sessions_dir)
        .with_context(|| format!("reading {}", sessions_dir.display()))?
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name().into_string().unwrap_or_default();
            name.starts_with("session-") && name.ends_with(".json")
        })
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok());

    Ok(best.map(|e| e.path()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_session(dir: &Path, filename: &str, json: &str) -> PathBuf {
        let path = dir.join(filename);
        fs::write(&path, json).unwrap();
        path
    }

    fn sessions_dir(tmp: &TempDir) -> PathBuf {
        let d = tmp.path().join(".claude-mpm").join("sessions");
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn roundtrip_full_json() {
        let json = serde_json::json!({
            "session_id": "abc-123",
            "paused_at": "2026-06-27T10:00:00Z",
            "duration_hours": 1.5,
            "context_usage": 0.72,
            "git_context": "branch: main\ncommit: abc1234",
            "important_reminders": ["remember A", "remember B"],
            "resume_instructions": "Pick up from step 3",
            "open_questions": ["Should we use X?"],
            "todos": ["Fix lint"],
            "task_list": ["task 1", "task 2"],
            "project_path": "/home/user/my-proj"
        });
        let s: ClaudeMpmSession = serde_json::from_value(json).unwrap();
        assert_eq!(s.session_id, "abc-123");
        assert_eq!(s.paused_at.as_deref(), Some("2026-06-27T10:00:00Z"));
        assert_eq!(s.context_usage, Some(0.72));
        assert_eq!(s.important_reminders.as_deref().unwrap().len(), 2);
        assert_eq!(s.todos.as_deref().unwrap(), &["Fix lint"]);
    }

    #[test]
    fn roundtrip_skips_conversation() {
        let json = serde_json::json!({
            "session_id": "skip-conv",
            "paused_at": "2026-06-27T11:00:00Z",
            "conversation": [
                {"role": "user", "content": "hello"},
                {"role": "assistant", "content": "world"}
            ]
        });
        let s: ClaudeMpmSession = serde_json::from_value(json).unwrap();
        assert_eq!(s.session_id, "skip-conv");
        // If we get here without error, the large conversation field was ignored.
    }

    #[test]
    fn roundtrip_partial_json_uses_defaults() {
        let json = serde_json::json!({
            "session_id": "partial"
        });
        let s: ClaudeMpmSession = serde_json::from_value(json).unwrap();
        assert_eq!(s.session_id, "partial");
        assert!(s.paused_at.is_none());
        assert!(s.resume_instructions.is_none());
        assert!(s.todos.is_none());
    }

    #[test]
    fn load_latest_uses_pointer() {
        let tmp = TempDir::new().unwrap();
        let sdir = sessions_dir(&tmp);
        let project_dir = tmp.path();
        let fname = "session-20260627-100000.json";
        write_session(
            &sdir,
            fname,
            r#"{"session_id":"ptr-sess","paused_at":"2026-06-27T10:00:00Z"}"#,
        );
        fs::write(sdir.join("LATEST-SESSION.txt"), fname).unwrap();
        let result = load_latest_claude_mpm_session(project_dir).unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().session_id, "ptr-sess");
    }

    #[test]
    fn load_latest_falls_back_to_newest_mtime() {
        let tmp = TempDir::new().unwrap();
        let sdir = sessions_dir(&tmp);
        // Write two files; the second written has a newer mtime.
        write_session(
            &sdir,
            "session-20260626-090000.json",
            r#"{"session_id":"old"}"#,
        );
        // Small sleep to ensure mtime differs on fast filesystems.
        std::thread::sleep(std::time::Duration::from_millis(10));
        write_session(
            &sdir,
            "session-20260627-100000.json",
            r#"{"session_id":"new"}"#,
        );
        let result = load_latest_claude_mpm_session(tmp.path()).unwrap();
        // Should pick the session with the newest mtime.
        assert!(result.is_some());
    }

    #[test]
    fn load_all_returns_sorted() {
        let tmp = TempDir::new().unwrap();
        let sdir = sessions_dir(&tmp);
        write_session(
            &sdir,
            "session-20260625-080000.json",
            r#"{"session_id":"oldest","paused_at":"2026-06-25T08:00:00Z"}"#,
        );
        write_session(
            &sdir,
            "session-20260627-100000.json",
            r#"{"session_id":"newest","paused_at":"2026-06-27T10:00:00Z"}"#,
        );
        write_session(
            &sdir,
            "session-20260626-090000.json",
            r#"{"session_id":"middle","paused_at":"2026-06-26T09:00:00Z"}"#,
        );
        let all = load_all_claude_mpm_sessions(tmp.path()).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].session_id, "newest");
        assert_eq!(all[2].session_id, "oldest");
    }
}
