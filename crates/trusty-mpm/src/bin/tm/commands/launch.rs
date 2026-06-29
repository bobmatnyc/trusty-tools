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

/// `launch` subcommand — provision a managed clone and launch a configured session.
///
/// Why: `tm launch` should reproduce the `claude-mpm` experience — one command
/// that provisions a clean managed clone, deploys the framework, registers the
/// session, starts `claude` in a tmux host, and hands the current terminal over
/// to it. The live checkout is NEVER touched: `.claude` is deployed into the managed
/// clone, and the tmux cwd is the managed clone (#1590).
/// What: resolves `dir`, derives the GitHub remote from its origin, provisions a
/// managed clone under `~/trusty-mpm-projects/<owner>/<repo>/<id>/`, registers the
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
    //    (<workspace_root>/<owner>/<repo>/) using the same conventions as the daemon.
    let cfg = trusty_mpm::core::trusty_tools_config::TrustyToolsConfig::load();
    let project_dir = trusty_mpm::core::trusty_tools_config::workspace_subpath(&cfg, &gh);
    let project_dir_str = project_dir.to_string_lossy().to_string();

    // 4. Reconnect check — prefix-match any session whose workdir lives under the
    //    canonical project dir. The per-session UUID means an exact-match would
    //    never find an existing session; a prefix match reconnects to the last live
    //    session for this project.
    if let Some(existing) =
        find_existing_session(client, url, &live_workdir, Some(&project_dir_str)).await
        && !existing.is_empty()
        && tmux_has_session(&existing)
    {
        print_launch_banner_reconnecting(&live_workdir, &existing);
        let status = std::process::Command::new("tmux")
            .args(["attach-session", "-t", &existing])
            .status()?;
        if !status.success() {
            anyhow::bail!("tmux attach-session exited with failure");
        }
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

    // 7. Provision the managed workspace: shallow-clone origin + deploy .claude.
    //    `provision_in` handles the full deploy (agents, skills, palace pin) via
    //    `prepare_session_with_repo_url` — no separate `prepare_session_with_style`
    //    call is needed. This step can take 5–30 s for a first clone.
    eprintln!("provisioning managed workspace...");
    let session_uuid = trusty_mpm::session_manager::ManagedSessionId::new();
    let provisioner = trusty_mpm::provisioner::WorkspaceProvisioner::new(
        trusty_mpm::provisioner::RealGitBackend,
        std::path::PathBuf::new(),
    );
    let prepared = provisioner
        .provision_in(&project_dir, &session_uuid, &origin_url, "", "")
        .map_err(|e| anyhow::anyhow!("failed to provision managed workspace: {e}"))?;

    let managed_path = prepared.path;
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
    let claude_cmd = trusty_mpm::core::model_inject::build_claude_command(
        Some(&pm_model),
        prompt_path.as_deref(),
    );

    // 11. Print the full-screen robot splash then the rich info panel.
    //     The banner shows the MANAGED workdir so the user knows where the session runs.
    print_launch_banner(&managed_workdir, &tmux_name, prompt_path.as_deref());
    let _ = instructions_path;

    // 12. Create a detached tmux session rooted at the MANAGED clone directory.
    let new_session = std::process::Command::new("tmux")
        .args([
            "new-session",
            "-d",
            "-s",
            &tmux_name,
            "-c",
            &managed_workdir,
        ])
        .status();
    if !matches!(new_session, Ok(s) if s.success()) {
        anyhow::bail!("failed to create tmux session {tmux_name} in {managed_workdir}");
    }

    // 13. Start `claude` inside the tmux session.
    let send = std::process::Command::new("tmux")
        .args(["send-keys", "-t", &tmux_name, &claude_cmd, "Enter"])
        .status();
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
/// provisions a managed clone, `connect` runs directly in the live checkout without
/// any cloning. This is the recommended path for repos without a GitHub remote, or
/// when the operator needs to work with uncommitted local changes.
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
    //     exists — `connect` is idempotent by design. No project_dir prefix-match
    //     since `connect` always works on the live checkout.
    if let Some(existing) = find_existing_session(client, url, &workdir, None).await
        && !existing.is_empty()
        && tmux_has_session(&existing)
    {
        // Full-screen robot splash + rich info panel (reconnect mode).
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

    // 3. Print the full-screen robot splash + rich info panel before tmux takes over.
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

    // 5. Start `claude` with bypass-permissions inside a freshly-created session.
    //    `connect` does not compose a `--append-system-prompt` — it does no deployment.
    if !already_running {
        let claude_cmd = format!(
            "claude {}",
            trusty_mpm::core::model_inject::PERMISSION_MODE_FLAG
        );
        let send = std::process::Command::new("tmux")
            .args(["send-keys", "-t", &tmux_name, &claude_cmd, "Enter"])
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

/// Find a live session whose `workdir` matches `workdir` (exact) or lies under
/// `project_dir` (prefix, for managed-clone reconnect).
///
/// Why: `tm launch` uses a managed clone with a per-session UUID subdirectory, so
/// exact-matching `workdir` would never reconnect; a prefix-match on the canonical
/// `project_dir` (`~/trusty-mpm-projects/<owner>/<repo>`) reconnects to the last
/// live session for the same project. `tm connect` always passes `project_dir =
/// None` so it retains the original exact-match semantics.
/// What: fetches `GET /sessions`, normalizes each `workdir` (strip trailing slash,
/// canonicalize), and returns the first matching session's `tmux_name`. "Matching"
/// means either an exact match against `workdir` OR (when `project_dir` is
/// `Some`) a prefix match where the session's normalized workdir starts with the
/// normalized project dir + `/`.
/// Test: `normalize_workdir_strips_trailing_slash`; the prefix-match logic is
/// tested by `session_matches_workdir_prefix_and_exact` unit tests below.
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
            !r.tmux_name.is_empty() && session_matches_workdir(&r.workdir, workdir, project_dir)
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
        let session_wd = "/home/bob/trusty-mpm-projects/owner/repo/abc-123-uuid";

        // Prefix match: session workdir is under project_dir → match.
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
        // `/owner/repo/<id>` MUST match `/owner/repo`.
        assert!(session_matches_workdir(
            "/projects/owner/repo/some-uuid",
            "/other/live",
            Some(project_dir)
        ));
    }
}
