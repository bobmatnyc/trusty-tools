//! Start the Architect's deterministic poller (#8436 P4, ruling A).
//!
//! Why: the Architect wakes on the poller's pointer; an Architect without a
//! running poller is never woken and looks healthy. So a poller that cannot
//! start is a failed `init` step with its cause, never a warning beside an
//! ok report.
//! What: [`step`] does nothing under `--no-launch`. Otherwise it leaves a
//! poller already running in this directory alone, and fails on one running
//! from another directory, on a dead pane, and on a tmux answer it cannot
//! read. It runs the deployed `scripts/start-fleet-poll.sh` through [`start`],
//! which checks the script's exit status and then that tmux session
//! [`POLL_SESSION`] runs with a live pane. tmux is read through a [`Probe`].
//! Test: `a_failing_start_script_is_an_error_with_its_cause`,
//! `a_dead_or_unreadable_poller_pane_is_a_failed_step`,
//! `fleet_init_fails_closed_when_the_poller_does_not_start`,
//! `fleet_init_fails_when_the_poller_pane_is_dead`.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, bail};

use super::launch::PaneState;
use super::{ARCHITECT_SESSION, Probe, Step};

/// The poller's tmux session: `start-fleet-poll.sh`'s `<ARCHITECT_SESSION>-poll`.
pub(crate) const POLL_SESSION: &str = "tm-architect-poll";

/// The start script, relative to the Architect directory.
pub(crate) const START_SCRIPT: &str = "scripts/start-fleet-poll.sh";

/// How long a started poller must survive before `init` calls it running.
const SETTLE: Duration = Duration::from_secs(1);

/// The poller step of `tm fleet init` in `dir`; see the module doc.
///
/// Test: `fleet_init_launches_the_architect_and_status_is_complete`,
/// `a_dead_or_unreadable_poller_pane_is_a_failed_step`,
/// `fleet_init_fails_when_the_poller_pane_is_dead`.
pub(crate) fn step(dir: &Path, launch: bool, probe: Probe) -> Step {
    if !launch {
        return Step::Skipped(format!(
            "poller start (--no-launch); start it with `tm fleet init --dir {}`",
            dir.display()
        ));
    }
    // #8436 P4 fix: the pane, not the session, says whether the poller runs.
    match (probe.pane)(POLL_SESSION) {
        PaneState::Live(other) | PaneState::Dead(other) if other != dir => Step::Failed(format!(
            "poller: tmux session {POLL_SESSION} already runs in {}, not in this Architect; \
                 stop it with `tmux kill-session -t ={POLL_SESSION}` and run `tm fleet init` \
                 again",
            other.display()
        )),
        PaneState::Live(_) => Step::Unchanged(format!("poller session {POLL_SESSION} is running")),
        PaneState::Dead(_) => Step::Failed(dead_pane()),
        PaneState::Unknown(err) => Step::Failed(format!(
            "poller: cannot read tmux session {POLL_SESSION}: {err}"
        )),
        PaneState::Absent => match start(dir, probe) {
            Ok(()) => Step::Changed(format!(
                "started poller session {POLL_SESSION} ({START_SCRIPT})"
            )),
            Err(err) => Step::Failed(format!("poller start: {err:#}")),
        },
    }
}

/// The FAILED text for a poller session whose pane is dead.
fn dead_pane() -> String {
    format!(
        "poller: tmux session {POLL_SESSION} exists but its pane is dead (the poller exited); \
         read its last output with `tmux capture-pane -p -t ={POLL_SESSION}:`, stop it with \
         `tmux kill-session -t ={POLL_SESSION}` and run `tm fleet init` again"
    )
}

/// Run `dir`'s start script and confirm the poller runs.
///
/// Why: the script's own exit status is not proof (a poller that exits at
/// once leaves nothing behind, or a dead pane), so the pane is checked after
/// [`SETTLE`].
/// What: runs [`start_command`]. `Err` names the exit status and the script's
/// output, a missing session, a dead pane, or a tmux answer it cannot read.
/// Test: `a_failing_start_script_is_an_error_with_its_cause`,
/// `fleet_init_fails_closed_when_the_poller_does_not_start`,
/// `fleet_init_fails_when_the_poller_pane_is_dead`.
pub(crate) fn start(dir: &Path, probe: Probe) -> anyhow::Result<()> {
    let script = dir.join(START_SCRIPT);
    let out = start_command(dir)
        .output()
        .with_context(|| format!("cannot run {}", script.display()))?;
    if !out.status.success() {
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        bail!("{START_SCRIPT} failed ({}): {}", out.status, text.trim());
    }
    std::thread::sleep(SETTLE);
    match (probe.pane)(POLL_SESSION) {
        PaneState::Live(_) => Ok(()),
        PaneState::Dead(_) => bail!("{START_SCRIPT} exited 0, but {}", dead_pane()),
        PaneState::Unknown(err) => {
            bail!("{START_SCRIPT} exited 0, but tmux session {POLL_SESSION} cannot be read: {err}")
        }
        PaneState::Absent => bail!(
            "{START_SCRIPT} exited 0 but tmux session {POLL_SESSION} is not running; \
             check that `python3` runs `scripts/fleet-poll.py`"
        ),
    }
}

/// The `bash <dir>/scripts/start-fleet-poll.sh` command [`start`] runs.
///
/// What: sets the Architect's session, the poller's session and
/// `ARCHITECT_PROJECT_DIR`, so the poller wakes this Architect and measures
/// this project's context. Removes `TMUX_SOCKET`: the script's tmux would
/// honour it, and tm's own tmux calls, which read the session back, do not.
/// Test: `the_start_command_drops_tmux_socket`.
pub(crate) fn start_command(dir: &Path) -> Command {
    let mut cmd = Command::new("bash");
    cmd.arg(dir.join(START_SCRIPT))
        .current_dir(dir)
        .env("ARCHITECT_SESSION", ARCHITECT_SESSION)
        .env("ARCHITECT_POLL_SESSION", POLL_SESSION)
        .env("ARCHITECT_PROJECT_DIR", dir)
        // #8436 P4 fix: one tmux server for the script and for tm.
        .env_remove("TMUX_SOCKET");
    cmd
}
