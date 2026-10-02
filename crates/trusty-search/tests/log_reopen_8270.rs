//! A rotated stderr log is detected and reopened by the daemon itself (#8270).
//!
//! Why: launchd hands the daemon its `StandardErrorPath` as an open fd 2 and
//! never reopens it. After newsyslog renames the log, a daemon that does not
//! notice keeps writing to the renamed (later deleted) file. The rotator sends
//! no signal, so the daemon must notice on its own. Only a real process with a
//! real file on fd 2 shows what happens, so these tests spawn one.
//! What: the parent tests re-run this test binary as a child (`writer_child`
//! or `probe_child`, selected by an env var). The writer arms
//! `service::log_reopen` with a 50 ms check interval, prints `ready`, and writes
//! a numbered line to fd 2 every 10 ms. The parent renames the log, as
//! newsyslog does, and checks where later lines land. The probe reports on
//! stdout whether arming did anything for a pipe or `/dev/null` stderr.
//! Test: `a_rename_moves_writes_to_a_new_file_at_the_original_path`,
//! `failed_reopen_keeps_the_old_fd_and_logs_an_error`,
//! `a_pipe_or_dev_null_stderr_arms_nothing`.
#![cfg(unix)]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use trusty_search::service::log_reopen;

/// Env var carrying the log path; its presence selects writer-child mode.
const CHILD_ENV: &str = "TRUSTY_SEARCH_8270_WRITER_LOG";
/// Env var whose presence selects probe-child mode.
const PROBE_ENV: &str = "TRUSTY_SEARCH_8270_PROBE";
const CHECK_INTERVAL: Duration = Duration::from_millis(50);
const WAIT: Duration = Duration::from_secs(10);

fn in_child() -> bool {
    std::env::var_os(CHILD_ENV).is_some() || std::env::var_os(PROBE_ENV).is_some()
}

/// Writer-child entry point. A no-op in a normal test run.
///
/// What: arms the rotation watch on the current stderr, prints `ready`, then
/// writes `tick N` to fd 2 every 10 ms for at most 20 s.
#[test]
fn writer_child() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    rt.block_on(async {
        let _ = tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_ansi(false)
            .try_init();
        log_reopen::arm_for_current_stderr_every(CHECK_INTERVAL).expect("stderr is a regular file");
        let _ = writeln!(std::io::stderr(), "ready");
        for i in 0..2000 {
            let _ = writeln!(std::io::stderr(), "tick {i}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });
}

/// Probe-child entry point. A no-op in a normal test run.
///
/// What: tries to arm on the current stderr and prints `armed=<bool>` to stdout.
#[test]
fn probe_child() {
    if std::env::var_os(PROBE_ENV).is_none() {
        return;
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let armed =
        rt.block_on(async { log_reopen::arm_for_current_stderr_every(CHECK_INTERVAL).is_some() });
    println!("armed={armed}");
}

/// Kills the writer on drop so a failed assertion never leaks a process.
struct Writer(Child);

impl Drop for Writer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn child_command(name: &str) -> Command {
    let mut cmd = Command::new(std::env::current_exe().expect("test exe"));
    cmd.args(["--exact", name, "--nocapture", "--test-threads=1"])
        .stdin(Stdio::null());
    cmd
}

fn spawn_writer(log: &Path) -> Writer {
    let stderr = std::fs::File::create(log).expect("create log");
    let child = child_command("writer_child")
        .env(CHILD_ENV, log)
        .stdout(Stdio::null())
        .stderr(stderr)
        .spawn()
        .expect("spawn writer");
    Writer(child)
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// Poll `cond` every 20 ms for up to [`WAIT`]; panic with `what` on timeout.
fn wait_for(what: &str, cond: impl Fn() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Rename the log as newsyslog does, after the writer is up. No signal is sent.
fn rotate(log: &Path, rotated: &Path, prepare: impl FnOnce()) {
    wait_for("the writer to be ready", || read(log).contains("ready"));
    std::fs::rename(log, rotated).expect("rename");
    prepare();
}

fn alive(w: &mut Writer) -> bool {
    matches!(w.0.try_wait(), Ok(None))
}

/// #8270: after a rename, within the check interval new lines land in a NEW
/// file at the original path, with no signal sent.
///
/// Why: this is the reported defect. Pre-fix a writer kept appending to the
/// renamed file forever; the SIGHUP design needed a pidfile that could go stale.
/// Test: this test.
#[test]
fn a_rename_moves_writes_to_a_new_file_at_the_original_path() {
    if in_child() {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("stderr.log");
    let rotated = dir.path().join("stderr.log.0");
    let mut writer = spawn_writer(&log);

    rotate(&log, &rotated, || {});

    wait_for("ticks in the new file at the original path", || {
        read(&log).contains("tick")
    });
    assert!(alive(&mut writer), "the rotation must not end the writer");
    let old_len = read(&rotated).len();
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        read(&rotated).len(),
        old_len,
        "nothing may keep writing to the rotated file"
    );
    let pids: Vec<_> = std::fs::read_dir(dir.path())
        .expect("read dir")
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "pid"))
        .collect();
    assert!(pids.is_empty(), "no pidfile may be written: {pids:?}");
}

/// #8270 Fail-Open Check: a reopen that fails keeps fd 2 on the old file,
/// logs an error there, and keeps writing. stderr is never closed.
///
/// Why: a directory now sitting at the log path makes the open fail. Closing
/// or losing fd 2 would drop every later log line silently.
/// Test: this test.
#[test]
fn failed_reopen_keeps_the_old_fd_and_logs_an_error() {
    if in_child() {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("stderr.log");
    let rotated = dir.path().join("stderr.log.0");
    let mut writer = spawn_writer(&log);

    let blocker: PathBuf = log.clone();
    rotate(&log, &rotated, move || {
        std::fs::create_dir(&blocker).expect("block the log path");
    });

    wait_for("the reopen error in the old file", || {
        read(&rotated).contains("could not reopen log")
    });
    let after_error = read(&rotated).len();
    wait_for("ticks after the error in the old file", || {
        read(&rotated)[after_error..].contains("tick")
    });
    assert!(
        alive(&mut writer),
        "a failed reopen must not end the writer"
    );
}

/// #8270: a pipe or `/dev/null` on fd 2 arms nothing.
///
/// Why: the detached `start` child runs with `/dev/null` stderr and a
/// supervisor may hand over a pipe; neither has a path to reopen, and a watch
/// over one would reopen the wrong thing.
/// Test: this test.
#[test]
fn a_pipe_or_dev_null_stderr_arms_nothing() {
    if in_child() {
        return;
    }
    for (label, stderr) in [("pipe", Stdio::piped()), ("/dev/null", Stdio::null())] {
        let out = child_command("probe_child")
            .env(PROBE_ENV, "1")
            .stdout(Stdio::piped())
            .stderr(stderr)
            .output()
            .expect("run probe");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{label}: probe failed: {stdout}");
        assert!(
            stdout.contains("armed=false"),
            "{label}: nothing may be armed: {stdout}"
        );
    }
}
