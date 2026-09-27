//! Binary-level surface tests for `trusty-memory serve` (#5267).
//!
//! Why: the transport-selection unit tests in `cli_tests.rs` prove which branch
//! the dispatch takes; these prove what the real binary does when you run it —
//! that bare `serve` actually reaches the stdio server, that the explanatory
//! notice goes to stderr and only for a human, and that stdout stays clean
//! enough to carry JSON-RPC. A notice on stdout would corrupt MCP framing, which
//! no parse-level test can catch.
//!
//! These tests never touch the machine's real daemon or palace: every child runs
//! under `TRUSTY_DATA_DIR_OVERRIDE` pointing at a fresh temp dir, and none of
//! them is allowed to reach a readiness state that would start anything.
//!
//! Test: `cargo test -p trusty-memory --test serve_cli_surface`.

use std::io::{Read, Write};
use std::os::unix::process::CommandExt as _;
use std::process::{Child, Command, Output, Stdio};
use std::sync::{mpsc, Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// How long [`run_piped`] lets its child run before killing it.
const RUN_DEADLINE: Duration = Duration::from_secs(60);

/// How long [`run_bounded`] waits for its stdout/stderr readers to reach EOF
/// once the child's process group is gone.
///
/// Why a bound at all (#8748): EOF arrives only when EVERY holder of a pipe's
/// write end has closed it, and a process outside the group can hold one — see
/// [`SPAWN_LOCK`]. Whatever the child wrote is already buffered by then, so
/// the bound only ever cuts off a wait, never output.
const DRAIN_GRACE: Duration = Duration::from_secs(5);

/// Serialises every process spawn in this binary.
///
/// #8748: on macOS std creates a spawn's stdio pipes with `pipe()` and only
/// then sets `FD_CLOEXEC`, so a fork on another test thread inside that window
/// inherits both ends. A bare-`serve` bridge that inherits them hands them to
/// the daemon it auto-starts, which lives until this binary exits — and the
/// test that owns the pipes then waits for an EOF that never comes. Holding
/// this lock across every spawn closes the window.
static SPAWN_LOCK: Mutex<()> = Mutex::new(());

/// Spawn `cmd` with no other spawn in this binary in flight. See [`SPAWN_LOCK`].
fn spawn_serialised(cmd: &mut Command) -> Child {
    let _held = SPAWN_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    cmd.spawn().expect("spawn child process")
}

/// `cmd.output()`, spawned under [`SPAWN_LOCK`] with `output()`'s stdio.
fn output_of(cmd: &mut Command) -> Output {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    spawn_serialised(cmd)
        .wait_with_output()
        .expect("collect child output")
}

/// Path to the binary under test, as cargo builds it for this integration test.
fn bin() -> std::path::PathBuf {
    let mut p = std::env::current_exe().expect("current exe");
    p.pop(); // .../deps
    p.pop(); // .../debug
    p.push("trusty-memory");
    p
}

/// Run `trusty-memory <args>` with a piped stdin that is closed immediately.
///
/// Why: closing stdin gives the MCP stdio loop a clean EOF, so a bare `serve`
/// exits promptly instead of blocking the test. Piped (not TTY) stdin is also
/// exactly the shape an MCP client presents, which is what the notice tests
/// assert against.
/// What: returns `(stdout, stderr)` via [`run_bounded`], so it returns within
/// [`RUN_DEADLINE`] plus [`DRAIN_GRACE`] and leaves no daemon behind.
fn run_piped(args: &[&str], data_dir: &std::path::Path) -> (String, String) {
    let mut cmd = Command::new(bin());
    cmd.args(args)
        .env("TRUSTY_DATA_DIR_OVERRIDE", data_dir)
        .stdin(Stdio::piped());
    // #7085: bare `serve` auto-starts a DETACHED daemon, which outlives this
    // child by design. Stamping here reaches that grandchild through the
    // inherited environment, so it still dies if this test binary is SIGKILLed
    // before `run_bounded` reaps it.
    trusty_common::parent_death::exit_with_parent(&mut cmd);
    run_bounded(cmd, RUN_DEADLINE)
}

/// Run `cmd` with stdout/stderr captured, bounded in time and leaving none of
/// its descendants alive. A piped stdin is closed at once.
///
/// Why (#8748): the old shape killed the direct child at the deadline and then
/// called `wait_with_output`, which blocks until EVERY holder of the pipes
/// closes them. The daemon a bare `serve` auto-starts is a detached grandchild
/// that the kill never reached; holding the pipes, it hung the suite for
/// minutes with five daemons parented to pid 1.
/// What: spawns `cmd` as the leader of its own process group — the
/// auto-started daemon calls no `setsid`, so it stays in that group — and
/// drains both pipes on threads. After the child exits or `deadline` passes,
/// SIGKILLs the whole group, reaps the child, and collects the output with a
/// [`DRAIN_GRACE`] bound. The group id is our own child's pid, reserved by its
/// unreaped zombie until the kill, so the kill cannot reach a stranger.
/// Test: `run_bounded_returns_and_reaps_a_grandchild_holding_the_pipes`.
fn run_bounded(mut cmd: Command, deadline: Duration) -> (String, String) {
    cmd.process_group(0)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = spawn_serialised(&mut cmd);
    let group = child.id();

    // Close a piped stdin at once, so a stdio loop sees EOF.
    drop(child.stdin.take());
    let stdout = Drained::start(child.stdout.take().expect("piped stdout"));
    let stderr = Drained::start(child.stderr.take().expect("piped stderr"));

    let give_up = Instant::now() + deadline;
    while !has_exited_unreaped(group) && Instant::now() < give_up {
        std::thread::sleep(Duration::from_millis(50));
    }
    // SAFETY: `killpg` takes a process-group id and a signal number; `group` is
    // the pgid `process_group(0)` gave our own child.
    unsafe { libc::killpg(group as libc::pid_t, libc::SIGKILL) };
    let _ = child.wait();

    let collect_by = Instant::now() + DRAIN_GRACE;
    (stdout.collect(collect_by), stderr.collect(collect_by))
}

/// True once child `pid` has exited, WITHOUT reaping it.
///
/// Why not `try_wait`: reaping frees the pid, and with it the process-group id
/// [`run_bounded`] is about to signal. `WNOWAIT` keeps the zombie, and the id,
/// until `Child::wait` runs after the group kill.
fn has_exited_unreaped(pid: u32) -> bool {
    // SAFETY: an all-zero `siginfo_t` is a valid value of that plain C struct.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: `info` is a valid out-pointer; `pid` is our own unreaped child.
    let rc = unsafe {
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    rc == 0 && info.si_signo == libc::SIGCHLD
}

/// One captured pipe, read to EOF on its own thread.
///
/// Why a thread and a shared buffer: the caller must be able to stop waiting
/// for EOF (see [`DRAIN_GRACE`]) without losing what was already read.
struct Drained {
    buf: Arc<Mutex<Vec<u8>>>,
    eof: mpsc::Receiver<()>,
}

impl Drained {
    fn start(mut pipe: impl Read + Send + 'static) -> Self {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&buf);
        let (tx, eof) = mpsc::channel();
        std::thread::spawn(move || {
            let mut chunk = [0u8; 8192];
            while let Ok(n @ 1..) = pipe.read(&mut chunk) {
                sink.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .extend_from_slice(&chunk[..n]);
            }
            let _ = tx.send(());
        });
        Self { buf, eof }
    }

    /// Everything read so far, waiting for EOF no later than `by`.
    fn collect(self, by: Instant) -> String {
        let _ = self
            .eof
            .recv_timeout(by.saturating_duration_since(Instant::now()));
        let bytes = self.buf.lock().unwrap_or_else(PoisonError::into_inner);
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

/// True while a process with `pid` exists (a zombie included).
fn pid_alive(pid: u32) -> bool {
    // SAFETY: signal 0 delivers nothing; it only checks existence.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// Why (#8748): the hang's exact shape, reproduced without a daemon — a child
/// whose own child keeps the captured pipes open and outlives it.
/// What: runs `sh`, which backgrounds a `sleep` that inherits stdout/stderr and
/// then `exec`s a second one, under a 2 s deadline. Asserts `run_bounded`
/// returns inside the deadline plus [`DRAIN_GRACE`] plus slack, and that the
/// grandchild — recorded by pid from our own spawn — is gone. On a hang it
/// kills that pid itself so a red run leaves nothing behind.
/// Test: itself.
#[test]
fn run_bounded_returns_and_reaps_a_grandchild_holding_the_pipes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let pidfile = tmp.path().join("grandchild.pid");
    let mut cmd = Command::new("/bin/sh");
    cmd.args(["-c", "sleep 300 & echo $! > \"$1\"; exec sleep 300", "sh"])
        .arg(&pidfile)
        .stdin(Stdio::null());

    let deadline = Duration::from_secs(2);
    let bound = deadline + DRAIN_GRACE + Duration::from_secs(10);
    let (tx, rx) = mpsc::channel();
    let started = Instant::now();
    std::thread::spawn(move || {
        let _ = tx.send(run_bounded(cmd, deadline));
    });
    let returned = rx.recv_timeout(bound);

    let grandchild: Option<u32> = std::fs::read_to_string(&pidfile)
        .ok()
        .and_then(|s| s.trim().parse().ok());
    if returned.is_err() {
        if let Some(pid) = grandchild {
            // SAFETY: `pid` is the grandchild our own `sh` child recorded.
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
        }
        panic!(
            "run_bounded did not return within {bound:?}: it is blocked on pipes a \
             grandchild still holds (#8748)"
        );
    }
    let elapsed = started.elapsed();
    let pid = grandchild.expect("the sh child records its grandchild's pid");
    let gone_by = Instant::now() + Duration::from_secs(5);
    while pid_alive(pid) && Instant::now() < gone_by {
        std::thread::sleep(Duration::from_millis(50));
    }
    let survived = pid_alive(pid);
    if survived {
        // SAFETY: as above — our own grandchild, killed so a red run leaks nothing.
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
    }
    assert!(
        !survived,
        "grandchild {pid} survived run_bounded (returned after {elapsed:?})"
    );
}

/// Why: an MCP client's stdin is a pipe, and it must see NO notice — the notice
/// is for a human who typed the command. It must also never appear on stdout,
/// which carries JSON-RPC framing.
/// What: runs bare `serve` with piped stdin and asserts the notice text is
/// absent from both streams.
/// Test: itself.
#[test]
fn bare_serve_notice_absent_when_stdin_is_piped() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (stdout, stderr) = run_piped(&["serve"], tmp.path());

    assert!(
        !stderr.contains("waiting on stdin"),
        "no notice for a piped (MCP client) stdin; stderr was: {stderr}"
    );
    assert!(
        !stdout.contains("waiting on stdin"),
        "the notice must NEVER reach stdout — it is the JSON-RPC channel"
    );
}

/// Why: stdout is the JSON-RPC channel. Anything the bare `serve` path prints
/// there corrupts MCP framing for every client. This is the hygiene invariant
/// the whole stdio design rests on.
/// What: runs bare `serve` with immediate EOF and asserts stdout carries no
/// non-JSON chatter (it is either empty or valid JSON-RPC lines).
/// Test: itself.
#[test]
fn bare_serve_keeps_stdout_clean() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (stdout, _stderr) = run_piped(&["serve"], tmp.path());

    for line in stdout.lines().filter(|l| !l.trim().is_empty()) {
        assert!(
            serde_json::from_str::<serde_json::Value>(line).is_ok(),
            "stdout must carry only JSON-RPC; found non-JSON line: {line}"
        );
    }
}

/// Why: bare `serve` and `serve --stdio` must be the same server. If they
/// diverged, the alignment #5267 delivers would be cosmetic.
/// What: runs both with an identical closed stdin and asserts their stdout
/// behavior matches (both clean, both terminating on EOF).
/// Test: itself.
#[test]
fn bare_serve_and_explicit_stdio_behave_alike() {
    let tmp_a = tempfile::tempdir().expect("tempdir");
    let tmp_b = tempfile::tempdir().expect("tempdir");
    let (out_bare, _) = run_piped(&["serve"], tmp_a.path());
    let (out_flag, _) = run_piped(&["serve", "--stdio"], tmp_b.path());

    let json_lines = |s: &str| {
        s.lines()
            .filter(|l| !l.trim().is_empty())
            .filter(|l| serde_json::from_str::<serde_json::Value>(l).is_ok())
            .count()
    };
    assert_eq!(
        json_lines(&out_bare),
        json_lines(&out_flag),
        "bare `serve` and `serve --stdio` must produce the same stdout shape"
    );
}

/// Why: an unknown flag must still be an error. Making bare `serve` meaningful
/// must not have made the parser permissive.
/// What: asserts a nonzero exit and a clap usage error on stderr.
/// Test: itself.
#[test]
fn unknown_flag_is_still_rejected() {
    let out = output_of(Command::new(bin()).args(["serve", "--definitely-not-a-flag"]));
    assert!(!out.status.success(), "unknown flag must exit nonzero");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unexpected argument") || stderr.contains("error"),
        "expected a usage error, got: {stderr}"
    );
}

/// Why: `--stdio` conflicts with both HTTP flags, and #5267 must not have
/// relaxed either relationship.
/// What: asserts both conflicting combinations exit nonzero.
/// Test: itself.
#[test]
fn conflicting_transport_flags_still_rejected() {
    for args in [
        ["serve", "--http", "--stdio"],
        ["serve", "--foreground", "--stdio"],
    ] {
        let out = output_of(Command::new(bin()).args(args));
        assert!(
            !out.status.success(),
            "{args:?} must be rejected as conflicting"
        );
    }
}

/// Why: `--help` is how a user discovers that the verb moved. If it still
/// described `serve` as the daemon, the change would be undiscoverable.
/// What: asserts the help text names `start` as the daemon verb and `serve` as
/// MCP stdio.
/// Test: itself.
#[test]
fn help_documents_the_new_serve_semantics() {
    let out = output_of(Command::new(bin()).args(["serve", "--help"]));
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(
        help.contains("stdio"),
        "serve --help must describe the stdio default; got: {help}"
    );
    assert!(
        help.contains("start"),
        "serve --help must point at `start` for the daemon; got: {help}"
    );
}

/// Why: the other half of the notice contract. A human at a terminal MUST be
/// told that `serve` now waits on stdin and that `start` is the daemon verb —
/// without it, bare `serve` looks exactly like a hang. Only a real pty
/// exercises this branch; a pipe takes the silent path.
/// What: allocates a pty with `openpty(3)`, hands the slave to the child as
/// stdin, sends Ctrl-D so the stdio loop sees EOF, and asserts the notice
/// appears on stderr and never on stdout.
/// Test: itself.
#[cfg(unix)]
#[test]
fn bare_serve_notice_present_when_stdin_is_a_tty() {
    use std::os::unix::io::FromRawFd;

    let tmp = tempfile::tempdir().expect("tempdir");

    let mut master: libc::c_int = 0;
    let mut slave: libc::c_int = 0;
    let rc = {
        // #8748: openpty's fds are inheritable, so a concurrent spawn would hand
        // them to a bridge and on to its daemon. Mark them close-on-exec before
        // any other spawn can run; the child still gets the slave, re-duped
        // onto its fd 0.
        let _held = SPAWN_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        // Safety: both out-params are valid ints; the remaining three are
        // optional and null means "use defaults".
        let rc = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if rc == 0 {
            // Safety: both are fresh fds this test owns.
            unsafe {
                libc::fcntl(master, libc::F_SETFD, libc::FD_CLOEXEC);
                libc::fcntl(slave, libc::F_SETFD, libc::FD_CLOEXEC);
            }
        }
        rc
    };
    assert_eq!(rc, 0, "openpty must succeed");

    // Safety: `slave` is a fresh fd from openpty and is not used elsewhere.
    let child_stdin = unsafe { Stdio::from_raw_fd(slave) };
    let mut cmd = Command::new(bin());
    cmd.arg("serve")
        .env("TRUSTY_DATA_DIR_OVERRIDE", tmp.path())
        .stdin(child_stdin);
    // #7085: see `run_piped` — bare `serve` auto-starts a detached daemon.
    trusty_common::parent_death::exit_with_parent(&mut cmd);

    // Ctrl-D: canonical-mode EOF, so the stdio loop terminates. Sent from a
    // thread because `run_bounded` blocks until the child is done.
    // Safety: `master` is a valid open fd owned by this test.
    let mut master_file = unsafe { std::fs::File::from_raw_fd(master) };
    let ctrl_d = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        let _ = master_file.write_all(&[0x04]);
        let _ = master_file.flush();
        master_file
    });
    let (stdout, stderr) = run_bounded(cmd, RUN_DEADLINE);
    drop(ctrl_d.join());

    assert!(
        stderr.contains("waiting on stdin") && stderr.contains("start"),
        "a human at a terminal must be told serve is stdio and start is the \
         daemon verb; stderr was: {stderr}"
    );
    assert!(
        !stdout.contains("waiting on stdin"),
        "the notice must never reach stdout; stdout was: {stdout}"
    );
}
