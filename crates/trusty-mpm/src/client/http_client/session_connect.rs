//! Session launch and connect methods for [`DaemonClient`].
//!
//! Why: `launch_session` and `connect_session` are the two largest methods on
//! [`DaemonClient`] (~170 SLOC combined) and are cohesive enough to live
//! together — both POST a session registration to the daemon then drive tmux to
//! create or attach the actual shell. Isolating them here keeps `mod.rs` under
//! the 500-SLOC production cap while preserving the logical grouping.
//! What: a second `impl DaemonClient` block containing only the two session-
//! lifecycle entry-point methods.
//! Test: `launch_session_errors_when_daemon_unreachable`,
//! `connect_session_errors_when_daemon_unreachable` in `tests.rs`.

use serde::Deserialize;

use super::DaemonClient;

impl DaemonClient {
    /// Launch a fresh Claude Code session in `workdir`.
    ///
    /// Why: the TUI's `/connect <dir>` command is the single entry point for
    /// "connect to or launch a session for a project" — when no session exists
    /// for a directory it must start one, mirroring `tm sessions start`. A
    /// trusty-mpm session is always the `claude` (Claude Code) CLI, never
    /// `claude-mpm`; the trusty-mpm behaviour comes from the custom instructions
    /// (deployed agents + project `CLAUDE.md`) prepared before launch.
    /// What: runs [`crate::core::session_launch::prepare_session`] (deploy
    /// agents + merge `CLAUDE.md`), POSTs `{project, project_path}` to
    /// `/sessions`, then creates a detached tmux session via `tmux new-session`
    /// and starts `claude` in it via `tmux send-keys`. Returns the
    /// daemon-assigned tmux session name. The daemon only registers session
    /// state; the prep and launch (tmux + process) are owned by the client,
    /// exactly as the CLI does it.
    /// Test: `launch_session_errors_when_daemon_unreachable`.
    pub async fn launch_session(&self, workdir: &str) -> anyhow::Result<String> {
        // Prepare the custom instructions Claude Code reads at startup: deploy
        // composed agents to `~/.claude/agents/` and merge the project
        // `CLAUDE.md`. A prep failure is logged but not fatal — the session can
        // still launch with whatever instructions already exist on disk.
        let fw = crate::core::paths::FrameworkPaths::default();
        if let Err(err) =
            crate::core::session_launch::prepare_session(&fw, std::path::Path::new(workdir))
        {
            tracing::warn!(%err, "session pre-launch preparation failed");
        }

        #[derive(Deserialize)]
        struct Body {
            #[serde(default)]
            name: String,
        }
        let url = format!("{}/sessions", self.base);
        let body: Body = self
            .http
            .post(&url)
            .json(&serde_json::json!({
                "project": workdir,
                "project_path": workdir,
            }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        // Build the combined `--append-system-prompt` text (claude-mpm PM
        // instructions + trusty tool-priority block), resolved *for this project
        // directory* so override files under `<workdir>/.trusty-mpm/` take effect
        // (issue #381). Write it to a temp file and pass it via
        // `--append-system-prompt-file` so every launched `claude` is a properly
        // configured PM instance while preserving Claude Code's built-in tool use
        // instructions. The temp file persists because `claude` reads it at
        // startup; it lives in `/tmp` and is superseded by the next launch — no
        // explicit cleanup is performed.
        let prompt =
            crate::core::session_launch::build_system_prompt_for(std::path::Path::new(workdir));
        let claude_cmd = {
            let path = std::env::temp_dir().join(format!(
                "trusty-mpm-system-prompt-{}.txt",
                uuid::Uuid::new_v4()
            ));
            match std::fs::write(&path, &prompt) {
                Ok(()) => format!("claude --append-system-prompt-file {}", path.display()),
                Err(err) => {
                    tracing::warn!(%err, "failed to write system prompt file; launching bare claude");
                    "claude".to_string()
                }
            }
        };

        let new_session = std::process::Command::new("tmux")
            .args(["new-session", "-d", "-s", &body.name, "-c", workdir])
            .status();
        match new_session {
            Ok(status) if status.success() => {
                let send = std::process::Command::new("tmux")
                    .args(["send-keys", "-t", &body.name, &claude_cmd, "Enter"])
                    .status();
                if !matches!(send, Ok(s) if s.success()) {
                    return Err(anyhow::anyhow!(
                        "tmux session {} created but failed to start claude",
                        body.name
                    ));
                }
            }
            Ok(_) | Err(_) => {
                return Err(anyhow::anyhow!(
                    "failed to create tmux session {} in {}",
                    body.name,
                    workdir
                ));
            }
        }
        Ok(body.name)
    }

    /// Connect to — or start — a Claude Code session in `workdir` *without*
    /// running the framework-deployment sequence.
    ///
    /// Why: `tm connect` is the lightweight sibling of `launch_session`. Where
    /// `launch_session` first runs
    /// [`crate::core::session_launch::prepare_session`] to deploy
    /// instructions, agents, and skills into the project, `connect` deliberately
    /// skips all of that — it assumes the framework is already deployed (or that
    /// the operator does not want it touched) and only wants the daemon to know
    /// about the session and the tmux host to be running.
    /// What: POSTs `{project, project_path}` to `/api/v1/sessions/connect`, then
    /// runs `tmux new-session -A` (idempotent — creates the session when absent,
    /// no-ops when it already exists). When the session is freshly created it
    /// starts `claude` in it via `tmux send-keys`; an already-running session is
    /// left untouched. The system-prompt file is still built and passed so a
    /// freshly-started `claude` is a configured PM — that is prompt composition,
    /// not artifact deployment. Returns the daemon-assigned tmux session name.
    /// Test: `connect_session_errors_when_daemon_unreachable`.
    pub async fn connect_session(&self, workdir: &str) -> anyhow::Result<String> {
        #[derive(Deserialize)]
        struct Body {
            #[serde(default)]
            name: String,
        }
        let url = format!("{}/api/v1/sessions/connect", self.base);
        let body: Body = self
            .http
            .post(&url)
            .json(&serde_json::json!({
                "project": workdir,
                "project_path": workdir,
            }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        // Build the `--append-system-prompt` text so a freshly-started `claude`
        // is a configured PM, resolved *for this project directory* so override
        // files under `<workdir>/.trusty-mpm/` take effect (issue #381). This is
        // prompt composition from bundled assets + project overrides, not
        // deployment of agents/skills/hooks into the project — `connect` only
        // skips the latter (`prepare_session`).
        let prompt =
            crate::core::session_launch::build_system_prompt_for(std::path::Path::new(workdir));
        let claude_cmd = {
            let path = std::env::temp_dir().join(format!(
                "trusty-mpm-system-prompt-{}.txt",
                uuid::Uuid::new_v4()
            ));
            match std::fs::write(&path, &prompt) {
                Ok(()) => format!("claude --append-system-prompt-file {}", path.display()),
                Err(err) => {
                    tracing::warn!(%err, "failed to write system prompt file; launching bare claude");
                    "claude".to_string()
                }
            }
        };

        // `tmux new-session -A` is idempotent: it attaches to the session when
        // it already exists and creates it (detached, `-d`) otherwise. The
        // `has-session` probe distinguishes the two so `claude` is started only
        // for a freshly-created session — an already-running one is left alone.
        let already_running = std::process::Command::new("tmux")
            .args(["has-session", "-t", &body.name])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        let new_session = std::process::Command::new("tmux")
            .args(["new-session", "-A", "-d", "-s", &body.name, "-c", workdir])
            .status();
        match new_session {
            Ok(status) if status.success() => {
                if !already_running {
                    let send = std::process::Command::new("tmux")
                        .args(["send-keys", "-t", &body.name, &claude_cmd, "Enter"])
                        .status();
                    if !matches!(send, Ok(s) if s.success()) {
                        return Err(anyhow::anyhow!(
                            "tmux session {} created but failed to start claude",
                            body.name
                        ));
                    }
                }
            }
            Ok(_) | Err(_) => {
                return Err(anyhow::anyhow!(
                    "failed to create tmux session {} in {}",
                    body.name,
                    workdir
                ));
            }
        }
        Ok(body.name)
    }
}
