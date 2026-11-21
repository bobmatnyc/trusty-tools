//! The one real relaunch: `fleet-poll.py`'s `run_architect_relaunch`.
//!
//! Why: the dead-Architect branch needs one relaunch, and it is the highest
//! blast-radius path in the poller (MB1252 R9). The plan keeps the Python
//! `session-relaunch.sh` and runs it as a subprocess; nothing here
//! reimplements it (#9779).
//! What: [`ScriptRelauncher::relaunch`] fails with no tmux call when the
//! script is not installed (the `fleet_poll_watchdog.py` guard); re-lists
//! panes and aborts when a live Architect pane exists; creates the detached
//! session in the Architect root when it is missing; runs
//! `session-relaunch.sh --rc <session>` through a [`ScriptRunner`] with
//! [`RELAUNCH_TIMEOUT`]; then confirms the pane left the shell.
//! Test: `test_relaunch_aborts_when_live`,
//! `test_relaunch_creates_missing_session`, `test_relaunch_fails_on_script_error`,
//! `test_relaunch_without_the_script_never_touches_tmux`,
//! `system_runner_reports_exit_codes_and_timeouts`.
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use super::{Relaunch, Relauncher};
use crate::poller::architect::is_shell;
use crate::poller::listing::list_panes;
use crate::tmux::Tmux;

/// How long `session-relaunch.sh` may run.
pub const RELAUNCH_TIMEOUT: Duration = Duration::from_secs(300);

/// How a script run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptExit {
    /// The script exited with this code (`-1` when a signal ended it).
    Code(i32),
    /// The script outran the timeout and was killed.
    TimedOut,
}

/// Runs the relaunch script; injected so tests never start a process.
pub trait ScriptRunner {
    /// Runs `script args...`, discarding its output.
    ///
    /// # Errors
    /// The process could not be started or waited on.
    fn run(&self, script: &Path, args: &[&str], timeout: Duration) -> io::Result<ScriptExit>;
}

/// The real [`ScriptRunner`]: spawn, poll for exit, kill at the timeout.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemScriptRunner;

impl ScriptRunner for SystemScriptRunner {
    fn run(&self, script: &Path, args: &[&str], timeout: Duration) -> io::Result<ScriptExit> {
        let mut child = Command::new(script)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let start = Instant::now();
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok(ScriptExit::Code(status.code().unwrap_or(-1)));
            }
            if start.elapsed() >= timeout {
                // A kill error means it exited meanwhile; reap either way.
                let _ = child.kill();
                let _ = child.wait();
                return Ok(ScriptExit::TimedOut);
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
}

/// Relaunches the Architect through `session-relaunch.sh`.
pub struct ScriptRelauncher<'a> {
    /// The tmux server holding the Architect.
    pub tmux: &'a dyn Tmux,
    /// Runs the script.
    pub runner: &'a dyn ScriptRunner,
    /// `<root>/scripts/session-relaunch.sh`.
    pub script: PathBuf,
    /// The Architect's tmux session.
    pub session: String,
    /// The Architect project directory: a new session starts there.
    pub root: PathBuf,
    /// How long the script may run.
    pub timeout: Duration,
}

impl ScriptRelauncher<'_> {
    /// `(has a pane in the session, has a live one)`.
    fn live(&self) -> Result<(bool, bool), String> {
        let listing = list_panes(self.tmux).map_err(|e| format!("TmuxError: {e}"))?;
        let mine: Vec<_> = listing
            .panes
            .iter()
            .filter(|p| p.session == self.session)
            .collect();
        Ok((!mine.is_empty(), mine.iter().any(|p| !is_shell(&p.cmd))))
    }

    fn attempt(&self) -> Result<Relaunch, String> {
        let (any, alive) = self.live()?;
        if alive {
            return Ok(Relaunch::Aborted("a live claude process exists".to_owned()));
        }
        if !any {
            let root = self.root.to_string_lossy();
            self.tmux
                .run(&["new-session", "-d", "-s", &self.session, "-c", &root])
                .map_err(|e| format!("TmuxError: {e}"))?;
        }
        let exit = self
            .runner
            .run(&self.script, &["--rc", &self.session], self.timeout)
            .map_err(|e| format!("OSError: {e}"))?;
        match exit {
            ScriptExit::TimedOut => Ok(Relaunch::Failed(format!(
                "timeout after {}s",
                self.timeout.as_secs()
            ))),
            ScriptExit::Code(0) if self.live()?.1 => {
                Ok(Relaunch::Ok("session-relaunch.sh exit 0".to_owned()))
            }
            ScriptExit::Code(0) => Ok(Relaunch::Failed(
                "pane still at a shell after relaunch".to_owned(),
            )),
            ScriptExit::Code(c) => Ok(Relaunch::Failed(format!("session-relaunch.sh exit {c}"))),
        }
    }
}

impl Relauncher for ScriptRelauncher<'_> {
    fn relaunch(&self) -> Relaunch {
        if !self.script.is_file() {
            let name = self
                .script
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
            return Relaunch::Failed(format!("{name} is not installed"));
        }
        self.attempt().unwrap_or_else(Relaunch::Failed)
    }
}
