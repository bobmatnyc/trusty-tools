//! Start the Architect's session detached, and read it back (#8436).
//!
//! Why: `tm connect` names its tmux session through the daemon and then takes
//! over the terminal. The Architect needs a fixed name — the P1 poller sends
//! keys to `ARCHITECT_SESSION`, default `tm-architect` — and `tm fleet init`
//! must return, also when a PM runs it.
//! What: [`start`] runs `tm connect`'s preparation seams (framework deploy,
//! config-dir relocation, prompt, scoped MCP), refuses unless the profile
//! resolves to supervisor, creates `tm-architect` detached in the project and
//! types the `claude` line on the `opus` alias. It records the launch stamp in
//! the tmux session environment, where [`launch_stamp`] reads it, and records
//! the `claude` it started as the Architect's process ([`record_process`],
//! #8878 ruling A). No daemon registration: the session is not in `tm ls`
//! until #8536.
//! Test: `fleet_init_launches_the_architect_and_status_is_complete`.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use trusty_mpm::core::architect_launch;
use trusty_mpm::core::session_profile::{self, SESSION_PROFILE_ENV};
use trusty_mpm::core::tmux::{self, TmuxCommand, TmuxTarget};
use trusty_mpm::core::twin_identity::ArmingRecord;

/// The Architect's tmux session; the P1 poller's `ARCHITECT_SESSION` default.
pub(crate) const ARCHITECT_SESSION: &str = "tm-architect";

/// The directory the running `tm-architect` session was created in.
///
/// Why: status and the one-Architect check both need to know whether the
/// running session is THIS project's.
/// What: `None` when no session of that exact name runs; otherwise tmux's
/// `#{session_path}`, canonicalized when it can be.
/// Test: `fleet_init_launches_the_architect_and_status_is_complete`.
pub(crate) fn running_session_dir() -> Option<PathBuf> {
    let argv = [
        "display-message",
        "-p",
        "-t",
        &tmux::exact_window_target(ARCHITECT_SESSION),
        "#{session_path}",
    ]
    .map(str::to_owned);
    let out = tmux::run_tmux_argv(&argv).ok()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if !out.status.success() || text.is_empty() {
        return None;
    }
    let path = PathBuf::from(text);
    Some(std::fs::canonicalize(&path).unwrap_or(path))
}

/// The profile stamp recorded on the running `tm-architect` session, if any.
///
/// Why: the stamp itself lives in the `claude` process environment, which
/// `tm` cannot read portably; [`start`] records the same value in the tmux
/// session environment table.
/// What: `show-environment -t =tm-architect TRUSTY_MPM_SESSION_PROFILE`,
/// parsed from `KEY=value`; `None` when unset or no session runs.
/// Test: `fleet_init_launches_the_architect_and_status_is_complete`.
pub(crate) fn launch_stamp() -> Option<String> {
    let argv = tmux::show_environment_argv(Some(ARCHITECT_SESSION), SESSION_PROFILE_ENV);
    let out = tmux::run_tmux_argv(&argv).ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    text.trim()
        .strip_prefix(SESSION_PROFILE_ENV)
        .and_then(|rest| rest.strip_prefix('='))
        .map(str::to_owned)
}

/// Start the Architect's session in `dir`, detached.
///
/// Why: acceptance 1 of #8436 — a running supervisor session on the Opus
/// tier alias, with no manual follow-up.
/// What: see the module doc. Fails before creating the session when the
/// profile would resolve to PM; kills the session when the `claude` line
/// cannot be sent. `home` is the user home every write goes under. Returns
/// the [`record_process`] outcome.
/// Test: `fleet_init_launches_the_architect_and_status_is_complete`.
pub(crate) fn start(dir: &Path, home: &Path) -> anyhow::Result<Result<ArmingRecord, String>> {
    require_supervisor(dir)?;
    match trusty_mpm::core::session_launch::prepare_isolated_session_under(dir, None, Some(home)) {
        Ok(report) => {
            for err in &report.roster_errors {
                eprintln!(
                    "warning: roster provisioning gap for {}: {err}",
                    dir.display()
                );
            }
        }
        Err(err) if err.is_fatal() => bail!("{err}"),
        Err(err) => eprintln!(
            "warning: session prep failed for {} (non-fatal): {err}",
            dir.display()
        ),
    }
    let config_dir = crate::commands::launch_home::relocate_config_dir(dir, Some(home));
    let cli = trusty_mpm::core::session_launch::cli_launch(dir, None);
    if !cli.profile.is_supervisor() {
        bail!("{}", pm_refusal(dir));
    }
    let config = trusty_mpm::core::config::MpmConfig::load_effective_default(Some(dir));
    let model = session_profile::launch_model(cli.profile, &config);
    let prompt = trusty_mpm::core::model_inject::write_prompt_file(&cli.prompt)
        .context("failed to write the Architect's system prompt file")?;
    let scoped_mcp = crate::commands::launch_home::provision_session_mcp(
        dir,
        config_dir.as_deref(),
        Some(home),
    )?;
    let claude_cmd = trusty_mpm::core::spawn_disclaim::disclaim_pane_command(
        &crate::commands::launch::launch_claude_cmd(
            trusty_mpm::core::alt_screen::operator_config_root().as_deref(),
            &model,
            Some(prompt.as_path()),
            config_dir.as_deref(),
            &cli.env,
            scoped_mcp.as_deref(),
        ),
    );
    let workdir = dir
        .to_str()
        .context("the Architect directory is not valid UTF-8")?;
    let created = tmux::create_managed_session_exclusive(None, ARCHITECT_SESSION, Some(workdir))
        .with_context(|| format!("failed to run tmux to create {ARCHITECT_SESSION}"))?;
    if !created.output.status.success() {
        bail!(
            "tmux refused to create {ARCHITECT_SESSION}: {}",
            String::from_utf8_lossy(&created.output.stderr).trim()
        );
    }
    if let Err(err) = stamp_and_send(&cli.profile, &claude_cmd) {
        let _ = tmux::run_tmux(&TmuxCommand::KillSession {
            name: ARCHITECT_SESSION.to_owned(),
        });
        return Err(err);
    }
    Ok(record_process(dir, home))
}

/// Bind the Architect identity to the `claude` [`start`] just launched
/// (#8878, ruling A).
///
/// Why: the trust-anchor floor trusts a process record, not the environment,
/// and only this launch path may write one.
/// What: finds the `claude` under the pane shell of the `tm-architect`
/// session [`start`] created exclusively (one hop through the #2997 disclaim
/// wrapper), so the PID is always a process tm started, never the caller.
/// Then records its PID and start time under `home`'s `~/.trusty-mpm`. A
/// failure leaves the session running unbound — it cannot write the trust
/// anchors — and is reported, never fatal.
/// Test: `fleet_init_launches_the_architect_and_status_is_complete`.
fn record_process(dir: &Path, home: &Path) -> Result<ArmingRecord, String> {
    let pid = trusty_mpm::core::process::find_claude_pid_in_tmux(
        ARCHITECT_SESSION,
        20,
        std::time::Duration::from_millis(500),
    )
    .ok_or_else(|| format!("no `claude` process appeared in {ARCHITECT_SESSION} within 10 s"))?;
    architect_launch::record_architect(&home.join(".trusty-mpm"), pid, dir)
}

/// Record the stamp on the session, then type the `claude` line into it.
fn stamp_and_send(
    profile: &session_profile::SessionProfile,
    claude_cmd: &str,
) -> anyhow::Result<()> {
    let (key, value) = session_profile::launch_env(*profile);
    let set = tmux::run_tmux(&TmuxCommand::SetEnvironment {
        session: ARCHITECT_SESSION.to_owned(),
        key,
        value,
    })?;
    if !set.status.success() {
        bail!("failed to record the launch stamp on {ARCHITECT_SESSION}");
    }
    let sent = tmux::send_line(None, &TmuxTarget::session(ARCHITECT_SESSION), claude_cmd)?;
    if !sent.status.success() {
        bail!("{ARCHITECT_SESSION} was created but `claude` could not be started in it");
    }
    Ok(())
}

/// Fail unless a launch in `dir` resolves to the supervisor profile.
fn require_supervisor(dir: &Path) -> anyhow::Result<()> {
    if session_profile::resolve_ambient(dir).is_supervisor() {
        return Ok(());
    }
    bail!("{}", pm_refusal(dir))
}

/// Why a launch in `dir` would run the PM profile.
fn pm_refusal(dir: &Path) -> String {
    let config = trusty_mpm::core::config::MpmConfig::load_default();
    let reason = session_profile::refusal(dir, &config)
        .unwrap_or("the project does not request `profile = \"supervisor\"`");
    format!(
        "not starting {ARCHITECT_SESSION}: a launch in {} would run the PM profile ({reason})",
        dir.display()
    )
}
