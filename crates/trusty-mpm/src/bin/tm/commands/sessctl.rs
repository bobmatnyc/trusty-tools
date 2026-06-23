//! `sessctl-run` command handler — Phase-1 gate for the SESSCTL control plane.
//!
//! Why: WI-1 (#1592) requires a `tm sessctl-run [--tmux] <project-id>` command
//! that allocates a `ControlSessionId`, spawns a `SessionActor` via the
//! `SessionRegistry`, and returns the session ID. This is the single gate test
//! that proves the registry + actor + backend plumbing works end-to-end.
//! What: resolves the project's workdir from the daemon's project registry (via
//! HTTP), selects the backend, calls `SessionRegistry::run_session`, and prints
//! the allocated session ID to stdout.
//! Test: `cli_parses_sessctl_run` in tests.rs; integration test exercises the
//! full round-trip when the daemon is running.

use std::path::PathBuf;

use trusty_mpm::control::{BackendKind, RunParams, SessionRegistry};

/// Execute `tm sessctl-run [--tmux] <project-id>`.
///
/// Why: the Phase-1 acceptance criterion (§11 gate for Phase 1) is that
/// `tm sessctl-run <project-id>` and `tm sessctl-run --tmux <project-id>`
/// each spawn a session via the registry. This handler implements that gate.
/// What: resolves `workdir` (falling back to the current directory when the
/// daemon is unreachable), selects the backend, runs the session, and prints
/// the allocated session ID.
/// Test: `cli_parses_sessctl_run`; live integration test requires the daemon.
pub(crate) async fn sessctl_run(
    client: &reqwest::Client,
    url: &str,
    project_id: String,
    use_tmux: bool,
    prompt_file: Option<String>,
) -> anyhow::Result<()> {
    // Resolve the project's working directory from the daemon project registry.
    // Fall back to the current directory so the Phase-1 gate can be exercised
    // without a fully-configured project registry.
    let workdir = resolve_workdir(client, url, &project_id).await;
    let prompt_path = prompt_file.map(PathBuf::from);

    let backend = if use_tmux {
        BackendKind::Tmux
    } else {
        BackendKind::StreamJson
    };

    // Construct a local registry for the Phase-1 demo path.
    // Phase 2 will wire this to the daemon's shared registry via HTTP.
    let registry = SessionRegistry::new();
    let params = RunParams {
        project_id: project_id.clone(),
        workdir,
        backend,
        prompt_file: prompt_path,
        claude_cmd: None,
    };

    let session_id = registry.run_session(params).await?;
    println!("{session_id}");
    Ok(())
}

/// Resolve a project's workdir from the daemon registry, falling back to cwd.
///
/// Why: Phase-1 gate should work even when the daemon is not running; using
/// the current directory as a fallback lets the gate test run in the project's
/// source tree.
/// What: attempts `GET /projects/<project-id>` on the daemon; on any failure
/// (unreachable, not found) logs a warning and returns the current directory.
/// Test: fallback path is exercised by `sessctl_run_cwd_fallback` in tests.rs.
async fn resolve_workdir(
    client: &reqwest::Client,
    url: &str,
    project_id: &str,
) -> PathBuf {
    #[derive(serde::Deserialize)]
    struct ProjectRow {
        workdir: String,
    }

    match client
        .get(format!("{url}/projects/{project_id}"))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            if let Ok(row) = resp.json::<ProjectRow>().await {
                return PathBuf::from(row.workdir);
            }
        }
        Ok(resp) => {
            eprintln!(
                "warning: daemon returned {} for project '{project_id}'; using cwd",
                resp.status()
            );
        }
        Err(e) => {
            eprintln!("warning: daemon unreachable ({e}); using cwd as workdir");
        }
    }

    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}
