//! [`ExternalCliCommand`] — the shared runner that hands a secret value to a
//! vendor CLI (`op`, `keeper`, `vercel`, `gh secret set`) on its stdin
//! (issue #9311, DOC-74 §8.2).
//!
//! Why: every CLI-backed secrets integration (#7519 1Password and Keeper,
//! #9069 Vercel and GitHub Actions) must write a value into a child process.
//! `gh::GhCommand` has no stdin input, and a hand-rolled
//! `Command::new` per vendor would re-decide, N times, the three things that
//! leak a value: putting it in argv (world-readable through `ps`), putting it
//! in an environment variable (inherited by every grandchild), and copying the
//! child's stderr into an error message (vendor CLIs echo bad input).
//!
//! What: a builder in the shape of `GhCommand` (program, argv, working
//! directory, environment overlay) with four runners —
//! [`ExternalCliCommand::output_with_stdin_blocking`] /
//! [`ExternalCliCommand::output_with_stdin`] write a [`Secret`] to the child's
//! stdin and close it; [`ExternalCliCommand::output_blocking`] /
//! [`ExternalCliCommand::output`] give the child a null stdin, for a read such
//! as `op read <ref>` whose stdout is the value. Every runner returns an
//! [`ExternalCliOutput`] whose stdout and stderr are [`Secret`]s, and never
//! treats a non-zero exit as an error itself; [`ExternalCliOutput::ok`] does,
//! with an error that carries the exit code and no child output. The value
//! never enters argv, the environment, an error, or a `Debug` rendering, and
//! the module emits no tracing events. A missing binary is
//! [`ExternalCliError::NotInstalled`]. The child is reaped on every path: a
//! failed stdin write kills it first, so a CLI that read part of the value
//! cannot act on it at leisure.
//!
//! Test: `credentials::external_cli::tests::*` in `external_cli_tests.rs` —
//! a `sh` child reports its stdin, argv and environment back, and a child
//! that echoes the value to stderr proves the error withholds it.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ExitStatus, Stdio};

use super::Secret;

/// Every way an [`ExternalCliCommand`] run can fail. No variant carries the
/// stdin value or any byte of the child's stdout or stderr.
///
/// Why: DOC-74 §8.5 — a vendor CLI's output can hold the value it was given,
/// so an error built from that output is a leak path, and a missing binary
/// must fail closed with a type a caller can degrade on.
/// What: `program` and `args` are the caller's non-secret command line;
/// [`ExternalCliCommand::output_with_stdin_blocking`] refuses to run a command
/// line that contains the value, so rendering them here is safe.
/// Test: `missing_binary_fails_closed_without_the_value`,
/// `spawn_failure_is_typed_and_carries_no_value`,
/// `nonzero_exit_error_withholds_stderr_that_echoes_the_value`,
/// `stdin_write_failure_kills_and_reaps_the_child`,
/// `value_in_the_command_line_is_refused_before_spawn`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ExternalCliError {
    /// The binary does not exist or is not on `PATH`.
    #[error("`{program}` is not installed or not on PATH")]
    NotInstalled {
        /// The program as given to [`ExternalCliCommand::new`].
        program: String,
    },
    /// The stdin value also appears in the argv or the environment overlay.
    #[error(
        "refused to run `{program}`: the stdin value also appears in its argv or environment overlay"
    )]
    ValueInCommandLine {
        /// The program as given to [`ExternalCliCommand::new`].
        program: String,
    },
    /// The binary exists but the process could not be started.
    #[error("failed to spawn `{program} {args}`: {source}")]
    Spawn {
        /// The program.
        program: String,
        /// The rendered argv.
        args: String,
        /// The OS error.
        #[source]
        source: std::io::Error,
    },
    /// Writing the value to the child's stdin failed; the child was killed.
    #[error("failed to write stdin to `{program} {args}` (child killed): {source}")]
    StdinWrite {
        /// The program.
        program: String,
        /// The rendered argv.
        args: String,
        /// The OS error.
        #[source]
        source: std::io::Error,
    },
    /// Waiting for the child or collecting its output failed.
    #[error("failed to collect the result of `{program} {args}`: {source}")]
    Collect {
        /// The program.
        program: String,
        /// The rendered argv.
        args: String,
        /// The OS error.
        #[source]
        source: std::io::Error,
    },
    /// The child exited non-zero. Its output is withheld: it may echo the value.
    #[error("`{program} {args}` failed ({}); output withheld", exit_label(.code))]
    NonZero {
        /// The program.
        program: String,
        /// The rendered argv.
        args: String,
        /// Exit code, `None` when the child was terminated by a signal.
        code: Option<i32>,
    },
}

fn exit_label(code: &Option<i32>) -> String {
    code.map_or_else(|| "signalled".to_string(), |c| format!("exit {c}"))
}

/// The result of one run. Both streams are [`Secret`]s: `op read` prints the
/// value on stdout, and a vendor CLI may echo it on stderr.
///
/// Why: `Debug` on this struct reaches logs; wrapping the streams makes that
/// rendering value-free by construction rather than by call-site care.
/// What: the command line, the exit code (`None` if signalled), whether it
/// was zero, and lossy-decoded stdout/stderr, read through
/// [`Secret::expose`].
/// Test: `stdin_value_reaches_the_child_on_stdin_only`.
#[derive(Debug)]
#[non_exhaustive]
pub struct ExternalCliOutput {
    /// The program as given to [`ExternalCliCommand::new`].
    pub program: String,
    /// The argv, rendered space-separated (no program).
    pub args: String,
    /// Exit code, or `None` when the process was terminated by a signal.
    pub code: Option<i32>,
    /// Whether the process exited zero.
    pub success: bool,
    /// Lossy-decoded stdout, verbatim.
    pub stdout: Secret<String>,
    /// Lossy-decoded stderr, verbatim.
    pub stderr: Secret<String>,
}

impl ExternalCliOutput {
    fn new(cmd: &ExternalCliCommand, status: ExitStatus, out: Vec<u8>, err: Vec<u8>) -> Self {
        Self {
            program: cmd.program_display(),
            args: cmd.argv_display(),
            code: status.code(),
            success: status.success(),
            stdout: Secret::new(String::from_utf8_lossy(&out).into_owned()),
            stderr: Secret::new(String::from_utf8_lossy(&err).into_owned()),
        }
    }

    /// Treat a non-zero exit as [`ExternalCliError::NonZero`], which carries
    /// the exit code and nothing the child printed.
    ///
    /// Test: `nonzero_exit_error_withholds_stderr_that_echoes_the_value`.
    pub fn ok(self) -> Result<Self, ExternalCliError> {
        if self.success {
            return Ok(self);
        }
        Err(ExternalCliError::NonZero {
            program: self.program,
            args: self.args,
            code: self.code,
        })
    }
}

/// A vendor-CLI invocation, built then run. Never goes through a shell.
///
/// Why: one place decides how a secrets CLI is spawned, so the stdin-only
/// delivery rule (DOC-74 §8.2) is implemented once.
/// What: program, argv, optional working directory, and an environment
/// overlay applied in call order. The argv and overlay are for references
/// and flags, never the value; `Debug` renders overlay keys only, because an
/// overlay may carry a session token.
/// Test: `command_debug_renders_env_keys_not_values`.
#[derive(Clone)]
pub struct ExternalCliCommand {
    program: OsString,
    args: Vec<OsString>,
    cwd: Option<PathBuf>,
    /// `Some(v)` sets, `None` removes; later entries win, as in `GhCommand`.
    envs: Vec<(OsString, Option<OsString>)>,
}

impl fmt::Debug for ExternalCliCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let env: Vec<String> = self
            .envs
            .iter()
            .map(|(k, v)| {
                let op = if v.is_some() { "set" } else { "removed" };
                format!("{}=<{op}>", k.to_string_lossy())
            })
            .collect();
        f.debug_struct("ExternalCliCommand")
            .field("program", &self.program)
            .field("args", &self.args)
            .field("cwd", &self.cwd)
            .field("env", &env)
            .finish()
    }
}

impl ExternalCliCommand {
    /// A command for `program` (a bare name resolved on `PATH`, or a path).
    pub fn new(program: impl AsRef<OsStr>) -> Self {
        Self {
            program: program.as_ref().to_os_string(),
            args: Vec::new(),
            cwd: None,
            envs: Vec::new(),
        }
    }

    /// Append one argument. Never the value: see [`ExternalCliError::ValueInCommandLine`].
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

    /// Run the child in `dir` rather than the caller's working directory.
    #[must_use]
    pub fn cwd(mut self, dir: impl AsRef<Path>) -> Self {
        self.cwd = Some(dir.as_ref().to_path_buf());
        self
    }

    /// Overlay one environment variable on the child.
    #[must_use]
    pub fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.envs.push((
            key.as_ref().to_os_string(),
            Some(value.as_ref().to_os_string()),
        ));
        self
    }

    /// Remove one inherited environment variable from the child.
    #[must_use]
    pub fn env_remove(mut self, key: impl AsRef<OsStr>) -> Self {
        self.envs.push((key.as_ref().to_os_string(), None));
        self
    }

    /// The program, lossy-rendered.
    pub fn program_display(&self) -> String {
        self.program.to_string_lossy().into_owned()
    }

    /// The argv rendered for error messages — lossy, space-separated, no program.
    pub fn argv_display(&self) -> String {
        self.args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Run the CLI, writing `value` to its stdin and then closing it.
    ///
    /// Why: DOC-74 §8.2 — a secret reaches a vendor CLI only through stdin,
    /// never argv or the environment, and nothing the run reports may carry
    /// it (#9311).
    /// What: refuses with [`ExternalCliError::ValueInCommandLine`] before
    /// spawning if a non-empty `value` occurs in the program, an argument, or
    /// an overlay value. Otherwise spawns with all three streams piped,
    /// drains stdout and stderr on two threads so a chatty child cannot
    /// deadlock the write, writes `value`, closes stdin, and waits. A failed
    /// write kills and reaps the child and returns
    /// [`ExternalCliError::StdinWrite`]. A non-zero exit is returned as an
    /// [`ExternalCliOutput`], not an error — see [`ExternalCliOutput::ok`].
    /// Test: `stdin_value_reaches_the_child_on_stdin_only`,
    /// `nonzero_exit_error_withholds_stderr_that_echoes_the_value`,
    /// `stdin_write_failure_kills_and_reaps_the_child`,
    /// `missing_binary_fails_closed_without_the_value`,
    /// `value_in_the_command_line_is_refused_before_spawn`.
    pub fn output_with_stdin_blocking<T: AsRef<[u8]>>(
        &self,
        value: &Secret<T>,
    ) -> Result<ExternalCliOutput, ExternalCliError> {
        let value = value.expose().as_ref();
        self.refuse_value_in_command_line(value)?;
        self.run_blocking(Some(value))
    }

    /// Run the CLI with a null stdin — for a read whose stdout is the value.
    ///
    /// Test: `output_without_stdin_sees_eof_and_wraps_stdout`.
    pub fn output_blocking(&self) -> Result<ExternalCliOutput, ExternalCliError> {
        self.run_blocking(None)
    }

    /// Async [`ExternalCliCommand::output_with_stdin_blocking`], on tokio.
    ///
    /// What: the write and both reads run concurrently in this task; a failed
    /// write kills the child, which is then reaped. The child is spawned with
    /// `kill_on_drop`, so a dropped future kills it too and tokio reaps it.
    /// Test: `async_stdin_value_reaches_the_child_on_stdin_only`,
    /// `async_stdin_write_failure_kills_and_reaps_the_child`.
    pub async fn output_with_stdin<T: AsRef<[u8]>>(
        &self,
        value: &Secret<T>,
    ) -> Result<ExternalCliOutput, ExternalCliError> {
        let value = value.expose().as_ref();
        self.refuse_value_in_command_line(value)?;
        self.run_async(Some(value)).await
    }

    /// Async [`ExternalCliCommand::output_blocking`], on tokio.
    ///
    /// Test: `async_missing_binary_fails_closed`.
    pub async fn output(&self) -> Result<ExternalCliOutput, ExternalCliError> {
        self.run_async(None).await
    }

    // #9311: the runner, not each caller, guarantees the value is absent from
    // argv and the overlay — the only other routes into the child.
    fn refuse_value_in_command_line(&self, value: &[u8]) -> Result<(), ExternalCliError> {
        if value.is_empty() {
            return Ok(());
        }
        let overlay = self.envs.iter().filter_map(|(_, v)| v.as_ref());
        let found = std::iter::once(&self.program)
            .chain(&self.args)
            .chain(overlay)
            .any(|s| contains(s.as_encoded_bytes(), value));
        if found {
            return Err(ExternalCliError::ValueInCommandLine {
                program: self.program_display(),
            });
        }
        Ok(())
    }

    fn std_command(&self, stdin: Stdio) -> std::process::Command {
        let mut cmd = std::process::Command::new(&self.program);
        cmd.args(&self.args)
            .stdin(stdin)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(dir) = &self.cwd {
            cmd.current_dir(dir);
        }
        for (k, v) in &self.envs {
            match v {
                Some(v) => cmd.env(k, v),
                None => cmd.env_remove(k),
            };
        }
        cmd
    }

    fn spawn_error(&self, err: std::io::Error) -> ExternalCliError {
        if err.kind() == std::io::ErrorKind::NotFound {
            ExternalCliError::NotInstalled {
                program: self.program_display(),
            }
        } else {
            ExternalCliError::Spawn {
                program: self.program_display(),
                args: self.argv_display(),
                source: err,
            }
        }
    }

    fn stdin_error(&self, source: std::io::Error) -> ExternalCliError {
        ExternalCliError::StdinWrite {
            program: self.program_display(),
            args: self.argv_display(),
            source,
        }
    }

    fn collect_error(&self, source: std::io::Error) -> ExternalCliError {
        ExternalCliError::Collect {
            program: self.program_display(),
            args: self.argv_display(),
            source,
        }
    }

    fn run_blocking(&self, stdin: Option<&[u8]>) -> Result<ExternalCliOutput, ExternalCliError> {
        let piped = if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        };
        let child = self
            .std_command(piped)
            .spawn()
            .map_err(|e| self.spawn_error(e))?;
        // #9311: from here every return path reaps the child — the guard's
        // Drop kills and waits unless `wait` already reaped it.
        let mut guard = ReapGuard {
            child,
            reaped: false,
        };
        let out = drain(guard.child.stdout.take()).map_err(|e| self.collect_error(e))?;
        let err = drain(guard.child.stderr.take()).map_err(|e| self.collect_error(e))?;
        if let (Some(value), Some(mut pipe)) = (stdin, guard.child.stdin.take()) {
            pipe.write_all(value).map_err(|e| self.stdin_error(e))?;
            // `pipe` drops here, closing stdin so the child sees EOF.
        }
        let status = guard.wait().map_err(|e| self.collect_error(e))?;
        let out = join(out).map_err(|e| self.collect_error(e))?;
        let err = join(err).map_err(|e| self.collect_error(e))?;
        Ok(ExternalCliOutput::new(self, status, out, err))
    }

    async fn run_async(&self, stdin: Option<&[u8]>) -> Result<ExternalCliOutput, ExternalCliError> {
        use tokio::io::AsyncWriteExt;

        let piped = if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        };
        let mut cmd = tokio::process::Command::from(self.std_command(piped));
        cmd.kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| self.spawn_error(e))?;
        let stdin_pipe = child.stdin.take();
        let read_out = read_all(child.stdout.take());
        let read_err = read_all(child.stderr.take());
        let child_ref = &mut child;
        let write = async move {
            let result = match (stdin, stdin_pipe) {
                // `pipe` drops at the end of the arm, closing stdin.
                (Some(value), Some(mut pipe)) => pipe.write_all(value).await,
                _ => Ok(()),
            };
            if result.is_err() {
                // #9311: a CLI that read part of the value must not finish.
                let _ = child_ref.start_kill();
            }
            result
        };
        let (written, out, err) = tokio::join!(write, read_out, read_err);
        if written.is_err() || out.is_err() || err.is_err() {
            let _ = child.start_kill();
        }
        let status = child.wait().await;
        written.map_err(|e| self.stdin_error(e))?;
        let status = status.map_err(|e| self.collect_error(e))?;
        let out = out.map_err(|e| self.collect_error(e))?;
        let err = err.map_err(|e| self.collect_error(e))?;
        Ok(ExternalCliOutput::new(self, status, out, err))
    }
}

/// Kills and reaps a blocking child on any early return.
struct ReapGuard {
    child: Child,
    reaped: bool,
}

impl ReapGuard {
    fn wait(&mut self) -> std::io::Result<ExitStatus> {
        let status = self.child.wait()?;
        self.reaped = true;
        Ok(status)
    }
}

impl Drop for ReapGuard {
    fn drop(&mut self) {
        if !self.reaped {
            // Errors are ignored: the child may already have exited, and a
            // zombie is still reaped by the `wait` below.
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

type Drain = Option<std::thread::JoinHandle<std::io::Result<Vec<u8>>>>;

/// Read a pipe to EOF on its own thread.
fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> std::io::Result<Drain> {
    let Some(mut pipe) = pipe else {
        return Ok(None);
    };
    let handle = std::thread::Builder::new()
        .name("external-cli-drain".to_string())
        .spawn(move || {
            let mut buf = Vec::new();
            pipe.read_to_end(&mut buf).map(|_| buf)
        })?;
    Ok(Some(handle))
}

fn join(drain: Drain) -> std::io::Result<Vec<u8>> {
    match drain {
        None => Ok(Vec::new()),
        Some(handle) => handle
            .join()
            .map_err(|_| std::io::Error::other("output reader thread panicked"))?,
    }
}

/// Read an async pipe to EOF; an absent pipe reads as empty.
async fn read_all<R: tokio::io::AsyncRead + Unpin>(pipe: Option<R>) -> std::io::Result<Vec<u8>> {
    use tokio::io::AsyncReadExt;

    let mut buf = Vec::new();
    if let Some(mut pipe) = pipe {
        pipe.read_to_end(&mut buf).await?;
    }
    Ok(buf)
}

/// Whether `needle` occurs as a contiguous run inside `haystack`.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

#[cfg(all(test, unix))]
#[path = "external_cli_tests.rs"]
mod tests;
