//! Shared Claude Code relaunch-line builder for ad hoc (non-managed) tmux spawns.
//!
//! Why: [`crate::daemon::services::tmux_service::TmuxService::spawn_claude`]
//! (the GUI "New Session" bootstrap) and
//! [`crate::daemon::claude_config::restarter::ClaudeCodeRestarter::restart_in_session`]
//! (the config-apply restart) each send a `claude` launch line into an
//! already-created tmux pane, independently of one another. Before this module
//! existed, each call site built its own literal `"claude"` string inline —
//! two independent places a future flag/env change could land in one and be
//! forgotten in the other, silently drifting the two flows apart (#2010).
//! Routing both through one function makes that impossible: a future change
//! to the launch line is a single edit.
//!
//! This is a DIFFERENT, simpler builder than the managed-session one in
//! [`crate::runtime::claude_code`] (its private `spawn_command`, which
//! resolves an absolute `claude` binary, scrubs `ANTHROPIC_API_KEY` via
//! `env_bin_prefix`, and injects `CLAUDE_CONFIG_DIR` plus the
//! `--setting-sources` / `--dangerously-skip-permissions` isolation flags) —
//! hence the distinct name [`relaunch_command`] rather than reusing
//! `spawn_command`, to avoid two same-named-but-semantically-opposite
//! functions in the crate.
//!
//! NOTE / KNOWN LIMITATION (not fixed here): both call sites emit a bare
//! `claude` with no env-scrubbing or `CLAUDE_CONFIG_DIR` isolation, on the
//! assumption that the target pane is a non-managed, already-interactive
//! session (a freshly-created GUI host, or the operator's own attached pane).
//! That assumption is NOT currently verified: `restart_in_session` is reachable
//! from `POST /claude-config/restart`
//! (`crates/trusty-mpm/src/daemon/api/claude_config_routes.rs`,
//! `restart_claude_code` / `RestartRequest`), which accepts an arbitrary
//! caller-supplied `tmux_session` with no check that it is a non-managed
//! session. If that endpoint is ever pointed at a managed
//! (`CLAUDE_CONFIG_DIR`-isolated) session, this bare `claude` relaunch would
//! silently drop that session's auth/roster isolation and unattended-permission
//! mode. Adding registry-aware validation is out of scope for this
//! consolidation (#2010) and is tracked separately in #2020.
//! What: [`relaunch_command`] returns the shell command sent to the pane —
//! `claude` behind the #4467 inherited-session-marker `env` scrub.
//!
//! #8286: the GUI spawn no longer types [`relaunch_command`]. It types
//! [`gui_spawn_command`], the same scrub plus the profile stamp and the PM
//! prompt file; only the config-apply restarter still relaunches a bare
//! `claude`.
//! Test: `relaunch_command_scrubs_inherited_session_markers`,
//! `gui_spawn_command_carries_the_prompt_file_and_the_stamp`.

/// The shell command used to (re)launch `claude` inside an already-running,
/// already-configured tmux pane.
///
/// Why: see the module doc — both the spawn-mode bootstrap
/// ([`crate::daemon::services::tmux_service::TmuxService::spawn_claude`]) and
/// the config-restart flow
/// ([`crate::daemon::claude_config::restarter::ClaudeCodeRestarter::restart_in_session`])
/// must send the identical launch line so the two can never drift apart
/// (#2010). Named distinctly from `runtime::claude_code::spawn_command` (which
/// builds the full env/flags managed-session command) so the two are never
/// confused for one another.
/// What: returns `env <-u marker…> claude`. This still intentionally carries
/// none of the managed-session AUTH isolation in
/// [`crate::runtime::claude_code`] (no absolute-path resolution, no
/// `ANTHROPIC_API_KEY` scrub, no `CLAUDE_CONFIG_DIR`) — see the module doc's
/// KNOWN LIMITATION note for why that is currently unverified rather than
/// deliberately safe for every caller of `restart_in_session`. That half remains
/// #2020's.
///
/// Issue #4467 adds ONLY the inherited-session-marker scrub, which is orthogonal
/// to #2020's auth question: a relaunch into an already-configured pane still
/// inherits `CLAUDE_CODE_CHILD_SESSION` from the pane's environment and would
/// silently save no transcript. Scrubbing it here is what lets the
/// `transcript_saving` doctor check cover this launch line instead of returning
/// `Ok` while it leaks.
/// Test: `relaunch_command_scrubs_inherited_session_markers`.
pub(crate) fn relaunch_command() -> String {
    // #4467: strip the inherited Claude Code session markers. No `NAME=VALUE`
    // assignments follow, so the POSIX "-u before assignments" rule is trivially
    // satisfied.
    format!(
        "env{} claude",
        crate::core::claude_env_scrub::env_unset_flags()
    )
}

/// The line the GUI "New Session" spawn types into its fresh pane (#8286).
///
/// Why: that spawn starts a PM session, and every PM launch mode delivers its
/// prompt through `--append-system-prompt-file`. [`relaunch_command`] stays
/// bare for the config-apply restarter, which relaunches an existing pane.
/// What: the same `env -u …` scrub, then the profile stamp the prompt was
/// composed for (#8453) and, for a PM, `CLAUDE_CODE_DISABLE_ARTIFACT='1'`
/// (#8992), then `claude --append-system-prompt-file '<file>'` and, for a PM,
/// the `--disallowedTools` pair. Every value is single-quoted because the pane
/// shell re-splits the line. #10127 Q2: applies
/// [`crate::core::architect_lockdown::Lockdown::PRODUCTION`] via
/// [`gui_spawn_command_with`].
/// Test: `gui_spawn_command_carries_the_prompt_file_and_the_stamp`,
/// `pm_launch_builders_carry_the_prompt_file`, `gui_spawn_command_trims_a_pm`.
pub(crate) fn gui_spawn_command(
    prompt_file: &std::path::Path,
    stamp: &(String, String),
) -> anyhow::Result<String> {
    gui_spawn_command_with(
        prompt_file,
        stamp,
        &crate::core::architect_lockdown::Lockdown::PRODUCTION,
    )
}

/// [`gui_spawn_command`] under the named `lockdown`, with no owner settings
/// file (#10127 Q2).
///
/// Test: `gui_spawn_command_carries_an_architect_lockdown_instead_of_refusing`,
/// `gui_spawn_command_launches_an_architect_under_the_empty_lockdown`.
pub(crate) fn gui_spawn_command_with(
    prompt_file: &std::path::Path,
    stamp: &(String, String),
    lockdown: &crate::core::architect_lockdown::Lockdown<'_>,
) -> anyhow::Result<String> {
    gui_spawn_command_full(prompt_file, stamp, None, lockdown)
}

/// The GUI spawn line with the owner `carrier` settings file (#8988) and the
/// #10127 `lockdown`.
///
/// Why: the #10127 review found that refusing an Architect here broke GUI New
/// Session in an allow-listed Architect dir; the line carries the lockdown
/// instead. Claude Code keeps only the last `--settings`, so the owner file
/// and the deny travel as one payload.
/// What: an Architect's [`crate::core::architect_lockdown::Lockdown::env_unset`]
/// joins the `env -u` scrub; the `--settings` pair is
/// [`crate::core::architect_lockdown::Lockdown::settings_file_argv`] — for an
/// Architect a file `<prompt_file>.lockdown.json`, else `carrier` unchanged.
/// Fail-closed: a payload error, or a line over
/// [`crate::core::tmux::MAX_PANE_COMMAND_BYTES`] (the tty would drop its tail,
/// the `--settings` with it), is an error.
/// Test: `gui_spawn_line_carries_the_architect_lockdown`,
/// `an_unwritable_lockdown_file_refuses_the_gui_architect_line`,
/// `an_over_long_gui_line_is_refused`.
fn gui_spawn_command_full(
    prompt_file: &std::path::Path,
    stamp: &(String, String),
    carrier: Option<&std::path::Path>,
    lockdown: &crate::core::architect_lockdown::Lockdown<'_>,
) -> anyhow::Result<String> {
    use crate::core::pm_tool_trim::{stamped_profile, trim_args, trim_env};
    use crate::core::spawn_disclaim::pane::shell_single_quote;
    // #8992: the stamp's profile decides the trim; each piece is one shell word.
    let profile = stamped_profile(std::slice::from_ref(stamp));
    // #10127: an Architect's chat tokens join the scrub, ahead of every assignment.
    let unset: String = lockdown
        .env_unset(profile)
        .iter()
        .map(|name| format!(" -u {name}"))
        .collect();
    let env: String = trim_env(profile)
        .iter()
        .map(|(k, v)| format!(" {k}={}", shell_single_quote(v)))
        .collect();
    let words = |argv: Vec<String>| -> String {
        argv.iter()
            .map(|a| format!(" {}", shell_single_quote(a)))
            .collect()
    };
    let args = words(trim_args(profile));
    let settings = words(lockdown.settings_file_argv(
        profile,
        carrier,
        &lockdown_settings_file(prompt_file),
    )?);
    let line = format!(
        "env{}{unset} {}={}{env} claude --append-system-prompt-file {}{args}{settings}",
        crate::core::claude_env_scrub::env_unset_flags(),
        stamp.0,
        shell_single_quote(&stamp.1),
        shell_single_quote(&prompt_file.display().to_string())
    );
    // #10127 Q2: a truncated line would launch without its `--settings`.
    check_line_len(&line)?;
    Ok(line)
}

/// Refuse a typed line the pane tty would truncate (#10127 Q2).
///
/// Test: `an_over_long_gui_line_is_refused`.
fn check_line_len(line: &str) -> anyhow::Result<()> {
    let max = crate::core::tmux::MAX_PANE_COMMAND_BYTES;
    if line.len() > max {
        anyhow::bail!(
            "refusing the GUI spawn: its line is {} bytes, over the {max}-byte pane limit, so \
             the tty would drop its tail (#10127)",
            line.len()
        );
    }
    Ok(())
}

/// Where the GUI line writes an Architect's `--settings` payload: beside the
/// prompt file, as `<prompt_file>.lockdown.json`.
fn lockdown_settings_file(prompt_file: &std::path::Path) -> std::path::PathBuf {
    let mut name = prompt_file.as_os_str().to_owned();
    name.push(".lockdown.json");
    std::path::PathBuf::from(name)
}

/// Compose and write the GUI spawn's PM prompt for `workdir`, then return the
/// line that hands it to `claude` (#8286).
///
/// Why: a GUI session used to start a bare `claude` with no PM instructions.
/// What: one [`crate::core::session_launch::cli_launch`] resolution gives the
/// prompt and the stamp; the prompt is written under `prompt_dir` (production:
/// the process temp dir). `Err` names the file, the I/O cause and `workdir`,
/// and the caller refuses the spawn on it. #8988: the owner `--settings` pair,
/// shell-quoted, ends the line when the workdir's owner has a policy — for an
/// Architect merged with the #10127 lockdown ([`gui_spawn_command_full`]). On
/// any error the files written for this line are removed.
/// Test: `gui_spawn_line_writes_the_prompt_it_names`,
/// `gui_spawn_line_refuses_when_the_prompt_file_cannot_be_written`.
pub(crate) fn gui_spawn_line(
    workdir: &std::path::Path,
    prompt_dir: &std::path::Path,
) -> anyhow::Result<String> {
    // #9012: the PM instructions are runtime content; none refuses the spawn.
    let content = crate::core::content_source::framework_content_for(workdir)
        .map_err(|err| anyhow::anyhow!("cannot compose the PM instructions: {err}"))?;
    let cli = crate::core::session_launch::cli_launch(&content, workdir, None);
    let file = crate::core::model_inject::write_pm_prompt_file_in(
        prompt_dir,
        &cli.prompt,
        workdir,
        "spawn",
    )?;
    // #8988: unrelocated, so the policy reads the operator's own `.claude.json`.
    // #8992: the profile decides whether the skill and tool parts apply.
    let settings =
        crate::core::launch_policy::provision_settings_default(workdir, None, cli.profile);
    let stamp = crate::core::session_profile::launch_env(cli.profile);
    gui_spawn_command_full(
        &file,
        &stamp,
        settings.file.as_deref(),
        &crate::core::architect_lockdown::Lockdown::PRODUCTION,
    )
    .inspect_err(|_| {
        // #10127: no prompt or payload file outlives a refused spawn.
        let _ = std::fs::remove_file(lockdown_settings_file(&file));
        let _ = std::fs::remove_file(&file);
    })
}

#[cfg(test)]
mod tests {
    use super::{gui_spawn_command, gui_spawn_command_with, gui_spawn_line, relaunch_command};
    use crate::core::architect_lockdown::Lockdown;

    /// #8286: the GUI spawn line scrubs the markers, stamps the profile and
    /// hands `claude` the prompt file as one shell word.
    #[test]
    fn gui_spawn_command_carries_the_prompt_file_and_the_stamp() {
        let stamp = ("TRUSTY_MPM_SESSION_PROFILE".to_owned(), "pm".to_owned());
        let cmd = gui_spawn_command(std::path::Path::new("/t/with space/p.txt"), &stamp)
            .expect("a PM line");
        assert!(cmd.starts_with("env -u "), "{cmd}");
        assert!(cmd.contains("-u CLAUDE_CODE_CHILD_SESSION"), "{cmd}");
        assert!(
            cmd.ends_with(
                " TRUSTY_MPM_SESSION_PROFILE='pm' CLAUDE_CODE_DISABLE_ARTIFACT='1' claude \
                 --append-system-prompt-file '/t/with space/p.txt' \
                 '--disallowedTools' 'Workflow,DesignSync'"
            ),
            "{cmd}"
        );
    }

    /// #8992: the GUI spawn of a PM carries the Artifact switch and the deny
    /// flag. Before the fix the line ended at the prompt file.
    #[test]
    fn gui_spawn_command_trims_a_pm() {
        let file = std::path::Path::new("/t/p.txt");
        let pm = gui_spawn_command(file, &("TRUSTY_MPM_SESSION_PROFILE".into(), "pm".into()))
            .expect("a PM line");
        assert!(
            pm.contains(" CLAUDE_CODE_DISABLE_ARTIFACT='1' claude "),
            "{pm}"
        );
        assert!(
            pm.ends_with(" '--disallowedTools' 'Workflow,DesignSync'"),
            "{pm}"
        );
    }

    /// The `permissions.deny` array and the connector switch of a payload file.
    fn payload(path: &std::path::Path) -> (Vec<String>, serde_json::Value) {
        let body: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).expect("payload file"))
                .expect("payload JSON");
        let deny = body["permissions"]["deny"]
            .as_array()
            .expect("permissions.deny")
            .iter()
            .map(|v| v.as_str().expect("a rule").to_owned())
            .collect();
        (deny, body)
    }

    /// #10127 review HIGH (GUI refusal regression): GUI New Session in an
    /// allow-listed Architect dir gets its line under a non-empty lockdown —
    /// the chat tokens unset ahead of the stamp, the deny in a `--settings`
    /// file. At ad893bba23 the Architect stamp was refused and this fails.
    #[test]
    fn gui_spawn_command_carries_an_architect_lockdown_instead_of_refusing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = tmp.path().join("p.txt");
        let lockdown = Lockdown::new(&["mcp__*slack*"], &["SLACK_BOT_TOKEN"]);
        for value in ["architect", "supervisor"] {
            let stamp = ("TRUSTY_MPM_SESSION_PROFILE".into(), value.into());
            let line = gui_spawn_command_with(&file, &stamp, &lockdown)
                .expect("an Architect GUI spawn is not refused");
            let unset = line.find(" -u SLACK_BOT_TOKEN").expect("token unset");
            assert!(unset < line.find(" TRUSTY_MPM_SESSION_PROFILE=").expect("stamp"));
            let settings = format!(" '--settings' '{}.lockdown.json'", file.display());
            assert!(line.ends_with(&settings), "{line}");
            let out = file.with_file_name("p.txt.lockdown.json");
            let (deny, body) = payload(&out);
            assert!(deny.contains(&"mcp__*slack*".to_owned()), "{deny:?}");
            assert!(deny.contains(&"mcp__*Slack*".to_owned()), "{deny:?}");
            assert_eq!(body["disableClaudeAiConnectors"], true);
            // #10127: the payload file is created new, so the next line needs its path free.
            std::fs::remove_file(&out).expect("remove the payload");
        }
        let pm = ("TRUSTY_MPM_SESSION_PROFILE".into(), "pm".into());
        let line = gui_spawn_command_with(&file, &pm, &lockdown).expect("a PM line");
        assert!(
            !line.contains("SLACK_BOT_TOKEN") && !line.contains("--settings"),
            "{line}"
        );
    }

    /// #10127: the production line merges the owner carrier file and every
    /// shipped glob into ONE `--settings`, unsets every shipped chat token,
    /// and stays under the pane limit for a long temp path.
    #[test]
    fn gui_spawn_line_carries_the_architect_lockdown() {
        use crate::core::architect_lockdown::{ARCHITECT_DENY_GLOBS, ARCHITECT_ENV_UNSET};
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("T".repeat(60));
        std::fs::create_dir(&dir).expect("prompt dir");
        let file = dir.join("tm-spawn-0123456789abcdef0123456789abcdef.txt");
        let carrier = tmp.path().join("owner.json");
        std::fs::write(&carrier, r#"{"deniedMcpServers":[{"serverName":"x"}]}"#).expect("carrier");
        let stamp = ("TRUSTY_MPM_SESSION_PROFILE".into(), "supervisor".into());
        let line =
            super::gui_spawn_command_full(&file, &stamp, Some(&carrier), &Lockdown::PRODUCTION)
                .expect("the Architect line builds");
        assert_eq!(line.matches("'--settings'").count(), 1, "{line}");
        assert!(
            line.len() <= crate::core::tmux::MAX_PANE_COMMAND_BYTES,
            "{}",
            line.len()
        );
        for name in ARCHITECT_ENV_UNSET {
            assert!(line.contains(&format!(" -u {name} ")), "{name}: {line}");
        }
        let (deny, body) = payload(&super::lockdown_settings_file(&file));
        assert_eq!(body["deniedMcpServers"][0]["serverName"], "x");
        for glob in ARCHITECT_DENY_GLOBS {
            assert!(deny.contains(&(*glob).to_owned()), "{glob} in {deny:?}");
        }
    }

    /// #10127 Fail-Open Check: a payload file that cannot be written refuses
    /// the Architect line with the write error; dropping the deny to launch
    /// anyway fails this. At ad893bba23 the refusal named no write and fails.
    #[test]
    fn an_unwritable_lockdown_file_refuses_the_gui_architect_line() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = tmp.path().join("missing-dir").join("p.txt");
        let stamp = ("TRUSTY_MPM_SESSION_PROFILE".into(), "supervisor".into());
        let lockdown = Lockdown::new(&["mcp__*slack*"], &[]);
        let err = gui_spawn_command_with(&file, &stamp, &lockdown)
            .expect_err("an unwritable payload refuses the line");
        assert!(
            err.to_string().contains("cannot write the --settings file"),
            "{err}"
        );
    }

    /// Assert a supervisor GUI line for prompt `file` under a non-empty
    /// lockdown is refused with the payload write error.
    fn assert_architect_line_refused(file: &std::path::Path) {
        let stamp = ("TRUSTY_MPM_SESSION_PROFILE".into(), "supervisor".into());
        let lockdown = Lockdown::new(&["mcp__*slack*"], &[]);
        let err = gui_spawn_command_with(file, &stamp, &lockdown)
            .expect_err("a planted payload path refuses the line");
        assert!(
            err.to_string().contains("cannot write the --settings file"),
            "{err}"
        );
    }

    /// #10127: a file already at the payload path refuses the Architect line
    /// and keeps its bytes. At de81bbab63 it was truncated and reused.
    #[test]
    fn a_planted_lockdown_file_refuses_the_gui_architect_line() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = tmp.path().join("p.txt");
        let out = super::lockdown_settings_file(&file);
        std::fs::write(&out, "planted").expect("plant");
        assert_architect_line_refused(&file);
        assert_eq!(std::fs::read_to_string(&out).expect("planted"), "planted");
    }

    /// #10127: a symlink at the payload path refuses the Architect line and
    /// leaves its target untouched. At de81bbab63 the write followed it.
    #[cfg(unix)]
    #[test]
    fn a_planted_lockdown_symlink_refuses_the_gui_architect_line() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = tmp.path().join("p.txt");
        let target = tmp.path().join("target");
        std::fs::write(&target, "target").expect("target");
        std::os::unix::fs::symlink(&target, super::lockdown_settings_file(&file)).expect("symlink");
        assert_architect_line_refused(&file);
        assert_eq!(std::fs::read_to_string(&target).expect("target"), "target");
    }

    /// #10127 Q2 Fail-Open Check: a line over the pane limit would lose its
    /// tail in the tty, the `--settings` with it, so it is refused. At
    /// ad893bba23 the over-long line was returned and this fails.
    #[test]
    fn an_over_long_gui_line_is_refused() {
        let file = std::path::PathBuf::from(format!("/{}/p.txt", "d".repeat(1000)));
        let pm = ("TRUSTY_MPM_SESSION_PROFILE".into(), "pm".into());
        let err = gui_spawn_command(&file, &pm).expect_err("an over-long line is refused");
        assert!(err.to_string().contains("pane limit"), "{err}");
    }

    /// #10127: under an empty lockdown an Architect's GUI line is the
    /// pre-#10127 line — no unset, no `--settings`.
    #[test]
    fn gui_spawn_command_launches_an_architect_under_the_empty_lockdown() {
        let file = std::path::Path::new("/t/p.txt");
        for value in ["architect", "supervisor"] {
            let stamp = ("TRUSTY_MPM_SESSION_PROFILE".into(), value.into());
            let line = gui_spawn_command_with(file, &stamp, &Lockdown::new(&[], &[]))
                .expect("not refused");
            assert!(
                line.ends_with(" claude --append-system-prompt-file '/t/p.txt'"),
                "{line}"
            );
        }
    }

    /// #8286: the line names the file the prompt was written to.
    #[test]
    fn gui_spawn_line_writes_the_prompt_it_names() {
        let project = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let line = gui_spawn_line(project.path(), dir.path()).unwrap();
        let written: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(written.len(), 1, "{written:?}");
        assert!(
            line.contains(&format!(
                "--append-system-prompt-file '{}'",
                written[0].display()
            )),
            "{line}"
        );
        assert!(
            !std::fs::read_to_string(&written[0])
                .unwrap()
                .trim()
                .is_empty()
        );
    }

    /// #8286: a prompt that cannot be written refuses the spawn.
    #[test]
    fn gui_spawn_line_refuses_when_the_prompt_file_cannot_be_written() {
        let project = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let not_a_dir = tmp.path().join("not-a-dir");
        std::fs::write(&not_a_dir, "").unwrap();
        let err = gui_spawn_line(project.path(), &not_a_dir)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains(&*not_a_dir.to_string_lossy()) && err.contains("refusing to spawn"),
            "{err}"
        );
    }

    /// #4467: the relaunch line must scrub the inherited session markers, or a
    /// config-restart silently loses the pane's transcript. Marker names are
    /// hard-coded so this cannot go vacuous if the shared list is emptied. The
    /// `claude` program itself must still terminate the line (#2010 — no
    /// absolute-path resolution here; that stays #2020's).
    #[test]
    fn relaunch_command_scrubs_inherited_session_markers() {
        let cmd = relaunch_command();
        assert!(
            cmd.starts_with("env -u "),
            "the relaunch line must carry an env scrub prefix: {cmd}"
        );
        for marker in [
            "CLAUDE_CODE_CHILD_SESSION",
            "CLAUDE_CODE_SESSION_ID",
            "CLAUDECODE",
            "CLAUDE_PID",
            "CLAUDE_EFFORT",
            "CLAUDE_CODE_EXECPATH",
        ] {
            assert!(
                cmd.contains(&format!("-u {marker}")),
                "relaunch must unset {marker}: {cmd}"
            );
        }
        assert!(
            cmd.ends_with(" claude"),
            "the line must still invoke `claude` (#2010): {cmd}"
        );
        assert!(
            !cmd.contains("-u CLAUDE_CONFIG_DIR"),
            "must never unset CLAUDE_CONFIG_DIR (#4455): {cmd}"
        );
    }
}
