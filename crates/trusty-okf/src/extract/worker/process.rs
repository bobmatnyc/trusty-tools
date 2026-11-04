//! One worker process: spawn, bounded pipe I/O, kill and reap (SPEC-OKF-01
//! §2.16 "Cancellation, kill and reap", "Deadlines on every wait").
//!
//! Why: no host wait on the worker may be unbounded, and no worker may
//! outlive the host's interest in it — after a crash, a hang, an expired
//! deadline, oversize output, a cancel or a host panic. A blocking pipe
//! read or write cannot be timed out in place, so each pipe has its own
//! thread and the host waits on channels under a deadline.
//! What: [`WorkerProcess::spawn`] starts the worker in its own process
//! group, with a cleared environment plus `LANG=C.UTF-8` and `TZ=UTC`, in a
//! fresh empty working directory, under the sandbox [`super::sandbox`]
//! builds. A writer thread owns stdin; a reader thread reads stdout frames
//! up to the frame cap; a drain thread keeps the first 64 KiB of stderr
//! and discards the rest, so a stderr flood never blocks the worker.
//! [`WorkerProcess::send`] and [`WorkerProcess::recv`] wait under a caller
//! deadline and a cancel flag. [`WorkerProcess::kill`] runs the kill
//! sequence: close stdin, SIGTERM to the group, up to 2 s, SIGKILL to the
//! group, then a wait under the reap deadline; on expiry a reaper thread
//! takes the child (`ReapTimeout`). [`WorkerProcess::shutdown`] sends
//! `shutdown`, closes stdin and waits under the shutdown deadline, else
//! kills (`ShutdownTimeout`). `Drop` kills, so a host error or panic leaks
//! no process. The working directory is removed once the child is reaped.
//! Test: `c103_timeout_cancel_and_panic_leave_no_process`,
//! `c123_worker_ignoring_shutdown_is_killed_and_reaped`,
//! `c124_forced_reap_timeout_returns_within_the_deadline`.
//!
//! # Spec References
//! - SPEC-OKF-01 §2.16, Appendix B "Default limits".

use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;

use super::proto::{FrameRead, read_frame};
use super::sandbox::{self, SandboxPolicy, SpawnEvidence};
use super::{Limits, STDERR_CAP};
use crate::WorkerError;

/// The fixed worker environment (§2.16 "Local-only configuration"; the
/// reconciliation fixes `LANG` rather than reading the parent's).
pub(crate) const WORKER_ENV: [(&str, &str); 2] = [("LANG", "C.UTF-8"), ("TZ", "UTC")];

/// SIGTERM to SIGKILL (§2.16 "Cancellation, kill and reap").
const TERM_GRACE: Duration = Duration::from_secs(2);

/// How often a wait re-checks the cancel flag or the child.
const TICK: Duration = Duration::from_millis(20);

/// Why a bounded wait on the worker ended without its result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum IoFail {
    /// The deadline passed.
    Timeout,
    /// The cancel flag was set.
    Cancelled,
    /// The pipe closed: the worker exited or closed it.
    Closed,
    /// A frame declared a length over the cap.
    TooLarge(u64),
    /// A pipe error.
    Io(String),
}

enum Event {
    Frame(Vec<u8>),
    TooLarge(u64),
    Closed,
    Io(String),
}

/// Test-only fault hooks (#10746 S10 criterion 124).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Hooks {
    /// Treat the reap as never finishing within its deadline.
    #[cfg(test)]
    pub reap_timeout: bool,
}

/// A running worker; see the module doc.
pub(crate) struct WorkerProcess {
    child: Option<Child>,
    pgid: Pid,
    stdin: Option<Sender<Vec<u8>>>,
    written: Receiver<std::io::Result<()>>,
    frames: Receiver<Event>,
    stderr: Arc<Mutex<Vec<u8>>>,
    cwd: PathBuf,
    limits: Limits,
    hooks: Hooks,
    evidence: SpawnEvidence,
    status: Option<ExitStatus>,
}

static SPAWNS: AtomicU64 = AtomicU64::new(0);

/// A fresh, empty, private working directory under `scratch`.
fn fresh_dir(scratch: &Path) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    let n = SPAWNS.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let dir = scratch.join(format!(
        "trusty-okf-worker-{}-{n}-{nanos}",
        std::process::id()
    ));
    std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
    dir.canonicalize()
}

impl WorkerProcess {
    /// Start `worker` (canonical path) under `policy`. Errors: the working
    /// directory or the process could not be created (`Spawn`).
    pub(crate) fn spawn(
        worker: &Path,
        scratch: &Path,
        policy: SandboxPolicy,
        limits: Limits,
        hooks: Hooks,
    ) -> Result<WorkerProcess, WorkerError> {
        let cwd = fresh_dir(scratch)
            .map_err(|e| WorkerError::Spawn(format!("working directory: {e}")))?;
        let (mut cmd, evidence) = sandbox::command(policy, worker, &cwd);
        cmd.env_clear()
            .envs(WORKER_ENV)
            .current_dir(&cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&cwd); // okf: temp-cleanup
                return Err(WorkerError::Spawn(e.to_string()));
            }
        };
        let pgid = Pid::from_raw(child.id() as i32);
        let (stdin, stdout, stderr_pipe) = (
            child.stdin.take().expect("stdin is piped"),
            child.stdout.take().expect("stdout is piped"),
            child.stderr.take().expect("stderr is piped"),
        );
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let (wtx, written) = mpsc::channel();
        std::thread::spawn(move || {
            let mut stdin = stdin;
            for buf in rx {
                let r = stdin.write_all(&buf).and_then(|()| stdin.flush());
                let failed = r.is_err();
                if wtx.send(r).is_err() || failed {
                    break;
                }
            }
        });
        let cap = limits.frame_cap();
        let (ftx, frames) = mpsc::channel();
        std::thread::spawn(move || {
            let mut out = stdout;
            loop {
                let ev = match read_frame(&mut out, cap) {
                    Ok(FrameRead::Frame(f)) => Event::Frame(f),
                    Ok(FrameRead::TooLarge(n)) => Event::TooLarge(n),
                    Ok(FrameRead::Eof) => Event::Closed,
                    Err(e) => Event::Io(e.to_string()),
                };
                let last = !matches!(ev, Event::Frame(_));
                if ftx.send(ev).is_err() || last {
                    break;
                }
            }
        });
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let keep = Arc::clone(&stderr);
        std::thread::spawn(move || {
            let mut pipe = stderr_pipe;
            let mut buf = [0u8; 8192];
            while let Ok(n) = pipe.read(&mut buf) {
                if n == 0 {
                    break;
                }
                if let Ok(mut k) = keep.lock() {
                    let room = STDERR_CAP.saturating_sub(k.len());
                    k.extend_from_slice(&buf[..n.min(room)]);
                }
            }
        });
        Ok(WorkerProcess {
            child: Some(child),
            pgid,
            stdin: Some(tx),
            written,
            frames,
            stderr,
            cwd,
            limits,
            hooks,
            evidence,
            status: None,
        })
    }

    /// The sandbox the host applied at spawn.
    pub(crate) fn evidence(&self) -> SpawnEvidence {
        self.evidence
    }

    /// The process group ID (the worker's own PID).
    pub(crate) fn pgid(&self) -> i32 {
        self.pgid.as_raw()
    }

    /// The exit status, once reaped.
    pub(crate) fn status(&self) -> Option<ExitStatus> {
        self.status
    }

    /// The stderr bytes kept so far (at most 64 KiB).
    pub(crate) fn stderr(&self) -> Vec<u8> {
        self.stderr.lock().map(|k| k.clone()).unwrap_or_default()
    }

    /// Write `buf` to stdin before `deadline`.
    pub(crate) fn send(
        &mut self,
        buf: Vec<u8>,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> Result<(), IoFail> {
        let tx = self.stdin.as_ref().ok_or(IoFail::Closed)?;
        tx.send(buf).map_err(|_| IoFail::Closed)?;
        match wait(&self.written, deadline, cancel)? {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Err(IoFail::Closed),
            Err(e) => Err(IoFail::Io(e.to_string())),
        }
    }

    /// Read one frame before `deadline`.
    pub(crate) fn recv(
        &mut self,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> Result<Vec<u8>, IoFail> {
        match wait(&self.frames, deadline, cancel)? {
            Event::Frame(f) => Ok(f),
            Event::TooLarge(n) => Err(IoFail::TooLarge(n)),
            Event::Closed => Err(IoFail::Closed),
            Event::Io(e) => Err(IoFail::Io(e)),
        }
    }

    fn signal(&self, sig: Signal) {
        // ESRCH: the group is already empty, which is the goal.
        let _ = killpg(self.pgid, sig);
    }

    /// Poll for the leader's exit until `deadline`; reaps it when it exits.
    fn wait_exit(&mut self, deadline: Instant) -> bool {
        loop {
            let Some(child) = self.child.as_mut() else {
                return true;
            };
            if let Ok(Some(st)) = child.try_wait() {
                self.status = Some(st);
                self.child = None;
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(TICK);
        }
    }

    /// The kill sequence (see the module doc). Returns `ReapTimeout` when
    /// the reap deadline passed.
    pub(crate) fn kill(&mut self) -> Vec<WorkerError> {
        self.stdin = None;
        if self.child.is_none() {
            // Exited and reaped: SIGKILL any member left in the group.
            self.signal(Signal::SIGKILL);
            self.cleanup();
            return Vec::new();
        }
        self.signal(Signal::SIGTERM);
        self.wait_exit(Instant::now() + TERM_GRACE);
        self.signal(Signal::SIGKILL);
        self.reap()
    }

    fn reap(&mut self) -> Vec<WorkerError> {
        #[cfg(test)]
        let forced = self.hooks.reap_timeout;
        #[cfg(not(test))]
        let forced = false;
        let _ = self.hooks;
        if !forced && self.wait_exit(Instant::now() + self.limits.reap) {
            self.cleanup();
            return Vec::new();
        }
        if forced {
            std::thread::sleep(self.limits.reap);
        }
        if let Some(mut child) = self.child.take() {
            let cwd = self.cwd.clone();
            std::thread::spawn(move || {
                let _ = child.wait();
                let _ = std::fs::remove_dir_all(cwd); // okf: temp-cleanup
            });
        }
        vec![WorkerError::ReapTimeout]
    }

    fn cleanup(&mut self) {
        let _ = std::fs::remove_dir_all(&self.cwd); // okf: temp-cleanup
    }

    /// Send `shutdown`, close stdin and wait for the exit under the
    /// shutdown deadline; on expiry kill (`ShutdownTimeout`, then any
    /// `ReapTimeout`).
    pub(crate) fn shutdown(&mut self) -> Vec<WorkerError> {
        let deadline = Instant::now() + self.limits.shutdown;
        let never = AtomicBool::new(false);
        let _ = self.send(super::proto::shutdown(), deadline, &never);
        self.stdin = None;
        if self.wait_exit(deadline) {
            return self.kill();
        }
        let mut errs = vec![WorkerError::ShutdownTimeout];
        errs.extend(self.kill());
        errs
    }
}

impl Drop for WorkerProcess {
    fn drop(&mut self) {
        if self.child.is_some() {
            let _ = self.kill();
        } else {
            self.signal(Signal::SIGKILL);
            self.cleanup();
        }
    }
}

/// Wait on `rx` until `deadline`, checking `cancel` every tick.
fn wait<T>(rx: &Receiver<T>, deadline: Instant, cancel: &AtomicBool) -> Result<T, IoFail> {
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err(IoFail::Cancelled);
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(IoFail::Timeout);
        }
        match rx.recv_timeout(left.min(TICK * 5)) {
            Ok(v) => return Ok(v),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Err(IoFail::Closed),
        }
    }
}
