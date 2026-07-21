//! Native trusty-mpm session-pause snapshot writer (MCP `session_context_pause`
//! tool).
//!
//! Why: `/tm-session-pause` used to shell out to write the snapshot markdown
//! file and append the pause log line by hand. No reusable Rust writer existed
//! — [`crate::catchup::session_finder`] only ever *reads* the format. This
//! module is the write side, built to emit EXACTLY the section shape
//! [`crate::catchup::session_finder::find_paused_sessions`] already parses, so
//! a session this module writes round-trips through the existing reader
//! unchanged.
//! What: [`write_pause_snapshot`] writes `session-YYYYMMDD-HHMMSS.md` under
//! `<project_dir>/.trusty-mpm/sessions/` and appends the matching `pause` line
//! to `sessions-log.jsonl` via [`crate::catchup::session_log::append_entry`].
//! Test: `write_pause_snapshot_round_trips_through_reader`,
//! `write_pause_snapshot_omits_empty_sections`,
//! `write_pause_snapshot_appends_log_entry`.

use std::path::{Path, PathBuf};

use chrono::Utc;

use crate::catchup::git::capture_git_status;
use crate::catchup::session_log::{self, SessionLogEntry};

/// Input fields for a single pause snapshot.
///
/// Why: groups everything the `session_context_pause` MCP tool receives from
/// its caller so [`write_pause_snapshot`] takes one argument instead of a long
/// parameter list.
/// What: `session_id` keys the append-only log entry; `summary` is required
/// prose; `completed`/`in_progress`/`next_steps` are optional bullet lists;
/// `tmux_window` is the `session_name:window_index:window_id` string (omitted
/// entirely from the file when `None`, matching the reader's back-compat
/// contract for snapshots that predate the field).
/// Test: exercised by every `write_pause_snapshot_*` test.
#[derive(Debug, Clone)]
pub struct PauseSnapshotInput<'a> {
    /// Stable id for the originating session (keys the append-only log).
    pub session_id: &'a str,
    /// The `## Summary` section body (required, non-empty by caller contract).
    pub summary: &'a str,
    /// The `## Completed` section's bullet items.
    pub completed: &'a [String],
    /// The `## In Progress` section's bullet items.
    pub in_progress: &'a [String],
    /// The `## Next Steps` section's bullet items.
    pub next_steps: &'a [String],
    /// The `## Tmux Window` section body, when captured inside tmux.
    pub tmux_window: Option<&'a str>,
}

/// Result of a successful pause-snapshot write.
///
/// Why: the MCP tool returns both the path and the timestamp it used, so the
/// caller (and the PM transcript) can reference the exact snapshot filename.
/// What: `snapshot_path` is the absolute path written; `timestamp` is the UTC
/// instant used to derive the filename and the log entry.
/// Test: exercised by every `write_pause_snapshot_*` test.
#[derive(Debug, Clone)]
pub struct PauseSnapshotOutcome {
    /// Absolute path of the written `session-*.md` file.
    pub snapshot_path: PathBuf,
    /// The UTC instant the filename/log entry were derived from.
    pub timestamp: chrono::DateTime<Utc>,
}

/// Render one `## <Header>` section from a bullet list, or `None` when empty.
///
/// Why: [`session_finder::extract_section`](crate::catchup::session_finder)
/// treats an empty section as absent; mirroring that here keeps back-compat
/// snapshots (predating a given section) indistinguishable from "nothing to
/// report this pause".
/// What: joins `items` as `- <item>` lines under `## <header>`; returns `None`
/// for an empty slice so the caller can skip the section entirely.
/// Test: `write_pause_snapshot_omits_empty_sections`.
fn render_bullet_section(header: &str, items: &[String]) -> Option<String> {
    if items.is_empty() {
        return None;
    }
    let mut out = format!("## {header}\n");
    for item in items {
        out.push_str("- ");
        out.push_str(item);
        out.push('\n');
    }
    Some(out)
}

/// Write a pause snapshot for `project_dir` and append its log entry.
///
/// Why: the single write-side entry point for `session_context_pause` — keeps
/// the markdown shape and the log append atomic-ish (write-then-append, same
/// order the bash skill used) and in one auditable place.
/// What: creates `<project_dir>/.trusty-mpm/sessions/` if needed, captures
/// git status via [`capture_git_status`] for the `## Git Context` section,
/// writes `session-<UTC timestamp>.md` with `## Summary` (always present,
/// even if `input.summary` is empty — the caller contract requires a
/// non-empty summary) followed by `## Completed` / `## In Progress` /
/// `## Next Steps` / `## Git Context` / `## Tmux Window`, each omitted when it
/// would be empty, then appends a `pause` [`SessionLogEntry`] naming the new
/// file. Returns the resolved path and timestamp.
/// Test: `write_pause_snapshot_round_trips_through_reader`,
/// `write_pause_snapshot_omits_empty_sections`,
/// `write_pause_snapshot_appends_log_entry`.
pub fn write_pause_snapshot(
    project_dir: &Path,
    input: &PauseSnapshotInput<'_>,
) -> anyhow::Result<PauseSnapshotOutcome> {
    let sessions_dir = project_dir.join(".trusty-mpm").join("sessions");
    std::fs::create_dir_all(&sessions_dir)?;

    let now = Utc::now();
    let filename = format!("session-{}.md", now.format("%Y%m%d-%H%M%S"));
    let snapshot_path = sessions_dir.join(&filename);

    let git = capture_git_status(project_dir);
    let mut git_context = String::new();
    if let Some(branch) = &git.branch {
        git_context.push_str(&format!("Branch: {branch}\n"));
    }
    if let Some(commit) = &git.last_commit {
        git_context.push_str(&format!("Last commit: {commit}\n"));
    }
    if let Some(summary) = &git.uncommitted_summary {
        git_context.push_str(&format!("Uncommitted changes: {summary}\n"));
    }

    let mut md = format!(
        "# Session Pause - {}\n\n## Summary\n{}\n\n",
        now.to_rfc3339(),
        input.summary
    );
    if let Some(section) = render_bullet_section("Completed", input.completed) {
        md.push_str(&section);
        md.push('\n');
    }
    if let Some(section) = render_bullet_section("In Progress", input.in_progress) {
        md.push_str(&section);
        md.push('\n');
    }
    if let Some(section) = render_bullet_section("Next Steps", input.next_steps) {
        md.push_str(&section);
        md.push('\n');
    }
    if !git_context.is_empty() {
        md.push_str("## Git Context\n");
        md.push_str(&git_context);
        md.push('\n');
    }
    if let Some(win) = input.tmux_window.filter(|w| !w.is_empty()) {
        md.push_str("## Tmux Window\n");
        md.push_str(win);
        md.push('\n');
    }

    std::fs::write(&snapshot_path, md)?;

    session_log::append_entry(
        &sessions_dir,
        &SessionLogEntry {
            session_id: input.session_id.to_string(),
            event: session_log::EVENT_PAUSE.to_string(),
            snapshot: filename,
            timestamp: now.to_rfc3339(),
        },
    )?;

    Ok(PauseSnapshotOutcome {
        snapshot_path,
        timestamp: now,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catchup::session_finder::{PausedSession, find_paused_sessions};
    use tempfile::TempDir;

    fn init_git_repo(dir: &Path) {
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .output()
                .unwrap();
        };
        run(&["init"]);
        run(&["config", "user.email", "t@t.com"]);
        run(&["config", "user.name", "T"]);
        std::fs::write(dir.join("README.md"), b"hi").unwrap();
        run(&["add", "."]);
        run(&["commit", "-m", "init"]);
    }

    #[test]
    fn write_pause_snapshot_round_trips_through_reader() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());

        let input = PauseSnapshotInput {
            session_id: "s1",
            summary: "Did the thing.",
            completed: &["step 1".to_string()],
            in_progress: &["step 2".to_string()],
            next_steps: &["step 3".to_string()],
            tmux_window: Some("main:2:@7"),
        };
        let outcome = write_pause_snapshot(tmp.path(), &input).unwrap();
        assert!(outcome.snapshot_path.exists());

        let sessions = find_paused_sessions(tmp.path()).unwrap();
        assert_eq!(sessions.len(), 1);
        match &sessions[0] {
            PausedSession::TrustyMpm {
                summary,
                in_progress,
                next_steps,
                tmux_window,
                git_context,
                ..
            } => {
                assert_eq!(summary, "Did the thing.");
                assert_eq!(in_progress.as_deref(), Some("- step 2"));
                assert_eq!(next_steps.as_deref(), Some("- step 3"));
                assert_eq!(tmux_window.as_deref(), Some("main:2:@7"));
                assert!(
                    git_context
                        .as_deref()
                        .is_some_and(|g| g.contains("Branch:")),
                    "git context should be present: {git_context:?}"
                );
            }
            other => panic!("expected TrustyMpm variant, got {other:?}"),
        }
    }

    #[test]
    fn write_pause_snapshot_omits_empty_sections() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());

        let input = PauseSnapshotInput {
            session_id: "s1",
            summary: "Just a summary.",
            completed: &[],
            in_progress: &[],
            next_steps: &[],
            tmux_window: None,
        };
        let outcome = write_pause_snapshot(tmp.path(), &input).unwrap();
        let content = std::fs::read_to_string(&outcome.snapshot_path).unwrap();
        assert!(!content.contains("## Completed"));
        assert!(!content.contains("## In Progress"));
        assert!(!content.contains("## Next Steps"));
        assert!(!content.contains("## Tmux Window"));
        // Git Context still renders because it's a real repo.
        assert!(content.contains("## Git Context"));
    }

    #[test]
    fn write_pause_snapshot_appends_log_entry() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());

        let input = PauseSnapshotInput {
            session_id: "s-log",
            summary: "Summary.",
            completed: &[],
            in_progress: &[],
            next_steps: &[],
            tmux_window: None,
        };
        let outcome = write_pause_snapshot(tmp.path(), &input).unwrap();
        let sessions_dir = tmp.path().join(".trusty-mpm").join("sessions");
        let entries = session_log::read_log(&sessions_dir);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].session_id, "s-log");
        assert_eq!(entries[0].event, session_log::EVENT_PAUSE);
        assert_eq!(
            entries[0].snapshot,
            outcome.snapshot_path.file_name().unwrap().to_str().unwrap()
        );
    }

    #[test]
    fn write_pause_snapshot_nonrepo_still_writes() {
        // No git init at all — capture_git_status fails-open to all-None, and
        // the snapshot must still write (no Git Context section).
        let tmp = TempDir::new().unwrap();
        let input = PauseSnapshotInput {
            session_id: "s1",
            summary: "Summary.",
            completed: &[],
            in_progress: &[],
            next_steps: &[],
            tmux_window: None,
        };
        let outcome = write_pause_snapshot(tmp.path(), &input).unwrap();
        let content = std::fs::read_to_string(&outcome.snapshot_path).unwrap();
        assert!(!content.contains("## Git Context"));
    }
}
