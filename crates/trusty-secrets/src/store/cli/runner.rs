//! [`CliCommand`]: the blocking runner every CLI-backed backend spawns
//! through (#7519, DOC-74 §8.2).
//!
//! Why: `SecretBackend` is synchronous, so the runner blocks. A child
//! process can leak a value through argv (readable through `ps`), through
//! its environment, or through stderr copied into an error; a hung CLI can
//! hold a request forever; a grandchild can outlive a killed child. Each of
//! those is decided here once.
//! What: a builder (program, argv, env overlay, timeout, the vault and key
//! named in errors) with two runners, [`CliCommand::run`] (null stdin) and
//! [`CliCommand::run_with_stdin`]. Before spawning, a run refuses a command
//! line that contains the stdin value or an overlay value. The child runs
//! in its own process group; stdout and stderr drain on threads, capped at
//! [`OUTPUT_CAP`] each; the stdin write runs on a thread too, so a child
//! that never reads cannot block the timeout. A `try_wait` poll enforces
//! the timeout; on timeout, on a stdin write that did not finish, and on
//! every early return the group gets `SIGKILL` and the child is reaped. stderr only feeds the `classify`
//! verdict and is then dropped; no error carries child output.
//! Test: `runner_tests.rs` beside this file.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{ErrorKind, Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdin, ExitStatus, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use super::classify::{Verdict, classify};
use super::spec::CliSpec;
use crate::api::{SecretKey, SecretValue, SecretsError, VaultName};

/// The most bytes kept from each of stdout and stderr (1 MiB).
pub const OUTPUT_CAP: usize = 1024 * 1024;

/// Gap between `try_wait` polls.
const POLL: Duration = Duration::from_millis(10);

/// Upper bound on any timeout, so the deadline arithmetic cannot overflow.
const MAX_TIMEOUT: Duration = Duration::from_secs(3600);

/// The vault and key a run names in its errors when no target was set.
const NO_TARGET: &str = "(none)";

const REFUSED: &str =
    "refused before spawning: the value or an environment token appears in the command line";
const SPAWN_FAILED: &str = "the CLI could not be started";
const THREAD_FAILED: &str = "could not start a thread to talk to the CLI";
const WRITE_FAILED: &str = "writing to the CLI's stdin failed; the CLI was killed";
const COLLECT_FAILED: &str = "collecting the CLI's result failed; the CLI was killed";
const TIMED_OUT: &str = "the CLI did not finish within its timeout; its process group was killed";
const TOO_LARGE: &str = "the CLI's output exceeded 1 MiB";
const NOT_UTF8: &str = "the CLI's output is not UTF-8";
const NON_ZERO: &str = "the CLI exited unsuccessfully; its output was withheld";

/// One vendor-CLI invocation, built then run. Never goes through a shell.
///
/// Why: see the module docs.
/// What: argv and the overlay carry references and flags, never the value.
/// Every overlay value is checked against argv before a run, so an overlay
/// value must not also be an argv token. `Debug` renders overlay keys only,
/// because an overlay may carry a service-account token.
/// Test: `runner_env_overlay_debug_shows_keys_only`.
#[derive(Clone)]
pub struct CliCommand {
    spec: CliSpec,
    program: OsString,
    args: Vec<OsString>,
    envs: Vec<(OsString, OsString)>,
    /// Secrets refused in argv and the overlay besides the stdin value.
    hidden: Vec<SecretValue>,
    timeout: Duration,
    vault: String,
    key: String,
}

impl fmt::Debug for CliCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let env_keys: Vec<&OsStr> = self.envs.iter().map(|(k, _)| k.as_os_str()).collect();
        f.debug_struct("CliCommand")
            .field("backend", &self.spec.backend)
            .field("program", &self.program)
            .field("args", &self.args)
            .field("env_keys", &env_keys)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl CliCommand {
    /// A command for `spec.program`, with `spec.timeout`.
    pub fn new(spec: CliSpec) -> Self {
        Self {
            spec,
            program: OsString::from(spec.program),
            args: Vec::new(),
            envs: Vec::new(),
            hidden: Vec::new(),
            timeout: spec.timeout,
            vault: NO_TARGET.to_string(),
            key: NO_TARGET.to_string(),
        }
    }

    /// Run `program` instead of `spec.program`.
    #[must_use]
    pub fn program(mut self, program: impl AsRef<OsStr>) -> Self {
        self.program = program.as_ref().to_os_string();
        self
    }

    /// Append one argument. Never the value.
    #[must_use]
    pub fn arg(mut self, arg: impl AsRef<OsStr>) -> Self {
        self.args.push(arg.as_ref().to_os_string());
        self
    }

    /// Append several arguments.
    #[must_use]
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args
            .extend(args.into_iter().map(|a| a.as_ref().to_os_string()));
        self
    }

    /// Overlay one environment variable on the child, e.g. a token.
    #[must_use]
    pub fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.envs
            .push((key.as_ref().to_os_string(), value.as_ref().to_os_string()));
        self
    }

    /// Refuse `secret` in argv and the overlay too, beside the stdin value.
    ///
    /// Why: #7519 P3 — Keeper's stdin is a batch command holding the value
    /// encoded, so the value itself is not a substring of stdin and the
    /// stdin check alone would not catch it in argv.
    /// Test: `runner_refuses_a_hidden_secret_in_argv_before_spawn`.
    #[must_use]
    pub fn hide(mut self, secret: &SecretValue) -> Self {
        self.hidden.push(secret.clone());
        self
    }

    /// Replace `spec.timeout`.
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The vault and key this run serves, named in its errors.
    #[must_use]
    pub fn target(mut self, vault: &VaultName, key: &SecretKey) -> Self {
        self.vault = vault.as_str().to_string();
        self.key = key.as_str().to_string();
        self
    }

    /// Run with a null stdin — for a read whose stdout is the value.
    ///
    /// What: refuses an overlay value in argv, then runs; a non-zero exit is
    /// classified by stderr markers, or is `Other` when stderr overflowed
    /// [`OUTPUT_CAP`]. Errors are spawn, timeout and I/O only;
    /// [`CliRun::into_value`] turns a verdict into a result.
    /// Test: `runner_stderr_markers_map_to_verdicts`,
    /// `runner_stderr_over_the_cap_on_failure_is_other`,
    /// `runner_missing_program_is_cli_not_installed`,
    /// `runner_timeout_kills_the_process_group`,
    /// `runner_grandchild_holding_a_pipe_is_killed_after_the_leader_exits`,
    /// `runner_stdout_over_the_cap_is_an_error`.
    pub fn run(&self) -> Result<CliRun, SecretsError> {
        self.refuse_leaks(None)?;
        self.execute(None)
    }

    /// Run with `value` written to stdin, which is then closed.
    ///
    /// Why: DOC-74 §8.2 — a value reaches a vendor CLI only through stdin.
    /// What: refuses `value` in argv or the overlay, and an overlay value in
    /// argv, before spawning. The verdict is `Ok` or `Other` only: stderr of
    /// a value-bearing run is never read for meaning. A child that closes
    /// stdin before reading the whole value has its group killed, and the
    /// run is `Other` whatever its exit status; any other write failure
    /// kills the group and is an error.
    /// Test: `runner_value_reaches_the_child_on_stdin_only`,
    /// `runner_refuses_the_value_in_argv_before_spawn`,
    /// `runner_refuses_a_token_in_argv_before_spawn`,
    /// `runner_echoed_stdin_never_reaches_an_error`,
    /// `runner_early_exit_on_stdin_is_classified`,
    /// `runner_stdin_closed_early_is_never_ok_and_kills_the_group`.
    pub fn run_with_stdin(&self, value: &SecretValue) -> Result<CliRun, SecretsError> {
        let value = value.expose().as_bytes();
        self.refuse_leaks(Some(value))?;
        self.execute(Some(value))
    }

    // #7519: the runner, not each backend, keeps the value and any token out
    // of argv — the only route into the child that `ps` shows.
    fn refuse_leaks(&self, stdin: Option<&[u8]>) -> Result<(), SecretsError> {
        let argv: Vec<&[u8]> = std::iter::once(&self.program)
            .chain(&self.args)
            .map(|s| s.as_encoded_bytes())
            .collect();
        let overlay: Vec<&[u8]> = self
            .envs
            .iter()
            .map(|(_, v)| v.as_encoded_bytes())
            .collect();
        let value_leaks = stdin.is_some_and(|v| found_in(&argv, v) || found_in(&overlay, v));
        let token_leaks = overlay.iter().any(|token| found_in(&argv, token));
        // #7519 P3: a value a backend encoded into stdin, checked raw.
        let hidden_leaks = self.hidden.iter().any(|secret| {
            let secret = secret.expose().as_bytes();
            found_in(&argv, secret) || found_in(&overlay, secret)
        });
        if value_leaks || token_leaks || hidden_leaks {
            return Err(self.failure(REFUSED));
        }
        Ok(())
    }

    fn execute(&self, stdin: Option<&[u8]>) -> Result<CliRun, SecretsError> {
        let mut command = std::process::Command::new(&self.program);
        command
            .args(&self.args)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // #7519: its own group, so a kill reaches every grandchild too.
            .process_group(0);
        for (key, value) in &self.envs {
            command.env(key, value);
        }
        let child = command.spawn().map_err(|e| self.spawn_error(&e))?;
        // #7519: from here every return path kills the group and reaps.
        let mut guard = GroupGuard {
            child,
            reaped: false,
        };
        let deadline = Instant::now() + self.timeout.min(MAX_TIMEOUT);
        let (tx, rx) = mpsc::channel();
        let (stdout, stderr) = (guard.child.stdout.take(), guard.child.stderr.take());
        let stdin_pipe = guard.child.stdin.take();
        let feeding = stdin.is_some() && stdin_pipe.is_some();
        let started = drain(stdout, Event::Stdout, &tx)
            .and_then(|()| drain(stderr, Event::Stderr, &tx))
            .and_then(|()| match (stdin, stdin_pipe) {
                (Some(value), Some(pipe)) => feed(pipe, value.to_vec(), &tx),
                _ => Ok(()),
            });
        started.map_err(|_| self.failure(THREAD_FAILED))?;
        drop(tx);

        let mut got = Collected {
            stdin_pending: feeding,
            ..Collected::default()
        };
        let status = loop {
            while let Ok(event) = rx.try_recv() {
                self.absorb(event, &mut got, &guard)?;
            }
            // #7519: no reap until the stdin write ends, so a failed write
            // can still kill the group while the leader holds its id.
            if !got.stdin_pending
                && let Some(status) = guard.try_wait().map_err(|_| self.failure(COLLECT_FAILED))?
            {
                break status;
            }
            let now = Instant::now();
            if now >= deadline {
                // The guard's drop kills the group and reaps the child.
                return Err(self.failure(TIMED_OUT));
            }
            std::thread::sleep(POLL.min(deadline - now));
        };
        // The child is reaped; its streams close once the last holder exits.
        loop {
            match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(event) => self.absorb(event, &mut got, &guard)?,
                Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {
                    // A straggler still holds a pipe. The group id cannot be
                    // reused while the group has a member.
                    kill_group(guard.child.id());
                    return Err(self.failure(TIMED_OUT));
                }
            }
        }
        self.finish(status, got, stdin.is_some())
    }

    fn absorb(
        &self,
        event: Event,
        got: &mut Collected,
        guard: &GroupGuard,
    ) -> Result<(), SecretsError> {
        match event {
            Event::Stdout(read) => got.stdout = read.map_err(|_| self.failure(COLLECT_FAILED))?,
            Event::Stderr(read) => got.stderr = read.map_err(|_| self.failure(COLLECT_FAILED))?,
            Event::Stdin(written) => {
                got.stdin_pending = false;
                match written {
                    Ok(()) => {}
                    // #7519: the child closed stdin before reading the whole
                    // value. Kill the group; the run is classified, never Ok.
                    Err(e) if e.kind() == ErrorKind::BrokenPipe => {
                        guard.kill_unreaped_group();
                        got.stdin_short = true;
                    }
                    Err(_) => return Err(self.failure(WRITE_FAILED)),
                }
            }
        }
        Ok(())
    }

    fn finish(
        &self,
        status: ExitStatus,
        got: Collected,
        had_stdin: bool,
    ) -> Result<CliRun, SecretsError> {
        if got.stdout.overflow {
            return Err(self.failure(TOO_LARGE));
        }
        // #7519: a value the child did not fully read is never stored, and a
        // failure whose stderr was cut at the cap is never read for meaning.
        let verdict = if got.stdin_short || (got.stderr.overflow && !status.success()) {
            Verdict::Other
        } else {
            classify(status.success(), &got.stderr.bytes, had_stdin, &self.spec)
        };
        // #7519: stderr is dropped here; nothing but `classify` read it.
        drop(got.stderr);
        // #7519: a failed run's stdout is untrusted; only a success keeps it.
        let stdout = if verdict == Verdict::Ok {
            // A `FromUtf8Error` carries the bytes; it is dropped unread.
            String::from_utf8(got.stdout.bytes).map_err(|_| self.failure(NOT_UTF8))?
        } else {
            String::new()
        };
        Ok(CliRun {
            verdict,
            code: status.code(),
            stdout: SecretValue::new(stdout),
            spec: self.spec,
            vault: self.vault.clone(),
            key: self.key.clone(),
        })
    }

    fn spawn_error(&self, error: &std::io::Error) -> SecretsError {
        if error.kind() == ErrorKind::NotFound {
            // #7519: A4 — a missing CLI names itself and the fix.
            SecretsError::CliNotInstalled {
                program: self.program.to_string_lossy().into_owned(),
                hint: self.spec.install_hint,
            }
        } else {
            self.failure(SPAWN_FAILED)
        }
    }

    fn failure(&self, reason: &'static str) -> SecretsError {
        backend_error(&self.spec, &self.vault, &self.key, reason)
    }
}

/// The outcome of one finished run.
///
/// What: the [`Verdict`], the exit code (`None` when signalled), and stdout
/// verbatim as a [`SecretValue`] — empty unless the verdict is `Ok`. No
/// stderr is kept. [`CliRun::into_value`] maps the verdict to the
/// [`crate::store::SecretBackend`] contract.
/// Test: `runner_stderr_markers_map_to_verdicts`.
#[derive(Debug)]
#[non_exhaustive]
pub struct CliRun {
    /// What the run means.
    pub verdict: Verdict,
    /// Exit code, or `None` when the process was terminated by a signal.
    pub code: Option<i32>,
    /// stdout, verbatim, when the verdict is `Ok`; empty otherwise.
    pub stdout: SecretValue,
    spec: CliSpec,
    vault: String,
    key: String,
}

impl CliRun {
    /// `Ok` → `Some(stdout)`, `Missing` → `None`, `Locked` →
    /// [`SecretsError::BackendLocked`], `Other` → [`SecretsError::Backend`].
    ///
    /// Why: A3 — a miss is `Ok(None)`, and a locked or unknown failure is
    /// an `Err`, never a miss.
    /// Test: `runner_stderr_markers_map_to_verdicts`,
    /// `runner_echoed_stdin_never_reaches_an_error`.
    pub fn into_value(self) -> Result<Option<SecretValue>, SecretsError> {
        match self.verdict {
            Verdict::Ok => Ok(Some(self.stdout)),
            Verdict::Missing => Ok(None),
            Verdict::Locked => Err(SecretsError::BackendLocked {
                backend: self.spec.backend.to_string(),
                hint: self.spec.locked_hint,
            }),
            Verdict::Other => Err(backend_error(&self.spec, &self.vault, &self.key, NON_ZERO)),
        }
    }
}

fn backend_error(spec: &CliSpec, vault: &str, key: &str, reason: &'static str) -> SecretsError {
    SecretsError::Backend {
        backend: spec.backend.to_string(),
        vault: vault.to_string(),
        key: key.to_string(),
        reason: reason.to_string(),
    }
}

/// One stream's bytes, cut at [`OUTPUT_CAP`].
#[derive(Default)]
struct Capped {
    bytes: Vec<u8>,
    overflow: bool,
}

/// What the run has collected so far.
#[derive(Default)]
struct Collected {
    stdout: Capped,
    stderr: Capped,
    /// The stdin write has not reported yet.
    stdin_pending: bool,
    /// The child closed stdin before the whole value was written.
    stdin_short: bool,
}

/// One helper thread's result. Each thread sends exactly one, then exits.
enum Event {
    Stdout(std::io::Result<Capped>),
    Stderr(std::io::Result<Capped>),
    Stdin(std::io::Result<()>),
}

/// Kills the child's process group and reaps the child unless it was reaped.
struct GroupGuard {
    child: Child,
    reaped: bool,
}

impl GroupGuard {
    fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        let status = self.child.try_wait()?;
        self.reaped = status.is_some();
        Ok(status)
    }

    /// `SIGKILL` the group, only while the unreaped leader holds its id.
    fn kill_unreaped_group(&self) {
        if !self.reaped {
            kill_group(self.child.id());
        }
    }
}

impl Drop for GroupGuard {
    fn drop(&mut self) {
        if !self.reaped {
            // The unreaped leader keeps the group id from being reused.
            kill_group(self.child.id());
            // Backstop for the leader alone; `wait` must not block forever.
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// `SIGKILL` the process group led by `pid`.
fn kill_group(pid: u32) {
    // #7519: never `kill(0, …)` or `kill(-1, …)`, which reach this process's
    // own group or every process the user owns.
    let Some(pgid) = libc::pid_t::try_from(pid).ok().filter(|p| *p > 1) else {
        return;
    };
    // SAFETY: `kill` takes two integers and touches no caller memory. A
    // failure (the group is already gone) is harmless and ignored.
    unsafe {
        libc::kill(-pgid, libc::SIGKILL);
    }
}

/// Read `pipe` on its own thread, keep at most [`OUTPUT_CAP`] bytes, then
/// close it; a child still writing gets `EPIPE`.
fn drain<R: Read + Send + 'static>(
    pipe: Option<R>,
    wrap: fn(std::io::Result<Capped>) -> Event,
    tx: &Sender<Event>,
) -> std::io::Result<()> {
    let Some(pipe) = pipe else {
        return Ok(());
    };
    let tx = tx.clone();
    std::thread::Builder::new()
        .name("trusty-secrets-cli-drain".to_string())
        .spawn(move || {
            let mut bytes = Vec::new();
            let read = pipe
                .take(OUTPUT_CAP as u64 + 1)
                .read_to_end(&mut bytes)
                .map(|_| {
                    let overflow = bytes.len() > OUTPUT_CAP;
                    bytes.truncate(OUTPUT_CAP);
                    Capped { bytes, overflow }
                });
            let _ = tx.send(wrap(read));
        })?;
    Ok(())
}

/// Write `value` to `pipe` on its own thread, then close it.
fn feed(mut pipe: ChildStdin, value: Vec<u8>, tx: &Sender<Event>) -> std::io::Result<()> {
    let tx = tx.clone();
    std::thread::Builder::new()
        .name("trusty-secrets-cli-stdin".to_string())
        .spawn(move || {
            let written = pipe.write_all(&value);
            drop(pipe);
            drop(value);
            let _ = tx.send(Event::Stdin(written));
        })?;
    Ok(())
}

/// Whether `needle` occurs inside any of `haystacks`.
fn found_in(haystacks: &[&[u8]], needle: &[u8]) -> bool {
    haystacks.iter().any(|h| contains(h, needle))
}

/// Whether `needle` occurs as a contiguous run inside `haystack`.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}
