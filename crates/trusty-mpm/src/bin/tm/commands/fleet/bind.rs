//! `tm fleet bind`: re-bind a running Architect `claude` (#8878 PR2a).
//!
//! Why: plan.md §2(b) — an Architect `claude` that no tm launch path recorded
//! (the PR1 incident, claude 58941) is refused its anchor writes, and PR1
//! binds only a `claude` a launch just started. Re-binding a running one is
//! deferred to this verb, gated on presence, so a PM cannot self-promote.
//! What: [`bind`] runs from the Architect's own session. It refuses unless
//! the caller passes every identity check but the launch record
//! ([`session_binding`] fails only with no record or a reused PID's record),
//! and unless the `claude` under the caller's own tmux pane is the `claude`
//! above the Bash shell. It then asks for presence ([`prompt`]), records the
//! `claude` through `bind_architect` (the one record writer), and mints its
//! grant with the same answer. An already-bound session changes nothing.
//! Test: `bind_tests.rs` beside this file.

use std::path::Path;

use anyhow::{Context as _, bail};
use trusty_mpm::core::architect_launch::LaunchRefusal;
use trusty_mpm::core::architect_session::bind_architect;
use trusty_mpm::core::config::MpmConfig;
use trusty_mpm::core::presence_key::KeySource;
use trusty_mpm::core::presence_local::Authenticator;
use trusty_mpm::core::session_profile::hook_project_dir;

use crate::commands::pm_guard_architect_reason::{NotArchitect, session_binding};
use crate::commands::pm_guard_trust_anchor::HookEnv;
use crate::commands::presence::{mint_local, prompt, this_claude};

/// The caller's tmux pane: its session name and the `claude` under it.
pub(crate) struct Pane {
    /// The tmux session the pane belongs to.
    pub(crate) session: String,
    /// The `claude` under the pane's shell, if any.
    pub(crate) claude: Option<u32>,
}

/// Bind the `claude` running this command as the Architect, after presence.
///
/// Why/What: see the module doc. `pane` reads the caller's own pane;
/// `root` is `~/.trusty-mpm`. Returns the line to print.
/// Test: `a_pm_calling_fleet_bind_is_refused_before_any_prompt_8878`,
/// `fleet_bind_records_and_grants_after_presence_8878`,
/// `fleet_bind_refuses_a_claude_that_is_not_the_panes_8878`.
pub(crate) fn bind(
    env: &HookEnv,
    config: impl FnOnce() -> MpmConfig,
    auth: &dyn Authenticator,
    source: &KeySource,
    root: &Path,
    pane: impl FnOnce() -> anyhow::Result<Pane>,
) -> anyhow::Result<String> {
    match session_binding(env, config) {
        Ok(()) => return Ok("This session's `claude` is already bound; nothing changed.".into()),
        Err(NotArchitect::Launch(
            LaunchRefusal::NoLaunchRecord | LaunchRefusal::StartTimeMismatch,
        )) => {}
        Err(why) => bail!("refused: this session cannot be bound as the Architect: {why}"),
    }
    let claude = this_claude(env)?;
    let pane = pane()?;
    if pane.claude != Some(claude.pid) {
        bail!(
            "refused: the `claude` under this tmux pane is not the one running this command; run \
             `tm fleet bind` from the Architect's own pane"
        );
    }
    let project = hook_project_dir(env.project_dir.clone())
        .context("`CLAUDE_PROJECT_DIR` is unset or empty")?;
    prompt(
        auth,
        &format!(
            "bind claude pid {} in tmux session {} as the Architect",
            claude.pid, pane.session
        ),
    )?;
    bind_architect(root, claude.pid, &project, &pane.session).map_err(anyhow::Error::msg)?;
    let grant = mint_local(root, &claude, source)?;
    Ok(format!(
        "Bound claude pid {} in {} as the Architect; presence granted ({}).",
        claude.pid,
        pane.session,
        grant.display()
    ))
}

/// The live pane: `TMUX_PANE`'s session and the `claude` under its shell.
pub(crate) fn live_pane() -> anyhow::Result<Pane> {
    let raw = std::env::var("TMUX_PANE")
        .ok()
        .filter(|p| p.starts_with('%'))
        .context("not inside a tmux pane (`TMUX_PANE` is unset)")?;
    // #8443: a `%N` id passes through unchanged; anything else renders exact.
    let pane = trusty_common::tmux::exact_window_target(&raw);
    let argv = [
        "display-message",
        "-t",
        &pane,
        "-p",
        "#{session_name}\t#{pane_pid}",
    ]
    .map(String::from);
    let out = trusty_mpm::core::tmux::run_tmux_argv(&argv).context("tmux display-message")?;
    let text = String::from_utf8_lossy(&out.stdout);
    let (session, shell) = text
        .trim_end()
        .split_once('\t')
        .filter(|_| out.status.success())
        .context("tmux did not name this pane's session")?;
    let shell: u32 = shell.parse().context("tmux printed no pane PID")?;
    Ok(Pane {
        session: session.to_owned(),
        claude: trusty_mpm::core::process::claude_child_of(shell),
    })
}

#[cfg(test)]
#[path = "bind_tests.rs"]
mod tests;
