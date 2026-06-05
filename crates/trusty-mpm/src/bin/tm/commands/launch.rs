//! `launch` and `connect` command handlers.
//!
//! Why: these two commands manage the full session lifecycle (deploy + tmux +
//! attach) and are complex enough to warrant a dedicated file.
//! What: `launch` (deploy + register + tmux + attach), `connect` (register
//! only + idempotent tmux + attach), plus `find_existing_session` helper.
//! Test: `cli_parses_launch`, `cli_parses_connect`, `cli_parses_launch_with_dir`,
//! `cli_parses_connect_with_dir` in `tests.rs`.

use serde::Deserialize;

use crate::commands::project::resolve_dir;
use crate::formatters::banner::{
    fallback_session_name, normalize_workdir, print_launch_banner,
    print_launch_banner_reconnecting, tmux_has_session,
};

/// `launch` subcommand — launch a configured claude session and attach to it.
///
/// Why: `tm launch` should reproduce the `claude-mpm` experience — one command
/// that prepares the framework, registers the session, starts `claude` in a
/// tmux host, and hands the current terminal over to it. The user sees a
/// summary banner, then `claude` itself.
/// What: resolves `dir`, runs `prepare_session`, registers the session with the
/// daemon (`POST /sessions`, falling back to a generated name when the daemon
/// is unreachable), writes the system-prompt file, prints the banner, creates a
/// detached tmux session running `claude`, and `attach`es to it (blocking until
/// the user detaches/exits).
/// Test: `cli_parses_launch`, `cli_parses_launch_with_dir`.
pub(crate) async fn launch(
    client: &reqwest::Client,
    url: &str,
    dir: Option<String>,
) -> anyhow::Result<()> {
    // 1. Resolve the target directory (absolute, so the banner is unambiguous).
    //    `resolve_dir` already defaults to the process cwd when `dir` is None.
    let path = resolve_dir(dir)?;
    let path = path.canonicalize().unwrap_or(path);
    let workdir = path.to_string_lossy().to_string();

    // 1b. If a session already exists for this directory and its tmux session
    //     is still alive, reconnect to it instead of launching a new one. A
    //     daemon that is unreachable, or a stale record with no live tmux
    //     session, simply falls through to the normal launch sequence below.
    if let Some(existing) = find_existing_session(client, url, &workdir).await
        && !existing.is_empty()
        && tmux_has_session(&existing)
    {
        print_launch_banner_reconnecting(&workdir, &existing);
        let status = std::process::Command::new("tmux")
            .args(["attach-session", "-t", &existing])
            .status()?;
        if !status.success() {
            anyhow::bail!("tmux attach-session exited with failure");
        }
        return Ok(());
    }

    // 2. Prepare the framework: deploy composed agents and merge CLAUDE.md so a
    //    plain `claude` process behaves as a trusty-mpm PM session. A prep
    //    failure is logged but not fatal — the session can still launch.
    let fw = trusty_mpm::core::paths::FrameworkPaths::default();
    if let Err(err) = trusty_mpm::core::session_launch::prepare_session(&fw, &path) {
        eprintln!("warning: session preparation failed: {err}");
    }

    // 3. Register the session with the daemon. The tmux name is derived from
    //    the project folder (`tmpm-<folder>`); we send it so the daemon's
    //    registry stays consistent with the tmux session we create below. When
    //    the daemon is unreachable we still launch under the same folder name.
    #[derive(Deserialize)]
    struct Body {
        #[serde(default)]
        name: String,
        /// The registered session's id; `None` when the daemon is unreachable.
        id: Option<trusty_mpm::core::session::SessionId>,
    }
    let folder_name = fallback_session_name(&path);
    let (tmux_name, session_id) = match client
        .post(format!("{url}/sessions"))
        .json(&serde_json::json!({
            "project": path,
            "project_path": path,
            "name": folder_name,
        }))
        .send()
        .await
    {
        Ok(resp) => match resp.error_for_status() {
            Ok(resp) => match resp.json::<Body>().await {
                Ok(body) if !body.name.is_empty() => (body.name, body.id),
                Ok(body) => (folder_name.clone(), body.id),
                _ => (folder_name.clone(), None),
            },
            Err(err) => {
                eprintln!("warning: daemon rejected session registration: {err}");
                (folder_name.clone(), None)
            }
        },
        Err(err) => {
            eprintln!("warning: daemon unreachable ({err}); launching without registration");
            (folder_name.clone(), None)
        }
    };

    // 4. Regenerate `~/.trusty-mpm/framework/instructions/INSTRUCTIONS.md` from
    //    the bundled assets so the on-disk system prompt always reflects the
    //    current trusty-mpm build. A failure is logged but not fatal —
    //    `build_system_prompt` regenerates lazily if the file is missing.
    let instructions_path = match trusty_mpm::core::instruction_pipeline::install_system_prompt() {
        Ok(path) => Some(path),
        Err(err) => {
            eprintln!("warning: failed to install system prompt: {err}");
            None
        }
    };

    // Resolve the model to inject (issue #390): config > frontmatter > default.
    // The `launch` command acts as the PM session, not a named specialist agent.
    let mpm_cfg = trusty_mpm::core::config::MpmConfig::load_default();
    let pm_model = trusty_mpm::core::model_inject::resolve_pm_model(&mpm_cfg, None);

    // Build the `--append-system-prompt` text and write it to a temp file so
    // `claude` reads it at startup. The prompt is resolved *for this project
    // directory* so any override files under `<project>/.trusty-mpm/` take
    // effect (issue #381); `build_system_prompt_for` always yields a prompt.
    let prompt = trusty_mpm::core::session_launch::build_system_prompt_for(&path);
    let prompt_path = trusty_mpm::core::model_inject::write_prompt_file(&prompt);
    if prompt_path.is_none() {
        eprintln!("warning: failed to write system prompt file; launching without prompt");
    }
    let claude_cmd = trusty_mpm::core::model_inject::build_claude_command(
        Some(&pm_model),
        prompt_path.as_deref(),
    );

    // 5. Print the summary banner. The "Prompt:" line shows the canonical
    //    `~/.trusty-mpm/framework/instructions/INSTRUCTIONS.md` source path
    //    when it was installed, not the per-session temp copy.
    print_launch_banner(
        &workdir,
        &tmux_name,
        instructions_path.as_deref().or(prompt_path.as_deref()),
    );

    // 6. Create a detached tmux session in the project directory.
    let new_session = std::process::Command::new("tmux")
        .args(["new-session", "-d", "-s", &tmux_name, "-c", &workdir])
        .status();
    if !matches!(new_session, Ok(s) if s.success()) {
        anyhow::bail!("failed to create tmux session {tmux_name} in {workdir}");
    }

    // 7. Start `claude` inside the tmux session.
    let send = std::process::Command::new("tmux")
        .args(["send-keys", "-t", &tmux_name, &claude_cmd, "Enter"])
        .status();
    if !matches!(send, Ok(s) if s.success()) {
        anyhow::bail!("tmux session {tmux_name} created but failed to start claude");
    }

    // 7b. Find the claude process PID inside the tmux pane and report it to
    //     the daemon so it can monitor process liveness. `claude` takes 1-3 s
    //     to start after send-keys, so this retries for up to ~5 s.
    if let Some(session_id) = session_id {
        let claude_pid = trusty_mpm::core::process::find_claude_pid_in_tmux(
            &tmux_name,
            10,                                    // up to 10 attempts
            std::time::Duration::from_millis(500), // 500 ms between attempts
        );
        if let Some(pid) = claude_pid {
            // PATCH /sessions/{id}/pid — tell the daemon the real process PID.
            // Best-effort; failure is logged but does not abort launch.
            let _ = client
                .patch(format!("{url}/sessions/{}/pid", session_id.0))
                .json(&serde_json::json!({ "pid": pid }))
                .send()
                .await;
            tracing::info!(
                "claude process PID {pid} registered for session {}",
                session_id.0
            );
        } else {
            tracing::warn!("could not find claude PID for session {tmux_name} after retries");
        }
    }

    // 8. Attach to the session — this takes over the current terminal and
    //    blocks until the user detaches or exits claude.
    let status = std::process::Command::new("tmux")
        .args(["attach-session", "-t", &tmux_name])
        .status()?;
    if !status.success() {
        anyhow::bail!("tmux attach-session exited with failure");
    }
    Ok(())
}

/// `connect` subcommand — start or attach to a session without deployment.
///
/// Why: `tm connect` is the lightweight sibling of `tm launch`. Where `launch`
/// runs the full framework-deployment sequence (`prepare_session` +
/// `install_system_prompt`) before bringing the session up, `connect`
/// deliberately skips all deployment — it assumes the framework is already in
/// place (or that the operator does not want it touched) and only ensures the
/// tmux-hosted session is running, then attaches.
/// What: resolves `dir`, reconnects to a live session for the directory when
/// one exists, otherwise registers the session via
/// `POST /api/v1/sessions/connect`, creates the tmux host idempotently
/// (`tmux new-session -A`), starts `claude` only when the session is freshly
/// created, and `attach`es to it. No agents, skills, instructions, or
/// system-prompt files are written.
/// Test: `cli_parses_connect`, `cli_parses_connect_with_dir`.
pub(crate) async fn connect(
    client: &reqwest::Client,
    url: &str,
    dir: Option<String>,
) -> anyhow::Result<()> {
    // 1. Resolve the target directory (absolute, so the banner is unambiguous).
    let path = resolve_dir(dir)?;
    let path = path.canonicalize().unwrap_or(path);
    let workdir = path.to_string_lossy().to_string();

    // 1b. Reconnect to an existing live session for this directory if one
    //     exists — `connect` is idempotent by design.
    if let Some(existing) = find_existing_session(client, url, &workdir).await
        && !existing.is_empty()
        && tmux_has_session(&existing)
    {
        print_launch_banner_reconnecting(&workdir, &existing);
        let status = std::process::Command::new("tmux")
            .args(["attach-session", "-t", &existing])
            .status()?;
        if !status.success() {
            anyhow::bail!("tmux attach-session exited with failure");
        }
        return Ok(());
    }

    // 2. Register the session with the daemon via the connect endpoint. No
    //    `prepare_session` and no `install_system_prompt` — `connect` skips the
    //    entire deployment sequence. When the daemon is unreachable we still
    //    bring the session up under the folder-derived name.
    #[derive(Deserialize)]
    struct Body {
        #[serde(default)]
        name: String,
    }
    let folder_name = fallback_session_name(&path);
    let tmux_name = match client
        .post(format!("{url}/api/v1/sessions/connect"))
        .json(&serde_json::json!({
            "project": path,
            "project_path": path,
            "name": folder_name,
        }))
        .send()
        .await
    {
        Ok(resp) => match resp.error_for_status() {
            Ok(resp) => match resp.json::<Body>().await {
                Ok(body) if !body.name.is_empty() => body.name,
                _ => folder_name.clone(),
            },
            Err(err) => {
                eprintln!("warning: daemon rejected session registration: {err}");
                folder_name.clone()
            }
        },
        Err(err) => {
            eprintln!("warning: daemon unreachable ({err}); connecting without registration");
            folder_name.clone()
        }
    };

    // 3. Print the summary banner. The "Prompt:" line is "(default)" — `connect`
    //    writes no system-prompt file.
    print_launch_banner(&workdir, &tmux_name, None);

    // 4. Create the tmux host idempotently. `new-session -A` attaches to an
    //    existing session and creates a detached one (`-d`) otherwise; the
    //    `has-session` probe tells us which happened so `claude` is started
    //    only for a freshly-created session.
    let already_running = tmux_has_session(&tmux_name);
    let new_session = std::process::Command::new("tmux")
        .args(["new-session", "-A", "-d", "-s", &tmux_name, "-c", &workdir])
        .status();
    if !matches!(new_session, Ok(s) if s.success()) {
        anyhow::bail!("failed to create tmux session {tmux_name} in {workdir}");
    }

    // 5. Start bare `claude` inside a freshly-created session. `connect` does
    //    not compose a `--append-system-prompt` — it does no deployment.
    if !already_running {
        let send = std::process::Command::new("tmux")
            .args(["send-keys", "-t", &tmux_name, "claude", "Enter"])
            .status();
        if !matches!(send, Ok(s) if s.success()) {
            anyhow::bail!("tmux session {tmux_name} created but failed to start claude");
        }
    }

    // 6. Attach — takes over the current terminal until the user detaches.
    let status = std::process::Command::new("tmux")
        .args(["attach-session", "-t", &tmux_name])
        .status()?;
    if !status.success() {
        anyhow::bail!("tmux attach-session exited with failure");
    }
    Ok(())
}

/// Find a live session whose `workdir` matches `workdir` via `GET /sessions`.
///
/// Why: `tm launch` should reconnect to an existing session for a directory
/// rather than spawning a duplicate; the daemon owns the session registry, so
/// the match must be resolved against the live `GET /sessions` list.
/// What: fetches `GET /sessions`, normalizes each `workdir` (strip trailing
/// slash, canonicalize) and compares against the normalized target; returns the
/// first matching session's `tmux_name`, or `None` when the daemon is
/// unreachable or no session matches.
/// Test: `normalize_workdir_strips_trailing_slash`.
async fn find_existing_session(
    client: &reqwest::Client,
    url: &str,
    workdir: &str,
) -> Option<String> {
    /// One session row including its tmux name, as returned by `GET /sessions`.
    #[derive(Deserialize)]
    struct Row {
        #[serde(default)]
        workdir: String,
        #[serde(default)]
        tmux_name: String,
    }

    let target = normalize_workdir(workdir);
    let resp = client.get(format!("{url}/sessions")).send().await.ok()?;
    let rows: Vec<Row> = resp.error_for_status().ok()?.json().await.ok()?;
    rows.into_iter()
        .find(|r| !r.tmux_name.is_empty() && normalize_workdir(&r.workdir) == target)
        .map(|r| r.tmux_name)
}
