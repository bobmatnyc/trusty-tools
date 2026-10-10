//! A load deadline (#8454): a caller's budget bounds the whole load, every
//! project it did not reach is refused, and no git process outlives it.
//!
//! Why: tm doctor gives a load 30 s on its own thread; without a deadline a
//! blocked git step held that thread, and its process group, for up to
//! [`GIT_TIMEOUT`] per project after the caller gave up.
//! What: three listed projects; a `git` wrapper on the gate's PATH blocks in
//! the first one (a grandchild holding stdout, then a long sleep) and runs
//! the real git elsewhere. The deadline is far shorter than [`GIT_TIMEOUT`].
//! Test: this module is the test.

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use super::load::{file_error, file_state};
use super::repo::{slack_routes, tempdir, within, Home, Repo};
use crate::policy::gate::{with_git_path, GIT_TIMEOUT};
use crate::policy::{load_effective_until, FileState, GateError, LoadReport, ProjectFileError};

/// The caller's budget for one load.
const DEADLINE: Duration = Duration::from_secs(1);
/// What a load may take past its deadline to kill and reap git.
const MARGIN: Duration = Duration::from_secs(2);

/// Three committed projects; git blocks in the first.
struct Blocked {
    repos: Vec<Repo>,
    home: Home,
    _tmp: tempfile::TempDir,
    bin: PathBuf,
    pids: PathBuf,
}

/// The first `git` on this process's PATH.
fn real_git() -> PathBuf {
    let path = std::env::var_os("PATH").expect("PATH");
    std::env::split_paths(&path)
        .map(|d| d.join("git"))
        .find(|p| p.is_file())
        .expect("git on PATH")
}

impl Blocked {
    fn new() -> Self {
        assert!(
            DEADLINE + MARGIN < GIT_TIMEOUT,
            "the test needs a short deadline"
        );
        // Distinct recipients: no cross-file overlap withholds a project.
        let repos: Vec<Repo> = [
            ("a-dm", "U0ABCDEF1"),
            ("b-dm", "U0ABCDEF2"),
            ("c-dm", "U0ABCDEF3"),
        ]
        .into_iter()
        .map(|(name, recipient)| {
            let repo = Repo::init("main");
            repo.commit_routes(&slack_routes(name, recipient));
            repo
        })
        .collect();
        std::fs::write(repos[0].dir().join(".block-git"), b"").expect("marker");
        let dirs: Vec<&Path> = repos.iter().map(Repo::dir).collect();
        let home = Home::listing(&dirs);
        let (tmp, bin) = tempdir();
        let pids = bin.join("pids");
        // In the blocked project: a grandchild that holds stdout, both pids
        // recorded, then a sleep far past GIT_TIMEOUT.
        let script = format!(
            "#!/bin/sh\nif [ -e .block-git ]; then\n  /bin/sleep 300 &\n  \
             echo \"$$ $!\" >> '{}'\n  exec /bin/sleep 300\nfi\nexec '{}' \"$@\"\n",
            pids.display(),
            real_git().display()
        );
        let wrapper = bin.join("git");
        std::fs::write(&wrapper, script).expect("write git wrapper");
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        // A sibling test's fork can briefly hold the new script's write fd
        // (ETXTBSY), so wait until the wrapper runs before any load uses it.
        let runs = (0..50).any(|_| {
            let ok = Command::new(&wrapper)
                .arg("--version")
                .current_dir(&bin)
                .output()
                .is_ok_and(|o| o.status.success());
            if !ok {
                std::thread::sleep(Duration::from_millis(20));
            }
            ok
        });
        assert!(runs, "the git wrapper never ran");
        Self {
            repos,
            home,
            _tmp: tmp,
            bin,
            pids,
        }
    }

    /// The gate's PATH: the wrapper first.
    fn path(&self) -> OsString {
        let rest = std::env::var_os("PATH").unwrap_or_default();
        let dirs = std::iter::once(self.bin.clone()).chain(std::env::split_paths(&rest));
        std::env::join_paths(dirs).expect("join PATH")
    }

    /// Run a load with [`DEADLINE`] on its own thread; `None` when it has
    /// not returned within `wait`. Also returns the time taken.
    fn load(&self, wait: Duration) -> (Option<LoadReport>, Duration) {
        let (req, path) = (self.home.request(), self.path());
        let start = Instant::now();
        let deadline = start + DEADLINE;
        let report = within(wait, move || {
            with_git_path(path, || load_effective_until(&req, deadline))
        });
        (report, start.elapsed())
    }

    /// Every pid the wrapper recorded.
    fn recorded(&self) -> Vec<libc::pid_t> {
        let text = std::fs::read_to_string(&self.pids).unwrap_or_default();
        text.split_whitespace()
            .map(|p| p.parse().expect("pid"))
            .collect()
    }

    /// The recorded pids still alive after up to `grace`; each is killed.
    fn survivors(&self, grace: Duration) -> Vec<libc::pid_t> {
        let until = Instant::now() + grace;
        let mut alive: Vec<libc::pid_t> = self.recorded();
        loop {
            // SAFETY: kill(2) with signal 0 only checks the pid.
            alive.retain(|&pid| unsafe { libc::kill(pid, 0) } == 0);
            if alive.is_empty() || Instant::now() >= until {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        for &pid in &alive {
            // SAFETY: as above; frees a sleep this test started.
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
        alive
    }
}

impl Drop for Blocked {
    fn drop(&mut self) {
        self.survivors(Duration::ZERO);
    }
}

#[test]
fn load_with_a_deadline_returns_within_deadline_and_margin() {
    let blocked = Blocked::new();
    let (report, took) = blocked.load(DEADLINE + MARGIN);
    assert!(
        report.is_some(),
        "the load had not returned after {took:?}; deadline {DEADLINE:?} + margin {MARGIN:?}"
    );
    assert!(took >= DEADLINE, "returned before the deadline: {took:?}");
}

#[test]
fn load_deadline_refuses_every_project_not_yet_loaded() {
    let blocked = Blocked::new();
    let (report, took) = blocked.load(Duration::from_secs(60));
    let report = report.unwrap_or_else(|| panic!("the load did not return in {took:?}"));
    assert!(report.policy.is_empty(), "routes loaded: {report:#?}");
    for repo in &blocked.repos {
        assert_eq!(
            file_state(&report, repo.dir()),
            FileState::Refused,
            "{}",
            repo.dir().display()
        );
        let error = file_error(&report, &repo.routes_file());
        assert!(
            matches!(
                error,
                Some(ProjectFileError::Gate {
                    error: GateError::GitTimedOut { .. }
                })
            ),
            "{}: {error:?}",
            repo.dir().display()
        );
    }
}

#[test]
fn no_git_process_survives_a_load_deadline() {
    let blocked = Blocked::new();
    let (_report, took) = blocked.load(DEADLINE + MARGIN);
    assert!(!blocked.recorded().is_empty(), "the wrapper never blocked");
    let left = blocked.survivors(Duration::from_millis(500));
    assert!(
        left.is_empty(),
        "wrapper processes {left:?} still ran {took:?} after a {DEADLINE:?} deadline"
    );
}
