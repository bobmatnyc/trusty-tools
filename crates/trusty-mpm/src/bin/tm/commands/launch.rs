//! `launch` and `connect` command handlers.
//!
//! Why: these two commands manage the full session lifecycle (deploy + tmux +
//! attach) and are complex enough to warrant a dedicated file.
//! What: `launch` (managed clone + register + tmux + attach), `connect` (register
//! only + idempotent tmux + attach), plus `find_existing_session` helper.
//! Test: `cli_parses_launch`, `cli_parses_connect`, `cli_parses_launch_with_dir`,
//! `cli_parses_connect_with_dir` in `tests.rs`.

use serde::Deserialize;

use crate::commands::project::resolve_dir;
use crate::formatters::banner::{
    fallback_session_name, normalize_workdir, print_launch_banner,
    print_launch_banner_reconnecting, tmux_has_session,
};

/// RAII guard that kills a tmux session on drop unless explicitly disarmed.
///
/// Why: `launch()` and `connect()` create real tmux sessions; if any step after
/// creation fails (e.g. `send-keys` error, or `attach-session` failing because
/// there is no TTY available — which is always the case in integration tests),
/// the session would otherwise be permanently orphaned, filling the host tmux
/// with leaked managed (`tm-*`/`tmpm-*`) sessions (#1815).
/// What: on `drop` while armed, runs `tmux kill-session -t <name>` best-effort
/// (ignores errors, since the session may already be gone). [`disarm`] turns
/// `drop` into a no-op; call it before returning `Ok(())` so a session that
/// the user successfully attached to persists for future re-attachment.
/// Test: verified by `cargo test -p trusty-mpm --bin tm`; the before/after
/// managed (`tm-*`/`tmpm-*`) session count must be equal after the test run (issue #1815).
struct LaunchSessionGuard {
    name: String,
    armed: bool,
}

impl LaunchSessionGuard {
    /// Construct an armed guard owning `name`.
    ///
    /// Why: callers create the guard immediately after `tmux new-session` so the
    /// cleanup window opens with no gap.
    /// What: stores `name` with `armed = true`.
    /// Test: see module-level doc.
    fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            armed: true,
        }
    }

    /// Transfer ownership away: `drop` becomes a no-op after this call.
    ///
    /// Why: a session the user successfully attached to must NOT be killed when
    /// they detach; disarming transfers lifetime responsibility to the live tmux
    /// server.
    /// What: clears `armed`. Idempotent.
    /// Test: see module-level doc.
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for LaunchSessionGuard {
    /// Why: the whole point — any error path between session creation and a
    /// successful `attach-session` must reap the session so it cannot leak.
    /// What: when `armed`, issues `tmux kill-session -t <name>` best-effort.
    /// Never panics — `Drop` must not unwind.
    /// Test: see module-level doc.
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // #2398 architecture consolidation: route through the crate's single
        // tmux entry point instead of shelling out independently.
        let _ =
            trusty_mpm::core::tmux::run_tmux(&trusty_mpm::core::tmux::TmuxCommand::KillSession {
                name: self.name.clone(),
            });
    }
}

/// `launch` subcommand — provision a managed clone and launch a configured session.
///
/// Why: `tm launch` should reproduce the `claude-mpm` experience — one command
/// that provisions a clean managed clone, deploys the framework, registers the
/// session, starts `claude` in a tmux host, and hands the current terminal over
/// to it. The live checkout is NEVER touched: `.claude` is deployed into the managed
/// clone, and the tmux cwd is the managed clone (#1590).
/// What: resolves `dir`, derives the GitHub remote from its origin, provisions a
/// shared base clone under `<repos_root>/<owner>/<repo>/` (resolved via the SAME
/// `inproject::base_clone_path` the daemon uses, so both entry points agree — #1807)
/// plus a per-session worktree, registers the
/// session with the daemon, prints the banner, creates a detached tmux session
/// running `claude` in the managed clone, and `attach`es to it. Errors with a
/// `tm connect` hint when the directory has no parseable GitHub remote.
/// Test: `cli_parses_launch`, `cli_parses_launch_with_dir`,
/// `cli_parses_launch_with_style`.
pub(crate) async fn launch(
    client: &reqwest::Client,
    url: &str,
    dir: Option<String>,
    style: Option<String>,
) -> anyhow::Result<()> {
    // 1. Resolve the live source directory (absolute, so the banner is unambiguous).
    let live_path = resolve_dir(dir)?;
    let live_path = live_path.canonicalize().unwrap_or(live_path);

    // Auto-register the git project root as a local path alias (non-fatal, silent).
    super::managed_root::try_register_alias(&live_path);
    let live_workdir = live_path.to_string_lossy().to_string();

    // 2. Derive the GitHub identity from the origin remote.
    //    Managed sessions REQUIRE a parseable GitHub remote (#1590). A missing or
    //    unparseable remote is an immediate error that points the user to `tm connect`.
    let origin_url = trusty_mpm::daemon::managed_routes::inproject::get_origin_url(&live_path)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no git origin remote found in '{live_workdir}'\n\
                     Managed sessions require a GitHub remote. \
                     Run `tm connect` to start a session in the live checkout instead."
            )
        })?;
    let gh = trusty_common::github_path::parse_github_path(&origin_url).ok_or_else(|| {
        anyhow::anyhow!(
            "could not parse a GitHub owner/repo from origin remote: {origin_url:?}\n\
             Run `tm connect` to start a session in the live checkout instead."
        )
    })?;
    let source_id = format!("{}/{}", gh.owner, gh.repo);

    // 3. Compute the canonical managed project directory
    //    (<repos_root>/<owner>/<repo>/) via the SAME resolver the daemon's
    //    in-project spawn path uses (`base_clone_path` → `repos_root_from`), so
    //    `tm launch` and daemon-spawned sessions can never diverge (#1807). This
    //    resolver honours the full precedence chain — `TRUSTY_MPM_REPOS_ROOT` env
    //    > `TRUSTY_MPM_WORKSPACE_ROOT` env > config `workspace_root_template` >
    //    built-in `~/trusty-mpm-projects`. The pre-#1807 code called
    //    `workspace_subpath`, which skipped `TRUSTY_MPM_REPOS_ROOT` entirely and
    //    re-introduced the root divergence #1803 set out to eliminate.
    let project_dir =
        trusty_mpm::daemon::managed_routes::inproject::base_clone_path(&gh.owner, &gh.repo);
    let project_dir_str = project_dir.to_string_lossy().to_string();

    // 4. Reconnect check — prefix-match any LIVE session whose workdir lives under
    //    the canonical project dir. The per-session UUID means an exact-match would
    //    never find an existing session; a prefix match reconnects to the last live
    //    session for this project. Liveness is checked inside `find_existing_session`
    //    so ALL candidates are evaluated (not just the first one returned by the
    //    daemon), which correctly handles projects with stale historical sessions.
    if let Some(existing) =
        find_existing_session(client, url, &live_workdir, Some(&project_dir_str)).await
        && !existing.is_empty()
    {
        print_launch_banner_reconnecting(&live_workdir, &existing);
        crate::commands::tmux_attach::tmux_attach(&existing)?;
        return Ok(());
    }

    // 5. Warn about uncommitted local changes (one-time; non-fatal).
    eprintln!(
        "note: uncommitted local changes are not carried into the managed clone. \
         Use `tm connect` if you need to work from the live checkout."
    );

    // 6. --style is not yet honoured in managed mode.
    if style.is_some() {
        tracing::warn!("--style not yet supported for managed launches; ignoring style flag");
        // TODO(follow-up): prepare_session_with_repo_url_and_style
    }

    // 7. Provision the managed workspace using the shared base-clone + per-session
    //    worktree mechanism (#1803). This matches what `tm` (guided default) does
    //    via the daemon's `spawn_managed_inproject` path — both now converge on the
    //    SAME base clone at `~/trusty-mpm-projects/<owner>/<repo>/` and the SAME
    //    per-session worktree at `<base>/.worktrees/<session-id>/`.
    //    Step 7a creates or reuses the shared base clone (idempotent); step 7b adds
    //    a fresh git worktree for this session; step 7c deploys `.claude` into the
    //    worktree so the session has the framework available.
    eprintln!("provisioning managed workspace...");
    let session_uuid = trusty_mpm::session_manager::ManagedSessionId::new();

    // 7a. Ensure the shared base clone exists. Idempotent: returns immediately when
    //     `<project_dir>/.git` is already present so a second `tm launch` reuses
    //     the existing clone rather than re-cloning.
    trusty_mpm::daemon::managed_routes::inproject::ensure_base_clone(&origin_url, &project_dir)
        .map_err(|e| anyhow::anyhow!("failed to provision base clone: {e}"))?;

    // 7b. Create a per-session git worktree at `<project_dir>/.worktrees/<session-id>/`.
    //     Each session gets an isolated branch so concurrent sessions never collide.
    //     NOTE (#2032): `tm launch` is a standalone CLI flow, not the daemon's
    //     managed `SessionManager` spawn path — the tmux name here is derived
    //     from the live folder AFTER the worktree already exists (see
    //     `folder_name`/`fallback_session_name` below), so there is no
    //     resolved semantic name available yet at worktree-creation time.
    //     This keeps the pre-#2032 UUID-named worktree; only
    //     `spawn_managed_inproject` (the daemon's HTTP/MCP spawn path) uses
    //     the new semantic-name layout.
    let managed_path = trusty_mpm::daemon::managed_routes::inproject::create_session_worktree(
        &project_dir,
        &session_uuid.to_string(),
    )
    .map_err(|e| anyhow::anyhow!("failed to create session worktree: {e}"))?;

    // 7c. Deploy the `.claude` framework into the worktree (best-effort).
    //     Non-fatal: a deploy failure never aborts the session — the operator can
    //     run `tm install` / `tm catalog sync` to populate agents manually.
    {
        let fw = trusty_mpm::core::paths::FrameworkPaths::default();
        match trusty_mpm::core::session_launch::prepare_session_with_repo_url(
            &fw,
            &managed_path,
            Some(&origin_url),
        ) {
            Ok(report) => {
                // Issue #2149: a roster-deploy failure no longer aborts
                // preparation — surface it loudly rather than let it hide.
                for err in &report.roster_errors {
                    tracing::error!(
                        "roster provisioning gap for worktree {}: {err}",
                        managed_path.display()
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    "session prep failed for worktree {} (non-fatal): {e}",
                    managed_path.display()
                );
            }
        }
        // Pre-seed workspace trust so claude starts without blocking prompts.
        if let Err(e) = trusty_mpm::core::home_trust_seed::preseed_home_trust(&managed_path) {
            tracing::warn!(
                "home trust pre-seed failed for {} (non-fatal): {e}",
                managed_path.display()
            );
        }
    }
    let managed_workdir = managed_path.to_string_lossy().to_string();

    // 8. Write project-scoped MPM hooks into the managed clone (NOT the live checkout).
    //    Both steps are best-effort — a failure is logged but never fatal.
    if let Err(e) = crate::commands::install::remove_global_trusty_mpm_hooks() {
        eprintln!("warning: could not remove global MPM hooks: {e:#}");
    }
    if let Err(e) = crate::commands::install::write_project_hooks_for_dir(&managed_path) {
        eprintln!("warning: could not write project-scoped MPM hooks: {e:#}");
    }

    // 9. Register the session with the daemon. The tmux name is derived from
    //    the LIVE project folder so the human-readable name reflects the repo.
    //    `project`/`project_path` are the MANAGED clone path; `source_id` links
    //    this session back to its GitHub identity for future reconnects.
    //    Unknown fields (like `source_id`) are currently ignored by the daemon —
    //    non-breaking forward-compatible addition.
    #[derive(Deserialize)]
    struct Body {
        #[serde(default)]
        name: String,
        /// The registered session's id; `None` when the daemon is unreachable.
        id: Option<trusty_mpm::core::session::SessionId>,
    }
    let folder_name = fallback_session_name(&live_path);
    let (tmux_name, session_id) = match client
        .post(format!("{url}/sessions"))
        .json(&serde_json::json!({
            "project": managed_workdir,
            "project_path": managed_workdir,
            "name": folder_name,
            "source_id": source_id,
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

    // 10. Regenerate `~/.trusty-mpm/framework/instructions/INSTRUCTIONS.md` from
    //     the bundled assets so the on-disk system prompt always reflects the
    //     current trusty-mpm build.
    let instructions_path = match trusty_mpm::core::instruction_pipeline::install_system_prompt() {
        Ok(path) => Some(path),
        Err(err) => {
            eprintln!("warning: failed to install system prompt: {err}");
            None
        }
    };

    // Resolve the PM model (config > frontmatter > default).
    let mpm_cfg = trusty_mpm::core::config::MpmConfig::load_default();
    let pm_model = trusty_mpm::core::model_inject::resolve_pm_model(&mpm_cfg, None);

    // Build the `--append-system-prompt` text from the managed clone (where the
    // framework was deployed by provision_in). Style is not supported in managed
    // mode so we always pass `None` here.
    let prompt =
        trusty_mpm::core::session_launch::build_system_prompt_for_with_style(&managed_path, None);
    let prompt_path = trusty_mpm::core::model_inject::write_prompt_file(&prompt);
    if prompt_path.is_none() {
        eprintln!("warning: failed to write system prompt file; launching without prompt");
    }
    // #2997: `tm launch` also creates a detached tmux session and types the
    // `claude` line into it via send-keys, so the pane's `claude` is forked by
    // the shared tmux server just like the daemon path — wrap it in the
    // disclaim-exec so tccd blames Claude Code, not the tmux server. No-op off
    // macOS / under TM_DISABLE_SPAWN_DISCLAIM.
    let claude_cmd = trusty_mpm::core::spawn_disclaim::disclaim_pane_command(
        &trusty_mpm::core::model_inject::build_claude_command(
            Some(&pm_model),
            prompt_path.as_deref(),
        ),
    );

    // 11. Print the full-screen robot splash then the rich info panel.
    //     Pass the real managed worktree path so the banner shows the session
    //     directory (not just the base project dir).
    print_launch_banner(
        &live_workdir,
        &tmux_name,
        prompt_path.as_deref(),
        Some(&managed_path),
    );
    let _ = instructions_path;

    // 12. Create a detached tmux session rooted at the MANAGED clone directory.
    //     #2398: routes through `core::tmux::create_managed_session`, the
    //     crate's single session-creation choke point — this is what applies
    //     the configured scrollback/mouse ergonomics BEFORE the pane exists
    //     (a bare `tmux new-session` here would silently bypass them, the
    //     exact QA-caught regression this consolidation closes).
    let new_session =
        trusty_mpm::core::tmux::create_managed_session(None, &tmux_name, Some(&managed_workdir))
            .map(|output| output.status);
    if !matches!(new_session, Ok(s) if s.success()) {
        anyhow::bail!("failed to create tmux session {tmux_name} in {managed_workdir}");
    }
    // RAII guard — disarmed only after successful attach so the session persists
    // when the user detaches normally. On any error path (including attach
    // failing because there is no TTY, which is always the case in tests) the
    // guard's Drop impl reaps the session, preventing leaks (#1815).
    let mut session_guard = LaunchSessionGuard::new(&tmux_name);

    // 13. Start `claude` inside the tmux session.
    let send = trusty_mpm::core::tmux::send_line(
        None,
        &trusty_mpm::core::tmux::TmuxTarget::session(&tmux_name),
        &claude_cmd,
    )
    .map(|output| output.status);
    if !matches!(send, Ok(s) if s.success()) {
        anyhow::bail!("tmux session {tmux_name} created but failed to start claude");
    }

    // 13b. Find the claude process PID inside the tmux pane and report it to
    //      the daemon so it can monitor process liveness.
    if let Some(session_id) = session_id {
        let claude_pid = trusty_mpm::core::process::find_claude_pid_in_tmux(
            &tmux_name,
            10,
            std::time::Duration::from_millis(500),
        );
        if let Some(pid) = claude_pid {
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

    // 14. Attach to the session — blocks until the user detaches or exits claude.
    crate::commands::tmux_attach::tmux_attach(&tmux_name)?;
    // Attach succeeded: the user detached normally. Disarm the guard so the
    // session persists and can be resumed with `tmux attach -t <name>`.
    session_guard.disarm();
    Ok(())
}

/// `connect` subcommand — start or attach to a session in the live checkout.
///
/// Why: `tm connect` is the lightweight sibling of `tm launch`. Where `launch`
/// provisions a managed clone, `connect` runs directly in the live checkout without
/// any cloning. This is the recommended path for repos without a GitHub remote, or
/// when the operator needs to work with uncommitted local changes. Issue #2230:
/// `connect` used to deploy NOTHING and launch a bare
/// `claude --dangerously-skip-permissions` with no PM persona — the one launch
/// path that could silently spawn vanilla Claude Code. It now runs the same
/// [`trusty_mpm::core::session_launch::prepare_session`] seam
/// `DaemonClient::launch_session` (the TUI's `/connect`) uses, deploying agents,
/// skills, the project-tier output-style, and the `trusty-memory`/PM-guard
/// project hooks directly into the live checkout — no clone, no git-remote or
/// clean-tree requirement, so repos with no GitHub remote or uncommitted
/// changes (this command's whole reason to exist) still launch.
/// What: resolves `dir`, reconnects to a live session for the directory when
/// one exists, otherwise runs [`trusty_mpm::core::session_launch::prepare_session`]
/// (best-effort — logs and continues on any failure, never aborts the
/// connect), registers the session via `POST /api/v1/sessions/connect`,
/// builds the PM system prompt via
/// [`trusty_mpm::core::session_launch::build_system_prompt_for_with_style_and_native`]
/// and writes it to a temp file, creates the tmux host idempotently
/// (`tmux new-session -A`), and — only when the session is freshly created —
/// starts `claude` via [`connect_claude_cmd`] (`--append-system-prompt-file`
/// plus the shared `--setting-sources project,local` /
/// `--dangerously-skip-permissions` isolation flags), then `attach`es to it.
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

    // Auto-register the git project root as a local path alias (non-fatal, silent).
    super::managed_root::try_register_alias(&path);

    // 1b. Reconnect to an existing LIVE session for this directory if one
    //     exists — `connect` is idempotent by design. No project_dir prefix-match
    //     since `connect` always works on the live checkout. Liveness is checked
    //     inside `find_existing_session` so stale historical sessions don't block.
    if let Some(existing) = find_existing_session(client, url, &workdir, None).await
        && !existing.is_empty()
    {
        // Full-screen robot splash + rich info panel (reconnect mode).
        print_launch_banner_reconnecting(&workdir, &existing);
        crate::commands::tmux_attach::tmux_attach(&existing)?;
        return Ok(());
    }

    // 1c. Deploy the framework directly into the live checkout — agents,
    //     skills, the project-tier output-style, and the
    //     `trusty-memory`/PM-guard project hooks (issue #2230). This is the
    //     SAME `prepare_session` seam `DaemonClient::launch_session` (the
    //     TUI's `/connect`) uses; unlike `tm launch` it performs no
    //     git-remote or clean-tree check at all — only filesystem writes
    //     under `path` — so a repo with no GitHub remote or with uncommitted
    //     changes still launches. Best-effort: a prep failure is logged and
    //     never aborts the connect.
    let fw = trusty_mpm::core::paths::FrameworkPaths::default();
    match trusty_mpm::core::session_launch::prepare_session(&fw, &path) {
        Ok(report) => {
            for err in &report.roster_errors {
                tracing::error!("roster provisioning gap for {}: {err}", path.display());
            }
        }
        Err(err) => {
            tracing::warn!(
                "session prep failed for {} (non-fatal): {err}",
                path.display()
            );
        }
    }

    // 1d. Build the PM system-prompt text for the live checkout (where 1c just
    //     deployed the framework) and write it to a temp file for
    //     `--append-system-prompt-file` (issue #2230). Non-fatal: a write
    //     failure omits the flag rather than blocking the connect.
    let native = trusty_mpm::core::output_style::claude_supports_native_output_style();
    let prompt = trusty_mpm::core::session_launch::build_system_prompt_for_with_style_and_native(
        &path, None, native,
    );
    let prompt_path = trusty_mpm::core::model_inject::write_prompt_file(&prompt);
    if prompt_path.is_none() {
        eprintln!("warning: failed to write system prompt file; connecting without prompt");
    }

    // 2. Register the session with the daemon via the connect endpoint. When
    //    the daemon is unreachable we still bring the session up under the
    //    folder-derived name.
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

    // 3. Print the full-screen robot splash + rich info panel before tmux takes over.
    print_launch_banner(&workdir, &tmux_name, prompt_path.as_deref(), None);

    // 4. Create the tmux host idempotently. `new-session -A` attaches to an
    //    existing session and creates a detached one (`-d`) otherwise; the
    //    `has-session` probe tells us which happened so `claude` is started
    //    only for a freshly-created session. #2398: routes through
    //    `core::tmux::create_managed_session`, the crate's single session-
    //    creation choke point, so the configured scrollback/mouse ergonomics
    //    are applied before the pane exists.
    let already_running = tmux_has_session(&tmux_name);
    let new_session =
        trusty_mpm::core::tmux::create_managed_session(None, &tmux_name, Some(&workdir))
            .map(|output| output.status);
    if !matches!(new_session, Ok(s) if s.success()) {
        anyhow::bail!("failed to create tmux session {tmux_name} in {workdir}");
    }
    // Guard only needed for freshly-created sessions; if the session was already
    // running we must NOT kill it on attach failure — another user may be attached.
    let mut session_guard = if already_running {
        None
    } else {
        Some(LaunchSessionGuard::new(&tmux_name))
    };

    // 5. Start `claude` inside a freshly-created session, carrying the PM
    //    system-prompt injection plus the shared isolation flags (#2230).
    if !already_running {
        // #2997: disclaim the pane's `claude` off the shared tmux server (same
        // wrapper the daemon + `tm launch` paths use). No-op off macOS / under
        // TM_DISABLE_SPAWN_DISCLAIM.
        let claude_cmd = trusty_mpm::core::spawn_disclaim::disclaim_pane_command(
            &connect_claude_cmd(prompt_path.as_deref()),
        );
        let send = trusty_mpm::core::tmux::send_line(
            None,
            &trusty_mpm::core::tmux::TmuxTarget::session(&tmux_name),
            &claude_cmd,
        )
        .map(|output| output.status);
        if !matches!(send, Ok(s) if s.success()) {
            anyhow::bail!("tmux session {tmux_name} created but failed to start claude");
        }
    }

    // 6. Attach — takes over the current terminal until the user detaches.
    crate::commands::tmux_attach::tmux_attach(&tmux_name)?;
    // Attach succeeded: disarm the guard so the session persists for re-attachment.
    if let Some(ref mut g) = session_guard {
        g.disarm();
    }
    Ok(())
}

/// Compose the `claude` invocation `connect` sends to a freshly-created tmux pane.
///
/// Why (issue #2230): before this fix, `connect` sent a bare
/// `claude --dangerously-skip-permissions` with no PM system-prompt injection
/// and no `--setting-sources` isolation — the one launch path that could
/// silently spawn vanilla Claude Code. This gives `connect` the same carrier
/// every other launch path (`spawn`/`resume_command` in the daemon adapter,
/// `launch`'s own `claude_cmd`) already has.
/// What: thin wrapper over [`trusty_mpm::core::model_inject::build_claude_command`]
/// with no `--model` override (`connect` does not resolve a PM model tier);
/// always carries `SETTING_SOURCES_FLAG` (`--setting-sources project,local`)
/// and `PERMISSION_MODE_FLAG` (`--dangerously-skip-permissions`), plus
/// `--append-system-prompt-file <path>` when `prompt_file` is `Some`.
/// Test: `cli_parses_connect`, `cli_parses_connect_with_dir` (in
/// `tests_behavior_b_tests.rs`) assert both flags are present in the output.
pub(crate) fn connect_claude_cmd(prompt_file: Option<&std::path::Path>) -> String {
    trusty_mpm::core::model_inject::build_claude_command(None, prompt_file)
}

/// Find the first LIVE session whose `workdir` matches `workdir` (exact) or lies
/// under `project_dir` (prefix, for managed-clone reconnect).
///
/// Why: `tm launch` uses a managed clone with a per-session UUID subdirectory, so
/// exact-matching `workdir` would never reconnect; a prefix-match on the canonical
/// `project_dir` (`~/trusty-mpm-projects/<owner>/<repo>`) reconnects to the last
/// live session for the same project. `tm connect` always passes `project_dir =
/// None` so it retains the original exact-match semantics.
/// Liveness (`tmux has-session`) is checked HERE (not at the call site) so ALL
/// candidates are evaluated — not just the first workdir-matching one. This prevents
/// a project with stale historical sessions from blocking reconnect to a later live one.
/// What: fetches `GET /sessions`, iterates all rows, skips empty tmux names and
/// sessions where `tmux has-session` fails, and returns the first matching live
/// session's `tmux_name`. `workdir`/`project_dir` matching is delegated to
/// [`session_matches_workdir`]. Returns `None` when the daemon is unreachable or no
/// live matching session exists.
/// Test: `session_matches_workdir_*` unit tests cover the matching logic;
/// `normalize_workdir_strips_trailing_slash` covers the normalization invariant.
async fn find_existing_session(
    client: &reqwest::Client,
    url: &str,
    workdir: &str,
    project_dir: Option<&str>,
) -> Option<String> {
    /// One session row including its tmux name, as returned by `GET /sessions`.
    #[derive(Deserialize)]
    struct Row {
        #[serde(default)]
        workdir: String,
        #[serde(default)]
        tmux_name: String,
    }

    let resp = client.get(format!("{url}/sessions")).send().await.ok()?;
    let rows: Vec<Row> = resp.error_for_status().ok()?.json().await.ok()?;
    rows.into_iter()
        .find(|r| {
            !r.tmux_name.is_empty()
                && session_matches_workdir(&r.workdir, workdir, project_dir)
                && tmux_has_session(&r.tmux_name)
        })
        .map(|r| r.tmux_name)
}

/// Return true when `session_workdir` matches `target` exactly OR lies under `project_dir`.
///
/// Why: extracted from `find_existing_session` so the matching logic can be unit-tested
/// independently of the HTTP client call. Both the exact-match (live-checkout, `connect`)
/// and the prefix-match (managed clone, `launch`) branches are exercised this way.
/// What: normalizes all three paths (trailing-slash strip via `normalize_workdir`), then
/// checks (a) exact equality or (b) when `project_dir` is `Some`, that the session workdir
/// starts with `<normalized_project_dir>/` — the trailing slash prevents a false match
/// where `/owner/repo-extra/<id>` matches `/owner/repo`.
/// Test: `session_matches_workdir_prefix_and_exact` below.
fn session_matches_workdir(session_workdir: &str, target: &str, project_dir: Option<&str>) -> bool {
    let w = normalize_workdir(session_workdir);
    let t = normalize_workdir(target);
    if w == t {
        return true;
    }
    let Some(proj) = project_dir else {
        return false;
    };
    let mut prefix = normalize_workdir(proj);
    if !prefix.ends_with('/') {
        prefix.push('/');
    }
    w.starts_with(prefix.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exact-match: a session whose workdir equals the live checkout directory must match.
    ///
    /// Why: `tm connect` (and the pre-#1590 `tm launch`) used exact-match semantics;
    /// this test locks in that existing behaviour so the refactor cannot regress it.
    /// Test: this function IS the test.
    #[test]
    fn session_matches_workdir_exact() {
        assert!(session_matches_workdir(
            "/home/bob/projects/trusty-tools",
            "/home/bob/projects/trusty-tools",
            None
        ));
        // Trailing slashes are normalized away before comparison.
        assert!(session_matches_workdir(
            "/home/bob/projects/trusty-tools/",
            "/home/bob/projects/trusty-tools",
            None
        ));
    }

    /// Prefix-match: a session under `project_dir/<uuid>` must match when `project_dir` is provided.
    ///
    /// Why: `tm launch` provisions clones at `<project_dir>/<session_uuid>/`; reconnect
    /// must find the existing session without knowing the UUID. The prefix is
    /// `<project_dir>/` (with trailing slash) so `/owner/repo-extra/<id>` does NOT
    /// falsely match `/owner/repo`.
    /// Test: this function IS the test.
    #[test]
    fn session_matches_workdir_prefix_and_exact() {
        let project_dir = "/home/bob/trusty-mpm-projects/owner/repo";
        let session_wd = "/home/bob/trusty-mpm-projects/owner/repo/.worktrees/abc-123-uuid";

        // Prefix match: session workdir is under project_dir (in .worktrees/) → match.
        assert!(session_matches_workdir(
            session_wd,
            "/other/live/dir",
            Some(project_dir)
        ));

        // Exact match against live workdir also works (no project_dir needed).
        assert!(session_matches_workdir(session_wd, session_wd, None));

        // No-match: different project dir — must NOT match.
        assert!(!session_matches_workdir(
            "/home/bob/trusty-mpm-projects/owner/repo-extra/abc-123-uuid",
            "/other/live/dir",
            Some(project_dir)
        ));

        // No-match: project_dir is None and workdir differs — no match.
        assert!(!session_matches_workdir(
            session_wd,
            "/other/live/dir",
            None
        ));
    }

    /// Trailing-slash normalization: a session workdir with a trailing slash must
    /// still match (regression guard for `normalize_workdir`).
    ///
    /// Why: paths with trailing slashes appear in real daemon responses; stripping
    /// them before comparison prevents false negatives.
    /// Test: this function IS the test.
    #[test]
    fn session_matches_workdir_strips_trailing_slash() {
        assert!(session_matches_workdir(
            "/home/bob/project/",
            "/home/bob/project",
            None
        ));
    }

    /// No-remote error message: when project_dir prefix does not match the session
    /// workdir, the caller (launch) will not reconnect and will provision a new clone.
    /// This test asserts the guard against false-prefix matching.
    ///
    /// Why: without the trailing-`/` guard, `/owner/repo-extra/id` could match
    /// `/owner/repo` via a simple `starts_with` check.
    /// Test: this function IS the test.
    #[test]
    fn session_matches_workdir_no_false_prefix_match() {
        let project_dir = "/projects/owner/repo";
        // `/owner/repo-extra/<id>` must NOT match `/owner/repo`.
        assert!(!session_matches_workdir(
            "/projects/owner/repo-extra/some-uuid",
            "/other/live",
            Some(project_dir)
        ));
        // `/owner/repo/.worktrees/<id>` MUST match `/owner/repo`.
        assert!(session_matches_workdir(
            "/projects/owner/repo/.worktrees/some-uuid",
            "/other/live",
            Some(project_dir)
        ));
    }
}
