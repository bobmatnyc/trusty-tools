//! Running one child: the seam between the engine and the operating system.
//!
//! Why: the engine's rules (overlap, cap, backoff) are tested with a scripted
//! runner; only this file starts a process. A timeout must kill the whole
//! process group, or a script's grandchildren outlive it.
//! What: [`RunSpec`] is argv (no shell), cwd, an explicit environment, an
//! optional stdin file and the files stdout and stderr append to.
//! [`JobRunner::run`] runs it to the end or the deadline. [`SystemRunner`]
//! puts the child in its own process group, polls `try_wait`, and on timeout
//! sends SIGTERM to the group, waits two seconds, then SIGKILL, and reaps.
//! The agent kind never runs in tests: they inject a fake [`JobRunner`].
//! Test: `system_runner_runs_argv_without_a_shell`,
//! `system_runner_kills_the_group_at_the_timeout`,
//! `system_runner_reports_exit_codes_and_a_missing_program`.

use std::fs::{File, OpenOptions};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// What to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSpec {
    /// Program and arguments; never a shell line.
    pub argv: Vec<String>,
    /// Working directory.
    pub cwd: PathBuf,
    /// The whole child environment (nothing else is inherited).
    pub env: Vec<(String, String)>,
    /// A file to feed on stdin; `None` closes stdin.
    pub stdin: Option<PathBuf>,
    /// Appended with the child's stdout.
    pub stdout: PathBuf,
    /// Appended with the child's stderr (may equal `stdout`).
    pub stderr: PathBuf,
    /// Kill after this long.
    pub timeout: Duration,
}

/// How a run ended.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RunResult {
    /// The exit code; a signal death reads as 128 + signal. `None` when the
    /// child never ran or was killed at the deadline.
    pub exit: Option<i32>,
    /// The deadline passed and the group was killed.
    pub timed_out: bool,
    /// The child could not be started or waited on.
    pub error: Option<String>,
    /// Seconds the run took.
    pub elapsed_s: i64,
}

/// Runs a [`RunSpec`] to the end or its deadline. `Send + Sync`: the engine
/// calls it from one thread per run.
pub trait JobRunner: Send + Sync {
    /// Runs `spec`.
    fn run(&self, spec: &RunSpec) -> RunResult;
}

/// The real runner.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemRunner;

fn open_append(path: &Path) -> std::io::Result<File> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    OpenOptions::new().create(true).append(true).open(path)
}

fn kill_group(pid: u32, sig: libc::c_int) {
    // SAFETY: `kill` with a negative pid signals the group whose id is the
    // child's pid (it called setsid-equivalent `process_group(0)`); a stale
    // pid at worst returns ESRCH.
    let _ = unsafe { libc::kill(-(pid as libc::pid_t), sig) };
}

fn failed(msg: String, started: Instant) -> RunResult {
    RunResult {
        error: Some(msg),
        elapsed_s: started.elapsed().as_secs() as i64,
        ..RunResult::default()
    }
}

impl JobRunner for SystemRunner {
    fn run(&self, spec: &RunSpec) -> RunResult {
        let started = Instant::now();
        let Some((program, args)) = spec.argv.split_first() else {
            return failed("empty argv".to_owned(), started);
        };
        let open = |p: &Path| open_append(p).map_err(|e| format!("open {}: {e}", p.display()));
        let (out, err) = match (open(&spec.stdout), open(&spec.stderr)) {
            (Ok(o), Ok(e)) => (o, e),
            (Err(m), _) | (_, Err(m)) => return failed(m, started),
        };
        let stdin = match &spec.stdin {
            None => Stdio::null(),
            Some(p) => match File::open(p) {
                Ok(f) => Stdio::from(f),
                Err(e) => return failed(format!("open {}: {e}", p.display()), started),
            },
        };
        let mut cmd = Command::new(program);
        cmd.args(args)
            .env_clear()
            .envs(spec.env.iter().map(|(k, v)| (k, v)))
            .current_dir(&spec.cwd)
            .stdin(stdin)
            .stdout(Stdio::from(out))
            .stderr(Stdio::from(err))
            .process_group(0);
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return failed(format!("spawn {program}: {e}"), started),
        };
        let deadline = started + spec.timeout;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let exit = status.code().or_else(|| status.signal().map(|s| 128 + s));
                    return RunResult {
                        exit,
                        elapsed_s: started.elapsed().as_secs() as i64,
                        ..RunResult::default()
                    };
                }
                Ok(None) if Instant::now() >= deadline => {
                    kill_group(child.id(), libc::SIGTERM);
                    let grace = Instant::now() + Duration::from_secs(2);
                    while Instant::now() < grace && matches!(child.try_wait(), Ok(None)) {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    kill_group(child.id(), libc::SIGKILL);
                    let _ = child.kill();
                    let _ = child.wait();
                    return RunResult {
                        timed_out: true,
                        elapsed_s: started.elapsed().as_secs() as i64,
                        ..RunResult::default()
                    };
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(e) => {
                    kill_group(child.id(), libc::SIGKILL);
                    let _ = child.wait();
                    return failed(format!("wait: {e}"), started);
                }
            }
        }
    }
}
