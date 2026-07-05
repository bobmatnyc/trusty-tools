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
//! What: [`relaunch_command`] returns the literal shell command sent to the
//! pane.
//! Test: `relaunch_command_returns_bare_claude`.

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
/// What: returns the literal `"claude"` command. This intentionally carries
/// none of the managed-session isolation in
/// [`crate::runtime::claude_code`] (no absolute-path resolution, no
/// `ANTHROPIC_API_KEY` scrub, no `CLAUDE_CONFIG_DIR`) — see the module doc's
/// KNOWN LIMITATION note for why that is currently unverified rather than
/// deliberately safe for every caller of `restart_in_session`.
/// Test: `relaunch_command_returns_bare_claude`.
pub(crate) fn relaunch_command() -> &'static str {
    "claude"
}

#[cfg(test)]
mod tests {
    use super::relaunch_command;

    /// The shared builder must keep returning the bare `claude` literal that
    /// both call sites relied on before this consolidation (#2010) — this
    /// change removes the drift risk, not the behavior.
    #[test]
    fn relaunch_command_returns_bare_claude() {
        assert_eq!(relaunch_command(), "claude");
    }
}
