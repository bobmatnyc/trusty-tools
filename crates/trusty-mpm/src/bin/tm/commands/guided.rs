//! Bare `tm` guided default mode (#1708).
//!
//! Why: typing `tm` alone — with no subcommand — should do the most useful
//! thing for the operator's current context rather than printing help and
//! exiting. Inside a git repository with a GitHub remote, the daemon's
//! in-project spawn path (#1706) is the right action: it either reconnects to
//! an existing session for the same project (#1707) or creates a fresh
//! per-session worktree, then attaches the terminal to it. Outside a git repo
//! (or when no GitHub remote is found), falling back to `tm launch` (the full
//! framework-deploy path) preserves existing behaviour for non-git directories.
//!
//! What: [`run_guided_default`] detects the current directory, queries the
//! managed spawn endpoint, and then attaches the terminal to the resulting
//! tmux session. A reconnect is fully transparent — the operator sees the same
//! `tmux attach-session -t <name>` outcome whether the session is new or reused.
//!
//! Test: the happy-path and fallback branches are unit-tested in
//! `tests_behavior_a.rs`; the daemon interaction is exercised via the managed
//! spawn integration tests.

use anyhow::Context as _;
use serde::Deserialize;

/// Response shape for `POST /api/v1/sessions/managed` (subset we need).
#[derive(Debug, Deserialize)]
struct SpawnManagedResponse {
    #[serde(default)]
    name: String,
    #[serde(default)]
    state: String,
}

/// Bare `tm` guided default — detect context and spawn or reconnect (#1708).
///
/// Why: operators invoke `tm` without a subcommand from inside a project
/// directory; the guided default eliminates the need to remember whether to
/// type `tm launch`, `tm connect`, `tm sessions new`, or a managed-route
/// command — the daemon decides based on the directory's git identity.
/// What: (1) resolves the current working directory; (2) if the daemon is
/// reachable, posts `POST /api/v1/sessions/managed` with `repo_url = <cwd>` —
/// the daemon's in-project spawn path handles reconnect (Active session for the
/// same `owner/repo` is returned immediately) and new-session creation
/// (a per-session worktree is provisioned); (3) attaches to the resulting tmux
/// session name. When the daemon is unreachable or the spawn fails, falls back
/// to the `tm launch` path so the operator always gets a session.
/// Test: `guided_default_falls_back_when_daemon_unreachable` in
/// `tests_behavior_a.rs` (verifies no panic on a refused connection);
/// `guided_default_attaches_on_success` (mocked daemon response).
pub(crate) async fn run_guided_default(client: &reqwest::Client, url: &str) -> anyhow::Result<()> {
    // 1. Resolve the current directory.
    let cwd = std::env::current_dir().context("cannot resolve current directory")?;
    let workdir = cwd.to_string_lossy().to_string();

    eprintln!("tm: no subcommand — using guided default for {workdir}");

    // 2. Try the managed spawn/reconnect via the daemon.
    //    The daemon's in-project spawn path handles all cases:
    //    - Git repo with GitHub remote → reconnect or new worktree session.
    //    - Non-git dir or no GitHub remote → local-path fast path.
    //    We keep the task and git_ref minimal so the daemon picks sensible defaults.
    let resp = client
        .post(format!("{url}/api/v1/sessions/managed"))
        .json(&serde_json::json!({
            "repo_url": workdir,
            "ref": "HEAD",
            "task": "",
        }))
        .send()
        .await;

    match resp {
        Ok(r) if r.status().is_success() => match r.json::<SpawnManagedResponse>().await {
            Ok(body) if !body.name.is_empty() => {
                eprintln!("tm: attaching to session '{}' ({})", body.name, body.state);
                let status = std::process::Command::new("tmux")
                    .args(["attach-session", "-t", &body.name])
                    .status()
                    .context("failed to invoke tmux")?;
                if !status.success() {
                    anyhow::bail!("tmux attach-session exited with failure");
                }
                return Ok(());
            }
            Ok(_) => {
                eprintln!("tm: daemon returned empty session name; falling back to launch");
            }
            Err(e) => {
                eprintln!("tm: failed to parse daemon response ({e}); falling back to launch");
            }
        },
        Ok(r) => {
            eprintln!(
                "tm: daemon returned {} for managed spawn; falling back to launch",
                r.status()
            );
        }
        Err(e) => {
            eprintln!("tm: daemon unreachable ({e}); falling back to tm launch");
        }
    }

    // 3. Fallback: run the classic `tm launch` path (framework deploy + attach).
    super::launch::launch(client, url, None, None).await
}
