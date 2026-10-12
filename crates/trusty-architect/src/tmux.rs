//! tmux access behind a trait, and the exact pane ids it targets.

use std::fmt;
use std::path::PathBuf;
use std::process::Command;
use std::str::FromStr;
use std::time::Duration;

use crate::error::TmuxError;
use crate::process::{RunFailure, run_bounded};

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

/// Serializes as the bare `%N` string, as the prototype's pane dicts hold it.
impl serde::Serialize for PaneId {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl fmt::Display for PaneId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One window, named exactly: `=<session>:<index>`.
///
/// Why: `rename-window` needs a window target, and a bare `session:index` is
/// prefix-matched (#8443). This leaf crate cannot use trusty-common's
/// `exact_window_target` (#9171 A3b-2).
/// What: the `=` prefix makes tmux match the session name exactly;
/// [`WindowTarget::as_target`] passes it to `-t`.
/// Test: `test_runner_renames_with_exact_target_and_no_send_keys`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WindowTarget(String);

impl WindowTarget {
    /// The exact target for window `index` of `session`.
    pub(crate) fn exact(session: &str, index: &str) -> Self {
        Self(format!("={session}:{index}"))
    }

    /// The `-t` target.
    pub(crate) fn as_target(&self) -> &str {
        &self.0
    }
}

/// Proof that a call came from this crate's guarded send path.
///
/// Why: [`Tmux::run`] must not carry `send-keys`, yet [`send_guarded`] has to
/// reach the same tmux. The token has no public constructor, so only code in
/// this crate can pass one to [`Tmux::execute`].
/// What: a zero-sized value; [`Tmux::run`] and the crate-private `send_keys`
/// are the only places that make one.
/// Test: `run_refuses_send_keys_on_every_implementation`.
///
/// [`send_guarded`]: crate::send_guarded
#[derive(Debug, Clone, Copy)]
pub struct Permit(());

/// Access to one tmux server.
///
/// Why: three copies of "shell out to tmux" exist today (Python `fleet-poll.py`,
/// trusty-mpm's `fleet/launch.rs` and `fleet/poller.rs`); the port needs one
/// seam that tests can replace without a tmux server.
/// What: [`Tmux::run`] runs one tmux command and returns its stdout, except
/// `send-keys`, which it refuses with [`TmuxError::SendKeysRefused`]. Keys go
/// out only through [`crate::send_guarded`], which checks the input box and
/// then calls [`Tmux::execute`] with a [`Permit`] that only this crate can
/// make. An implementor writes `execute`; `run` is not overridden.
/// Test: `system_run_rejects_empty_args`, `send_keys_presses_named_keys`
/// (via [`crate::FakeTmux`]), `run_refuses_send_keys_on_every_implementation`.
pub trait Tmux {
    /// Runs `tmux <args>` and returns stdout. `args[0]` is the verb; a
    /// `send-keys` verb is [`TmuxError::SendKeysRefused`].
    fn run(&self, args: &[&str]) -> Result<String, TmuxError> {
        if args.first() == Some(&"send-keys") {
            return Err(TmuxError::SendKeysRefused);
        }
        self.execute(args, Permit(()))
    }

    /// Runs `tmux <args>` with no verb filter. A caller outside this crate
    /// cannot make the [`Permit`], so it cannot call this; implementors only
    /// supply the body.
    #[doc(hidden)]
    fn execute(&self, args: &[&str], permit: Permit) -> Result<String, TmuxError>;
}

/// A tmux key name the send path may press.
///
/// Why: the poller presses only Enter (submit) and BSpace (remove its own
/// typed text); a closed set keeps a key name from ever carrying text.
/// What: each variant is the exact tmux key name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamedKey {
    /// `Enter`: submit the input box.
    Enter,
    /// `BSpace`: delete one character before the cursor.
    BSpace,
}

impl NamedKey {
    /// The tmux key name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Enter => "Enter",
            Self::BSpace => "BSpace",
        }
    }
}

/// What one `send-keys` call delivers.
///
/// Why: A1's `send-keys -t P -- <keys>` cannot type the poller's pointer: the
/// prototype's `wake()` sends `-l <msg>`, and after `--` a `-l` is typed as
/// text (#9171 A3a plan finding 4).
/// What: `Literal` types its text verbatim (`send-keys -t P -l -- <text>`);
/// `Named` presses key names (`send-keys -t P <name>…`).
/// Test: `send_keys_types_a_dash_leading_literal_as_text`,
/// `send_keys_presses_named_keys`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keys<'a> {
    /// Text typed as-is, never read as key names or flags.
    Literal(&'a str),
    /// Key names pressed in order.
    Named(&'a [NamedKey]),
}

/// Sends `keys` to `pane` with `send-keys`, unguarded.
///
/// Crate-private, and the only caller of [`Tmux::execute`] with `send-keys`, so
/// [`crate::send_guarded`], which checks the input box first, and the wake
/// pointer's typed steps (`crate::pointer`, #9171 A3b-1), which check the
/// owner before each send, are the only ways to send through a [`Tmux`] (a
/// caller that bypasses the trait and spawns `tmux` itself is outside what
/// this crate can stop). In literal mode `-l` precedes `--`, so a
/// text that starts with `-` (`-t foo`, `-l`) reaches the pane as text.
/// Test: `send_keys_types_a_dash_leading_literal_as_text`,
/// `send_keys_presses_named_keys`.
pub(crate) fn send_keys<T: Tmux + ?Sized>(
    tmux: &T,
    pane: &PaneId,
    keys: Keys<'_>,
) -> Result<(), TmuxError> {
    let mut args = vec!["send-keys", "-t", pane.as_target()];
    match keys {
        Keys::Literal(text) => args.extend(["-l", "--", text]),
        Keys::Named(names) => args.extend(names.iter().map(|k| k.as_str())),
    }
    tmux.execute(&args, Permit(())).map(drop)
}

/// How long one tmux call may run: the prototype's `timeout=10`.
pub const TMUX_TIMEOUT: Duration = Duration::from_secs(10);

/// The real [`Tmux`]: spawns the `tmux` binary on `PATH`.
///
/// Why: production callers need a tmux server, optionally a non-default one
/// (the fleet socket, or a throwaway `-S` socket in an end-to-end test), and a
/// hung server must not hang the poller (#9171 A3a).
/// What: runs `tmux [-S <socket>] <args>` with stdin closed and stdout and
/// stderr drained on threads, bounded by a timeout ([`TMUX_TIMEOUT`] unless
/// [`SystemTmux::with_timeout`] says otherwise). Past the deadline the child is
/// killed and reaped and the call is [`TmuxError::Timeout`]; a non-zero exit is
/// [`TmuxError::Failed`].
/// Test: `system_command_prepends_socket`, `system_run_rejects_empty_args`,
/// `system_tmux_times_out_and_reaps_the_child`,
/// `system_tmux_reads_a_megabyte_of_stdout`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemTmux {
    program: PathBuf,
    socket: Option<PathBuf>,
    timeout: Duration,
}

impl Default for SystemTmux {
    fn default() -> Self {
        Self {
            program: PathBuf::from("tmux"),
            socket: None,
            timeout: TMUX_TIMEOUT,
        }
    }
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
            ..Self::default()
        }
    }

    /// Runs `program` instead of `tmux`: a stand-in for the timeout tests.
    pub fn with_program(mut self, program: impl Into<PathBuf>) -> Self {
        self.program = program.into();
        self
    }

    /// Bounds each call by `timeout` instead of [`TMUX_TIMEOUT`].
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The `Command` [`Tmux::run`] spawns for `args`; building it spawns nothing.
    pub fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(&self.program);
        if let Some(socket) = &self.socket {
            cmd.arg("-S").arg(socket);
        }
        cmd.args(args);
        cmd
    }
}

impl Tmux for SystemTmux {
    fn execute(&self, args: &[&str], _permit: Permit) -> Result<String, TmuxError> {
        let verb = (*args.first().ok_or(TmuxError::EmptyCommand)?).to_owned();
        tracing::debug!(verb, "running tmux");
        let done = run_bounded(self.command(args), self.timeout).map_err(|f| match f {
            RunFailure::Spawn(source) => TmuxError::Spawn {
                verb: verb.clone(),
                source,
            },
            RunFailure::Timeout => TmuxError::Timeout {
                verb: verb.clone(),
                after: self.timeout,
            },
            RunFailure::Wait(source) => TmuxError::Wait {
                verb: verb.clone(),
                source,
            },
        })?;
        if !done.status.success() {
            // The prototype keeps the first 200 characters of stderr.
            let stderr = String::from_utf8_lossy(&done.stderr);
            return Err(TmuxError::Failed {
                verb,
                code: done.status.code(),
                stderr: stderr.trim().chars().take(200).collect(),
            });
        }
        Ok(String::from_utf8_lossy(&done.stdout).into_owned())
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
    fn send_keys_presses_named_keys() -> Result<(), TmuxError> {
        let fake = FakeTmux::new();
        let pane = PaneId::parse("%3")?;
        send_keys(&fake, &pane, Keys::Named(&[NamedKey::Enter]))?;
        send_keys(&fake, &pane, Keys::Named(&[NamedKey::BSpace; 2]))?;
        assert_eq!(
            fake.calls(),
            [
                vec!["send-keys", "-t", "%3", "Enter"],
                vec!["send-keys", "-t", "%3", "BSpace", "BSpace"]
            ]
        );
        Ok(())
    }

    #[test]
    fn send_keys_types_a_dash_leading_literal_as_text() -> Result<(), TmuxError> {
        let fake = FakeTmux::new();
        send_keys(&fake, &PaneId::parse("%3")?, Keys::Literal("-t =x Enter"))?;
        assert_eq!(
            fake.calls(),
            [["send-keys", "-t", "%3", "-l", "--", "-t =x Enter"]]
        );
        Ok(())
    }

    /// A shell stand-in for tmux: `args` run as `sh -c <script>`.
    fn shell(timeout: Duration) -> SystemTmux {
        SystemTmux::new()
            .with_program("/bin/sh")
            .with_timeout(timeout)
    }

    #[test]
    fn system_tmux_times_out_and_reaps_the_child() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let pid_file = dir.path().join("pid");
        let script = format!("echo $$ > '{}'; exec sleep 30", pid_file.display());
        let timeout = Duration::from_millis(300);
        let start = std::time::Instant::now();
        let result = shell(timeout).run(&["-c", &script]);
        let elapsed = start.elapsed();
        assert!(
            matches!(result, Err(TmuxError::Timeout { ref verb, after }) if verb == "-c" && after == timeout),
            "{result:?}"
        );
        assert!(elapsed < timeout + Duration::from_secs(1), "{elapsed:?}");
        let pid: i32 = std::fs::read_to_string(&pid_file)?.trim().parse()?;
        // SAFETY: kill with signal 0 only probes; a reaped pid is ESRCH, while
        // an unreaped zombie would still answer 0.
        let alive = unsafe { libc::kill(pid, 0) } == 0;
        assert!(!alive, "child {pid} was not reaped");
        Ok(())
    }

    #[test]
    fn system_tmux_reads_a_megabyte_of_stdout() -> Result<(), TmuxError> {
        let out = shell(Duration::from_secs(20)).run(&["-c", "head -c 1048576 /dev/zero"])?;
        assert_eq!(out.len(), 1_048_576);
        let err = shell(Duration::from_secs(20)).run(&["-c", "echo boom >&2; exit 3"]);
        assert!(
            matches!(err, Err(TmuxError::Failed { code: Some(3), ref stderr, .. }) if stderr == "boom"),
            "{err:?}"
        );
        Ok(())
    }
}
