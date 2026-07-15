//! Unified paused-session discovery across trusty-mpm and claude-mpm formats.
//!
//! Why: during the migration period, a project may have paused sessions in both
//! the trusty-mpm native markdown format and the claude-mpm JSON format. This
//! module finds, merges, and renders sessions from both sources so the operator
//! gets a single, time-ordered catch-up digest.
//! What: [`find_paused_sessions`] returns a sorted list of [`PausedSession`];
//! [`render_resume_context`] renders a markdown digest from that list.
//! Test: `find_merges_both_formats`, `find_orders_newest_first`,
//! `render_contains_digest_not_conversation` in the inline test module.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

use crate::catchup::mpm_session::{ClaudeMpmSession, load_all_claude_mpm_sessions};

/// A discovered paused session in either the trusty-mpm or claude-mpm format.
///
/// Why: the two formats have different field sets; a tagged enum lets callers
/// dispatch cleanly while the shared rendering code handles both variants.
/// What: `TrustyMpm` wraps parsed data from a `.trusty-mpm/sessions/session-*.md`
/// file; `ClaudeMpm` wraps a [`ClaudeMpmSession`] loaded from the JSON format.
/// Test: `find_merges_both_formats` exercises both arms.
#[derive(Debug, Clone)]
pub enum PausedSession {
    /// A session paused by trusty-mpm (native markdown format).
    TrustyMpm {
        /// Path to the `.md` session file.
        path: PathBuf,
        /// Timestamp parsed from the filename or `## Paused At:` header.
        paused_at: Option<DateTime<Utc>>,
        /// Content of the `## Summary` section.
        summary: String,
        /// Content of the `## Git Context` section.
        git_context: Option<String>,
        /// Content of the `## In Progress` section.
        in_progress: Option<String>,
        /// Content of the `## Next Steps` section.
        next_steps: Option<String>,
        /// Why: lets resume re-align to the originating tmux window instead of
        /// leaving the operator on whatever window they happen to be on.
        /// What: the `session_name:window_index:window_id` string captured at
        /// pause time from `tmux display-message` and stored in the
        /// `## Tmux Window` section; `None` when the snapshot predates the
        /// feature or was created outside tmux.
        /// Test: `parse_extracts_tmux_window`, `parse_missing_tmux_window_is_none`.
        tmux_window: Option<String>,
    },
    /// A session paused by the claude-mpm Python tool (JSON format).
    ///
    // CUTOVER BRIDGE — remove post-migration (#1762)
    ClaudeMpm { session: ClaudeMpmSession },
}

impl PausedSession {
    /// Return a sortable pause timestamp, or `None` if not parseable.
    ///
    /// Why: needed to sort sessions newest-first regardless of format.
    /// What: for `TrustyMpm` returns `paused_at` directly; for `ClaudeMpm`
    /// parses the ISO-8601 `paused_at` string.
    /// Test: covered by `find_orders_newest_first`.
    pub fn sort_key(&self) -> Option<DateTime<Utc>> {
        match self {
            PausedSession::TrustyMpm { paused_at, .. } => *paused_at,
            PausedSession::ClaudeMpm { session } => session
                .paused_at
                .as_deref()
                .and_then(|s| s.parse::<DateTime<Utc>>().ok()),
        }
    }
}

/// Find all paused sessions for `project_dir`, merging both formats newest-first.
///
/// Why: during cutover a project may hold both a trusty-mpm `.md` session and
/// older claude-mpm `.json` sessions; a unified view lets the operator decide
/// which context to resume from.
/// What: scans `<project_dir>/.trusty-mpm/sessions/session-*.md` for native
/// sessions and `<project_dir>/.claude-mpm/sessions/session-*.json` for claude-mpm
/// sessions, merges them, and sorts newest-first by pause timestamp (unknown
/// timestamps sort last). Returns an empty vec — not an error — when no sessions
/// exist in either format.
/// Test: `find_merges_both_formats`, `find_orders_newest_first`.
pub fn find_paused_sessions(project_dir: &Path) -> anyhow::Result<Vec<PausedSession>> {
    let mut sessions = Vec::new();

    // ── trusty-mpm native format (.md) ───────────────────────────────────────
    let tm_sessions_dir = project_dir.join(".trusty-mpm").join("sessions");
    if tm_sessions_dir.is_dir()
        && let Ok(rd) = std::fs::read_dir(&tm_sessions_dir)
    {
        for entry in rd.flatten() {
            let name = entry.file_name().into_string().unwrap_or_default();
            if name.starts_with("session-")
                && name.ends_with(".md")
                && let Ok(s) = parse_trusty_mpm_session(&entry.path())
            {
                sessions.push(s);
            }
        }
    }

    // ── claude-mpm JSON format ────────────────────────────────────────────────
    // CUTOVER BRIDGE — remove post-migration (#1762)
    let claude_sessions = load_all_claude_mpm_sessions(project_dir).unwrap_or_default();
    for s in claude_sessions {
        sessions.push(PausedSession::ClaudeMpm { session: s });
    }

    // Sort newest-first; sessions with no parseable timestamp go last.
    sessions.sort_by(|a, b| match (a.sort_key(), b.sort_key()) {
        (Some(ta), Some(tb)) => tb.cmp(&ta),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });

    Ok(sessions)
}

/// Resolve the latest native `.trusty-mpm` snapshot, preferring the per-session
/// log entry when a session id is known.
///
/// Why: with concurrent `tm` sessions in one project, resume must reload the
/// current session's own newest snapshot, not whichever session paused last.
/// The append-only `sessions-log.jsonl` makes that recoverable; this is the
/// read side of the global-pointer fix.
/// What: when `session_id` is `Some`, returns the newest `pause` snapshot logged
/// for that id (if its file still exists); otherwise, or when that session has
/// no log entry, falls back to the shared resolver chain
/// (log-overall → legacy `LATEST-SESSION.txt` → newest `session-*.md` by mtime)
/// against `<project_dir>/.trusty-mpm/sessions/`. Returns `None` when nothing
/// resolves.
/// Test: `latest_snapshot_prefers_session_log`, `latest_snapshot_falls_back`.
pub fn latest_trusty_mpm_snapshot(project_dir: &Path, session_id: Option<&str>) -> Option<PathBuf> {
    let sessions_dir = project_dir.join(".trusty-mpm").join("sessions");
    if let Some(id) = session_id
        && let Some(name) =
            crate::catchup::session_log::latest_snapshot_for_session(&sessions_dir, id)
    {
        let path = sessions_dir.join(name);
        if path.exists() {
            return Some(path);
        }
    }
    crate::catchup::session_log::resolve_latest_snapshot(&sessions_dir, "md")
}

/// Render a markdown catch-up digest for a slice of paused sessions.
///
/// Why: `tm session catchup` prints this digest to stdout so the operator (or
/// the PM skill) can inject it as conversation context for the current Claude
/// session.
/// What: produces one block per session containing `resume_instructions`,
/// `important_reminders`, `open_questions`, `todos`/`task_list`, `git_context`,
/// `paused_at`, and `context_usage`. Raw `conversation` content is never included.
/// Returns an empty string when `sessions` is empty.
/// Test: `render_contains_digest_not_conversation`.
pub fn render_resume_context(sessions: &[PausedSession]) -> String {
    if sessions.is_empty() {
        return String::from("No paused sessions found.\n");
    }

    let mut out = String::from("# Paused Session Catch-Up\n\n");
    for (i, session) in sessions.iter().enumerate() {
        out.push_str(&format!("## Session {} of {}\n\n", i + 1, sessions.len()));
        render_session(&mut out, session);
        out.push('\n');
    }
    out
}

fn render_session(out: &mut String, session: &PausedSession) {
    match session {
        PausedSession::TrustyMpm {
            path,
            paused_at,
            summary,
            git_context,
            in_progress,
            next_steps,
            tmux_window,
        } => {
            out.push_str("**Format:** trusty-mpm (native)\n");
            if let Some(ts) = paused_at {
                out.push_str(&format!("**Paused At:** {ts}\n"));
            }
            if let Some(win) = tmux_window
                && !win.is_empty()
            {
                out.push_str(&format!("**Tmux Window:** {win}\n"));
            }
            out.push_str(&format!("**File:** {}\n\n", path.display()));
            if !summary.is_empty() {
                out.push_str(&format!("### Summary\n{summary}\n\n"));
            }
            if let Some(ctx) = in_progress
                && !ctx.is_empty()
            {
                out.push_str(&format!("### In Progress\n{ctx}\n\n"));
            }
            if let Some(steps) = next_steps
                && !steps.is_empty()
            {
                out.push_str(&format!("### Next Steps\n{steps}\n\n"));
            }
            if let Some(git) = git_context
                && !git.is_empty()
            {
                out.push_str(&format!("### Git Context\n{git}\n\n"));
            }
        }
        PausedSession::ClaudeMpm { session: s } => {
            // CUTOVER BRIDGE — remove post-migration (#1762)
            out.push_str("**Format:** claude-mpm (legacy)\n");
            if let Some(pa) = &s.paused_at {
                out.push_str(&format!("**Paused At:** {pa}\n"));
            }
            if let Some(cu) = s.context_usage {
                out.push_str(&format!("**Context Usage:** {:.0}%\n", cu * 100.0));
            }
            if let Some(dh) = s.duration_hours {
                out.push_str(&format!("**Duration:** {dh:.1}h\n"));
            }
            out.push('\n');
            if let Some(ri) = &s.resume_instructions
                && !ri.is_empty()
            {
                out.push_str(&format!("### Resume Instructions\n{ri}\n\n"));
            }
            if let Some(reminders) = &s.important_reminders
                && !reminders.is_empty()
            {
                out.push_str("### Important Reminders\n");
                for r in reminders {
                    out.push_str(&format!("- {r}\n"));
                }
                out.push('\n');
            }
            if let Some(oq) = &s.open_questions
                && !oq.is_empty()
            {
                out.push_str("### Open Questions\n");
                for q in oq {
                    out.push_str(&format!("- {q}\n"));
                }
                out.push('\n');
            }
            // Render todos + task_list together.
            let tasks: Vec<&String> = s
                .todos
                .as_deref()
                .unwrap_or(&[])
                .iter()
                .chain(s.task_list.as_deref().unwrap_or(&[]).iter())
                .collect();
            if !tasks.is_empty() {
                out.push_str("### Tasks\n");
                for t in tasks {
                    out.push_str(&format!("- {t}\n"));
                }
                out.push('\n');
            }
            if let Some(git) = &s.git_context
                && !git.is_empty()
            {
                out.push_str(&format!("### Git Context\n{git}\n\n"));
            }
        }
    }
}

/// Parse a trusty-mpm native session markdown file.
///
/// Why: isolates the markdown parsing so it can be tested independently.
/// What: reads the file, extracts sections by `## <Header>` delimiters, and
/// attempts to parse a timestamp from the filename (`session-YYYYMMDD-HHMMSS.md`).
/// Test: `parse_trusty_mpm_session_extracts_sections`.
fn parse_trusty_mpm_session(path: &Path) -> anyhow::Result<PausedSession> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))?;

    let summary = extract_section(&content, "Summary").unwrap_or_default();
    let git_context = extract_section(&content, "Git Context");
    let in_progress = extract_section(&content, "In Progress");
    let next_steps = extract_section(&content, "Next Steps");
    // `None` for snapshots without the section keeps older files back-compat.
    let tmux_window = extract_section(&content, "Tmux Window");

    // Try to parse a UTC timestamp from the filename: session-YYYYMMDD-HHMMSS.md
    let paused_at = path
        .file_stem()
        .and_then(|s| s.to_str())
        .and_then(|s| s.strip_prefix("session-"))
        .and_then(parse_filename_timestamp);

    Ok(PausedSession::TrustyMpm {
        path: path.to_owned(),
        paused_at,
        summary,
        git_context,
        in_progress,
        next_steps,
        tmux_window,
    })
}

/// Extract the content of a `## <header>` section from markdown text.
///
/// Why: the trusty-mpm session format uses level-2 headers as section delimiters.
/// What: returns the trimmed content between `## <header>` and the next `## `
/// or end-of-file. Returns `None` when the section is absent.
/// Test: covered indirectly by `parse_trusty_mpm_session_extracts_sections`.
fn extract_section(text: &str, header: &str) -> Option<String> {
    let needle = format!("## {header}");
    let start = text.find(&needle)?;
    let after = &text[start + needle.len()..];
    let end = after.find("\n## ").unwrap_or(after.len());
    let section = after[..end].trim().to_owned();
    if section.is_empty() {
        None
    } else {
        Some(section)
    }
}

/// Parse a timestamp from the `YYYYMMDD-HHMMSS` portion of a session filename.
///
/// Why: lets us sort native sessions by pause time even when no timestamp header
/// exists in the file body.
/// What: expects input like `20260627-142030`; parses it as UTC.
/// Test: covered by `parse_filename_timestamp_roundtrip`.
fn parse_filename_timestamp(stem: &str) -> Option<DateTime<Utc>> {
    // stem is like "20260627-142030"
    if stem.len() != 15 {
        return None;
    }
    let (date_part, time_part) = stem.split_once('-')?;
    if date_part.len() != 8 || time_part.len() != 6 {
        return None;
    }
    let s = format!(
        "{}-{}-{}T{}:{}:{}Z",
        &date_part[0..4],
        &date_part[4..6],
        &date_part[6..8],
        &time_part[0..2],
        &time_part[2..4],
        &time_part[4..6],
    );
    s.parse::<DateTime<Utc>>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catchup::mpm_session::ClaudeMpmSession;
    use std::fs;
    use tempfile::TempDir;

    fn write_file(dir: &Path, name: &str, content: &str) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, content).unwrap();
        p
    }

    #[test]
    fn parse_filename_timestamp_roundtrip() {
        let ts = parse_filename_timestamp("20260627-142030");
        assert!(ts.is_some());
        let ts = ts.unwrap();
        assert_eq!(ts.format("%Y-%m-%d").to_string(), "2026-06-27");
    }

    #[test]
    fn parse_filename_timestamp_rejects_short() {
        assert!(parse_filename_timestamp("2026062").is_none());
        assert!(parse_filename_timestamp("").is_none());
    }

    #[test]
    fn extract_section_finds_content() {
        let md = "# Title\n\n## Summary\nDid lots of work.\n\n## Next Steps\nFix tests.";
        assert_eq!(
            extract_section(md, "Summary").as_deref(),
            Some("Did lots of work.")
        );
        assert_eq!(
            extract_section(md, "Next Steps").as_deref(),
            Some("Fix tests.")
        );
        assert!(extract_section(md, "Missing").is_none());
    }

    #[test]
    fn find_merges_both_formats() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path();

        // Create trusty-mpm session file.
        let tm_dir = project.join(".trusty-mpm").join("sessions");
        fs::create_dir_all(&tm_dir).unwrap();
        write_file(
            &tm_dir,
            "session-20260627-100000.md",
            "## Summary\nDone something.\n## Git Context\nbranch: main",
        );

        // Create claude-mpm session file.
        let cm_dir = project.join(".claude-mpm").join("sessions");
        fs::create_dir_all(&cm_dir).unwrap();
        write_file(
            &cm_dir,
            "session-20260626-090000.json",
            r#"{"session_id":"cm1","paused_at":"2026-06-26T09:00:00Z"}"#,
        );

        let sessions = find_paused_sessions(project).unwrap();
        assert_eq!(sessions.len(), 2);
        assert!(
            matches!(sessions[0], PausedSession::TrustyMpm { .. }),
            "newer trusty-mpm session should be first"
        );
        assert!(
            matches!(sessions[1], PausedSession::ClaudeMpm { .. }),
            "older claude-mpm session should be second"
        );
    }

    #[test]
    fn find_orders_newest_first() {
        let tmp = TempDir::new().unwrap();
        let cm_dir = tmp.path().join(".claude-mpm").join("sessions");
        fs::create_dir_all(&cm_dir).unwrap();
        write_file(
            &cm_dir,
            "session-20260625-080000.json",
            r#"{"session_id":"old","paused_at":"2026-06-25T08:00:00Z"}"#,
        );
        write_file(
            &cm_dir,
            "session-20260627-100000.json",
            r#"{"session_id":"new","paused_at":"2026-06-27T10:00:00Z"}"#,
        );

        let sessions = find_paused_sessions(tmp.path()).unwrap();
        assert_eq!(sessions.len(), 2);
        let first_key = sessions[0].sort_key().unwrap();
        let second_key = sessions[1].sort_key().unwrap();
        assert!(first_key > second_key, "newest should be first");
    }

    #[test]
    fn render_contains_digest_not_conversation() {
        let session = ClaudeMpmSession {
            session_id: "test-123".to_string(),
            paused_at: Some("2026-06-27T10:00:00Z".to_string()),
            resume_instructions: Some("Resume from step 3".to_string()),
            important_reminders: Some(vec!["Don't break prod".to_string()]),
            git_context: Some("branch: main".to_string()),
            ..Default::default()
        };
        let sessions = vec![PausedSession::ClaudeMpm { session }];
        let output = render_resume_context(&sessions);
        assert!(
            output.contains("Resume from step 3"),
            "resume instructions should be present"
        );
        assert!(
            output.contains("Don't break prod"),
            "reminders should be present"
        );
        assert!(
            output.contains("branch: main"),
            "git context should be present"
        );
        assert!(
            !output.contains("conversation"),
            "conversation must NOT appear in rendered output"
        );
    }

    #[test]
    fn parse_extracts_tmux_window() {
        let tmp = TempDir::new().unwrap();
        let p = write_file(
            tmp.path(),
            "session-20260627-100000.md",
            "## Summary\nWork.\n\n## Tmux Window\nmain:2:@7\n\n## Git Context\nbranch: main",
        );
        let session = parse_trusty_mpm_session(&p).unwrap();
        match session {
            PausedSession::TrustyMpm { tmux_window, .. } => {
                assert_eq!(tmux_window.as_deref(), Some("main:2:@7"));
            }
            _ => panic!("expected TrustyMpm variant"),
        }
    }

    #[test]
    fn parse_missing_tmux_window_is_none() {
        let tmp = TempDir::new().unwrap();
        let p = write_file(
            tmp.path(),
            "session-20260627-100000.md",
            "## Summary\nWork.\n\n## Git Context\nbranch: main",
        );
        let session = parse_trusty_mpm_session(&p).unwrap();
        match session {
            PausedSession::TrustyMpm { tmux_window, .. } => {
                assert!(tmux_window.is_none(), "back-compat: absent section => None");
            }
            _ => panic!("expected TrustyMpm variant"),
        }
    }

    #[test]
    fn render_tmux_window_present_and_omitted() {
        // Present → line renders.
        let with = PausedSession::TrustyMpm {
            path: PathBuf::from("/tmp/session-20260627-100000.md"),
            paused_at: None,
            summary: "Work.".to_string(),
            git_context: None,
            in_progress: None,
            next_steps: None,
            tmux_window: Some("main:2:@7".to_string()),
        };
        let output = render_resume_context(std::slice::from_ref(&with));
        assert!(
            output.contains("**Tmux Window:** main:2:@7"),
            "recorded window should render"
        );

        // None → line omitted.
        let without = PausedSession::TrustyMpm {
            path: PathBuf::from("/tmp/session-20260627-100000.md"),
            paused_at: None,
            summary: "Work.".to_string(),
            git_context: None,
            in_progress: None,
            next_steps: None,
            tmux_window: None,
        };
        let output = render_resume_context(std::slice::from_ref(&without));
        assert!(
            !output.contains("Tmux Window"),
            "absent window must not render a line"
        );
    }

    #[test]
    fn latest_snapshot_prefers_session_log() {
        use crate::catchup::session_log::{SessionLogEntry, append_entry};
        let tmp = TempDir::new().unwrap();
        let sdir = tmp.path().join(".trusty-mpm").join("sessions");
        fs::create_dir_all(&sdir).unwrap();

        // Two sessions interleave; each snapshot file exists on disk.
        write_file(&sdir, "session-A.md", "## Summary\nS1 work");
        write_file(&sdir, "session-B.md", "## Summary\nS2 work");
        let mk = |id: &str, snap: &str, ts: &str| SessionLogEntry {
            session_id: id.to_string(),
            event: "pause".to_string(),
            snapshot: snap.to_string(),
            timestamp: ts.to_string(),
        };
        append_entry(&sdir, &mk("s1", "session-A.md", "t1")).unwrap();
        append_entry(&sdir, &mk("s2", "session-B.md", "t2")).unwrap();

        // s1 resumes its own snapshot even though s2 paused last.
        let got = latest_trusty_mpm_snapshot(tmp.path(), Some("s1")).unwrap();
        assert_eq!(got.file_name().unwrap(), "session-A.md");
        // No id → latest overall (s2's).
        let got = latest_trusty_mpm_snapshot(tmp.path(), None).unwrap();
        assert_eq!(got.file_name().unwrap(), "session-B.md");
    }

    #[test]
    fn latest_snapshot_falls_back() {
        let tmp = TempDir::new().unwrap();
        let sdir = tmp.path().join(".trusty-mpm").join("sessions");
        fs::create_dir_all(&sdir).unwrap();
        // No log at all → mtime scan of session-*.md.
        write_file(&sdir, "session-20260101-000000.md", "## Summary\nold");
        std::thread::sleep(std::time::Duration::from_millis(10));
        write_file(&sdir, "session-20260202-000000.md", "## Summary\nnew");
        let got = latest_trusty_mpm_snapshot(tmp.path(), Some("unknown-id")).unwrap();
        assert_eq!(got.file_name().unwrap(), "session-20260202-000000.md");
        // Missing project dir → None.
        assert!(latest_trusty_mpm_snapshot(&tmp.path().join("nope"), None).is_none());
    }

    #[test]
    fn render_empty_returns_no_sessions_message() {
        let output = render_resume_context(&[]);
        assert!(
            output.contains("No paused sessions"),
            "empty renders a notice"
        );
    }
}
