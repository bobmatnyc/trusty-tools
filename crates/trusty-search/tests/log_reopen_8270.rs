//! A rotated stderr log is reopened on SIGHUP (#8270).
//!
//! Why: launchd hands the daemon its `StandardErrorPath` as an open fd 2 and
//! never reopens it. After newsyslog renames the log, a daemon that ignores
//! SIGHUP keeps writing to the renamed (later deleted) file, and a daemon
//! with the default SIGHUP action dies. Only a real process with a real file
//! on fd 2 shows which one happens, so these tests spawn one.
//! What: the parent tests re-run this test binary as a writer child
//! (`writer_child`, selected by an env var) whose stderr is a temp file. The
//! child arms `service::log_reopen` exactly as the daemon does, publishes the
//! pidfile, and writes a numbered line to fd 2 every 10 ms. The parent renames
//! the log, sends SIGHUP to the pid from the pidfile, as newsyslog does, and
//! checks where later lines land.
//! Test: `sighup_after_a_rename_moves_writes_to_the_new_file`,
//! `failed_reopen_keeps_the_old_fd_and_logs_an_error`.
#![cfg(unix)]

use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use trusty_search::service::log_reopen;

/// Env var carrying the log path; its presence selects writer-child mode.
const CHILD_ENV: &str = "TRUSTY_SEARCH_8270_WRITER_LOG";
const WAIT: Duration = Duration::from_secs(10);

/// Writer-child entry point. A no-op in a normal test run.
///
/// What: arms the reopen on the current stderr, publishes the pidfile, prints
/// `ready`, then writes `tick N` to fd 2 every 10 ms for at most 20 s.
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
        log_reopen::arm_for_current_stderr().expect("stderr is a regular file");
        log_reopen::publish_pidfile();
        let _ = writeln!(std::io::stderr(), "ready");
        for i in 0..2000 {
            let _ = writeln!(std::io::stderr(), "tick {i}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });
}

/// Kills the writer on drop so a failed assertion never leaks a process.
struct Writer(Child);

impl Drop for Writer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn_writer(log: &Path) -> Writer {
    let stderr = std::fs::File::create(log).expect("create log");
    let child = Command::new(std::env::current_exe().expect("test exe"))
        .args(["--exact", "writer_child", "--nocapture", "--test-threads=1"])
        .env(CHILD_ENV, log)
        .stdin(Stdio::null())
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

/// Rename the log and SIGHUP the pid the child published, as newsyslog does.
fn rotate_and_signal(log: &Path, rotated: &Path, prepare: impl FnOnce()) {
    let pidfile = log_reopen::pidfile_for_log(log);
    wait_for("the writer to publish its pidfile", || {
        read(&pidfile).trim().parse::<i32>().is_ok()
    });
    wait_for("the writer to be ready", || read(log).contains("ready"));
    let pid: i32 = read(&pidfile).trim().parse().expect("pid");
    std::fs::rename(log, rotated).expect("rename");
    prepare();
    kill(Pid::from_raw(pid), Signal::SIGHUP).expect("SIGHUP");
}

fn alive(w: &mut Writer) -> bool {
    matches!(w.0.try_wait(), Ok(None))
}

/// #8270: after a rename and SIGHUP, new lines land in a NEW file at the
/// original path, and the writer survives the signal.
///
/// Why: this is the reported defect. Pre-fix the default SIGHUP action killed
/// the writer, and a writer that ignored it kept appending to the renamed file.
/// Test: this test.
#[test]
fn sighup_after_a_rename_moves_writes_to_the_new_file() {
    if std::env::var_os(CHILD_ENV).is_some() {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("stderr.log");
    let rotated = dir.path().join("stderr.log.0");
    let mut writer = spawn_writer(&log);

    rotate_and_signal(&log, &rotated, || {});

    wait_for("ticks in the new file at the original path", || {
        read(&log).contains("tick")
    });
    assert!(alive(&mut writer), "SIGHUP must not end the writer");
    let old_len = read(&rotated).len();
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        read(&rotated).len(),
        old_len,
        "nothing may keep writing to the rotated file"
    );
}

/// #8270 Fail-Open Check: a reopen that fails keeps fd 2 on the old file,
/// logs an error there, and keeps writing. stderr is never closed.
///
/// Why: a directory now sitting at the log path makes the open fail. Closing
/// or losing fd 2 would drop every later log line silently.
/// Test: this test.
#[test]
fn failed_reopen_keeps_the_old_fd_and_logs_an_error() {
    if std::env::var_os(CHILD_ENV).is_some() {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("stderr.log");
    let rotated = dir.path().join("stderr.log.0");
    let mut writer = spawn_writer(&log);

    let blocker: PathBuf = log.clone();
    rotate_and_signal(&log, &rotated, move || {
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
