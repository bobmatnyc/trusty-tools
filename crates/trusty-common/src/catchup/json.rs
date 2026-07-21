//! Structured (JSON) catch-up digest — the `session_context_catchup` MCP
//! tool's core payload.
//!
//! Why: split out of `catchup/mod.rs` to keep it under the 500-SLOC
//! production cap once this JSON sibling of [`super::generate_catchup_context`]
//! pushed the parent file over the line. Kept in the `catchup` module (not a
//! standalone crate concept) since it reuses the exact same three sources —
//! paused sessions, git commits, palace drawers — `generate_catchup_context`
//! does.
//! What: [`PausedSessionJson`] / [`RecentMemoryJson`] / [`CatchupJson`] are the
//! structured output types; [`generate_catchup_json`] is the entry point,
//! mirroring [`super::generate_catchup_context`]'s watermark-filtering logic
//! but building typed values instead of markdown.
//! Test: `generate_catchup_json_returns_structured_fields`,
//! `generate_catchup_json_respects_watermark`,
//! `paused_session_to_json_maps_trusty_mpm_fields`,
//! `paused_session_to_json_maps_claude_mpm_fields`.

use chrono::{DateTime, Utc};
use serde::Serialize;

use super::CatchupOptions;
use super::derive_palace_id_for;
use super::git::{CommitSummary, git_commits_since};
use super::palace::fetch_recent_palace_drawers;
use super::session_finder::{PausedSession, find_paused_sessions};
use super::state::load_catchup_state;

/// A single paused session, restructured as JSON fields instead of markdown
/// prose (MCP `session_context_catchup` tool).
///
/// Why: an MCP tool caller needs typed fields it can branch on (e.g. "does this
/// session have a tmux window to realign to?") rather than parsing a rendered
/// digest back apart. This is the structured sibling of
/// [`session_finder::render_session`] (private) — same source fields, JSON
/// shape instead of markdown.
/// What: `format` is `"trusty-mpm"` or `"claude-mpm"`; the remaining fields are
/// populated from whichever [`PausedSession`] variant produced them. A
/// `claude-mpm` (legacy) session has no on-disk single-file `source_file` (its
/// loader discards the path — see [`mpm_session::load_all_claude_mpm_sessions`]),
/// so that field is `None` for that variant; its `in_progress`/`next_steps`
/// are best-effort folds of `todos`+`task_list` / `open_questions`.
/// Test: `paused_session_to_json_maps_trusty_mpm_fields`,
/// `paused_session_to_json_maps_claude_mpm_fields`.
#[derive(Debug, Clone, Serialize)]
pub struct PausedSessionJson {
    /// `"trusty-mpm"` (native markdown) or `"claude-mpm"` (legacy JSON).
    pub format: String,
    /// UTC pause timestamp, when parseable.
    pub paused_at: Option<DateTime<Utc>>,
    /// The session's summary / resume-instructions text.
    pub summary: String,
    /// In-progress work, when recorded.
    pub in_progress: Option<String>,
    /// Recommended next steps, when recorded.
    pub next_steps: Option<String>,
    /// Git branch/commit/status context captured at pause time.
    pub git_context: Option<String>,
    /// `session_name:window_index:window_id`, trusty-mpm sessions only.
    pub tmux_window: Option<String>,
    /// Absolute path to the on-disk snapshot file, trusty-mpm sessions only.
    pub source_file: Option<String>,
}

/// Map a [`PausedSession`] to its structured JSON form.
///
/// Why: isolates the per-variant field mapping so [`generate_catchup_json`]
/// stays a straight orchestration function.
/// What: see [`PausedSessionJson`] field docs for the exact mapping.
/// Test: `paused_session_to_json_maps_trusty_mpm_fields`,
/// `paused_session_to_json_maps_claude_mpm_fields`.
fn paused_session_to_json(session: &PausedSession) -> PausedSessionJson {
    match session {
        PausedSession::TrustyMpm {
            path,
            paused_at,
            summary,
            git_context,
            in_progress,
            next_steps,
            tmux_window,
        } => PausedSessionJson {
            format: "trusty-mpm".to_string(),
            paused_at: *paused_at,
            summary: summary.clone(),
            in_progress: in_progress.clone(),
            next_steps: next_steps.clone(),
            git_context: git_context.clone(),
            tmux_window: tmux_window.clone(),
            source_file: Some(path.display().to_string()),
        },
        PausedSession::ClaudeMpm { session: s } => {
            let in_progress = {
                let mut items: Vec<String> = Vec::new();
                if let Some(t) = &s.todos {
                    items.extend(t.iter().cloned());
                }
                if let Some(t) = &s.task_list {
                    items.extend(t.iter().cloned());
                }
                if items.is_empty() {
                    None
                } else {
                    Some(items.join("\n"))
                }
            };
            PausedSessionJson {
                format: "claude-mpm".to_string(),
                paused_at: s
                    .paused_at
                    .as_deref()
                    .and_then(|ts| ts.parse::<DateTime<Utc>>().ok()),
                summary: s.resume_instructions.clone().unwrap_or_default(),
                in_progress,
                next_steps: s
                    .open_questions
                    .as_ref()
                    .filter(|q| !q.is_empty())
                    .map(|q| q.join("\n")),
                git_context: s.git_context.clone(),
                tmux_window: None,
                source_file: None,
            }
        }
    }
}

/// A recent memory-palace drawer, trimmed to the fields the catch-up JSON
/// output surfaces.
///
/// Why: [`palace::DrawerSummary`] also carries `created_at`, which the digest
/// doesn't need once drawers are already newest-first; keeping the tool output
/// to `title`/`tags` matches the documented `session_context_catchup` schema.
/// What: a two-field projection of [`palace::DrawerSummary`].
/// Test: covered by `generate_catchup_json_returns_structured_fields`.
#[derive(Debug, Clone, Serialize)]
pub struct RecentMemoryJson {
    /// The drawer's stored content / title.
    pub title: String,
    /// Tags associated with the drawer.
    pub tags: Vec<String>,
}

/// Structured (non-markdown) catch-up digest — the `session_context_catchup`
/// MCP tool's core payload.
///
/// Why: the MCP tool contract is JSON, not prose; this is the JSON sibling of
/// [`super::generate_catchup_context`]'s markdown string, built from the exact
/// same three sources so both surfaces stay in lockstep.
/// What: `sessions` (structured paused-session records), `recent_commits`
/// (unchanged [`git::CommitSummary`] values), and `recent_memory` (title+tags
/// drawer projections). Callers (the MCP daemon backend) layer
/// `resolved_snapshot` and `watermark_advanced` on top, since those depend on
/// an explicit `session_id` this function does not take.
/// Test: `generate_catchup_json_returns_structured_fields`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct CatchupJson {
    /// Paused sessions since the watermark (or all, when `full`), newest-first.
    pub sessions: Vec<PausedSessionJson>,
    /// Recent git commits since the watermark (or the last `git_limit`).
    pub recent_commits: Vec<CommitSummary>,
    /// Recent memory-palace drawers since the watermark.
    pub recent_memory: Vec<RecentMemoryJson>,
}

/// Generate a structured (JSON) catch-up digest for the given options.
///
/// Why: the `session_context_catchup` MCP tool needs typed fields, not a
/// rendered markdown block — this is [`super::generate_catchup_context`]
/// restructured to build [`CatchupJson`] instead of a `String`, reusing the
/// exact same three sources (paused sessions, git commits, palace drawers) and
/// the exact same watermark-filtering logic.
/// What: mirrors [`super::generate_catchup_context`]'s watermark load /
/// per-source collection, but maps each source into structured values instead
/// of markdown lines. Never persists anything — like `generate_catchup_context`,
/// advancing the watermark is exclusively [`super::run_catchup`]'s job, and
/// this function has no equivalent wrapper: callers that need JSON MUST NOT
/// advance the watermark (the `session_context_catchup` MCP tool is a
/// manual-peek operation, same contract as `tm session catchup`).
/// Test: `generate_catchup_json_returns_structured_fields`,
/// `generate_catchup_json_respects_watermark`.
pub async fn generate_catchup_json(opts: &CatchupOptions) -> CatchupJson {
    let palace_id = derive_palace_id_for(&opts.project_dir);

    let watermark: Option<DateTime<Utc>> = if opts.full {
        None
    } else {
        load_catchup_state(&palace_id).map(|s| s.last_catchup_at)
    };

    let sessions = match find_paused_sessions(&opts.project_dir) {
        Ok(sessions) => {
            let sessions: Vec<_> = if let Some(wm) = watermark {
                sessions
                    .into_iter()
                    .filter(|s| s.sort_key().is_none_or(|ts| ts > wm))
                    .collect()
            } else {
                sessions
            };
            sessions.iter().map(paused_session_to_json).collect()
        }
        Err(e) => {
            eprintln!("catchup: warning: could not scan paused sessions: {e}");
            Vec::new()
        }
    };

    let recent_commits = if opts.include_git {
        git_commits_since(&opts.project_dir, watermark)
            .into_iter()
            .take(opts.git_limit)
            .collect()
    } else {
        Vec::new()
    };

    let recent_memory = if opts.include_palace {
        match fetch_recent_palace_drawers(
            &opts.memory_url,
            &palace_id,
            opts.drawer_limit,
            watermark,
        )
        .await
        {
            Some(drawers) => drawers
                .iter()
                .map(|d| RecentMemoryJson {
                    title: d.title.clone(),
                    tags: d.tags.clone(),
                })
                .collect(),
            None => Vec::new(),
        }
    } else {
        Vec::new()
    };

    CatchupJson {
        sessions,
        recent_commits,
        recent_memory,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catchup::CatchupOptions;
    use crate::catchup::state::{CatchupState, save_catchup_state};
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn init_git_repo(tmp: &TempDir) {
        let p = tmp.path();
        std::process::Command::new("git")
            .arg("-C")
            .arg(p)
            .args(["init"])
            .output()
            .unwrap();
        std::process::Command::new("git")
            .arg("-C")
            .arg(p)
            .args(["config", "user.email", "t@t.com"])
            .output()
            .unwrap();
        std::process::Command::new("git")
            .arg("-C")
            .arg(p)
            .args(["config", "user.name", "T"])
            .output()
            .unwrap();
        fs::write(p.join("README.md"), b"test").unwrap();
        std::process::Command::new("git")
            .arg("-C")
            .arg(p)
            .args(["add", "."])
            .output()
            .unwrap();
        std::process::Command::new("git")
            .arg("-C")
            .arg(p)
            .args(["commit", "-m", "init"])
            .output()
            .unwrap();
    }

    #[tokio::test]
    async fn generate_catchup_json_returns_structured_fields() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(&tmp);

        // Also seed a native trusty-mpm paused session so the sessions array
        // is exercised, not just commits.
        let sessions_dir = tmp.path().join(".trusty-mpm").join("sessions");
        fs::create_dir_all(&sessions_dir).unwrap();
        fs::write(
            sessions_dir.join("session-20260627-100000.md"),
            "## Summary\nDid the thing.\n\n## Next Steps\nShip it.\n",
        )
        .unwrap();

        let opts = CatchupOptions {
            project_dir: tmp.path().to_path_buf(),
            memory_url: "http://127.0.0.1:19999".to_string(),
            include_git: true,
            include_palace: true,
            git_limit: 50,
            drawer_limit: 15,
            full: true,
        };

        let json = generate_catchup_json(&opts).await;

        assert_eq!(json.sessions.len(), 1, "fixture snapshot should be found");
        assert_eq!(json.sessions[0].format, "trusty-mpm");
        assert_eq!(json.sessions[0].summary, "Did the thing.");
        assert_eq!(json.sessions[0].next_steps.as_deref(), Some("Ship it."));
        assert!(
            json.sessions[0].source_file.is_some(),
            "trusty-mpm sessions carry a source_file"
        );

        assert!(
            json.recent_commits.iter().any(|c| c.msg == "init"),
            "commit summary should be structured, not prose: {:?}",
            json.recent_commits
        );

        // Palace daemon unreachable (fail-open) → empty, not an error.
        assert!(json.recent_memory.is_empty());
    }

    #[tokio::test]
    async fn generate_catchup_json_respects_watermark() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(&tmp);

        // A far-future watermark should exclude every commit/session.
        let palace_id = derive_palace_id_for(tmp.path());
        let state = CatchupState {
            last_catchup_at: "2099-01-01T00:00:00Z".parse().unwrap(),
            palace_id: palace_id.clone(),
            last_git_sha: None,
        };
        save_catchup_state(&palace_id, &state).unwrap();

        let opts = CatchupOptions {
            project_dir: tmp.path().to_path_buf(),
            memory_url: "http://127.0.0.1:19999".to_string(),
            include_git: true,
            include_palace: false,
            git_limit: 50,
            drawer_limit: 15,
            // full=false → the watermark above must apply.
            full: false,
        };

        let json = generate_catchup_json(&opts).await;
        assert!(
            json.recent_commits.is_empty(),
            "future watermark should exclude all commits: {:?}",
            json.recent_commits
        );
    }

    #[test]
    fn paused_session_to_json_maps_trusty_mpm_fields() {
        let session = PausedSession::TrustyMpm {
            path: PathBuf::from("/tmp/session-1.md"),
            paused_at: None,
            summary: "Summary text".to_string(),
            git_context: Some("branch: main".to_string()),
            in_progress: Some("doing X".to_string()),
            next_steps: Some("do Y".to_string()),
            tmux_window: Some("main:2:@7".to_string()),
        };
        let json = paused_session_to_json(&session);
        assert_eq!(json.format, "trusty-mpm");
        assert_eq!(json.summary, "Summary text");
        assert_eq!(json.git_context.as_deref(), Some("branch: main"));
        assert_eq!(json.tmux_window.as_deref(), Some("main:2:@7"));
        assert_eq!(json.source_file.as_deref(), Some("/tmp/session-1.md"));
    }

    #[test]
    fn paused_session_to_json_maps_claude_mpm_fields() {
        use crate::catchup::mpm_session::ClaudeMpmSession;
        let session = ClaudeMpmSession {
            resume_instructions: Some("Resume from step 3".to_string()),
            todos: Some(vec!["todo 1".to_string()]),
            open_questions: Some(vec!["q1".to_string()]),
            git_context: Some("branch: main".to_string()),
            ..Default::default()
        };
        let json = paused_session_to_json(&PausedSession::ClaudeMpm { session });
        assert_eq!(json.format, "claude-mpm");
        assert_eq!(json.summary, "Resume from step 3");
        assert_eq!(json.in_progress.as_deref(), Some("todo 1"));
        assert_eq!(json.next_steps.as_deref(), Some("q1"));
        assert!(json.tmux_window.is_none());
        assert!(json.source_file.is_none());
    }
}
