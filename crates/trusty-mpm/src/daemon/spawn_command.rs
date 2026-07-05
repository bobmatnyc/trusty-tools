//! Shared Claude Code launch-line builder for ad hoc (non-managed) tmux spawns.
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
//! This deliberately does NOT reuse the managed-session builder in
//! [`crate::runtime::claude_code`] (its private `spawn_command`, which
//! resolves an absolute `claude` binary, scrubs `ANTHROPIC_API_KEY` via
//! `env_bin_prefix`, and injects `CLAUDE_CONFIG_DIR` plus the
//! `--setting-sources` / `--dangerously-skip-permissions` isolation flags).
//! That builder targets a brand-new managed session whose pane may inherit a
//! minimal launchd `PATH`. Both call sites here instead relaunch `claude`
//! inside a pane that already has a normal interactive shell environment: the
//! GUI bootstrap creates a fresh interactive tmux host, and the config-restart
//! relaunches inside the *same* pane the operator was already attached to.
//! Neither needs absolute-binary resolution or env-scrubbing — so the shared
//! builder here stays a bare `claude` invocation, matching prior behavior
//! exactly. If either flow's requirements ever diverge from the other, add
//! parameters to [`spawn_command`] rather than re-duplicating construction at
//! the call sites.
//! What: [`spawn_command`] returns the literal shell command sent to the pane.
//! Test: `spawn_command_returns_bare_claude`.

/// The shell command used to (re)launch `claude` inside an already-running,
/// already-configured tmux pane.
///
/// Why: see the module doc — both the spawn-mode bootstrap
/// ([`crate::daemon::services::tmux_service::TmuxService::spawn_claude`]) and
/// the config-restart flow
/// ([`crate::daemon::claude_config::restarter::ClaudeCodeRestarter::restart_in_session`])
/// must send the identical launch line so the two can never drift apart
/// (#2010).
/// What: returns the literal `"claude"` command — the pane inherits an
/// interactive shell's `PATH` and environment, so no absolute-path resolution
/// or env scrubbing is needed here (contrast with the managed-session builder
/// in [`crate::runtime::claude_code`]).
/// Test: `spawn_command_returns_bare_claude`.
pub(crate) fn spawn_command() -> &'static str {
    "claude"
}

#[cfg(test)]
mod tests {
    use super::spawn_command;

    /// The shared builder must keep returning the bare `claude` literal that
    /// both call sites relied on before this consolidation (#2010) — this
    /// change removes the drift risk, not the behavior.
    #[test]
    fn spawn_command_returns_bare_claude() {
        assert_eq!(spawn_command(), "claude");
    }
}
