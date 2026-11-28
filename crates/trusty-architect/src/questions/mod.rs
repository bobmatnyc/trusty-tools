//! The questions session: Remote Control that follows the open interactive
//! questions, and the launcher that queues ids for it (#12029 P2f).
//!
//! Why: ports `questions_rc.py` and `start-architect-questions.sh`, frozen at
//! architect commit `0fbbc81eed`. Interactive dashboard items are answered in
//! a separate tmux session, never in the Architect's own; that session needs
//! Remote Control on only while a question waits.
//! What: [`rc`] is `questions_rc.py` (count, decide, toggle, settle, state
//! file); [`start`] is the shell launcher (queue append, prompt or launch,
//! `relaunch.log` line); [`cases`] mirrors the replay driver. Both reach tmux
//! through [`TmuxCli`]: [`TmuxCli::live`] takes the binary from a fixed
//! directory list and never reads `PATH`; every other constructor carries a
//! private server socket, and only [`TmuxCli::on_path_with_socket`] (the
//! replay arm) looks `tmux` up on `PATH`.
//! Fail-closed where the frozen scripts are not (#12029 P2f, requirement 4): a
//! tmux call that cannot run, times out, or (for `capture-pane` and
//! `new-session`) exits non-zero, and a failed write, return an error and
//! leave the state file, queue and `relaunch.log` as they were. `has-session`
//! exiting non-zero is the "no session" answer, not a failure.
//! Test: `tests/replay_questions_rc.rs`, `tests/questions_live.rs`.

pub mod cases;
pub mod rc;
pub mod start;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::process::{RunFailure, run_bounded};

/// A step of the questions ports that could not complete.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum QuestionsError {
    /// A tmux call could not start, timed out, or failed where an answer was needed.
    #[error("tmux {verb}: {detail}")]
    Tmux {
        /// The tmux verb.
        verb: String,
        /// What went wrong.
        detail: String,
    },
    /// A helper script or command could not start or timed out.
    #[error("{program}: {detail}")]
    Command {
        /// The program as invoked.
        program: String,
        /// What went wrong.
        detail: String,
    },
    /// A file write failed; nothing after it ran.
    #[error("cannot write {path}: {source}")]
    Write {
        /// The path.
        path: String,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },
    /// A read the frozen script lets raise (a store error, a queue that is not UTF-8).
    #[error("{0}")]
    Read(String),
    /// No tmux binary in any of [`LIVE_TMUX`].
    #[error("no tmux binary at any of {0}")]
    NoTmux(String),
}

/// Where the live ports look for tmux, in order. `PATH` is never consulted,
/// so a directory put first on `PATH` cannot stand in for tmux (#12029 P2f C4).
pub const LIVE_TMUX: [&str; 4] = [
    "/opt/homebrew/bin/tmux",
    "/usr/local/bin/tmux",
    "/usr/bin/tmux",
    "/bin/tmux",
];

/// How long one tmux call may run: the crate's [`crate::TMUX_TIMEOUT`].
const TMUX_CALL: Duration = crate::TMUX_TIMEOUT;

/// A tmux server socket other than the default one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Socket {
    /// `-L <name>`: a named socket in tmux's socket directory.
    Name(String),
    /// `-S <path>`: a socket at this path.
    Path(PathBuf),
}

/// An exact tmux target: `=<session>` or `=<session>:` (its active window).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExactTarget(String);

impl ExactTarget {
    /// `=<name>`: the session named exactly `name` (#8443).
    pub(crate) fn session(name: &str) -> Self {
        Self(format!("={name}"))
    }

    /// `=<name>:`: the active window of that session.
    pub(crate) fn active_window(name: &str) -> Self {
        Self(format!("={name}:"))
    }

    /// The `-t` value.
    pub(crate) fn as_target(&self) -> &str {
        &self.0
    }
}

/// How the questions ports run tmux.
///
/// Why: the frozen scripts run `tmux` by `PATH`, so any directory put first
/// on `PATH` would answer for the fleet's tmux. The port separates the two
/// uses (#12029 P2f, tmux rules C2 and C4).
/// What: argv is `<program> [-L name | -S path] <args>`, run with stdin closed
/// and a 10 s bound. [`TmuxCli::live`] resolves an absolute binary from
/// [`LIVE_TMUX`] and uses tmux's own server resolution, as the scripts do;
/// [`TmuxCli::private`] and [`TmuxCli::on_path_with_socket`] (a `PATH`-resolved
/// `tmux`, the replay arm) always carry a private socket.
/// Test: `live_options_never_resolve_tmux_through_path`,
/// `questions_replay_tmux_calls_use_the_private_socket`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxCli {
    program: PathBuf,
    socket: Option<Socket>,
}

impl TmuxCli {
    /// The live tmux: the first of [`LIVE_TMUX`] that is a file.
    ///
    /// # Errors
    /// [`QuestionsError::NoTmux`] when none is.
    pub fn live() -> Result<Self, QuestionsError> {
        Self::first_of(&LIVE_TMUX)
    }

    /// The first of `candidates` that is a file, with no socket.
    fn first_of(candidates: &[&str]) -> Result<Self, QuestionsError> {
        candidates
            .iter()
            .map(Path::new)
            .find(|p| p.is_file())
            .map(|p| Self {
                program: p.to_path_buf(),
                socket: None,
            })
            .ok_or_else(|| QuestionsError::NoTmux(candidates.join(", ")))
    }

    /// `program` on the private server `socket`.
    pub fn private(program: impl Into<PathBuf>, socket: Socket) -> Self {
        Self {
            program: program.into(),
            socket: Some(socket),
        }
    }

    /// `tmux` looked up on `PATH`, always on `socket`: the replay arm.
    pub fn on_path_with_socket(socket: Socket) -> Self {
        Self::private("tmux", socket)
    }

    /// The program this runner spawns.
    pub fn program(&self) -> &Path {
        &self.program
    }

    /// The socket this runner passes, if any.
    pub fn socket(&self) -> Option<&Socket> {
        self.socket.as_ref()
    }

    /// The command for `args`; building it spawns nothing.
    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(&self.program);
        match &self.socket {
            Some(Socket::Name(n)) => cmd.arg("-L").arg(n),
            Some(Socket::Path(p)) => cmd.arg("-S").arg(p),
            None => &mut cmd,
        };
        cmd.args(args);
        cmd
    }

    /// Runs `args`; `(exit code, stdout)`. A spawn failure, a timeout or a
    /// signal is an error.
    fn call(&self, args: &[&str]) -> Result<(i32, String), QuestionsError> {
        let verb = args.first().copied().unwrap_or_default().to_owned();
        let fail = |detail: String| QuestionsError::Tmux {
            verb: verb.clone(),
            detail,
        };
        let done = run_bounded(self.command(args), TMUX_CALL).map_err(|f| match f {
            RunFailure::Spawn(e) | RunFailure::Wait(e) => fail(e.to_string()),
            RunFailure::Timeout => fail(format!("timed out after {TMUX_CALL:?}")),
        })?;
        let code = done
            .status
            .code()
            .ok_or_else(|| fail("ended by a signal".into()))?;
        Ok((code, String::from_utf8_lossy(&done.stdout).into_owned()))
    }

    /// `tmux has-session -t =<session>`: true on exit 0.
    pub(crate) fn has_session(&self, session: &str) -> Result<bool, QuestionsError> {
        let target = ExactTarget::session(session);
        Ok(self.call(&["has-session", "-t", target.as_target()])?.0 == 0)
    }

    /// `tmux capture-pane -t =<session>: -p -J -S -200`: its stdout.
    ///
    /// # Errors
    /// A non-zero exit too (#12029 P2f: the script reads it as an empty pane).
    pub(crate) fn capture(&self, session: &str) -> Result<String, QuestionsError> {
        let target = ExactTarget::active_window(session);
        let args = ["capture-pane", "-t", target.as_target(), "-p", "-J"];
        let (code, out) = self.call(&[&args[..], &["-S", "-200"]].concat())?;
        if code != 0 {
            return Err(QuestionsError::Tmux {
                verb: "capture-pane".into(),
                detail: format!("exit {code}"),
            });
        }
        Ok(out)
    }

    /// `tmux new-session -d -s <session> -c <dir> -e <env> <command>`.
    pub(crate) fn new_session(
        &self,
        session: &str,
        dir: &Path,
        env: &str,
        command: &str,
    ) -> Result<(), QuestionsError> {
        let dir = dir.to_string_lossy();
        let args = ["new-session", "-d", "-s", session, "-c", &dir, "-e", env];
        let (code, _) = self.call(&[&args[..], &[command]].concat())?;
        if code != 0 {
            return Err(QuestionsError::Tmux {
                verb: "new-session".into(),
                detail: format!("exit {code}"),
            });
        }
        Ok(())
    }
}

/// Runs `cmd` to completion within `timeout`: `(exit code, stdout, stderr)`.
fn run_command(cmd: Command, timeout: Duration) -> Result<(i32, String, String), QuestionsError> {
    let program = cmd.get_program().to_string_lossy().into_owned();
    let fail = |detail: String| QuestionsError::Command {
        program: program.clone(),
        detail,
    };
    // Every child here runs with stdin closed; none of them reads it.
    let done = run_bounded(cmd, timeout).map_err(|f| match f {
        RunFailure::Spawn(e) | RunFailure::Wait(e) => fail(e.to_string()),
        RunFailure::Timeout => fail(format!("timed out after {timeout:?}")),
    })?;
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    // A signal reads as the shell's 128 + n.
    let code = done.status.code().unwrap_or(128);
    Ok((code, text(&done.stdout), text(&done.stderr)))
}

/// `${name:-default}`: the variable when set and non-empty.
fn env_or(env: &dyn Fn(&str) -> Option<String>, name: &str) -> Option<String> {
    env(name).filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_of_takes_the_first_file_and_reports_every_candidate_when_none_is()
    -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let real = dir.path().join("tmux");
        std::fs::write(&real, "")?;
        let real_s = real.to_string_lossy().into_owned();
        let tmux = TmuxCli::first_of(&["/nonexistent/tmux", &real_s])?;
        assert_eq!(tmux.program(), real);
        assert!(tmux.socket().is_none());
        let err = TmuxCli::first_of(&["/nonexistent/a", "/nonexistent/b"]);
        assert!(
            matches!(err, Err(QuestionsError::NoTmux(ref m)) if m == "/nonexistent/a, /nonexistent/b")
        );
        Ok(())
    }

    #[test]
    fn a_path_tmux_always_carries_its_socket() {
        let argv = |t: &TmuxCli| -> Vec<String> {
            let cmd = t.command(&["has-session"]);
            std::iter::once(cmd.get_program())
                .chain(cmd.get_args())
                .map(|a| a.to_string_lossy().into_owned())
                .collect()
        };
        let named = TmuxCli::on_path_with_socket(Socket::Name("p".into()));
        assert_eq!(argv(&named), ["tmux", "-L", "p", "has-session"]);
        let at = TmuxCli::on_path_with_socket(Socket::Path("/tmp/s".into()));
        assert_eq!(argv(&at), ["tmux", "-S", "/tmp/s", "has-session"]);
    }
}
