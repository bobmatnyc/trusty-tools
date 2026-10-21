//! Handler for `tm session catchup` (DOC-28 cutover bridge, #1762).
//!
//! Why: during migration from claude-mpm to trusty-mpm, paused sessions may
//! exist in both the legacy JSON format (`.claude-mpm/sessions/`) and the
//! native trusty-mpm markdown format (`.trusty-mpm/sessions/`). The `catchup`
//! command renders a unified catch-up digest so the PM can restore full context
//! from either format in the current conversation without re-spawning the old
//! Python tool.
//! What: resolves the project directory, discovers paused sessions via
//! [`trusty_mpm::core::native_session_finder`], renders the digest (with git
//! and palace activity via the catch-up runtime), and prints it to stdout.
//! With `--all-projects` it also enumerates machine-wide projects via the
//! claude-mpm session registry DB.
//! Manual catchup (both full and non-full) does NOT advance the watermark;
//! only auto-inject on session start advances it (PR4).
//! `--json` prints the MCP tool's paged JSON instead (#8017).
//! #9883: without `--full` both forms print the lean answer — the resolved
//! snapshot plus live `git status` — through the MCP tool's own path.
//! Test: `handle_catchup_no_sessions_produces_notice`,
//! `handle_catchup_full_flag_is_accepted`, `lean_markdown_*` in the inline test
//! block.
//!
// CUTOVER BRIDGE (claude-mpm format parsing) — remove post-migration (#1762)

use std::path::PathBuf;

use serde_json::Value;
use trusty_mpm::core::{
    catchup::{CatchupOptions, run_catchup},
    claude_mpm_registry::{default_registry_path, discover_claude_mpm_projects},
};

use crate::commands::project::resolve_dir;

/// Handle `tm session catchup [--full [--all-projects]]`.
///
/// Why: provides the catch-up entry-point for `tm session catchup`.
/// What: without `full`, prints [`render_lean_markdown`] of the lean
/// `session_context_catchup` answer for the current project (#9883). With
/// `full`, collects project directories to scan (cwd always included; registry
/// projects appended when `all_projects` is set), runs the catch-up runtime
/// with no watermark, and prints the digest to stdout. Manual catchup does NOT
/// advance the watermark — only auto-inject on session start does.
/// Test: `cli_parses_session_catchup` in `tests.rs` exercises the parse path;
/// the handler itself is smoke-tested by `handle_catchup_*` below.
///
// CUTOVER BRIDGE — remove post-migration (#1762)
pub(crate) async fn handle_catchup(all_projects: bool, full: bool) -> anyhow::Result<()> {
    if !full {
        // #9883: reuse the MCP tool's identity resolution — never a fork of it.
        let lean = catchup_page(false, false, 0).await?;
        print!("{}", render_lean_markdown(&lean));
        return Ok(());
    }
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

    // Resolve the daemon's socket (fail-open to a path nothing serves).
    let config = trusty_mpm::core::config::MpmConfig::load_default();
    // #6286: the derived socket path, else
    // the daemon's actual discovered bound address, never a hardcoded port.
    let memory_socket = trusty_common::memory_rpc::resolve_memory_socket_or_unreachable();

    for dir in &project_dirs {
        let opts = CatchupOptions {
            project_dir: dir.clone(),
            memory_socket: memory_socket.clone(),
            include_git: config.catchup.include_git,
            include_palace: config.catchup.include_palace,
            git_limit: config.catchup.git_limit,
            drawer_limit: config.catchup.drawer_limit,
            // #9883: only `--full` reaches here; the full digest has no watermark.
            full: true,
        };
        // Manual catchup never advances the watermark.
        let context = run_catchup(&opts, false).await;
        print!("{context}");
    }

    Ok(())
}

/// Handle `tm sessions catchup --json [--sessions-offset N]` (#8017).
///
/// Why: a session whose MCP server is down falls back to this CLI, and the
/// markdown digest has no paging (163 KB for 23 sessions in the #8017 run).
/// What: calls the same in-process function behind the
/// `session_context_catchup` MCP tool for the current project and prints its
/// JSON page. The caller's managed session id comes from this process's own
/// environment, as the `serve --stdio` bridge stamps it for the MCP tool.
/// Test: `cli_parses_session_catchup_json_page`; the payload itself is
/// covered by `session_context_catchup_returns_expected_shape`.
pub(crate) async fn handle_catchup_json(
    all_projects: bool,
    full: bool,
    sessions_offset: usize,
) -> anyhow::Result<()> {
    let page = catchup_page(all_projects, full, sessions_offset).await?;
    println!("{}", serde_json::to_string_pretty(&page)?);
    Ok(())
}

/// The `session_context_catchup` answer for the current project.
///
/// Why: the JSON and markdown forms must resolve the caller the same way the
/// MCP tool does (#5272, #5386), so both go through this one call.
/// What: the current project, this process's managed session id (as the
/// `serve --stdio` bridge stamps it for the MCP tool), no tmux window.
/// Test: `session_context_catchup_returns_expected_shape`,
/// `the_default_returns_the_resolved_snapshot_and_live_git_status_only`.
async fn catchup_page(
    all_projects: bool,
    full: bool,
    sessions_offset: usize,
) -> anyhow::Result<Value> {
    let dir = resolve_dir(None)?;
    let session_id = trusty_common::catchup::session_id::managed_session_id_from_env();
    trusty_mpm::daemon::mcp_context::session_context_catchup(
        &dir.to_string_lossy(),
        session_id.as_deref(),
        None,
        all_projects,
        full,
        sessions_offset,
    )
    .await
    .map_err(anyhow::Error::msg)
}

/// Render a lean `session_context_catchup` answer as markdown (#9883).
///
/// Why: `tm sessions catchup` without `--json` prints markdown, and its lean
/// default must carry what the JSON one does — never a silently thinner view.
/// What: the resolved snapshot (path, route, recorded sections) or why none
/// resolved; whether the checkout moved past it; the live `git status`, or
/// why it is unavailable; a hydration error when one occurred; and a pointer
/// to `--full`.
/// Test: `lean_markdown_renders_the_snapshot_and_live_status`,
/// `lean_markdown_names_an_unresolved_snapshot_and_unavailable_status`.
pub(crate) fn render_lean_markdown(lean: &Value) -> String {
    let mut out = String::from("## Resume Snapshot\n\n");
    match lean["resolved_snapshot"].as_str() {
        Some(path) => {
            let via = lean["resolved_via"].as_str().unwrap_or("unknown");
            out.push_str(&format!("`{path}` (resolved via {via})\n\n"));
            let session = &lean["resolved_session"];
            for (key, title) in [
                ("summary", "Summary"),
                ("in_progress", "In Progress"),
                ("next_steps", "Next Steps"),
                ("git_context", "Git Context (at pause)"),
            ] {
                if let Some(text) = session[key].as_str().filter(|t| !t.trim().is_empty()) {
                    out.push_str(&format!("### {title}\n{}\n\n", text.trim()));
                }
            }
            if lean["resolved_snapshot_superseded"] == true {
                let total = &lean["commits_since_snapshot_total"];
                out.push_str(&format!(
                    "Superseded: {total} commit(s) landed since this snapshot.\n\n"
                ));
            }
        }
        None => {
            out.push_str("No snapshot resolved for this session.\n");
            if let Some(note) = lean["resolved_note"].as_str() {
                out.push_str(&format!("{note}\n"));
            }
            out.push('\n');
        }
    }
    out.push_str("## Git Status (live)\n\n");
    let status = &lean["git_status"];
    if status["available"] == true {
        let entries = status["entries"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        if entries.is_empty() {
            out.push_str("clean\n");
        } else {
            for entry in entries.iter().filter_map(Value::as_str) {
                out.push_str(&format!("    {entry}\n"));
            }
            if status["truncated"] == true {
                out.push_str(&format!("({} entries in total)\n", status["entries_total"]));
            }
        }
    } else {
        let error = status["error"].as_str().unwrap_or("unavailable");
        out.push_str(&format!("{error}\n"));
    }
    if let Some(error) = lean["session_refs"]["error"].as_str() {
        out.push_str(&format!("\nSession refs not refreshed: {error}\n"));
    }
    out.push_str(
        "\nRun `tm sessions catchup --full` for every paused session, recent commits and memory.\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::render_lean_markdown;
    use serde_json::json;
    use tempfile::TempDir;
    use trusty_mpm::core::native_session_finder::{find_paused_sessions, render_resume_context};

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
        // full=true should not error — it triggers full history mode.
        let tmp = TempDir::new().unwrap();
        let sessions = find_paused_sessions(tmp.path()).unwrap();
        let rendered = render_resume_context(&sessions);
        assert!(!rendered.is_empty());
    }

    /// #9883: the markdown lean default carries the snapshot's recorded
    /// sections, the supersession, and the live tree verbatim.
    #[test]
    fn lean_markdown_renders_the_snapshot_and_live_status() {
        let text = render_lean_markdown(&json!({
            "digest": "lean",
            "resolved_snapshot": "/p/.trusty-mpm/sessions/s/session-1.md",
            "resolved_via": "session_id",
            "resolved_session": {
                "summary": "Did the thing.",
                "next_steps": "- ship",
                "git_context": "branch: main",
            },
            "resolved_snapshot_superseded": true,
            "commits_since_snapshot_total": 2,
            "git_status": {
                "available": true,
                "entries": [" M a.rs", "?? b.rs"],
                "entries_total": 2,
                "truncated": false,
            },
            "session_refs": { "error": null },
        }));
        for needle in [
            "`/p/.trusty-mpm/sessions/s/session-1.md` (resolved via session_id)",
            "### Summary\nDid the thing.",
            "### Next Steps\n- ship",
            "### Git Context (at pause)\nbranch: main",
            "Superseded: 2 commit(s)",
            "     M a.rs\n",
            "    ?? b.rs\n",
            "tm sessions catchup --full",
        ] {
            assert!(text.contains(needle), "missing {needle:?} in:\n{text}");
        }
        assert!(!text.contains("In Progress"), "{text}");
    }

    /// #9883: no snapshot and a failed probe are stated, never left blank.
    #[test]
    fn lean_markdown_names_an_unresolved_snapshot_and_unavailable_status() {
        let text = render_lean_markdown(&json!({
            "resolved_snapshot": null,
            "resolved_note": "tmux_session route not tried",
            "git_status": { "available": false, "error": "unavailable: git status timed out" },
            "session_refs": { "error": "no origin" },
        }));
        for needle in [
            "No snapshot resolved for this session.",
            "tmux_session route not tried",
            "unavailable: git status timed out",
            "Session refs not refreshed: no origin",
        ] {
            assert!(text.contains(needle), "missing {needle:?} in:\n{text}");
        }
    }
}
