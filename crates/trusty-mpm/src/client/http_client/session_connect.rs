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
    /// `CLAUDE.md`), builds the `claude` line, POSTs `{project, project_path, name, tmux_socket}`
    /// to `/sessions`, then creates a detached tmux session via
    /// `tmux new-session` and starts `claude` in it via `tmux send-keys`.
    /// Returns the daemon-assigned tmux session name. The daemon only registers
    /// session state; the prep and launch (tmux + process) are owned by the
    /// client, exactly as the CLI does it. #8719: an unreachable daemon fails
    /// the probe before prep writes anything, and a fatal prep failure returns
    /// before the POST; only a tmux failure follows the registration. #10101
    /// D3c: the CLI's name rule runs before prep (a `Stopped`/`Errored` holder
    /// is resumed, a live holder refuses), and the PM lands on its workspace
    /// server through [`Self::start_on_workspace_server`].
    /// Test: `launch_session_errors_when_daemon_unreachable`,
    /// `launch_session_writes_nothing_when_daemon_unreachable`,
    /// `launch_session_prepares_under_the_pinned_home_before_tmux`,
    /// `launch_session_writes_the_user_tier_under_the_config_dir_9999`.
    pub async fn launch_session(&self, workdir: &str) -> anyhow::Result<String> {
        // #8405: the operator's config decides the renderer (see `client_claude_spec`).
        let config_root = crate::core::alt_screen::operator_config_root();
        // #8719: probe first, so an unreachable daemon fails the launch before
        // prep writes anything to the project or the home.
        self.get("/health").send().await?.error_for_status()?;
        // #10101: D3c — the CLI's name rule, before prep writes anything.
        let dir = std::path::Path::new(workdir);
        if let Some(name) = self.settle_claim(dir).await? {
            return Ok(name);
        }

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
        // #9999: the user tier `CLAUDE_CONFIG_DIR` names, unless a test pinned the home.
        let config_dir = crate::core::session_launch::user_tier_config_dir(
            self.home.as_deref(),
            self.claude_config_dir.as_deref(),
        );
        let prep = crate::core::session_launch::prepare_session_with_home_in(
            &fw,
            dir,
            None,
            native,
            home.as_deref(),
            config_dir.as_deref(),
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
        self.start_on_workspace_server("/sessions", workdir, &claude_spec)
            .await
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
    /// What: POSTs `{project, project_path, name, tmux_socket}` to `/api/v1/sessions/connect`, then
    /// runs `tmux new-session -A` (idempotent — creates the session when absent,
    /// no-ops when it already exists). When the session is freshly created it
    /// starts `claude` in it via `tmux send-keys`; an already-running session is
    /// left untouched. The system-prompt file is still built and passed so a
    /// freshly-started `claude` is a configured PM — that is prompt composition,
    /// not artifact deployment. Returns the daemon-assigned tmux session name.
    /// #10101 D3c: the same name rule and workspace server as `launch_session`.
    /// Test: `connect_session_errors_when_daemon_unreachable`,
    /// `the_tui_connect_launches_on_the_workspace_server_10101`,
    /// `a_live_legacy_record_refuses_the_launch_10101`,
    /// `the_tui_resumes_a_stopped_holder_10101`.
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
        // #10101: D3c — the CLI's name rule, after the #10459 floor check.
        if let Some(name) = self.settle_claim(std::path::Path::new(workdir)).await? {
            return Ok(name);
        }
        self.start_on_workspace_server("/api/v1/sessions/connect", workdir, &claude_spec)
            .await
    }

    /// Act on the CLI's name rule for a TUI launch in `dir` (#10101 D3c).
    ///
    /// What: [`DaemonClient::claim_launch_name`] for `dir`'s session name. A
    /// `Stopped`/`Errored` holder is resumed through the daemon's resume
    /// route and its name answered; this directory's running legacy session
    /// answers its name, untouched; `None` means launch fresh.
    ///
    /// # Errors
    ///
    /// The daemon cannot be read, a live record holds the name, or the
    /// resume failed or left the record errored.
    async fn settle_claim(&self, dir: &std::path::Path) -> anyhow::Result<Option<String>> {
        use super::launch_claim::LaunchClaim;
        let name = crate::core::names::name_from_dir(dir);
        match self.claim_launch_name(&name, dir).await? {
            LaunchClaim::Free => Ok(None),
            LaunchClaim::Running { name, .. } => Ok(Some(name)),
            LaunchClaim::Resume(holder) => {
                let resumed = self.resume_managed_session(&holder.id).await?;
                if resumed.state == "errored" {
                    anyhow::bail!(
                        "session {} ({}) could not be resumed: the daemon left it errored",
                        resumed.name,
                        resumed.id
                    );
                }
                Ok(Some(resumed.name))
            }
        }
    }

    /// Register a fresh PM for `workdir` through `route`, then create its
    /// tmux session on the workspace server and start `claude` (#10101).
    ///
    /// Why: the TUI `/connect` and `/launch` paths launch where the CLI does,
    /// by the same rule ([`crate::session_manager::launch_server::cli_launch_server`]),
    /// and never type into a session they did not create.
    /// What: POSTs `{project, project_path, name, tmux_socket}`; probes
    /// `has-session` on the server; creates (`-A`) there; starts `claude`
    /// only in a session it created. A failure after the registration kills
    /// a session it created and removes the registration ([`super::launch_claim::Registered`]).
    /// Returns the registered tmux name.
    /// Test: `the_tui_connect_launches_on_the_workspace_server_10101`,
    /// `the_tui_launch_never_types_into_a_session_it_did_not_create_10101`,
    /// `a_failed_tui_create_removes_the_registration_10101`.
    async fn start_on_workspace_server(
        &self,
        route: &str,
        workdir: &str,
        spec: &crate::runtime::launch_spec::LaunchSpec,
    ) -> anyhow::Result<String> {
        let dir = std::path::Path::new(workdir);
        let host = crate::session_manager::launch_server::cli_launch_server(dir);
        let server = host.as_ref().map(|(server, _)| server.as_str());
        #[derive(serde::Deserialize)]
        struct Body {
            #[serde(default)]
            name: String,
            id: Option<crate::core::session::SessionId>,
        }
        let body: Body = self
            .post(route)
            .json(&serde_json::json!({
                "project": workdir,
                "project_path": workdir,
                "name": crate::core::names::name_from_dir(dir),
                "tmux_socket": host.as_ref().map(|(_, socket)| socket),
            }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let name = body.name;
        let probe = crate::core::tmux::TmuxCommand::HasSession { name: name.clone() };
        let already_running = crate::core::tmux::run_tmux_on(
            &crate::core::tmux::resolve_tmux_binary_or_bare(),
            server,
            &probe,
        )
        .is_ok_and(|o| o.status.success());
        // A registration for a session that already ran is kept, as in `tm connect`.
        let registered =
            super::launch_claim::Registered::new(self, body.id.filter(|_| !already_running));
        // #2398: the crate's single session-creation choke point.
        match crate::core::tmux::create_managed_session_on(None, server, &name, Some(workdir), true)
        {
            // #3386 review: operator-visible through the tracing channel.
            Ok(outcome) if outcome.output.status.success() => {
                crate::core::tmux::warn_if_options_unverified(&outcome, &name);
            }
            Ok(_) | Err(_) => {
                let e = anyhow::anyhow!(
                    "failed to create tmux session {name} in {workdir} on tmux server {}",
                    server.unwrap_or("default")
                );
                return Err(registered.fail(e).await);
            }
        }
        if already_running {
            return Ok(name);
        }
        if let Err(e) = self.send_client_spec(server, &name, spec) {
            let kill = crate::core::tmux::TmuxCommand::KillSession { name: name.clone() };
            let _ = crate::core::tmux::run_tmux_on(
                &crate::core::tmux::resolve_tmux_binary_or_bare(),
                server,
                &kill,
            );
            return Err(registered.fail(e).await);
        }
        Ok(name)
    }

    /// Write `spec` under this client's home and start `claude` in `session`
    /// on `server` (#8308, #10101).
    ///
    /// What: [`crate::runtime::cli_launch::send_spec_launch`] into the spec
    /// directory under the pinned home (#8545) or the process home; the error
    /// names the session and the step that failed.
    fn send_client_spec(
        &self,
        server: Option<&str>,
        session: &str,
        spec: &crate::runtime::launch_spec::LaunchSpec,
    ) -> anyhow::Result<()> {
        let home = self.home.clone().or_else(dirs::home_dir);
        let spec_dir = crate::runtime::cli_launch::spec_dir(home.as_deref()).ok_or_else(|| {
            anyhow::anyhow!("tmux session {session} created but no home holds its launch spec")
        })?;
        crate::runtime::cli_launch::send_spec_launch(server, session, spec, &spec_dir).map_err(
            |e| anyhow::anyhow!("tmux session {session} created but failed to start claude: {e}"),
        )
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
/// `Err` names the file, the I/O cause and `workdir`. The profile the prompt was
/// composed for also decides the #8992 tool trim and, for an Architect, the
/// #10127 lockdown; a lockdown error refuses the launch.
/// Test: `launch_prompt_spec_refuses_when_the_prompt_file_cannot_be_written`,
/// `connect_prompt_spec_refuses_when_the_prompt_file_cannot_be_written`,
/// `client_prompt_spec_trims_a_pm_launch`,
/// `an_architect_client_prompt_spec_carries_the_lockdown`.
fn client_prompt_spec(
    dir: &std::path::Path,
    workdir: &str,
    config_root: Option<&std::path::Path>,
    action: &str,
) -> anyhow::Result<crate::runtime::launch_spec::LaunchSpec> {
    let project = std::path::Path::new(workdir);
    // #8992: resolve the profile once, so the prompt and the tool trim agree.
    // This is the resolution `build_system_prompt_for` performs internally.
    let profile = crate::core::session_profile::resolve_ambient(project);
    // #10459: the TUI `/connect` and launch paths run no prep, so the
    // Architect's Claude Code floor is checked here, before the prompt write,
    // the daemon POST and the spawn.
    crate::core::architect_claude_version::require_for_profile(profile)?;
    client_prompt_spec_as(dir, project, config_root, action, profile)
}

/// [`client_prompt_spec`] for a resolved `profile` (#10127 test seam).
///
/// Test: `an_architect_client_prompt_spec_carries_the_lockdown`.
fn client_prompt_spec_as(
    dir: &std::path::Path,
    project: &std::path::Path,
    config_root: Option<&std::path::Path>,
    action: &str,
    profile: crate::core::session_profile::SessionProfile,
) -> anyhow::Result<crate::runtime::launch_spec::LaunchSpec> {
    // #9012: the PM instructions are runtime content; none refuses the launch.
    let content = crate::core::content_source::framework_content_for(project)
        .map_err(|err| anyhow::anyhow!("cannot compose the PM instructions: {err}"))?;
    let prompt = crate::core::session_launch::build_system_prompt_for_profile(
        &content,
        project,
        None,
        crate::core::output_style::claude_supports_native_output_style(),
        profile,
    );
    let file = crate::core::model_inject::write_pm_prompt_file_in(dir, &prompt, project, action)?;
    let mut spec = client_claude_spec(project, config_root, Some(&file));
    spec.env_set
        .extend(crate::core::pm_tool_trim::trim_env(profile));
    spec.args
        .extend(crate::core::pm_tool_trim::trim_args(profile));
    // #8988: unrelocated, so the policy reads the operator's own `.claude.json`.
    // #8992: the same profile decides whether the skill and tool parts apply.
    let settings = crate::core::launch_policy::provision_settings_default(project, None, profile);
    // #10127: the owner file and an Architect's deny as ONE `--settings`.
    crate::core::architect_lockdown::Lockdown::PRODUCTION.apply_to_spec(
        profile,
        settings.file.as_deref(),
        &mut spec,
    )?;
    Ok(spec)
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

    /// #8992: a project with no supervisor grant launches a PM, so both
    /// `DaemonClient` paths carry the Artifact switch and the deny flag.
    /// Before the fix the client spec carried neither.
    #[test]
    fn client_prompt_spec_trims_a_pm_launch() {
        let project = tempfile::tempdir().expect("project");
        let dir = tempfile::tempdir().expect("prompt dir");
        let workdir = project.path().to_string_lossy().into_owned();
        for action in ["launch", "connect"] {
            let spec = super::client_prompt_spec(dir.path(), &workdir, None, action)
                .expect("a writable prompt dir builds the spec");
            assert!(
                spec.env_set
                    .contains(&("CLAUDE_CODE_DISABLE_ARTIFACT".to_owned(), "1".to_owned())),
                "{spec:?}"
            );
            assert!(
                spec.args
                    .windows(2)
                    .any(|w| w == ["--disallowedTools", "Workflow,DesignSync"]),
                "{spec:?}"
            );
        }
    }

    /// #10127: the daemon client's launch and connect of an Architect carry
    /// the production deny as ONE `--settings` and unset every chat token.
    /// Before the fix the Architect spec carried the bare owner file.
    #[test]
    fn an_architect_client_prompt_spec_carries_the_lockdown() {
        use crate::core::architect_lockdown::{assert_chat_tokens_unset, assert_production_deny};
        let project = tempfile::tempdir().expect("project");
        let dir = tempfile::tempdir().expect("prompt dir");
        for action in ["launch", "connect"] {
            let spec = super::client_prompt_spec_as(
                dir.path(),
                project.path(),
                None,
                action,
                crate::core::session_profile::SessionProfile::Architect,
            )
            .expect("the Architect spec builds");
            assert_production_deny(&spec.args);
            assert_chat_tokens_unset(&spec.env_unset);
        }
    }
}

#[cfg(test)]
#[path = "session_connect_10459_tests.rs"]
mod architect_version_tests;

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
