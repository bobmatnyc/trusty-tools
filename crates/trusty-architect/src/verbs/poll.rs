//! `tm architect poll`: the port of `fleet-poll.py`'s command line.
//!
//! Why: `start-fleet-poll.sh` runs the poller in its own tmux session with
//! `fleet-poll.py [--once] [--interval N] [--dry-run]` (#9822).
//! What: [`poll_config`] builds the poller [`Config`] from the environment,
//! with the shipped scripts' names: `ARCHITECT_INBOX_DIR` (which outranks
//! `SUPERVISOR_INBOX_DIR`, as in the script), `ARCHITECT_QUIET_SESSIONS_FILE` for the quiet
//! list, `ARCHITECT_SESSION` (default `tm-architect`) for the one pane the
//! poller wakes and `ARCHITECT_POLL_SESSION` (default `<that>-poll`) for its
//! own session; `tm fleet init --session` sets both. [`poll`] runs the
//! loop on the real tmux, clock and host: `--once` and `--dry-run` run one
//! cycle through [`Runner::run_single`], which fails where the script logs
//! and exits 0.
//! Test: `poll_config_reads_the_shipped_names`,
//! `run_single_fails_closed_when_tmux_cannot_list_panes` (tests/verbs_poll.rs).

use std::path::{Path, PathBuf};

use super::VerbError;
use crate::clock::SystemClock;
use crate::error::RunError;
use crate::poller::{Config, Runner, SUPERVISOR, SystemHostProbe, WakeAll};
use crate::tmux::SystemTmux;

/// `fleet-poll.py`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollArgs {
    /// `--once`: one cycle, then exit.
    pub once: bool,
    /// `--interval N`, as typed: seconds between cycles (default `60`).
    pub interval: String,
    /// `--dry-run`: one cycle that prints, writes no file and sends no keys.
    pub dry_run: bool,
}

/// The poller configuration for the Architect in `root`; see the module doc.
///
/// # Errors
/// [`VerbError::Usage`] when `ARCHITECT_SESSION` or `ARCHITECT_POLL_SESSION`
/// is not a tmux session name (it holds `:`, `.` or whitespace).
pub fn poll_config(
    root: &Path,
    home: &Path,
    get: &dyn Fn(&str) -> Option<String>,
) -> Result<Config, VerbError> {
    let set = |name: &str| get(name).filter(|v| !v.is_empty());
    let supervisor = set("ARCHITECT_SESSION").unwrap_or_else(|| SUPERVISOR.to_owned());
    let poll_session =
        set("ARCHITECT_POLL_SESSION").unwrap_or_else(|| format!("{supervisor}-poll"));
    for (name, value) in [
        ("ARCHITECT_SESSION", &supervisor),
        ("ARCHITECT_POLL_SESSION", &poll_session),
    ] {
        if value.contains(|c: char| c == ':' || c == '.' || c.is_whitespace()) {
            return Err(VerbError::Usage(format!(
                "{name}={value:?} is not a tmux session name"
            )));
        }
    }
    let lookup = |name: &str| match name {
        // #9822: fleet-poll.py reads only ARCHITECT_INBOX_DIR, so it wins.
        "SUPERVISOR_INBOX_DIR" => set("ARCHITECT_INBOX_DIR").or_else(|| set(name)),
        _ => get(name),
    };
    let mut config =
        Config::from_env(root, home, &lookup).map_err(|e| VerbError::Usage(e.to_string()))?;
    if let Some(file) = set("ARCHITECT_QUIET_SESSIONS_FILE") {
        config.quiet_sessions = PathBuf::from(file);
    }
    config.supervisor = supervisor;
    config.poll_session = poll_session;
    Ok(config)
}

/// Runs the poller as `fleet-poll.py` would with `args`; see the module doc.
///
/// # Errors
/// [`VerbError::Usage`] for an interval that is not a positive whole number;
/// [`VerbError::Failed`] when another poller holds the lock, the inbox cannot
/// be written, or a single run's cycle fails or is incomplete.
pub fn poll(args: &PollArgs, config: &Config) -> Result<(), VerbError> {
    let tmux = config
        .tmux_socket
        .as_ref()
        .map_or_else(SystemTmux::new, SystemTmux::with_socket);
    let runner = Runner {
        tmux: &tmux,
        clock: &SystemClock,
        probe: &SystemHostProbe,
        triage: &WakeAll,
        config,
        dry_run: args.dry_run,
    };
    let result = if args.once || args.dry_run {
        runner.run_single(&args.interval).map(drop)
    } else {
        runner.run_loop(&args.interval, None)
    };
    result.map_err(run_error)
}

/// A bad interval is a usage error, as argparse's `type=int` makes it.
fn run_error(e: RunError) -> VerbError {
    match e {
        RunError::Config(c) => VerbError::Usage(c.to_string()),
        other => VerbError::Failed(other.to_string()),
    }
}
