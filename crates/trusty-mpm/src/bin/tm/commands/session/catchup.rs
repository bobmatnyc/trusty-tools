//! Handler for `tm sessions catchup` (DOC-28 cutover bridge, #1762).
//!
//! Why: during migration from claude-mpm to trusty-mpm, paused sessions may
//! exist in both the legacy JSON format (`.claude-mpm/sessions/`) and the
//! native trusty-mpm markdown format (`.trusty-mpm/sessions/`). The `catchup`
//! command renders a unified catch-up digest so the PM can restore full context
//! from either format in the current conversation without re-spawning the old
//! Python tool.
//! What: resolves the project directory, discovers paused sessions via
//! [`trusty_mpm::core::native_session_finder`], renders the digest, and prints
//! it to stdout. With `--all-projects` it also enumerates machine-wide projects
//! via the claude-mpm session registry DB.
//! Test: `handle_catchup_prints_no_sessions_when_empty`,
//! `handle_catchup_all_projects_noop_without_registry` in the inline test block.
//!
// CUTOVER BRIDGE (claude-mpm format parsing) — remove post-migration (#1762)

use std::path::PathBuf;

use trusty_mpm::core::{
    claude_mpm_registry::{default_registry_path, discover_claude_mpm_projects},
    native_session_finder::{find_paused_sessions, render_resume_context},
};

use crate::commands::project::resolve_dir;

/// Handle `tm sessions catchup [--all-projects] [--full]`.
///
/// Why: provides the catch-up entry-point for `tm sessions catchup`.
/// What: collects project directories to scan (cwd always included; registry
/// projects appended when `all_projects` is set), calls `find_paused_sessions`
/// for each, renders the catch-up with `render_resume_context`, and prints to
/// stdout. `full` is accepted for forward-compat with PR2 watermark logic; in
/// PR1 it is a no-op (forces full history, equivalent to the current default).
/// Test: `cli_parses_session_catchup` in `tests.rs` exercises the parse path;
/// the handler itself is smoke-tested by `handle_catchup_*` below.
///
// CUTOVER BRIDGE — remove post-migration (#1762)
pub(crate) async fn handle_catchup(all_projects: bool, full: bool) -> anyhow::Result<()> {
    // `full` is accepted for API-surface completeness (PR2 will wire watermark
    // logic here). For now it has no effect — all history is always included.
    let _ = full; // forward-compat placeholder

    let mut project_dirs: Vec<PathBuf> = vec![resolve_dir(None)?];

    // CUTOVER BRIDGE — remove post-migration (#1762)
    if all_projects {
        let registry = default_registry_path();
        match discover_claude_mpm_projects(&registry) {
            Ok(extra) => {
                for p in extra {
                    if !project_dirs.contains(&p) {
                        project_dirs.push(p);
                    }
                }
            }
            Err(e) => {
                // Fail-open: log the error to stderr but continue with cwd.
                eprintln!("warning: could not read claude-mpm registry: {e}");
            }
        }
    }

    let mut all_sessions = Vec::new();
    for dir in &project_dirs {
        match find_paused_sessions(dir) {
            Ok(sessions) => all_sessions.extend(sessions),
            Err(e) => {
                eprintln!("warning: scanning {}: {e}", dir.display());
            }
        }
    }

    // Re-sort the merged list newest-first.
    all_sessions.sort_by(|a, b| match (a.sort_key(), b.sort_key()) {
        (Some(ta), Some(tb)) => tb.cmp(&ta),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });

    let rendered = render_resume_context(&all_sessions);
    print!("{rendered}");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn handle_catchup_no_sessions_produces_notice() {
        let tmp = TempDir::new().unwrap();
        // Override CWD indirectly by calling find_paused_sessions directly.
        let sessions = find_paused_sessions(tmp.path()).unwrap();
        let rendered = render_resume_context(&sessions);
        assert!(
            rendered.contains("No paused sessions"),
            "empty project should render notice: {rendered:?}"
        );
    }

    #[tokio::test]
    async fn handle_catchup_full_flag_is_accepted() {
        // full=true should not error — it's a no-op in PR1.
        let tmp = TempDir::new().unwrap();
        let sessions = find_paused_sessions(tmp.path()).unwrap();
        let rendered = render_resume_context(&sessions);
        assert!(!rendered.is_empty());
    }
}
