//! #8783: a detached daemon must outlive a SIGKILL of its spawner's process group.
//!
//! Why: `trusty-audit` runs `tga audit` in its own process group and SIGKILLs
//! that group on a budget timeout or Ctrl-C. A daemon `tga` auto-started
//! through [`super::spawn_detached`] used to inherit that group and die with
//! it, taking the operator's `trusty-search` / `trusty-analyze` down too.
//! What: re-invokes this test binary as a stub parent that leads its own
//! process group, has it start `/bin/sleep` through the real helper, SIGKILLs
//! the stub's whole group, and asserts the sleeper is still running. No trusty
//! daemon is started and no host state is read or written.
//! Test: `a_detached_daemon_survives_a_sigkill_of_its_spawners_group`, with
//! `stub_parent_spawns_a_detached_sleeper` as its child half.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt as _;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::spawn_detached;

/// Set on the stub parent only; the child half is a no-op without it.
const STUB_ENV: &str = "TRUSTY_COMMON_DETACH_SESSION_STUB";

/// Prefix of the line the stub prints once the sleeper is started.
///
/// Matched with `find`, never equality: under `--nocapture` libtest's own
/// unterminated `test <name> ... ` progress text shares the line.
const PID_MARKER: &str = "TRUSTY-DETACHED-PID=";

/// Hang guard: how long the stub gets to report the sleeper's pid.
const STUB_REPORT_BOUND: Duration = Duration::from_secs(60);

/// How long the sleeper is watched after the group kill. Not a timing
/// assertion: a sleeper inside the killed group is gone well inside it, and
/// one outside the group is still there at its end.
const SURVIVAL_WINDOW: Duration = Duration::from_secs(2);

/// Upper bound on the stub's own life, so a SIGKILLed test binary cannot leave
/// it behind forever.
const STUB_LIFETIME: Duration = Duration::from_secs(120);

/// True while `pid` exists. Signal 0 delivers nothing.
fn alive(pid: libc::pid_t) -> bool {
    // SAFETY: `kill` with signal 0 only checks that the pid exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// The stub parent: SIGKILLs its own process group on drop unless already reaped.
struct StubGroup {
    child: Child,
    reaped: bool,
}

impl StubGroup {
    /// SIGKILL the stub's whole process group, then reap the stub.
    fn kill_group(&mut self) -> std::io::Result<()> {
        // SAFETY: the group id is our own unreaped child's pid, which
        // `process_group(0)` made its pgid; the zombie keeps it reserved.
        let rc = unsafe { libc::killpg(self.child.id() as libc::pid_t, libc::SIGKILL) };
        let result = if rc == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        };
        let _ = self.child.wait();
        self.reaped = true;
        result
    }
}

impl Drop for StubGroup {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.kill_group();
        }
    }
}

/// The detached sleeper: SIGKILLed on drop when it was last seen alive.
struct Sleeper(Option<libc::pid_t>);

impl Drop for Sleeper {
    fn drop(&mut self) {
        if let Some(pid) = self.0.take() {
            // SAFETY: `pid` is the sleeper our own stub started, still alive
            // at the last check.
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
    }
}

#[test]
fn a_detached_daemon_survives_a_sigkill_of_its_spawners_group() {
    let exe = std::env::current_exe().expect("the test binary's own path");
    let mut cmd = Command::new(exe);
    cmd.args([
        "--exact",
        "daemon_guard::session_tests::stub_parent_spawns_a_detached_sleeper",
        "--nocapture",
        "--test-threads=1",
    ])
    .env(STUB_ENV, "1")
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::null())
    // The stub leads its own group, as `tga audit` does under trusty-audit.
    .process_group(0);
    let mut stub = StubGroup {
        child: cmd.spawn().expect("spawn the stub parent"),
        reaped: false,
    };
    let stub_pgid = stub.child.id() as libc::pid_t;

    let stdout = stub.child.stdout.take().expect("piped stdout");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut transcript = String::new();
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if let Some(at) = line.find(PID_MARKER) {
                let pid = line[at + PID_MARKER.len()..].trim().parse::<libc::pid_t>();
                let _ = tx.send(pid.map_err(|e| format!("{e}: {line}")));
                return;
            }
            transcript.push_str(&line);
            transcript.push('\n');
        }
        let _ = tx.send(Err(format!("stub exited without a pid: {transcript}")));
    });
    let daemon = match rx.recv_timeout(STUB_REPORT_BOUND) {
        Ok(Ok(pid)) => pid,
        Ok(Err(why)) => panic!("the stub parent did not report its sleeper: {why}"),
        Err(_) => panic!("the stub parent reported nothing within {STUB_REPORT_BOUND:?}"),
    };
    let mut sleeper = Sleeper(Some(daemon));
    // SAFETY: `getpgid` only reads the process group of an existing pid.
    let daemon_pgid = unsafe { libc::getpgid(daemon) };

    // The kill `trusty-audit` issues on a budget timeout or Ctrl-C.
    stub.kill_group().expect("SIGKILL the stub's process group");

    let give_up = Instant::now() + SURVIVAL_WINDOW;
    while alive(daemon) && Instant::now() < give_up {
        std::thread::sleep(Duration::from_millis(50));
    }
    let survived = alive(daemon);
    if !survived {
        sleeper.0 = None;
    }
    assert!(
        survived,
        "the detached child {daemon} died with its spawner's process group \
         {stub_pgid}; its pgid before the kill was {daemon_pgid} (#8783)"
    );
}

/// Child half of `a_detached_daemon_survives_a_sigkill_of_its_spawners_group`.
///
/// What: returns at once unless `STUB_ENV` is set, so an ordinary suite run
/// costs nothing. When set, starts `/bin/sleep` through [`spawn_detached`],
/// prints its pid after `PID_MARKER`, and waits to be killed.
#[test]
fn stub_parent_spawns_a_detached_sleeper() {
    if std::env::var_os(STUB_ENV).is_none() {
        return;
    }
    let pid = spawn_detached("/bin/sleep", &["120"]).expect("spawn the detached sleeper");
    println!("{PID_MARKER}{pid}");
    let _ = std::io::stdout().flush();
    std::thread::sleep(STUB_LIFETIME);
}
