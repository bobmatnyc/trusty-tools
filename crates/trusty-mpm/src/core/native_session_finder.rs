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

use crate::core::claude_mpm_session::{ClaudeMpmSession, load_all_claude_mpm_sessions};

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
    if tm_sessions_dir.is_dir() && let Ok(rd) = std::fs::read_dir(&tm_sessions_dir) {
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
        } => {
            out.push_str("**Format:** trusty-mpm (native)\n");
            if let Some(ts) = paused_at {
                out.push_str(&format!("**Paused At:** {ts}\n"));
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
    use crate::core::claude_mpm_session::ClaudeMpmSession;
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
    fn render_empty_returns_no_sessions_message() {
        let output = render_resume_context(&[]);
        assert!(
            output.contains("No paused sessions"),
            "empty renders a notice"
        );
    }
}
