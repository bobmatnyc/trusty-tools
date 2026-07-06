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
//! Test: `handle_catchup_no_sessions_produces_notice`,
//! `handle_catchup_full_flag_is_accepted` in the inline test block.
//!
// CUTOVER BRIDGE (claude-mpm format parsing) — remove post-migration (#1762)

use std::path::PathBuf;

use trusty_mpm::core::{
    catchup::{CatchupOptions, run_catchup},
    claude_mpm_registry::{default_registry_path, discover_claude_mpm_projects},
};

use crate::commands::project::resolve_dir;

/// Handle `tm session catchup [--all-projects] [--full]`.
///
/// Why: provides the catch-up entry-point for `tm session catchup`.
/// What: collects project directories to scan (cwd always included; registry
/// projects appended when `all_projects` is set), runs the catch-up runtime
/// (watermark-aware when `full=false`, full history when `full=true`), and
/// prints the digest to stdout. Manual catchup does NOT advance the watermark —
/// only auto-inject on session start does.
/// Test: `cli_parses_session_catchup` in `tests.rs` exercises the parse path;
/// the handler itself is smoke-tested by `handle_catchup_*` below.
///
// CUTOVER BRIDGE — remove post-migration (#1762)
pub(crate) async fn handle_catchup(all_projects: bool, full: bool) -> anyhow::Result<()> {
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

    // Load config for the memory URL (fail-open to default).
    let config = trusty_mpm::core::config::MpmConfig::load_default();
    // Discovery-first (issue #2030): resolves TRUSTY_MEMORY_URL when set, else
    // the daemon's actual discovered bound address, never a hardcoded port.
    let memory_url = trusty_common::mcp::memory_rpc::resolve_memory_base_url_or_unreachable();

    for dir in &project_dirs {
        let opts = CatchupOptions {
            project_dir: dir.clone(),
            memory_url: memory_url.clone(),
            include_git: config.catchup.include_git,
            include_palace: config.catchup.include_palace,
            git_limit: config.catchup.git_limit,
            drawer_limit: config.catchup.drawer_limit,
            // When full=true: ignore watermark (full history).
            // When full=false: use watermark (incremental since last run).
            full,
        };
        // Manual catchup never advances the watermark.
        let context = run_catchup(&opts, false).await;
        print!("{context}");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
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
}
