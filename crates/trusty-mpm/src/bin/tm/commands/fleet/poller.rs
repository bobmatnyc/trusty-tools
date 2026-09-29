//! Start the Architect's deterministic poller (#8436 P4, ruling A).
//!
//! Why: the Architect wakes on the poller's pointer; an Architect without a
//! running poller is never woken and looks healthy. So a poller that cannot
//! start is a failed `init` step with its cause, never a warning beside an
//! ok report.
//! What: [`step`] does nothing under `--no-launch`. Otherwise it leaves a
//! poller already running in this directory alone, fails on one running
//! from another directory, and runs the deployed `scripts/start-fleet-poll.sh`
//! through [`start`], which checks the script's exit status and then that
//! tmux session [`POLL_SESSION`] runs.
//! Test: `a_failing_start_script_is_an_error_with_its_cause`,
//! `fleet_init_fails_closed_when_the_poller_does_not_start`.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, bail};

use super::{ARCHITECT_SESSION, Step, launch};

/// The poller's tmux session: `start-fleet-poll.sh`'s `<ARCHITECT_SESSION>-poll`.
pub(crate) const POLL_SESSION: &str = "tm-architect-poll";

/// The start script, relative to the Architect directory.
pub(crate) const START_SCRIPT: &str = "scripts/start-fleet-poll.sh";

/// How long a started poller must survive before `init` calls it running.
const SETTLE: Duration = Duration::from_secs(1);

/// The poller step of `tm fleet init` in `dir`; see the module doc.
///
/// Test: `fleet_init_launches_the_architect_and_status_is_complete`,
/// `fleet_init_fails_closed_when_the_poller_does_not_start`.
pub(crate) fn step(dir: &Path, launch: bool) -> Step {
    if !launch {
        return Step::Skipped(format!(
            "poller start (--no-launch); start it with `tm fleet init --dir {}`",
            dir.display()
        ));
    }
    match launch::session_dir(POLL_SESSION) {
        Some(running) if running == dir => {
            Step::Unchanged(format!("poller session {POLL_SESSION} is running"))
        }
        Some(other) => Step::Failed(format!(
            "poller: tmux session {POLL_SESSION} already runs in {}, not in this Architect; \
             stop it with `tmux kill-session -t ={POLL_SESSION}` and run `tm fleet init` again",
            other.display()
        )),
        None => match start(dir) {
            Ok(()) => Step::Changed(format!(
                "started poller session {POLL_SESSION} ({START_SCRIPT})"
            )),
            Err(err) => Step::Failed(format!("poller start: {err:#}")),
        },
    }
}

/// Run `dir`'s start script and confirm the poller session runs.
///
/// Why: the script's own exit status is not proof (a poller that exits at
/// once leaves nothing behind), so the session is checked after [`SETTLE`].
/// What: `bash <dir>/scripts/start-fleet-poll.sh` with the Architect's session,
/// the poller's session and `ARCHITECT_PROJECT_DIR` set, so the poller wakes
/// this Architect and measures this project's context. `Err` names the exit
/// status and the script's output, or the missing session.
/// Test: `a_failing_start_script_is_an_error_with_its_cause`,
/// `fleet_init_fails_closed_when_the_poller_does_not_start`.
pub(crate) fn start(dir: &Path) -> anyhow::Result<()> {
    let script = dir.join(START_SCRIPT);
    let out = Command::new("bash")
        .arg(&script)
        .current_dir(dir)
        .env("ARCHITECT_SESSION", ARCHITECT_SESSION)
        .env("ARCHITECT_POLL_SESSION", POLL_SESSION)
        .env("ARCHITECT_PROJECT_DIR", dir)
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
    if launch::session_dir(POLL_SESSION).is_none() {
        bail!(
            "{START_SCRIPT} exited 0 but tmux session {POLL_SESSION} is not running; \
             check that `python3` runs `scripts/fleet-poll.py`"
        );
    }
    Ok(())
}
