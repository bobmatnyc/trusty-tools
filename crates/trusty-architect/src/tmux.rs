//! tmux access behind a trait, and the exact pane ids it targets.

use std::fmt;
use std::path::PathBuf;
use std::process::Command;
use std::str::FromStr;

use crate::error::TmuxError;

/// One tmux pane, named by its immutable `%N` id.
///
/// Why: tmux resolves a bare `-t name` by exact name, then by prefix, so a
/// session name can hit the wrong pane (#8443). A `%N` id names one pane for
/// the server's lifetime, and the Architect's poller already works in pane ids.
/// What: a `%` followed by one or more ASCII digits; [`PaneId::as_target`]
/// passes it to `-t` unchanged.
/// Test: `pane_id_accepts_only_percent_digits`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PaneId(String);

impl PaneId {
    /// Parses `%N`; anything else is [`TmuxError::InvalidPaneId`].
    pub fn parse(value: &str) -> Result<Self, TmuxError> {
        let digits = value.strip_prefix('%').unwrap_or_default();
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(TmuxError::InvalidPaneId(value.to_owned()));
        }
        Ok(Self(value.to_owned()))
    }

    /// The exact `-t` target for this pane: the `%N` id itself.
    pub fn as_target(&self) -> &str {
        &self.0
    }
}

impl FromStr for PaneId {
    type Err = TmuxError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl fmt::Display for PaneId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Access to one tmux server.
///
/// Why: three copies of "shell out to tmux" exist today (Python `fleet-poll.py`,
/// trusty-mpm's `fleet/launch.rs` and `fleet/poller.rs`); the port needs one
/// seam that tests can replace without a tmux server.
/// What: [`Tmux::run`] runs one tmux command and returns its stdout; the
/// provided methods build the argv for the verbs the Architect uses.
/// Test: `send_keys_builds_exact_target_argv` (via [`crate::FakeTmux`]).
pub trait Tmux {
    /// Runs `tmux <args>` and returns stdout. `args[0]` is the verb.
    fn run(&self, args: &[&str]) -> Result<String, TmuxError>;

    /// Sends `keys` to `pane` with `send-keys`, unguarded.
    ///
    /// Callers that type into a Claude Code input box use
    /// [`crate::send_guarded`] instead, which checks the box first.
    fn send_keys(&self, pane: &PaneId, keys: &[&str]) -> Result<(), TmuxError> {
        let mut args = vec!["send-keys", "-t", pane.as_target()];
        args.extend_from_slice(keys);
        self.run(&args).map(drop)
    }
}

/// The real [`Tmux`]: spawns the `tmux` binary on `PATH`.
///
/// Why: production callers need a tmux server, optionally a non-default one
/// (the fleet socket, or a throwaway `-S` socket in an end-to-end test).
/// What: runs `tmux [-S <socket>] <args>` with stdin closed and stdout and
/// stderr captured. A non-zero exit is [`TmuxError::Failed`]. There is no
/// timeout yet; the poller port (#9171 A3) adds one.
/// Test: `system_command_prepends_socket`, `system_run_rejects_empty_args`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SystemTmux {
    socket: Option<PathBuf>,
}

impl SystemTmux {
    /// tmux's own default server resolution (`$TMUX`, else the default socket).
    pub fn new() -> Self {
        Self::default()
    }

    /// A specific server socket, passed as `-S <path>`.
    pub fn with_socket(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: Some(socket.into()),
        }
    }

    /// The `Command` [`Tmux::run`] spawns for `args`; building it spawns nothing.
    pub fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new("tmux");
        if let Some(socket) = &self.socket {
            cmd.arg("-S").arg(socket);
        }
        cmd.args(args);
        cmd
    }
}

impl Tmux for SystemTmux {
    fn run(&self, args: &[&str]) -> Result<String, TmuxError> {
        let verb = *args.first().ok_or(TmuxError::EmptyCommand)?;
        tracing::debug!(verb, "running tmux");
        let output = self
            .command(args)
            .output()
            .map_err(|source| TmuxError::Spawn {
                verb: verb.to_owned(),
                source,
            })?;
        if !output.status.success() {
            return Err(TmuxError::Failed {
                verb: verb.to_owned(),
                code: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FakeTmux;

    fn argv(cmd: &Command) -> (String, Vec<String>) {
        let program = cmd.get_program().to_string_lossy().into_owned();
        let args = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        (program, args)
    }

    #[test]
    fn pane_id_accepts_only_percent_digits() {
        for good in ["%0", "%261"] {
            assert_eq!(
                PaneId::parse(good).map(|p| p.to_string()).ok(),
                Some(good.to_owned())
            );
        }
        for bad in ["", "%", "261", "%26a", "main", "=main", "$1", "@1", "% 1"] {
            assert!(
                matches!(PaneId::parse(bad), Err(TmuxError::InvalidPaneId(ref v)) if v == bad),
                "{bad:?} must be rejected"
            );
        }
    }

    #[test]
    fn system_command_prepends_socket() {
        let args = ["send-keys", "-t", "%7", "hi", "Enter"];

        let (program, plain) = argv(&SystemTmux::new().command(&args));
        assert_eq!(program, "tmux");
        assert_eq!(plain, args);

        let (program, socketed) = argv(&SystemTmux::with_socket("/tmp/fleet.sock").command(&args));
        assert_eq!(program, "tmux");
        assert_eq!(
            socketed,
            [
                "-S",
                "/tmp/fleet.sock",
                "send-keys",
                "-t",
                "%7",
                "hi",
                "Enter"
            ]
        );
    }

    #[test]
    fn system_run_rejects_empty_args() {
        assert!(matches!(
            SystemTmux::new().run(&[]),
            Err(TmuxError::EmptyCommand)
        ));
    }

    #[test]
    fn send_keys_builds_exact_target_argv() -> Result<(), TmuxError> {
        let fake = FakeTmux::new();
        fake.send_keys(&PaneId::parse("%3")?, &["hello", "Enter"])?;
        assert_eq!(fake.calls(), [["send-keys", "-t", "%3", "hello", "Enter"]]);
        Ok(())
    }
}
