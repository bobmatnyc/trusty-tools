//! The bounded clipboard read behind `tm secrets set` (#7524 P2-L6).
//!
//! Why: the paste tool is an outside program. Resolved through `PATH`, a
//! planted copy answers instead of the real one; run with no time limit, a
//! hung one blocks `tm secrets set` forever and is never killed.
//! What: the fixed directories the tool is looked up in, the time and size
//! limits, the typed errors a read can end in, and [`run_bounded`], which
//! runs one tool under both limits and kills its process group on the way
//! out. The pattern follows trusty-secrets' CLI runner (#7519), copied
//! because that runner is crate-private there.
//! Test: `paste_tests.rs` beside this file.

use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError, TryRecvError};
use std::time::{Duration, Instant};

/// How long a paste tool may run before it is killed (#7524 P2-L6).
pub(crate) const CLIPBOARD_READ_TIMEOUT: Duration = Duration::from_secs(5);

/// The most clipboard bytes `tm secrets set` accepts (#7524 P2-L6).
pub(crate) const CLIPBOARD_MAX_BYTES: usize = 1024 * 1024;

/// The directories searched for a paste tool, in order; never `PATH`.
#[cfg(target_os = "macos")]
pub(crate) const PASTE_DIRS: &[&str] = &["/usr/bin"];
/// See the macOS list above (#7524 P2-L6).
#[cfg(not(target_os = "macos"))]
pub(crate) const PASTE_DIRS: &[&str] = &[
    "/usr/bin",
    "/usr/local/bin",
    "/run/current-system/sw/bin",
    "/nix/var/nix/profiles/default/bin",
    "/snap/bin",
];

/// How a clipboard read failed. No variant carries clipboard bytes.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ClipboardError {
    /// The tool did not finish in time; it and its process group were killed.
    #[error(
        "tm secrets set: `{program}` did not answer within {limit:?}; it was stopped and nothing was stored"
    )]
    Timeout { program: String, limit: Duration },
    /// The tool printed more than [`CLIPBOARD_MAX_BYTES`].
    #[error("tm secrets set: the clipboard holds more than {limit} bytes; nothing was stored")]
    TooLarge { limit: usize },
    /// The tool exited with a failure status.
    #[error("tm secrets set: `{program}` could not read the clipboard ({status})")]
    Failed {
        program: String,
        status: std::process::ExitStatus,
    },
    /// The tool could not be started or its output could not be collected.
    #[error("tm secrets set: cannot run `{program}`: {source}")]
    Run {
        program: String,
        source: std::io::Error,
    },
    /// The clipboard is not UTF-8 text.
    #[error("tm secrets set: the clipboard does not hold UTF-8 text")]
    NotUtf8,
    /// No paste tool exists in any searched directory.
    #[error(
        "tm secrets set: no clipboard reader found in {searched}; use `--value -` to read stdin"
    )]
    NoReader { searched: String },
}

/// How often the run checks the child and the reader.
const POLL: Duration = Duration::from_millis(10);

/// Whether `path` is a regular file with an execute bit, following a symlink.
pub(crate) fn is_executable_file(path: &Path) -> bool {
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// Run `program` with `args` and return its stdout, within `timeout` and
/// [`CLIPBOARD_MAX_BYTES`].
///
/// Why: #7524 P2-L6 — `Command::output` waits forever and keeps every byte,
/// and a killed tool's backgrounded grandchild outlived it.
/// What: stdin and stderr are null; stdout is read on its own thread, at
/// most one byte past the cap. The child leads its own process group. On a
/// timeout or an oversize the group is killed with `SIGKILL` and the child
/// reaped before the error returns. A non-zero exit is `Failed`; an exit
/// with no output is `Ok(vec![])`.
/// Test: `a_hung_paste_tool_times_out_and_its_whole_process_group_is_killed`,
/// `output_over_the_cap_is_an_error_not_a_truncated_value`,
/// `a_fast_tool_answers_and_an_empty_one_is_ok_empty_not_a_timeout`.
pub(crate) fn run_bounded(
    program: &Path,
    args: &[String],
    timeout: Duration,
) -> Result<Vec<u8>, ClipboardError> {
    let name = program.display().to_string();
    let run_error = |source| ClipboardError::Run {
        program: name.clone(),
        source,
    };
    let timed_out = || ClipboardError::Timeout {
        program: name.clone(),
        limit: timeout,
    };
    let child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        // #7524: P2-L6 its own group, so a kill reaches every grandchild too.
        .process_group(0)
        .spawn()
        .map_err(run_error)?;
    // #7524: P2-L6 from here every return path kills the group and reaps.
    let mut guard = GroupGuard {
        child,
        reaped: false,
    };
    let deadline = Instant::now() + timeout;
    let (tx, rx) = mpsc::channel();
    let mut output = None;
    match guard.child.stdout.take() {
        Some(pipe) => drain(pipe, tx).map_err(run_error)?,
        None => output = Some(Vec::new()),
    }

    let status = loop {
        if output.is_none() {
            match rx.try_recv() {
                Ok(read) => output = Some(settle(read, &name)?),
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => return Err(run_error(reader_lost())),
            }
        }
        if let Some(status) = guard.try_wait().map_err(run_error)? {
            break status;
        }
        let now = Instant::now();
        if now >= deadline {
            // The guard's drop kills the group and reaps the child.
            return Err(timed_out());
        }
        std::thread::sleep(POLL.min(deadline - now));
    };
    let bytes = match output {
        Some(bytes) => bytes,
        // The child is reaped; stdout closes once the last holder exits.
        None => match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(read) => settle(read, &name)?,
            Err(RecvTimeoutError::Timeout) => {
                // A straggler still holds stdout. The group id cannot be
                // reused while the group has a member.
                kill_group(guard.child.id());
                return Err(timed_out());
            }
            Err(RecvTimeoutError::Disconnected) => return Err(run_error(reader_lost())),
        },
    };
    if !status.success() {
        return Err(ClipboardError::Failed {
            program: name,
            status,
        });
    }
    Ok(bytes)
}

/// The stdout reader's result: the bytes, or `TooLarge` past the cap.
fn settle(read: std::io::Result<Capped>, program: &str) -> Result<Vec<u8>, ClipboardError> {
    let capped = read.map_err(|source| ClipboardError::Run {
        program: program.to_string(),
        source,
    })?;
    if capped.overflow {
        return Err(ClipboardError::TooLarge {
            limit: CLIPBOARD_MAX_BYTES,
        });
    }
    Ok(capped.bytes)
}

/// The error for a reader thread that ended without reporting.
fn reader_lost() -> std::io::Error {
    std::io::Error::other("the output reader stopped without a result")
}

/// At most [`CLIPBOARD_MAX_BYTES`] of output, and whether more was offered.
struct Capped {
    bytes: Vec<u8>,
    overflow: bool,
}

/// Read `pipe` on its own thread, keep at most [`CLIPBOARD_MAX_BYTES`] bytes,
/// then close it; a child still writing gets `EPIPE`.
fn drain<R: Read + Send + 'static>(
    pipe: R,
    tx: mpsc::Sender<std::io::Result<Capped>>,
) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("tm-clipboard-read".to_string())
        .spawn(move || {
            let mut bytes = Vec::new();
            let read = pipe
                .take(CLIPBOARD_MAX_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
                .map(|_| {
                    let overflow = bytes.len() > CLIPBOARD_MAX_BYTES;
                    bytes.truncate(CLIPBOARD_MAX_BYTES);
                    Capped { bytes, overflow }
                });
            let _ = tx.send(read);
        })?;
    Ok(())
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
    // Never `kill(0, …)` or `kill(-1, …)`, which reach this process's own
    // group or every process the user owns.
    let Some(pgid) = libc::pid_t::try_from(pid).ok().filter(|p| *p > 1) else {
        return;
    };
    // SAFETY: `kill` takes two integers and touches no caller memory. A
    // failure (the group is already gone) is harmless and ignored.
    unsafe {
        libc::kill(-pgid, libc::SIGKILL);
    }
}
