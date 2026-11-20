//! The prototype's `run_*` steps: Python scripts the poller starts on a
//! cadence (df-T7i P1b; the plan's "run_* steps stay Python" shim).
//!
//! Why: the dashboard store, Intent Ledger, task database and GitHub tracking
//! stay Python until the ledger port; the poller only starts their scripts,
//! on the prototype's cadence, and never waits on the detached ones.
//! What: [`Subprocess`] is the seam every child goes through (the system one
//! bounds each run, [`SystemSubprocess`]). [`ScriptSteps::run`] starts, per
//! cycle: `board_feed.py` (240 s bound), `share_sync.py poll` (240 s; off
//! with `SHARE_SYNC_POLL=off`) and `questions_rc.py` (300 s), logging their
//! first output lines; then, detached, `task_ingest.py --sweep`,
//! `task_progress.py` and `il_dashboard_sync.py` every 60 s (the
//! `--pm-sweep` flag every 300 s), and `track_gh.py` and
//! `runtime_selftest.py` every 300 s, each off with its variable set to `off`
//! (`TASK_SWEEP`, `TASK_PROGRESS`, `IL_DASH_SYNC`, `TRACK_GH`,
//! `RUNTIME_SELFTEST`). GitHub is read only by `track_gh.py`, on that fixed
//! timer: no webhook, no listener. A failure is one `ERROR` log line and
//! never stops the cycle. A dry or shadow run starts nothing (the caller
//! does not call [`ScriptSteps::run`]). `dashboard_updates.ingest` is an
//! in-process call to the dashboard store and is not started here.
//! Test: `script_steps_follow_the_prototype_cadence`,
//! `script_steps_honour_off_switches`, `a_failing_script_is_a_log_line`.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

use crate::poller::Config;
use crate::process::{RunFailure, run_bounded};

/// A finished child's output.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChildOutput {
    /// Exit code; `None` when killed by a signal.
    pub code: Option<i32>,
    /// Standard output, lossily decoded.
    pub stdout: String,
    /// Standard error, lossily decoded.
    pub stderr: String,
}

/// Every child process the poller's steps start.
pub trait Subprocess {
    /// Starts `argv` detached in its own process group, stdin closed;
    /// output to `log` (appended) or discarded. Returns the child's pid.
    ///
    /// # Errors
    /// The child could not be started.
    fn spawn_detached(&self, argv: &[String], log: Option<&Path>) -> io::Result<u32>;

    /// Runs `argv` to completion within `timeout`.
    ///
    /// # Errors
    /// The child could not be started, timed out (killed), or not waited on.
    fn output(&self, argv: &[String], timeout: Duration) -> io::Result<ChildOutput>;
}

/// [`Subprocess`] over `std::process`, every run bounded.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemSubprocess;

fn command(argv: &[String]) -> io::Result<Command> {
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| io::Error::other("empty argv"))?;
    let mut cmd = Command::new(program);
    cmd.args(args);
    Ok(cmd)
}

impl Subprocess for SystemSubprocess {
    fn spawn_detached(&self, argv: &[String], log: Option<&Path>) -> io::Result<u32> {
        use std::os::unix::process::CommandExt;
        let mut cmd = command(argv)?;
        let (out, err) = match log {
            Some(p) => {
                let f = OpenOptions::new().create(true).append(true).open(p)?;
                (Stdio::from(f.try_clone()?), Stdio::from(f))
            }
            None => (Stdio::null(), Stdio::null()),
        };
        // The prototype's start_new_session=True: no terminal signal reaches it.
        let child = cmd
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err)
            .process_group(0)
            .spawn()?;
        Ok(child.id())
    }

    fn output(&self, argv: &[String], timeout: Duration) -> io::Result<ChildOutput> {
        let done = run_bounded(command(argv)?, timeout).map_err(|f| match f {
            RunFailure::Spawn(e) | RunFailure::Wait(e) => e,
            RunFailure::Timeout => io::Error::new(
                io::ErrorKind::TimedOut,
                format!("timed out after {} seconds", timeout.as_secs()),
            ),
        })?;
        Ok(ChildOutput {
            code: done.status.code(),
            stdout: String::from_utf8_lossy(&done.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&done.stderr).into_owned(),
        })
    }
}

/// One detached script and its cadence.
struct Detached {
    name: &'static str,
    script: &'static str,
    args: &'static [&'static str],
    every: f64,
    off: &'static str,
}

const DETACHED: [Detached; 5] = [
    Detached {
        name: "task sweep",
        script: "task_ingest.py",
        args: &["--sweep"],
        every: 60.0,
        off: "TASK_SWEEP",
    },
    Detached {
        name: "task progress",
        script: "task_progress.py",
        args: &[],
        every: 60.0,
        off: "TASK_PROGRESS",
    },
    Detached {
        name: "il dashboard sync",
        script: "il_dashboard_sync.py",
        args: &[],
        every: 60.0,
        off: "IL_DASH_SYNC",
    },
    Detached {
        name: "track_gh",
        script: "track_gh.py",
        args: &[],
        every: 300.0,
        off: "TRACK_GH",
    },
    Detached {
        name: "runtime selftest",
        script: "runtime_selftest.py",
        args: &[],
        every: 300.0,
        off: "RUNTIME_SELFTEST",
    },
];
/// `il_dashboard_sync.py --pm-sweep` cadence (the heavier PM-ledger sweep).
const IL_PM_SWEEP_EVERY: f64 = 300.0;

/// The script steps of one poller process: the scripts directory, the
/// interpreter, the off switches read once, and the in-memory cadence clocks
/// (the prototype keeps them in module globals, so a restart runs each at once).
pub struct ScriptSteps<'a> {
    /// Starts the children.
    pub runner: &'a dyn Subprocess,
    /// `<root>/scripts`.
    pub scripts: PathBuf,
    /// The Python interpreter (`ARCHITECT_PYTHON`, else `python3`).
    pub python: String,
    /// The variables that were set to `off`.
    pub off: Vec<String>,
    last: Mutex<BTreeMap<&'static str, f64>>,
}

/// The variables that turn a script step off when set to `off`.
pub const OFF_SWITCHES: [&str; 6] = [
    "SHARE_SYNC_POLL",
    "TASK_SWEEP",
    "TASK_PROGRESS",
    "IL_DASH_SYNC",
    "TRACK_GH",
    "RUNTIME_SELFTEST",
];

impl<'a> ScriptSteps<'a> {
    /// Steps over `runner` for `<root>/scripts`, with the config's
    /// interpreter and off switches.
    pub fn new(runner: &'a dyn Subprocess, cfg: &Config) -> Self {
        Self {
            runner,
            scripts: cfg.root.join("scripts"),
            python: cfg.python.clone(),
            off: cfg.scripts_off.clone(),
            last: Mutex::new(BTreeMap::new()),
        }
    }

    fn is_off(&self, name: &str) -> bool {
        self.off.iter().any(|o| o == name)
    }

    fn argv(&self, isolated: bool, script: &str, args: &[&str]) -> Vec<String> {
        let mut v = vec![self.python.clone()];
        if isolated {
            v.push("-I".to_owned());
        }
        v.push(self.scripts.join(script).to_string_lossy().into_owned());
        v.extend(args.iter().map(|a| (*a).to_owned()));
        v
    }

    /// A bounded run whose first `keep` output lines go to the log.
    fn bounded(
        &self,
        name: &str,
        argv: &[String],
        secs: u64,
        both: bool,
        keep: usize,
        log: &mut Vec<String>,
    ) {
        match self.runner.output(argv, Duration::from_secs(secs)) {
            Ok(out) => {
                let text = if both {
                    format!("{}{}", out.stdout, out.stderr)
                } else {
                    out.stdout
                };
                log.extend(text.lines().take(keep).map(str::to_owned));
            }
            Err(e) => log.push(format!("{name} ERROR {e}")),
        }
    }

    /// One cycle's script steps at `now`; returns the `poll.log` lines.
    pub fn run(&self, now: f64) -> Vec<String> {
        let mut log = Vec::new();
        let board = self.argv(false, "board_feed.py", &[]);
        self.bounded("board feed", &board, 240, false, 8, &mut log);
        if !self.is_off("SHARE_SYNC_POLL") {
            let share = self.argv(false, "share_sync.py", &["poll"]);
            self.bounded("share sync", &share, 240, true, 8, &mut log);
        }
        let rc = self.argv(false, "questions_rc.py", &[]);
        self.bounded("questions rc", &rc, 300, true, 4, &mut log);
        let mut last = match self.last.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        for d in &DETACHED {
            if self.is_off(d.off) || now - last.get(d.name).copied().unwrap_or(0.0) < d.every {
                continue;
            }
            last.insert(d.name, now);
            let mut args = d.args.to_vec();
            if d.script == "il_dashboard_sync.py"
                && now - last.get("il pm sweep").copied().unwrap_or(0.0) >= IL_PM_SWEEP_EVERY
            {
                last.insert("il pm sweep", now);
                args.push("--pm-sweep");
            }
            let argv = self.argv(true, d.script, &args);
            if let Err(e) = self.runner.spawn_detached(&argv, None) {
                log.push(format!("{} ERROR {e}", d.name));
            }
        }
        log
    }
}
