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
/// composed for (#8453), then `claude --append-system-prompt-file '<file>'`.
/// Both values are single-quoted because the pane shell re-splits the line.
/// Test: `gui_spawn_command_carries_the_prompt_file_and_the_stamp`,
/// `pm_launch_builders_carry_the_prompt_file`.
pub(crate) fn gui_spawn_command(prompt_file: &std::path::Path, stamp: &(String, String)) -> String {
    use crate::core::spawn_disclaim::pane::shell_single_quote;
    format!(
        "env{} {}={} claude --append-system-prompt-file {}",
        crate::core::claude_env_scrub::env_unset_flags(),
        stamp.0,
        shell_single_quote(&stamp.1),
        shell_single_quote(&prompt_file.display().to_string())
    )
}

/// Compose and write the GUI spawn's PM prompt for `workdir`, then return the
/// line that hands it to `claude` (#8286).
///
/// Why: a GUI session used to start a bare `claude` with no PM instructions.
/// What: one [`crate::core::session_launch::cli_launch`] resolution gives the
/// prompt and the stamp; the prompt is written under `prompt_dir` (production:
/// the process temp dir). `Err` names the file, the I/O cause and `workdir`,
/// and the caller refuses the spawn on it.
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
    Ok(gui_spawn_command(
        &file,
        &crate::core::session_profile::launch_env(cli.profile),
    ))
}

#[cfg(test)]
mod tests {
    use super::{gui_spawn_command, gui_spawn_line, relaunch_command};

    /// #8286: the GUI spawn line scrubs the markers, stamps the profile and
    /// hands `claude` the prompt file as one shell word.
    #[test]
    fn gui_spawn_command_carries_the_prompt_file_and_the_stamp() {
        let stamp = ("TRUSTY_MPM_SESSION_PROFILE".to_owned(), "pm".to_owned());
        let cmd = gui_spawn_command(std::path::Path::new("/t/with space/p.txt"), &stamp);
        assert!(cmd.starts_with("env -u "), "{cmd}");
        assert!(cmd.contains("-u CLAUDE_CODE_CHILD_SESSION"), "{cmd}");
        assert!(
            cmd.ends_with(
                " TRUSTY_MPM_SESSION_PROFILE='pm' claude \
                 --append-system-prompt-file '/t/with space/p.txt'"
            ),
            "{cmd}"
        );
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
