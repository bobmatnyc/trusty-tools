//! `tm architect serve`: the poll cycle, run on local events (#9779).
//!
//! Why: the same cycle as `tm architect poll`, started by an inbox write, a
//! tmux pane's activity or a transcript append within seconds instead of
//! at the next 60 s interval (plan section 2).
//! What: [`serve`] installs the SIGTERM/SIGINT stop flag, builds the
//! production [`crate::poller::Runner`] through the helper `poll` uses, and
//! runs [`ServeLoop::for_config`] through [`serve_runner`], which takes
//! `poll.lock` (not in `--shadow`) for the service's life. `--once` runs the
//! start cycle and exits, failing as a failed cycle. The least gap between
//! event cycles is the config's `serve_min_gap` (`SERVE_MIN_EVENT_GAP`).
//! Test: `tests/serve_runner.rs` (the runner path), trusty-mpm's
//! `cli_parses_architect_verbs` (the verb).

use std::time::Duration;

use super::VerbError;
use super::poll::{run_error, with_live_runner};
use crate::clock::{Clock, SystemClock};
use crate::poller::{Config, MessageSink, append_log};
use crate::serve::{ServeLoop, ServeOptions, install_stop_handlers, serve_runner};

/// `tm architect serve`'s flags.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ServeArgs {
    /// Writes only `<inbox>/shadow/`, takes no lock, sends no keys,
    /// relaunches, notifies and pushes nothing (as `poll --shadow`).
    pub shadow: bool,
    /// One cycle, then exit.
    pub once: bool,
}

/// Runs the service; see the module doc.
///
/// # Errors
/// [`VerbError::Failed`] when the handlers cannot be installed, another
/// poller holds the lock, the inbox cannot be written, or a `--once` cycle
/// fails.
pub fn serve(
    args: &ServeArgs,
    config: &Config,
    messages: &dyn MessageSink,
) -> Result<(), VerbError> {
    let mut config = config.clone();
    config.shadow = args.shadow;
    let stop = install_stop_handlers()
        .map_err(|e| VerbError::Failed(format!("cannot install signal handlers: {e}")))?;
    let log_path = config.log();
    let log = move |line: &str| {
        if let Err(e) = append_log(&log_path, SystemClock.now_unix(), line) {
            tracing::warn!(error = %e, line, "serve log append failed");
        }
    };
    let defaults = ServeOptions::default();
    let opts = ServeOptions {
        once: args.once,
        // #9779 review: a busy fleet cycles at most once per this gap.
        min_gap: Duration::try_from_secs_f64(config.serve_min_gap).unwrap_or(defaults.min_gap),
        ..defaults
    };
    with_live_runner(&config, false, messages, &mut |runner| {
        let tmux = runner.tmux.clone();
        let mut lp = ServeLoop::for_config(runner.config, tmux, stop, opts, &log);
        serve_runner(runner, &mut lp).map(drop)
    })
    .map_err(run_error)
}
