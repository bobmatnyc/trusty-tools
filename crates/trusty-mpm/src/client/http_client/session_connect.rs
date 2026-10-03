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
    /// for a directory it must start one, mirroring `tm session start`. A
    /// trusty-mpm session is always the `claude` (Claude Code) CLI, never
    /// `claude-mpm`; the trusty-mpm behaviour comes from the custom instructions
    /// (deployed agents + project `CLAUDE.md`) prepared before launch.
    /// What: probes `GET /health`, runs
    /// [`crate::core::session_launch::prepare_session`] (deploy agents + merge
    /// `CLAUDE.md`), builds the `claude` line, POSTs `{project, project_path}`
    /// to `/sessions`, then creates a detached tmux session via
    /// `tmux new-session` and starts `claude` in it via `tmux send-keys`.
    /// Returns the daemon-assigned tmux session name. The daemon only registers
    /// session state; the prep and launch (tmux + process) are owned by the
    /// client, exactly as the CLI does it. #8719: an unreachable daemon fails
    /// the probe before prep writes anything, and a fatal prep failure returns
    /// before the POST; only a tmux failure follows the registration.
    /// Test: `launch_session_errors_when_daemon_unreachable`,
    /// `launch_session_writes_nothing_when_daemon_unreachable`,
    /// `launch_session_prepares_under_the_pinned_home_before_tmux`.
    pub async fn launch_session(&self, workdir: &str) -> anyhow::Result<String> {
        // #8405: the operator's config decides the renderer (see `client_claude_spec`).
        let config_root = crate::core::alt_screen::operator_config_root();
        // #8719: probe first, so an unreachable daemon fails the launch before
        // prep writes anything to the project or the home.
        self.get("/health").send().await?.error_for_status()?;

        // Prepare the custom instructions Claude Code reads at startup: deploy
        // composed agents to `~/.claude/agents/` and merge the project
        // `CLAUDE.md`. Most prep failures are logged but not fatal (#2149) —
        // the session can still launch with whatever instructions already exist
        // on disk. The exception is #4752's compiled-prompt write.
        // #8545: under the pinned home when a test set one; else the process home.
        let home = self.home.clone().or_else(dirs::home_dir);
        let fw = home.as_deref().map_or_else(
            crate::core::paths::FrameworkPaths::default,
            crate::core::paths::FrameworkPaths::under,
        );
        let native = crate::core::output_style::claude_supports_native_output_style();
        let dir = std::path::Path::new(workdir);
        let prep = crate::core::session_launch::prepare_session_with_home(
            &fw,
            dir,
            None,
            native,
            home.as_deref(),
        );
        match prep {
            Ok(report) => {
                // Issue #2149: a roster-deploy failure no longer aborts
                // preparation — surface it loudly rather than let it hide.
                // #6649 folded the asset-hygiene lines in beside them, through
                // the one shared reporter.
                crate::core::session_launch::log_prep_findings(
                    &report.roster_errors,
                    &report.asset_notices,
                    crate::core::session_launch::PrepScope {
                        kind: "connect",
                        session: None,
                        dir: std::path::Path::new(workdir),
                    },
                );
            }
            // #4752: fatal — refuse the launch rather than start a session
            // whose compiled instructions could not be written.
            Err(err) if err.is_fatal() => {
                anyhow::bail!("{err}");
            }
            Err(err) => {
                tracing::warn!(%err, "session pre-launch preparation failed");
            }
        }

        // Build the combined `--append-system-prompt` text (claude-mpm PM
        // instructions + trusty tool-priority block), resolved *for this project
        // directory* so override files under `<workdir>/.trusty-mpm/` take effect
        // (issue #381). Write it to a temp file and pass it via
        // `--append-system-prompt-file` so every launched `claude` is a properly
        // configured PM instance while preserving Claude Code's built-in tool use
        // instructions. The temp file persists because `claude` reads it at
        // startup; it lives in `/tmp` and is superseded by the next launch — no
        // explicit cleanup is performed.
        // #8286: a prompt that cannot be written refuses the launch; no bare claude.
        let claude_spec = client_prompt_spec(
            &std::env::temp_dir(),
            workdir,
            config_root.as_deref(),
            "launch",
        )?;

        // #8719: register only once prep and the line are ready, so the
        // POST-to-tmux window holds nothing that can fail slowly.
        #[derive(Deserialize)]
        struct Body {
            #[serde(default)]
            name: String,
        }
        let body: Body = self
            .post("/sessions")
            .json(&serde_json::json!({
                "project": workdir,
                "project_path": workdir,
            }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        // #2398: routes through `core::tmux::create_managed_session`, the
        // crate's single session-creation choke point, so the configured
        // scrollback/mouse ergonomics are applied before the pane exists (a
        // bare `tmux new-session` here would silently bypass them — the
        // exact QA-caught regression this consolidation closes).
        let new_session =
            crate::core::tmux::create_managed_session(None, &body.name, Some(workdir));
        match new_session {
            Ok(outcome) if outcome.output.status.success() => {
                // #3386 review: operator-visible (not just grep-able) notice
                // — this client is shared by the TUI/bot surfaces, so the
                // `tracing::error!` `warn_if_options_unverified` emits is the
                // channel every one of them can observe.
                crate::core::tmux::warn_if_options_unverified(&outcome, &body.name);
                // #8308: the launch travels in a spec; the pane types a short line.
                self.send_client_spec(&body.name, &claude_spec)?;
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
        // #8405: the operator's config decides the renderer (see `client_claude_spec`).
        let config_root = crate::core::alt_screen::operator_config_root();
        // Build the `--append-system-prompt` text so a freshly-started `claude`
        // is a configured PM, resolved *for this project directory* so override
        // files under `<workdir>/.trusty-mpm/` take effect (issue #381). This is
        // prompt composition from bundled assets + project overrides, not
        // deployment of agents/skills/hooks into the project — `connect` only
        // skips the latter (`prepare_session`). #8286: written BEFORE the daemon
        // POST, so a prompt that cannot be written refuses with nothing registered.
        let claude_spec = client_prompt_spec(
            &std::env::temp_dir(),
            workdir,
            config_root.as_deref(),
            "connect",
        )?;
        #[derive(Deserialize)]
        struct Body {
            #[serde(default)]
            name: String,
        }
        let url = "/api/v1/sessions/connect".to_string();
        let body: Body = self
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

        // `tmux new-session -A` is idempotent: it attaches to the session when
        // it already exists and creates it (detached, `-d`) otherwise. The
        // `has-session` probe distinguishes the two so `claude` is started only
        // for a freshly-created session — an already-running one is left alone.
        // #2398: routes through the crate's single tmux entry point.
        let already_running =
            crate::core::tmux::run_tmux(&crate::core::tmux::TmuxCommand::HasSession {
                name: body.name.clone(),
            })
            .map(|output| output.status.success())
            .unwrap_or(false);

        // #2398: routes through `core::tmux::create_managed_session`, the
        // crate's single session-creation choke point, so the configured
        // scrollback/mouse ergonomics are applied before the pane exists.
        let new_session =
            crate::core::tmux::create_managed_session(None, &body.name, Some(workdir));
        match new_session {
            Ok(outcome) if outcome.output.status.success() => {
                // #3386 review: see `launch_session`'s identical notice above.
                crate::core::tmux::warn_if_options_unverified(&outcome, &body.name);
                if !already_running {
                    // #8308: same spec carrier as `launch_session`.
                    self.send_client_spec(&body.name, &claude_spec)?;
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

    /// Write `spec` under this client's home and start `claude` in `session`
    /// from it (#8308).
    ///
    /// What: [`crate::runtime::cli_launch::send_spec_launch`] into the spec
    /// directory under the pinned home (#8545) or the process home; the error
    /// names the session and the step that failed.
    fn send_client_spec(
        &self,
        session: &str,
        spec: &crate::runtime::launch_spec::LaunchSpec,
    ) -> anyhow::Result<()> {
        let home = self.home.clone().or_else(dirs::home_dir);
        let spec_dir = crate::runtime::cli_launch::spec_dir(home.as_deref()).ok_or_else(|| {
            anyhow::anyhow!("tmux session {session} created but no home holds its launch spec")
        })?;
        crate::runtime::cli_launch::send_spec_launch(session, spec, &spec_dir).map_err(|e| {
            anyhow::anyhow!("tmux session {session} created but failed to start claude: {e}")
        })
    }
}

/// Write the PM prompt for `workdir` under `dir` and build the client launch
/// spec that carries it (#8286, #4467).
///
/// Why: `launch_session` and `connect_session` used to start a bare `claude`
/// when this write failed, so the session ran without its PM instructions. A PM
/// launch has no optional prompt, so both refuse instead. The line is built in
/// `core::model_inject` so it carries the shared inherited-marker scrub the
/// `transcript_saving` doctor check reads (#4467).
/// What: composes the prompt for `workdir`, writes it with
/// [`crate::core::model_inject::write_pm_prompt_file_in`] (production: the
/// process temp dir; `action` is "launch" or "connect" in the refusal text), and returns [`client_claude_spec`] carrying that file;
/// `Err` names the file, the I/O cause and `workdir`.
/// Test: `launch_prompt_spec_refuses_when_the_prompt_file_cannot_be_written`,
/// `connect_prompt_spec_refuses_when_the_prompt_file_cannot_be_written`.
fn client_prompt_spec(
    dir: &std::path::Path,
    workdir: &str,
    config_root: Option<&std::path::Path>,
    action: &str,
) -> anyhow::Result<crate::runtime::launch_spec::LaunchSpec> {
    let project = std::path::Path::new(workdir);
    let prompt = crate::core::session_launch::build_system_prompt_for(project);
    let file = crate::core::model_inject::write_pm_prompt_file_in(dir, &prompt, project, action)?;
    Ok(client_claude_spec(project, config_root, Some(&file)))
}

/// The launch spec the daemon client starts a fresh pane with (#8405, #8308).
///
/// Why: the one seam where `DaemonClient::launch_session` / `connect_session`
/// turn the operator's config into the renderer, split out so a test can drive
/// it from a config root.
/// What: [`crate::runtime::cli_launch::client_spec`] rooted at `cwd`, with the
/// renderer [`crate::core::alt_screen::configured_alternate_screen_in`] reads
/// from `config_root`.
/// Test: `client_claude_spec_follows_the_configured_renderer`.
fn client_claude_spec(
    cwd: &std::path::Path,
    config_root: Option<&std::path::Path>,
    prompt_file: Option<&std::path::Path>,
) -> crate::runtime::launch_spec::LaunchSpec {
    crate::runtime::cli_launch::client_spec(
        cwd,
        prompt_file,
        crate::core::alt_screen::configured_alternate_screen_in(config_root),
    )
}

#[cfg(test)]
mod prompt_refusal_tests {
    /// Drive `client_prompt_spec` with a prompt dir that is a regular file, so
    /// the write fails for real (ENOTDIR), and return the refusal text.
    fn refusal(action: &str) -> (String, String) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let not_a_dir = tmp.path().join("not-a-dir");
        std::fs::write(&not_a_dir, "").expect("plant a file where the prompt dir goes");
        let workdir = tmp.path().to_string_lossy().into_owned();
        let err = super::client_prompt_spec(&not_a_dir, &workdir, None, action)
            .expect_err("a PM launch without its prompt must be refused");
        (err.to_string(), not_a_dir.to_string_lossy().into_owned())
    }

    /// #8286: `DaemonClient::launch_session` refuses, naming the file and cause.
    #[test]
    fn launch_prompt_spec_refuses_when_the_prompt_file_cannot_be_written() {
        let (err, file) = refusal("launch");
        assert!(err.contains(&file) && err.contains("os error"), "{err}");
        assert!(err.contains("refusing to launch"), "{err}");
    }

    /// #8286: `DaemonClient::connect_session` refuses, naming the file and cause.
    #[test]
    fn connect_prompt_spec_refuses_when_the_prompt_file_cannot_be_written() {
        let (err, file) = refusal("connect");
        assert!(err.contains(&file) && err.contains("os error"), "{err}");
        assert!(err.contains("refusing to connect"), "{err}");
    }
}

#[cfg(test)]
mod renderer_tests {
    /// #8405: both `DaemonClient` launch paths build their line here, so this
    /// pins the seam in both directions. Fails if it ignores the config.
    #[test]
    fn client_claude_spec_follows_the_configured_renderer() {
        let cwd = std::path::Path::new("/w");
        for (alternate_screen, value) in [(true, "0"), (false, "1")] {
            let want = (
                "CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN".to_owned(),
                value.to_owned(),
            );
            let root = tempfile::tempdir().expect("tempdir");
            std::fs::write(
                root.path().join("config.yaml"),
                format!("tmux:\n  alternate_screen: {alternate_screen}\n"),
            )
            .expect("write config");
            for prompt in [None, Some(std::path::Path::new("/tmp/p.txt"))] {
                let spec = super::client_claude_spec(cwd, Some(root.path()), prompt);
                assert!(spec.env_set.contains(&want), "want {want:?} in: {spec:?}");
            }
        }
    }

    /// #8453: `launch_session` / `connect_session` type this line into a pane
    /// whose environment comes from the tmux server, so a server started by a
    /// supervisor session carries its `TRUSTY_MPM_SESSION_PROFILE`. The line
    /// must unset it; both launch paths share it.
    #[test]
    fn client_claude_spec_never_passes_on_the_profile_stamp() {
        for prompt in [None, Some(std::path::Path::new("/tmp/p.txt"))] {
            let spec = super::client_claude_spec(std::path::Path::new("/w"), None, prompt);
            assert!(
                spec.env_unset
                    .iter()
                    .any(|n| n == "TRUSTY_MPM_SESSION_PROFILE"),
                "the daemon-client launch must unset the profile stamp: {spec:?}"
            );
        }
    }
}
