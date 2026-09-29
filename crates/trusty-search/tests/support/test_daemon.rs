//! The one way a `trusty-search` integration test starts the binary (#8900).
//!
//! Why: test-spawned daemons outlived their test run. On 2026-09-29 two debug
//! `trusty-search start --foreground` daemons had run for two days with ppid 1,
//! each against a temp data dir its test had already deleted. A `Drop` guard
//! covers a panic or a clean return, but a SIGKILLed test binary (a `cargo
//! test` timeout, an interrupted run) runs no destructor, and a daemon the CLI
//! auto-starts is a grandchild no test holds a handle to.
//! What: three pieces, the trusty-search counterpart of trusty-memory's #8757
//! fixture. [`command`] builds every `trusty-search` `Command` with the
//! `trusty_common::parent_death` stamp, so the daemon it starts, directly or
//! through a CLI auto-spawn, exits once this test binary is gone.
//! [`DaemonGuard`] owns a direct `start --foreground` spawn and kills it in
//! `Drop`. [`run_bounded`] runs a CLI with captured output that cannot hang on
//! a descendant holding its pipes.
//! Test: `test_daemon_exit_8900.rs`.

#![allow(dead_code)] // Each test binary uses a different subset.

use std::io::Read;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// How long [`run_bounded`] keeps reading a pipe after the child is reaped.
///
/// Why: a descendant that inherited the pipe holds EOF off; past this bound the
/// output read so far is returned rather than waiting for that descendant.
pub const DRAIN_GRACE: Duration = Duration::from_secs(5);

/// The `trusty-search` binary Cargo built for this test run.
pub fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_trusty-search"))
}

/// A `trusty-search` `Command` stamped so any daemon it starts dies with this
/// test binary.
///
/// Why: the stamp reaches a daemon the CLI auto-starts through the inherited
/// environment (`spawn_daemon_with_device` forwards it), which no `Drop` in
/// this process can reach.
/// What: `Command::new(binary())` plus
/// `trusty_common::parent_death::exit_with_parent`. Callers add arguments and
/// environment as before.
/// Test: `cli_auto_started_daemon_exits_when_its_test_binary_is_killed`.
pub fn command() -> Command {
    let mut cmd = Command::new(binary());
    // #8900: `start --foreground` arms `parent_death::arm_from_env`, so the
    // daemon watches this process and exits once it is gone.
    trusty_common::parent_death::exit_with_parent(&mut cmd);
    cmd
}

/// Owns a spawned `trusty-search start --foreground` daemon and kills it on
/// drop.
///
/// Why: `Drop` runs on the panic unwind, so the daemon dies with the test that
/// spawned it; the parent-death stamp [`command`] sets covers the SIGKILL case
/// `Drop` cannot. Owning the spawn keeps the stamp from being left off.
/// What: kills and reaps in `Drop`, discarding both results so teardown never
/// masks the test's own failure.
/// Test: `guard_drop_ends_its_daemon`,
/// `guard_daemon_exits_when_its_test_binary_is_killed`.
pub struct DaemonGuard {
    child: Child,
}

impl DaemonGuard {
    /// Spawn a hermetic `start --foreground` against `data_dir`.
    ///
    /// Why each setting: `TRUSTY_DATA_DIR` and `TRUSTY_DATA_DIR_OVERRIDE` keep
    /// the lockfile, discovery files and socket inside `data_dir`; `HOME` and
    /// `XDG_CONFIG_HOME` keep it off the operator's allowlist and config;
    /// `--no-auto-discover` registers no index; `TRUSTY_EMBEDDER=stdio` spawns
    /// no embedder until an embed request, which these tests never send.
    /// Stderr goes to a file in `data_dir`, not this process's stderr, so the
    /// daemon never holds a pipe a piped `cargo test` waits on.
    /// What: spawns on `port` and returns the guard. Panics on a spawn failure.
    /// Test: as the type.
    pub fn spawn(data_dir: &Path, port: u16) -> Self {
        let home = data_dir.join("home");
        std::fs::create_dir_all(&home).expect("create the daemon's fake HOME");
        let log = std::fs::File::create(data_dir.join("daemon.stderr.log"))
            .expect("create the daemon's stderr log");
        let child = command()
            .args(["start", "--foreground", "--no-auto-discover", "--port"])
            .arg(port.to_string())
            .env("TRUSTY_DATA_DIR", data_dir)
            .env("TRUSTY_DATA_DIR_OVERRIDE", data_dir)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", &home)
            .env("TRUSTY_EMBEDDER", "stdio")
            .env("TRUSTY_SKIP_RAM_CHECK", "1")
            .env("RUST_LOG", "warn")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .expect("spawn trusty-search start --foreground");
        Self { child }
    }

    /// The daemon's pid.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// True while a process with `pid` exists (a zombie included).
pub fn pid_alive(pid: u32) -> bool {
    // SAFETY: signal 0 delivers nothing; it only checks existence.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// Serialises every [`run_bounded`] spawn in this test binary.
///
/// Why (#8748): on macOS std sets `FD_CLOEXEC` on a spawn's pipes after
/// `pipe()`, so a concurrent spawn can inherit them and pass them on to a
/// daemon it auto-starts, which then holds another test's output open.
static SPAWN_LOCK: Mutex<()> = Mutex::new(());

/// Output of one [`run_bounded`] call.
pub struct Bounded {
    /// Exit code, or `None` when a signal ended the child.
    pub code: Option<i32>,
    /// Captured stdout followed by captured stderr.
    pub combined: String,
}

/// Run `cmd` with stdout and stderr captured, bounded by `deadline`, leaving
/// nothing in its process group alive.
///
/// Why (#8900, as #8748 for trusty-memory): `Command::output` blocks until
/// every holder of the pipes closes them. A daemon started below the child
/// that holds one keeps a piped run waiting as long as it lives.
/// What: spawns `cmd` as the leader of its own process group, drains both
/// pipes on threads, and waits for the child to exit or `deadline` to pass.
/// It then SIGKILLs the group, reaps the child, and collects the output within
/// [`DRAIN_GRACE`]. The group id is our own unreaped child's pid, so the kill
/// cannot reach a stranger. A daemon the CLI auto-starts leads its own session
/// (#8783) and is ended by the stamp [`command`] sets instead.
/// Test: `run_bounded_returns_while_a_grandchild_holds_the_pipes`.
pub fn run_bounded(cmd: &mut Command, deadline: Duration) -> Bounded {
    cmd.process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = {
        let _serial = SPAWN_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        cmd.spawn().expect("spawn the command under test")
    };
    let group = child.id();
    let stdout = Drained::start(child.stdout.take().expect("piped stdout"));
    let stderr = Drained::start(child.stderr.take().expect("piped stderr"));

    let give_up = Instant::now() + deadline;
    while !has_exited_unreaped(group) && Instant::now() < give_up {
        std::thread::sleep(Duration::from_millis(20));
    }
    // SAFETY: `group` is the pgid `process_group(0)` gave our own child, which
    // stays reserved by its unreaped zombie until `wait` below.
    unsafe { libc::killpg(group as libc::pid_t, libc::SIGKILL) };
    let status = child.wait().expect("reap the command under test");

    let by = Instant::now() + DRAIN_GRACE;
    let mut combined = stdout.collect(by);
    combined.push_str(&stderr.collect(by));
    Bounded {
        code: status.code(),
        combined,
    }
}

/// True once child `pid` has exited, WITHOUT reaping it.
///
/// Why not `try_wait`: reaping frees the pid and with it the process-group id
/// [`run_bounded`] is about to signal. `WNOWAIT` keeps the zombie.
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

/// One captured pipe, read to EOF on its own thread, so the caller can stop
/// waiting for EOF without losing what was already read.
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
