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
        /// Timestamp parsed from the `session-YYYYMMDD-HHMMSS.md` filename,
        /// falling back to the file's mtime when the filename carries no
        /// parseable stamp. `None` only when the filesystem cannot supply a
        /// modification time — and such a record is excluded from every
        /// watermark-filtered digest (see [`filter_sessions_since`], #5072).
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

/// Retain only the sessions that provably paused after `watermark`.
///
/// Why: this predicate previously lived, copy-pasted, in both
/// [`crate::catchup::generate_catchup_context`] and
/// [`crate::catchup::generate_catchup_json`] as
/// `s.sort_key().is_none_or(|ts| ts > wm)`. `is_none_or` yields `true` for an
/// unknown key, so a session whose pause instant could not be derived was
/// admitted by EVERY watermark, forever, while every session that could be
/// dated was correctly filtered out. On a real project that inverted the
/// digest: the one undatable record survived and dozens of newer well-formed
/// snapshots did not (#5072). "Sessions since T" is not a claim an undatable
/// record can satisfy, so it is now excluded — and the exclusion is logged
/// rather than silent, because a dropped record is a real (if bounded) loss.
/// What: with no watermark (a `full` catch-up) returns `sessions` untouched.
/// With a watermark, keeps only sessions whose [`PausedSession::sort_key`] is
/// `Some` and strictly greater, warning on stderr for each drop.
/// Test: `filter_sessions_since_drops_undatable_session`,
/// `filter_sessions_since_keeps_everything_without_watermark`,
/// `generate_catchup_json_excludes_undatable_session_behind_watermark`.
pub fn filter_sessions_since(
    sessions: Vec<PausedSession>,
    watermark: Option<DateTime<Utc>>,
) -> Vec<PausedSession> {
    let Some(wm) = watermark else {
        return sessions;
    };
    sessions
        .into_iter()
        // #5072: fail-closed — an undatable session cannot be shown to postdate
        // the watermark, so it is dropped instead of admitted unconditionally.
        .filter(|s| match s.sort_key() {
            Some(ts) => ts > wm,
            None => {
                eprintln!(
                    "catchup: warning: dropping a paused session with no derivable \
                     pause timestamp from the since-{wm} digest; re-run with \
                     full=true to see it"
                );
                false
            }
        })
        .collect()
}

/// Read a file's modification time as a UTC instant.
///
/// Why: the fallback source for [`PausedSession::TrustyMpm::paused_at`] when a
/// snapshot's filename carries no `YYYYMMDD-HHMMSS` stamp (#5072).
/// What: `metadata().modified()`, converted to `DateTime<Utc>`; `None` when the
/// platform or filesystem cannot supply one.
/// Test: `parse_falls_back_to_mtime_when_filename_lacks_timestamp`.
fn mtime_utc(path: &Path) -> Option<DateTime<Utc>> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()
        .map(DateTime::<Utc>::from)
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
    // #5072: hand-written snapshots (`session-20260730-bounce.md`) carry no
    // parseable stamp; fall back to the file's mtime so the record is still
    // datable — an undatable one is dropped by `filter_sessions_since`.
    let paused_at = path
        .file_stem()
        .and_then(|s| s.to_str())
        .and_then(|s| s.strip_prefix("session-"))
        .and_then(parse_filename_timestamp)
        .or_else(|| mtime_utc(path));

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
/// What: returns the trimmed content between the end of the `## <header>`
/// LINE and the next `## ` header or end-of-file. Returns `None` when the
/// section is absent or empty.
///
/// Trailing text on the header line (e.g. a hand-written `## Next Steps
/// (all Bob's call — none required)`) is skipped along with the rest of that
/// line rather than being rejected outright. An earlier version of this
/// function rejected any header with trailing text and returned `None` for
/// the whole section — that is a WORSE bug than the one it was meant to fix:
/// annotated headers (`## In Progress (BACKGROUND AGENTS DIE AT PAUSE — …)`,
/// `## Next Steps (RESUME PLAYBOOK)`, `## Completed (this leg)`) were this
/// project's own standard hand-written convention for weeks before
/// `pause.rs`'s native writer existed, not an isolated one-off. Simulating
/// the fail-closed version against this project's own 50-file
/// `.trusty-mpm/sessions/*.md` archive (07-06 through 07-24) regressed 64 of
/// 300 section extractions from present content to `None` — and
/// `render_session` silently *omits* a `None` section with no warning, so a
/// resume digest would misreport "nothing in progress" for sessions that
/// recorded substantial unfinished work. Skipping just the header's own
/// line — regardless of its trailer — fixes the original leak (the trailing
/// annotation text no longer ends up prepended to the body) without losing
/// content from any annotated historical header.
/// Test: `extract_section_finds_content`,
/// `extract_section_strips_trailing_header_annotation_from_body`,
/// `extract_section_allows_trailing_whitespace_on_header_line`,
/// `extract_section_first_occurrence_wins_when_header_repeated`,
/// `extract_section_handles_representative_corpus_header_styles`.
fn extract_section(text: &str, header: &str) -> Option<String> {
    let needle = format!("## {header}");
    let start = text.find(&needle)?;
    let after_needle = start + needle.len();
    let rest = &text[after_needle..];
    // Skip past the rest of the header LINE (through its own newline, if
    // any) so trailing annotation text never leaks into the captured body.
    let line_end = rest.find('\n').map(|i| i + 1).unwrap_or(rest.len());
    let body = &rest[line_end..];
    let end = body.find("\n## ").unwrap_or(body.len());
    let section = body[..end].trim().to_owned();
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
#[path = "session_finder_tests.rs"]
mod tests;
