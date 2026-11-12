//! Real-process tests for `trusty_common::linux_lifecycle` (#11084).
//!
//! Why: the defects this module replaces — a fixed health wait, trusting a
//! signal's return code, signalling an unidentified pid — all pass against
//! mocks. These tests run the module against real processes on a real socket.
//!
//! What: this test binary is also its own fixture. Run with the
//! `tc-lc-role=daemon` argument, `child_entry` becomes a small daemon that
//! binds the Unix socket named by `TC_LC_SOCKET` (its instance key, #11098)
//! and reacts to SIGTERM per its `mode=`; with
//! `tc-lc-role=runner` it becomes a separate process that calls `start`.
//! Each test hard-links this binary into a temp install dir under the names
//! `lc-daemon` and `lc-other`, so `/proc/<pid>/exe` names the install-dir
//! path. A process that survives SIGKILL cannot be made unprivileged, so the
//! survivor tests run a real SIGTERM-ignoring process and drop SIGKILL through
//! the module's `SignalDelivery` seam.
//!
//! No fixture outlives its test (#11084): every daemon role exits on its own
//! once its socket's directory is gone or `FIXTURE_LIFETIME` passes, and a
//! test arms its cleanup from the pids the fixtures write to their marker,
//! never from the result of the call under test.
//!
//! Test: this file. Linux only, and the module needs the `uds` feature.
#![cfg(all(target_os = "linux", feature = "uds"))]

use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use trusty_common::linux_lifecycle::{
    DaemonDescriptor, LifecycleError, PinnedProcess, Signal, SignalDelivery, StopOutcome,
    StoppedSignal, restart, restart_with, start, stop, stop_with,
};

const ROLE_DAEMON: &str = "tc-lc-role=daemon";
const ROLE_RUNNER: &str = "tc-lc-role=runner";
/// #11098: a stdio bridge stand-in — the same executable as the daemon, run
/// as `daemon_main` but without the daemon's `ROLE_DAEMON` argument.
const ROLE_BRIDGE: &str = "tc-lc-role=bridge";
/// #11098: the fixture's instance key. A fixture daemon reads its socket from
/// this variable when its argv names none, as a real daemon derives its socket
/// from its data-dir variable, so two instances can run with identical argv.
const SOCKET_ENV: &str = "TC_LC_SOCKET";
const DAEMON: &str = "lc-daemon";
const OTHER: &str = "lc-other";
/// Longer than `tm restart`'s 3 s health wait, which AC1 rules out.
const LINGER_MS: u64 = 3_500;
const SERVE_BOUND: Duration = Duration::from_secs(20);
/// Hard limit on any fixture's life, whatever its test does (#11084).
const FIXTURE_LIFETIME: Duration = Duration::from_secs(120);
const WATCH_TICK: Duration = Duration::from_millis(100);
/// How long a wrong implementation's spawned child gets to show up.
const SPAWN_SETTLE: Duration = Duration::from_millis(500);

// ─── Fixture roles ──────────────────────────────────────────────────────────

/// The fixture entry point; a no-op when run as an ordinary test.
#[test]
fn child_entry() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == ROLE_DAEMON || a == ROLE_BRIDGE) {
        daemon_main(&args);
    }
    if args.iter().any(|a| a == ROLE_RUNNER) {
        runner_main(&args);
    }
}

fn arg<'a>(args: &'a [String], key: &str) -> Option<&'a str> {
    args.iter()
        .find_map(|a| a.strip_prefix(key)?.strip_prefix('='))
}

static TERM: AtomicBool = AtomicBool::new(false);

extern "C" fn on_term(_: libc::c_int) {
    TERM.store(true, Ordering::SeqCst);
}

fn append(path: &str, line: &str) {
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("open marker");
    writeln!(f, "{line}").expect("write marker");
}

/// Exit this process once `dir` is gone or `FIXTURE_LIFETIME` has passed.
///
/// Why: a test that panics before its cleanup must not leave a fixture
/// running (#11084); the temp dir is removed as the panic unwinds.
fn arm_watchdog(dir: PathBuf) {
    std::thread::spawn(move || {
        let deadline = Instant::now() + FIXTURE_LIFETIME;
        while Instant::now() < deadline && fs::symlink_metadata(&dir).is_ok() {
            std::thread::sleep(WATCH_TICK);
        }
        std::process::exit(3);
    });
}

/// The watchdog's rule for a forked child, which has no watchdog thread: only
/// async-signal-safe calls (`access`, `nanosleep`, `_exit`).
fn forked_watch(dir: &CString) -> ! {
    let ticks = FIXTURE_LIFETIME.as_millis() / WATCH_TICK.as_millis();
    let tick = libc::timespec {
        tv_sec: 0,
        tv_nsec: WATCH_TICK.as_nanos() as libc::c_long,
    };
    for _ in 0..ticks {
        // SAFETY: a valid C string, and a valid timespec with a null remainder.
        unsafe {
            if libc::access(dir.as_ptr(), libc::F_OK) != 0 {
                break;
            }
            libc::nanosleep(&tick, std::ptr::null_mut());
        }
    }
    // SAFETY: `_exit` is async-signal-safe and never returns.
    unsafe { libc::_exit(3) }
}

/// A daemon stand-in. Modes: `serve` (exit on SIGTERM), `linger` (close the
/// socket on SIGTERM, exit `LINGER_MS` later), `ignore-term`, `never-bind`,
/// `exit-now`, `fork-lock-holder` (on SIGTERM, fork a child that keeps the
/// inherited lock, then exit), and (#11098) `unlink-socket` / `close-socket`
/// (after the bind, unlink the socket file or close the listener, write
/// `unserved` to the marker, and keep running). The `undumpable` flag makes
/// `/proc/<pid>/exe` unreadable to other processes.
fn daemon_main(args: &[String]) -> ! {
    let mode = arg(args, "mode").unwrap_or("serve");
    let socket = arg(args, "socket")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os(SOCKET_ENV).map(PathBuf::from))
        .expect("socket arg or env");
    let watch_dir = socket.parent().expect("socket dir").to_path_buf();
    arm_watchdog(watch_dir.clone());
    if args.iter().any(|a| a == "undumpable") {
        // SAFETY: PR_SET_DUMPABLE takes one integer argument.
        let rc = unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) };
        assert_eq!(rc, 0, "prctl(PR_SET_DUMPABLE)");
    }
    let marker = arg(args, "marker");
    if let Some(m) = marker {
        append(m, &std::process::id().to_string());
    }
    let _lock = arg(args, "lock").map(|p| {
        let f = File::create(p).expect("create lock");
        f.lock().expect("take lock");
        f
    });
    if mode == "exit-now" {
        std::process::exit(7);
    }
    // SAFETY: installing a handler that only stores to an atomic, or SIG_IGN.
    unsafe {
        if mode == "ignore-term" {
            libc::signal(libc::SIGTERM, libc::SIG_IGN);
        } else {
            libc::signal(libc::SIGTERM, on_term as *const () as libc::sighandler_t);
        }
    }
    if mode == "never-bind" {
        loop {
            if TERM.load(Ordering::SeqCst) {
                std::process::exit(0);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    let watch_c = CString::new(watch_dir.as_os_str().as_bytes()).expect("watch dir C string");
    // bind_singleton's shape: `<socket>.lock` is held only while binding, so
    // a module that read it as liveness would see every fixture as stopped.
    let bind_lock = File::create(format!("{}.lock", socket.display())).expect("bind lock");
    bind_lock.lock().expect("take bind lock");
    let _ = fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).expect("bind fixture socket");
    drop(bind_lock);
    listener.set_nonblocking(true).expect("nonblocking");
    let mut listener = Some(listener);
    if mode == "unlink-socket" || mode == "close-socket" {
        if mode == "unlink-socket" {
            fs::remove_file(&socket).expect("unlink fixture socket");
        } else {
            drop(listener.take());
        }
        if let Some(m) = marker {
            append(m, "unserved");
        }
    }
    loop {
        if TERM.load(Ordering::SeqCst) {
            match mode {
                "linger" => {
                    drop(listener.take());
                    if let Some(m) = marker {
                        append(m, "closed");
                    }
                    std::thread::sleep(Duration::from_millis(LINGER_MS));
                    std::process::exit(0);
                }
                "fork-lock-holder" => {
                    drop(listener.take());
                    let _ = fs::remove_file(&socket);
                    // SAFETY: the forked child runs only `forked_watch`,
                    // which is async-signal-safe.
                    let holder = unsafe { libc::fork() };
                    if holder == 0 {
                        forked_watch(&watch_c);
                    }
                    if let Some(m) = marker {
                        append(m, &format!("holder={holder}"));
                    }
                    std::process::exit(0);
                }
                _ => {
                    let _ = fs::remove_file(&socket);
                    std::process::exit(0);
                }
            }
        }
        if let Some(l) = &listener {
            while let Ok((conn, _)) = l.accept() {
                drop(conn);
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Make every later `pidfd_open` in this process (and its children) fail with
/// `ENOSYS`, as on a kernel older than 5.3.
///
/// What: a seccomp filter that compares the native syscall number only; the
/// fixture issues native syscalls, so no arch check is needed. Unprivileged,
/// because `PR_SET_NO_NEW_PRIVS` is set first.
fn deny_pidfd_open() {
    let stmt = |code: u32, k: u32| libc::sock_filter {
        code: code as u16,
        jt: 0,
        jf: 0,
        k,
    };
    let filter = [
        // A = seccomp_data.nr (offset 0).
        stmt(libc::BPF_LD | libc::BPF_W | libc::BPF_ABS, 0),
        // nr == pidfd_open ? next : skip one.
        libc::sock_filter {
            code: (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
            jt: 0,
            jf: 1,
            k: libc::SYS_pidfd_open as u32,
        },
        stmt(
            libc::BPF_RET | libc::BPF_K,
            libc::SECCOMP_RET_ERRNO | libc::ENOSYS as u32,
        ),
        stmt(libc::BPF_RET | libc::BPF_K, libc::SECCOMP_RET_ALLOW),
    ];
    let prog = libc::sock_fprog {
        len: filter.len() as u16,
        filter: filter.as_ptr().cast_mut(),
    };
    // SAFETY: documented prctl forms; `prog` and `filter` outlive the call,
    // and the kernel copies the program.
    unsafe {
        assert_eq!(libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0), 0);
        assert_eq!(
            libc::prctl(
                libc::PR_SET_SECCOMP,
                libc::SECCOMP_MODE_FILTER as libc::c_ulong,
                &raw const prog,
            ),
            0,
            "seccomp filter"
        );
    }
}

/// Pids whose parent is this process, zombies included.
fn children_of_self() -> Vec<u32> {
    let me = std::process::id();
    let Ok(dir) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    dir.flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter(|&pid| {
            fs::read_to_string(format!("/proc/{pid}/stat"))
                .ok()
                .and_then(|s| {
                    let rest = s.rsplit_once(')')?.1;
                    rest.split_whitespace().nth(1)?.parse::<u32>().ok()
                })
                == Some(me)
        })
        .collect()
}

/// A process other than the test that calls `start`, writes the outcome to
/// `out=`, and exits. With `no-pidfd`, `pidfd_open` fails in this process and
/// an error report also lists this process's children left after `start`.
/// With `emfile` (#11098) it calls `stop` instead, with one fd free, and
/// writes the result's `Debug` form.
fn runner_main(args: &[String]) -> ! {
    if args.iter().any(|a| a == "no-pidfd") {
        deny_pidfd_open();
    }
    let socket = PathBuf::from(arg(args, "socket").expect("socket"));
    let marker = PathBuf::from(arg(args, "marker").expect("marker"));
    let out = arg(args, "out").expect("out");
    let mut desc = DaemonDescriptor::with_install_dir(
        DAEMON,
        arg(args, "install").expect("install"),
        &socket,
        StoppedSignal::SocketRefuses,
    )
    .expect("descriptor");
    desc.spawn_args = daemon_args("serve", &[format!("marker={}", marker.display())]);
    set_instance(&mut desc, &socket);
    // #11098: `allow=NAME` puts one name on the environment allowlist.
    desc.env_passthrough = arg(args, "allow").map(str::to_string).into_iter().collect();
    desc.ready_timeout = SERVE_BOUND;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    if args.iter().any(|a| a == "emfile") {
        // #11098: one free fd — the dial's, then `read_dir("/proc")`'s — so
        // the scan's first `/proc/<pid>/status` open fails with EMFILE.
        let fillers = exhaust_fds();
        let stopped = rt.block_on(stop(&desc));
        drop(fillers);
        fs::write(out, format!("{stopped:?}")).expect("write out");
        std::process::exit(0);
    }
    match rt.block_on(start(&desc)) {
        Ok(outcome) => {
            fs::write(out, outcome.pid.to_string()).expect("write out");
            std::process::exit(0);
        }
        Err(e) => {
            let children: Vec<String> = children_of_self().iter().map(u32::to_string).collect();
            fs::write(out, format!("error: {e}\nchildren={}", children.join(",")))
                .expect("write out");
            std::process::exit(1);
        }
    }
}

/// Lower the soft fd limit, open files until `EMFILE`, then free exactly one.
fn exhaust_fds() -> Vec<File> {
    let mut lim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: get/setrlimit with a valid rlimit pointer.
    unsafe {
        assert_eq!(libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut lim), 0);
        lim.rlim_cur = lim.rlim_cur.min(128);
        assert_eq!(libc::setrlimit(libc::RLIMIT_NOFILE, &raw const lim), 0);
    }
    let mut files = Vec::new();
    loop {
        match File::open("/dev/null") {
            Ok(f) => files.push(f),
            Err(e) if e.raw_os_error() == Some(libc::EMFILE) => break,
            Err(e) => panic!("filling the fd table: {e}"),
        }
    }
    files.pop().expect("at least one filler");
    files
}

fn daemon_args(mode: &str, extra: &[String]) -> Vec<String> {
    role_args(ROLE_DAEMON, mode, extra)
}

/// Point `desc` at the instance on `socket` (#11098). The liveness argument is
/// the real daemons' shape — the role alone, as `serve --foreground` is — so
/// every test's fixture daemon matches it, and only `instance_env` tells this
/// test's instance from the others.
fn set_instance(desc: &mut DaemonDescriptor, socket: &Path) {
    desc.socket = socket.to_path_buf();
    desc.liveness_args = vec![ROLE_DAEMON.to_string()];
    desc.instance_env.retain(|(name, _)| name != SOCKET_ENV);
    desc.instance_env
        .push((SOCKET_ENV.to_string(), Some(socket.as_os_str().to_owned())));
}

fn role_args(role: &str, mode: &str, extra: &[String]) -> Vec<String> {
    let mut args = vec![
        "--exact".to_string(),
        "child_entry".to_string(),
        "-q".to_string(),
        role.to_string(),
        format!("mode={mode}"),
    ];
    args.extend_from_slice(extra);
    args
}

// ─── Test harness ───────────────────────────────────────────────────────────

struct Fixture {
    _install_dir: tempfile::TempDir,
    _work_dir: tempfile::TempDir,
    install: PathBuf,
    work: PathBuf,
    socket: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let exe = std::env::current_exe().expect("current_exe");
        let exe_dir = exe.parent().expect("exe dir");
        // Same filesystem as the test binary, so a hard link always works and
        // no copy (with its open write fd) races a concurrent fork.
        let install_dir = tempfile::Builder::new()
            .prefix("lc-install")
            .tempdir_in(exe_dir)
            .expect("install dir");
        for name in [DAEMON, OTHER] {
            fs::hard_link(&exe, install_dir.path().join(name)).expect("hard link fixture");
        }
        let work_dir = tempfile::tempdir().expect("work dir");
        let install = fs::canonicalize(install_dir.path()).expect("canonical install");
        let work = work_dir.path().to_path_buf();
        let socket = work.join("d.sock");
        Self {
            _install_dir: install_dir,
            _work_dir: work_dir,
            install,
            work,
            socket,
        }
    }

    fn descriptor(&self, signal: StoppedSignal) -> DaemonDescriptor {
        self.descriptor_at(signal, &self.socket)
    }

    /// [`Fixture::descriptor`] for the instance on another socket path.
    fn descriptor_at(&self, signal: StoppedSignal, socket: &Path) -> DaemonDescriptor {
        let mut desc = DaemonDescriptor::with_install_dir(DAEMON, &self.install, socket, signal)
            .expect("descriptor");
        desc.stop_grace = Duration::from_secs(30);
        desc.kill_wait = Duration::from_secs(5);
        desc.ready_timeout = SERVE_BOUND;
        set_instance(&mut desc, socket);
        desc
    }

    /// The socket of another instance, in its own directory under the work
    /// dir, so that instance's watchdog ends with the test.
    fn instance_socket(&self, name: &str) -> PathBuf {
        let dir = self.path(name);
        fs::create_dir(&dir).expect("instance dir");
        dir.join("d.sock")
    }

    fn path(&self, name: &str) -> PathBuf {
        self.work.join(name)
    }

    /// Spawn a fixture daemon directly (not through the module) and wait for it
    /// to serve.
    fn spawn(&self, binary: &str, mode: &str, extra: &[String]) -> Running {
        self.spawn_at(binary, mode, &self.socket, extra)
    }

    /// [`Fixture::spawn`] on another socket path.
    fn spawn_at(&self, binary: &str, mode: &str, socket: &Path, extra: &[String]) -> Running {
        let running = self.launch(binary, mode, socket, extra);
        assert!(wait_serving(socket), "fixture never served");
        running
    }

    /// Spawn a fixture daemon without waiting for it to serve.
    fn launch(&self, binary: &str, mode: &str, socket: &Path, extra: &[String]) -> Running {
        self.launch_as(ROLE_DAEMON, binary, mode, socket, extra)
    }

    /// [`Fixture::launch`] with another role argument (#11098: a bridge).
    fn launch_as(
        &self,
        role: &str,
        binary: &str,
        mode: &str,
        socket: &Path,
        extra: &[String],
    ) -> Running {
        let child = Command::new(self.install.join(binary))
            .args(role_args(role, mode, extra))
            .env(SOCKET_ENV, socket)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn fixture");
        Running { child }
    }

    /// A [`Reap`] for every pid the fixtures wrote to `marker`.
    fn reap_reported(&self, marker: &Path) -> Vec<Reap> {
        lines(marker)
            .iter()
            .filter_map(|l| l.parse().ok())
            .map(|pid| Reap::arm(pid, &self.install))
            .collect()
    }
}

/// A fixture child that is SIGKILLed and reaped on drop.
struct Running {
    child: Child,
}

impl Running {
    fn pid(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn wait_serving(socket: &Path) -> bool {
    wait_for(|| UnixStream::connect(socket).is_ok())
}

/// Poll `cond` until it holds or `SERVE_BOUND` passes.
fn wait_for(mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + SERVE_BOUND;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

/// Whether another open file description holds `lock`'s `flock`.
fn lock_held(lock: &Path) -> bool {
    File::open(lock).is_ok_and(|f| matches!(f.try_lock(), Err(fs::TryLockError::WouldBlock)))
}

/// Alive = `/proc/<pid>` exists and is not a zombie.
fn is_alive(pid: u32) -> bool {
    let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    let state = stat
        .rsplit_once(')')
        .and_then(|(_, rest)| rest.trim_start().chars().next());
    !matches!(state, None | Some('Z' | 'X'))
}

fn kill_pid(pid: u32) {
    // SAFETY: plain kill(2) on a pid this test owns.
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
}

/// SIGKILLs a fixture pid on drop unless disarmed.
///
/// Why: the module now reaps a child it failed to start, so its pid can be
/// reused; a bare `kill(pid)` on drop could reach an unrelated process (#11084).
/// What: `arm` pins the pid with a pidfd and keeps it only when
/// `/proc/<pid>/exe` lies in the test's install dir; drop signals through the
/// pidfd, which never reaches a newcomer.
struct Reap(Option<OwnedFd>);

impl Reap {
    fn arm(pid: u32, install: &Path) -> Self {
        // SAFETY: pidfd_open takes a pid and flags and returns a new fd or -1.
        let rc = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0) };
        if rc < 0 {
            return Self(None);
        }
        // SAFETY: a fresh fd owned by no one else.
        let fd = unsafe { OwnedFd::from_raw_fd(rc as i32) };
        let ours =
            fs::read_link(format!("/proc/{pid}/exe")).is_ok_and(|exe| exe.starts_with(install));
        Self(ours.then_some(fd))
    }

    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for Reap {
    fn drop(&mut self) {
        if let Some(fd) = &self.0 {
            // SAFETY: an open pidfd, SIGKILL, null siginfo, zero flags.
            unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    fd.as_raw_fd(),
                    libc::SIGKILL,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                );
            }
        }
    }
}

/// Restores a directory's mode on drop, so the temp dir can be removed.
struct ModeGuard(PathBuf);

impl ModeGuard {
    fn lock(dir: &Path) -> Self {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o000)).expect("chmod 000");
        Self(dir.to_path_buf())
    }
}

impl Drop for ModeGuard {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
    }
}

fn lines(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

/// Records every signal; forwards SIGTERM, and SIGKILL only when `kill_takes`.
struct Recording {
    kill_takes: bool,
    sent: Mutex<Vec<Signal>>,
}

impl Recording {
    fn new(kill_takes: bool) -> Self {
        Self {
            kill_takes,
            sent: Mutex::new(Vec::new()),
        }
    }

    fn sent(&self) -> Vec<Signal> {
        self.sent.lock().expect("sent lock").clone()
    }
}

impl SignalDelivery for Recording {
    fn deliver(&self, target: &PinnedProcess, signal: Signal) -> std::io::Result<()> {
        self.sent.lock().expect("sent lock").push(signal);
        if signal == Signal::Kill && !self.kill_takes {
            return Ok(());
        }
        target.send_signal(signal)
    }
}

// ─── stop ───────────────────────────────────────────────────────────────────

// AC5.
#[tokio::test]
async fn stop_returns_not_running_when_socket_is_absent() {
    let fx = Fixture::new();
    let rec = Recording::new(true);
    let out = stop_with(&fx.descriptor(StoppedSignal::SocketRefuses), &rec).await;
    assert_eq!(out.expect("stop"), StopOutcome::NotRunning);
    assert!(rec.sent().is_empty(), "no signal may be sent");
}

// AC5.
#[tokio::test]
async fn stop_returns_not_running_when_socket_refuses_connections() {
    let fx = Fixture::new();
    drop(UnixListener::bind(&fx.socket).expect("bind"));
    assert!(fx.socket.exists(), "stale socket file must remain");
    // #11098: another test's fork holds a copy of the listener until its exec,
    // so the socket can accept for that window; wait until it refuses.
    assert!(
        wait_for(|| UnixStream::connect(&fx.socket)
            .is_err_and(|e| e.kind() == std::io::ErrorKind::ConnectionRefused)),
        "fixture precondition: the stale socket refuses"
    );
    let rec = Recording::new(true);
    let out = stop_with(&fx.descriptor(StoppedSignal::SocketRefuses), &rec).await;
    assert_eq!(out.expect("stop"), StopOutcome::NotRunning);
    assert!(rec.sent().is_empty(), "no signal may be sent");
}

// Clean stop; a fixed sleep as long as the grace would fail the time bound.
#[tokio::test]
async fn stop_returns_stopped_promptly_after_a_clean_exit() {
    let fx = Fixture::new();
    let mut d = fx.spawn(DAEMON, "serve", &[]);
    let rec = Recording::new(true);
    let t0 = Instant::now();
    let out = stop_with(&fx.descriptor(StoppedSignal::SocketRefuses), &rec).await;
    let elapsed = t0.elapsed();
    let pid = d.pid();
    assert_eq!(
        out.expect("stop"),
        StopOutcome::Stopped { pid, forced: false }
    );
    assert!(
        d.child.try_wait().expect("try_wait").is_some(),
        "pid must have exited"
    );
    assert!(elapsed < Duration::from_secs(10), "stop took {elapsed:?}");
    assert_eq!(rec.sent(), vec![Signal::Term]);
}

// AC1: the socket closes at SIGTERM, the pid lives LINGER_MS longer.
#[tokio::test]
async fn stop_waits_for_pid_exit_not_socket_close() {
    let fx = Fixture::new();
    let marker = fx.path("marker");
    let mut d = fx.spawn(DAEMON, "linger", &[format!("marker={}", marker.display())]);
    let t0 = Instant::now();
    let out = stop(&fx.descriptor(StoppedSignal::SocketRefuses)).await;
    let elapsed = t0.elapsed();
    let pid = d.pid();
    assert_eq!(
        out.expect("stop"),
        StopOutcome::Stopped { pid, forced: false }
    );
    assert!(
        d.child.try_wait().expect("try_wait").is_some(),
        "pid must have exited"
    );
    assert!(
        elapsed >= Duration::from_millis(LINGER_MS),
        "returned after {elapsed:?}, before the pid exited"
    );
    assert!(
        lines(&marker).iter().any(|l| l == "closed"),
        "socket closed first"
    );
}

// AC2.
#[tokio::test]
async fn stop_sends_sigkill_to_a_process_that_ignores_sigterm() {
    let fx = Fixture::new();
    let mut d = fx.spawn(DAEMON, "ignore-term", &[]);
    let mut desc = fx.descriptor(StoppedSignal::SocketRefuses);
    desc.stop_grace = Duration::from_millis(300);
    let rec = Recording::new(true);
    let out = stop_with(&desc, &rec).await;
    let pid = d.pid();
    assert_eq!(
        out.expect("stop"),
        StopOutcome::Stopped { pid, forced: true }
    );
    let status = d
        .child
        .try_wait()
        .expect("try_wait")
        .expect("pid must have exited");
    assert_eq!(status.signal(), Some(libc::SIGKILL));
    assert_eq!(rec.sent(), vec![Signal::Term, Signal::Kill]);
}

// AC3.
#[tokio::test]
async fn stop_reports_a_survivor_when_sigkill_does_not_take() {
    let fx = Fixture::new();
    let d = fx.spawn(DAEMON, "ignore-term", &[]);
    let mut desc = fx.descriptor(StoppedSignal::SocketRefuses);
    desc.stop_grace = Duration::from_millis(200);
    desc.kill_wait = Duration::from_millis(300);
    let rec = Recording::new(false);
    match stop_with(&desc, &rec).await {
        Err(LifecycleError::Survivor { pid }) => assert_eq!(pid, d.pid()),
        other => panic!("expected Survivor, got {other:?}"),
    }
    assert!(is_alive(d.pid()), "the survivor is still running");
    assert_eq!(rec.sent(), vec![Signal::Term, Signal::Kill]);
}

// AC4.
#[tokio::test]
async fn stop_refuses_to_signal_a_process_with_another_name() {
    let fx = Fixture::new();
    let d = fx.spawn(OTHER, "serve", &[]);
    let rec = Recording::new(true);
    match stop_with(&fx.descriptor(StoppedSignal::SocketRefuses), &rec).await {
        Err(LifecycleError::IdentityMismatch {
            pid,
            expected,
            found,
        }) => {
            assert_eq!(
                (pid, expected.as_str(), found.as_str()),
                (d.pid(), DAEMON, OTHER)
            );
        }
        other => panic!("expected IdentityMismatch, got {other:?}"),
    }
    assert!(rec.sent().is_empty(), "no signal may be sent");
    assert!(is_alive(d.pid()), "the other process is untouched");
}

// AC8: the pid exits, but a forked child keeps the flock.
#[tokio::test]
async fn stop_with_lock_signal_is_not_stopped_while_another_process_holds_the_lock() {
    let fx = Fixture::new();
    let lock = fx.path("daemon.lock");
    let marker = fx.path("marker");
    let mut d = fx.spawn(
        DAEMON,
        "fork-lock-holder",
        &[
            format!("lock={}", lock.display()),
            format!("marker={}", marker.display()),
        ],
    );
    let mut desc = fx.descriptor(StoppedSignal::LockFree(lock.clone()));
    desc.kill_wait = Duration::from_secs(1);
    let out = stop(&desc).await;
    let holder: u32 = lines(&marker)
        .iter()
        .find_map(|l| l.strip_prefix("holder=")?.parse().ok())
        .expect("holder pid");
    let holder_alive = is_alive(holder);
    kill_pid(holder);
    match out {
        Err(LifecycleError::StoppedSignalNotReached { pid, .. }) => assert_eq!(pid, d.pid()),
        other => panic!("expected StoppedSignalNotReached, got {other:?}"),
    }
    assert!(
        d.child.try_wait().expect("try_wait").is_some(),
        "the daemon pid exited"
    );
    assert!(holder_alive, "the lock holder was alive during stop");

    let deadline = Instant::now() + SERVE_BOUND;
    while is_alive(holder) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(stop(&desc).await.expect("stop"), StopOutcome::NotRunning);
}

// AC8, positive half.
#[tokio::test]
async fn stop_with_lock_signal_returns_stopped_once_the_lock_is_released() {
    let fx = Fixture::new();
    let lock = fx.path("daemon.lock");
    let d = fx.spawn(DAEMON, "serve", &[format!("lock={}", lock.display())]);
    let desc = fx.descriptor(StoppedSignal::LockFree(lock.clone()));
    let pid = d.pid();
    assert_eq!(
        stop(&desc).await.expect("stop"),
        StopOutcome::Stopped { pid, forced: false }
    );
    let probe = File::open(&lock).expect("lock file");
    assert!(probe.try_lock().is_ok(), "the flock is free after Stopped");
}

// ─── start ──────────────────────────────────────────────────────────────────

// AC6 and AC9: a separate process with a decoy first on PATH calls `start`
// and exits; the daemon runs the install-dir binary and keeps serving.
#[tokio::test]
async fn start_from_another_process_spawns_the_install_dir_binary_and_it_outlives_the_caller() {
    let fx = Fixture::new();
    let decoy_dir = fx.path("decoy");
    fs::create_dir(&decoy_dir).expect("decoy dir");
    let decoy_ran = fx.path("decoy-ran");
    let decoy = decoy_dir.join(DAEMON);
    fs::write(
        &decoy,
        format!("#!/bin/sh\ntouch '{}'\n", decoy_ran.display()),
    )
    .expect("decoy");
    fs::set_permissions(&decoy, fs::Permissions::from_mode(0o755)).expect("chmod decoy");
    let path = format!(
        "{}:{}",
        decoy_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = fx.path("out");
    let marker = fx.path("marker");
    let status = Command::new(std::env::current_exe().expect("current_exe"))
        .args(["--exact", "child_entry", "-q", ROLE_RUNNER])
        .arg(format!("install={}", fx.install.display()))
        .arg(format!("socket={}", fx.socket.display()))
        .arg(format!("marker={}", marker.display()))
        .arg(format!("out={}", out.display()))
        .env("PATH", path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run runner");
    let mut reaps = fx.reap_reported(&marker);
    let report = fs::read_to_string(&out).unwrap_or_default();
    assert!(status.success(), "runner failed: {report}");
    let pid: u32 = report.trim().parse().expect("runner pid");
    assert_eq!(
        lines(&marker),
        vec![pid.to_string()],
        "one daemon, reported"
    );

    assert!(is_alive(pid), "the daemon outlived the runner");
    assert!(UnixStream::connect(&fx.socket).is_ok(), "and still serves");
    let exe = fs::read_link(format!("/proc/{pid}/exe")).expect("exe link");
    assert_eq!(exe, fx.install.join(DAEMON));
    assert!(!decoy_ran.exists(), "the PATH decoy must never run");

    let desc = fx.descriptor(StoppedSignal::SocketRefuses);
    assert_eq!(
        stop(&desc).await.expect("cleanup stop"),
        StopOutcome::Stopped { pid, forced: false }
    );
    reaps.iter_mut().for_each(Reap::disarm);
}

// AC7, and #11084: the unready child is killed and reaped, not left running
// where no `stop` can find it.
#[tokio::test]
async fn start_errors_when_the_socket_never_serves() {
    let fx = Fixture::new();
    let marker = fx.path("marker");
    let mut desc = fx.descriptor(StoppedSignal::SocketRefuses);
    desc.spawn_args = daemon_args("never-bind", &[format!("marker={}", marker.display())]);
    desc.ready_timeout = Duration::from_secs(1);
    let out = start(&desc).await;
    let _reaps = fx.reap_reported(&marker);
    let (pid, err) = match out {
        Err(e @ LifecycleError::NotReady { pid, .. }) => (pid, e),
        other => panic!("expected NotReady, got {other:?}"),
    };
    assert_eq!(lines(&marker), vec![pid.to_string()], "the child ran");
    assert!(!is_alive(pid), "the unready child must be gone: {err}");
    assert!(
        !Path::new(&format!("/proc/{pid}")).exists(),
        "and reaped, not a zombie"
    );
    assert!(err.to_string().contains("killed and reaped"), "{err}");
}

// #11084: with no pidfd for the child (ENOSYS through seccomp, as on a kernel
// before 5.3), `start` still kills and reaps it. A separate runner process
// takes the filter, so the test process keeps its pidfds.
#[test]
fn start_kills_the_child_when_its_pidfd_cannot_be_opened() {
    let fx = Fixture::new();
    let marker = fx.path("marker");
    let out = fx.path("out");
    let status = Command::new(std::env::current_exe().expect("current_exe"))
        .args(["--exact", "child_entry", "-q", ROLE_RUNNER, "no-pidfd"])
        .arg(format!("install={}", fx.install.display()))
        .arg(format!("socket={}", fx.socket.display()))
        .arg(format!("marker={}", marker.display()))
        .arg(format!("out={}", out.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run runner");
    let report = fs::read_to_string(&out).unwrap_or_default();
    let children: Vec<u32> = report
        .lines()
        .find_map(|l| l.strip_prefix("children="))
        .unwrap_or_default()
        .split(',')
        .filter_map(|p| p.parse().ok())
        .collect();
    let _reaps: Vec<Reap> = children
        .iter()
        .map(|&pid| Reap::arm(pid, &fx.install))
        .chain(fx.reap_reported(&marker))
        .collect();
    assert!(
        !status.success(),
        "start must fail without a pidfd: {report}"
    );
    assert!(report.contains("pidfd"), "the pidfd arm was hit: {report}");
    assert!(
        children.is_empty(),
        "start left children {children:?} behind: {report}"
    );
    for pid in lines(&marker).iter().filter_map(|l| l.parse::<u32>().ok()) {
        assert!(!is_alive(pid), "spawned pid {pid} still runs: {report}");
    }
}

#[tokio::test]
async fn start_errors_when_the_child_exits_at_boot() {
    let fx = Fixture::new();
    let mut desc = fx.descriptor(StoppedSignal::SocketRefuses);
    desc.spawn_args = daemon_args("exit-now", &[]);
    desc.ready_timeout = Duration::from_secs(30);
    let t0 = Instant::now();
    let out = start(&desc).await;
    assert!(
        matches!(out, Err(LifecycleError::ChildExited { .. })),
        "expected ChildExited, got {out:?}"
    );
    assert!(
        t0.elapsed() < Duration::from_secs(10),
        "reported as a death, not a timeout"
    );
}

// ─── restart ────────────────────────────────────────────────────────────────

// AC5 + P-b.
#[tokio::test]
async fn restart_starts_a_daemon_that_is_not_running() {
    let fx = Fixture::new();
    let marker = fx.path("marker");
    let mut desc = fx.descriptor(StoppedSignal::SocketRefuses);
    desc.spawn_args = daemon_args("serve", &[format!("marker={}", marker.display())]);
    let out = restart(&desc).await;
    let mut reaps = fx.reap_reported(&marker);
    let out = out.expect("restart");
    assert_eq!(out.stop, StopOutcome::NotRunning);
    let pid = out.start.pid;
    assert_eq!(
        lines(&marker),
        vec![pid.to_string()],
        "one daemon, reported"
    );
    assert!(
        UnixStream::connect(&fx.socket).is_ok(),
        "the new daemon serves"
    );
    let exe = fs::read_link(format!("/proc/{pid}/exe")).expect("exe link");
    assert_eq!(exe, fx.install.join(DAEMON));
    assert_eq!(
        stop(&desc).await.expect("cleanup stop"),
        StopOutcome::Stopped { pid, forced: false }
    );
    reaps.iter_mut().for_each(Reap::disarm);
}

// AC3: a survivor aborts the restart before any spawn.
#[tokio::test]
async fn restart_does_not_start_when_stop_reports_a_survivor() {
    let fx = Fixture::new();
    let marker = fx.path("marker");
    let marker_arg = format!("marker={}", marker.display());
    let d = fx.spawn(DAEMON, "ignore-term", std::slice::from_ref(&marker_arg));
    let mut desc = fx.descriptor(StoppedSignal::SocketRefuses);
    desc.spawn_args = daemon_args("serve", &[marker_arg]);
    desc.stop_grace = Duration::from_millis(200);
    desc.kill_wait = Duration::from_millis(300);
    match restart_with(&desc, &Recording::new(false)).await {
        Err(LifecycleError::Survivor { pid }) => assert_eq!(pid, d.pid()),
        other => panic!("expected Survivor, got {other:?}"),
    }
    // A spawned daemon writes its pid to the marker first thing; give a wrong
    // implementation's child a bounded moment to show up.
    std::thread::sleep(SPAWN_SETTLE);
    let _reaps = fx.reap_reported(&marker);
    assert_eq!(
        lines(&marker),
        vec![d.pid().to_string()],
        "no second process"
    );
    assert!(is_alive(d.pid()));
}

// ─── fail-open arms (#11084) ────────────────────────────────────────────────

// The lock is held but nothing serves the socket: the holder cannot be
// identified, so neither `stop` nor `restart` may treat it as not running.
#[tokio::test]
async fn stop_and_restart_refuse_when_the_lock_is_held_without_a_socket() {
    let fx = Fixture::new();
    let lock = fx.path("daemon.lock");
    let marker = fx.path("marker");
    let marker_arg = format!("marker={}", marker.display());
    let holder = fx.launch(
        DAEMON,
        "never-bind",
        &fx.socket,
        &[format!("lock={}", lock.display()), marker_arg.clone()],
    );
    assert!(
        wait_for(|| lock_held(&lock)),
        "the holder never took the lock"
    );
    let mut desc = fx.descriptor(StoppedSignal::LockFree(lock.clone()));
    desc.spawn_args = daemon_args("serve", &[marker_arg]);
    let rec = Recording::new(true);
    let stopped = stop_with(&desc, &rec).await;
    let restarted = restart_with(&desc, &rec).await;
    std::thread::sleep(SPAWN_SETTLE);
    let _reaps = fx.reap_reported(&marker);

    assert!(
        matches!(&stopped, Err(LifecycleError::Unidentified { lock: l, .. }) if *l == lock),
        "stop: expected Unidentified, got {stopped:?}"
    );
    assert!(
        matches!(restarted, Err(LifecycleError::Unidentified { .. })),
        "restart: expected Unidentified, got {restarted:?}"
    );
    assert!(rec.sent().is_empty(), "no signal may be sent");
    assert_eq!(
        lines(&marker),
        vec![holder.pid().to_string()],
        "restart spawned nothing"
    );
    assert!(is_alive(holder.pid()), "the holder is untouched");
}

// `start` against a served socket must not spawn: the fixture unlinks and
// rebinds the socket, so a second daemon would serve too — the marker counts.
#[tokio::test]
async fn start_refuses_a_socket_that_already_serves_and_spawns_no_second_daemon() {
    let fx = Fixture::new();
    let marker = fx.path("marker");
    let marker_arg = format!("marker={}", marker.display());
    let d = fx.spawn(DAEMON, "serve", std::slice::from_ref(&marker_arg));
    let mut desc = fx.descriptor(StoppedSignal::SocketRefuses);
    desc.spawn_args = daemon_args("serve", &[marker_arg]);
    let out = start(&desc).await;
    std::thread::sleep(SPAWN_SETTLE);
    let _reaps = fx.reap_reported(&marker);

    assert_eq!(
        lines(&marker),
        vec![d.pid().to_string()],
        "exactly one daemon process (start returned {out:?})"
    );
    match out {
        Err(LifecycleError::AlreadyRunning { pid, .. }) => assert_eq!(pid, Some(d.pid())),
        other => panic!("expected AlreadyRunning, got {other:?}"),
    }
    assert!(is_alive(d.pid()), "the running daemon is untouched");
}

// An undumpable listener's `/proc/<pid>/exe` cannot be read: no identity, so
// no signal.
#[tokio::test]
async fn stop_refuses_to_signal_a_process_whose_executable_cannot_be_read() {
    let fx = Fixture::new();
    // #11098: the bridge role keeps this process out of every concurrent
    // test's scan, where its unreadable environment would fail closed.
    let d = fx.launch_as(
        ROLE_BRIDGE,
        DAEMON,
        "serve",
        &fx.socket,
        &["undumpable".to_string()],
    );
    assert!(wait_serving(&fx.socket), "fixture never served");
    assert!(
        fs::read_link(format!("/proc/{}/exe", d.pid())).is_err(),
        "fixture precondition: the exe link is unreadable (not running as root)"
    );
    let rec = Recording::new(true);
    match stop_with(&fx.descriptor(StoppedSignal::SocketRefuses), &rec).await {
        Err(LifecycleError::IdentityUnreadable { pid, .. }) => assert_eq!(pid, d.pid()),
        other => panic!("expected IdentityUnreadable, got {other:?}"),
    }
    assert!(rec.sent().is_empty(), "no signal may be sent");
    assert!(is_alive(d.pid()), "the process stays alive");
}

// A connect that fails with EACCES (socket in a mode-000 directory) proves
// nothing: `stop` is inconclusive and `restart` does not start.
#[tokio::test]
async fn stop_and_restart_stop_short_when_the_socket_cannot_be_probed() {
    let fx = Fixture::new();
    let dir = fx.path("locked");
    fs::create_dir(&dir).expect("socket dir");
    let socket = dir.join("d.sock");
    let marker = fx.path("marker");
    let marker_arg = format!("marker={}", marker.display());
    let d = fx.spawn_at(DAEMON, "serve", &socket, std::slice::from_ref(&marker_arg));
    let mut desc = fx.descriptor_at(StoppedSignal::SocketRefuses, &socket);
    desc.spawn_args = daemon_args("serve", &[marker_arg]);
    let guard = ModeGuard::lock(&dir);
    let probe = UnixStream::connect(&socket).map(drop);
    assert!(
        matches!(&probe, Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied),
        "fixture precondition: connect is refused with EACCES, got {probe:?}"
    );
    let rec = Recording::new(true);
    let stopped = stop_with(&desc, &rec).await;
    let restarted = restart_with(&desc, &rec).await;
    std::thread::sleep(SPAWN_SETTLE);
    drop(guard);
    let _reaps = fx.reap_reported(&marker);

    for (call, out) in [
        ("stop", stopped.map(|o| format!("{o:?}"))),
        ("restart", restarted.map(|o| format!("{o:?}"))),
    ] {
        assert!(
            matches!(&out, Err(LifecycleError::ProbeInconclusive { socket: s }) if *s == socket),
            "{call}: expected ProbeInconclusive, got {out:?}"
        );
    }
    assert!(rec.sent().is_empty(), "no signal may be sent");
    assert_eq!(
        lines(&marker),
        vec![d.pid().to_string()],
        "restart spawned nothing"
    );
    assert!(is_alive(d.pid()), "the daemon is untouched");
}

// ─── #11098: liveness without a socket, and the spawn's cwd/env ─────────────

/// The pid lines of a marker; other lines (`unserved`, `closed`) are skipped.
fn pid_lines(marker: &Path) -> Vec<String> {
    lines(marker)
        .into_iter()
        .filter(|l| l.parse::<u32>().is_ok())
        .collect()
}

// #11098: a daemon that unlinked its socket file, or closed its listener,
// still runs. The shortcut "socket absent ⇒ not running" reports `NotRunning`
// and lets `restart` spawn a second daemon; here every call refuses, names
// the pid, and signals and spawns nothing.
#[tokio::test]
async fn stop_start_and_restart_refuse_while_a_daemon_runs_without_its_socket() {
    for mode in ["unlink-socket", "close-socket"] {
        let fx = Fixture::new();
        let marker = fx.path("marker");
        let marker_arg = format!("marker={}", marker.display());
        let d = fx.launch(DAEMON, mode, &fx.socket, std::slice::from_ref(&marker_arg));
        assert!(
            wait_for(|| lines(&marker).iter().any(|l| l == "unserved")),
            "{mode}: the fixture never dropped its socket"
        );
        assert!(
            UnixStream::connect(&fx.socket).is_err(),
            "{mode}: fixture precondition: the socket does not serve"
        );
        let mut desc = fx.descriptor(StoppedSignal::SocketRefuses);
        desc.spawn_args = daemon_args("serve", &[marker_arg]);
        let rec = Recording::new(true);
        let stopped = stop_with(&desc, &rec).await;
        let restarted = restart_with(&desc, &rec).await;
        let started = start(&desc).await;
        std::thread::sleep(SPAWN_SETTLE);
        let _reaps = fx.reap_reported(&marker);

        assert_eq!(
            pid_lines(&marker),
            vec![d.pid().to_string()],
            "{mode}: a second daemon was spawned (stop: {stopped:?}, restart: {restarted:?})"
        );
        for (call, out) in [
            ("stop", stopped.map(|o| format!("{o:?}"))),
            ("restart", restarted.map(|o| format!("{o:?}"))),
            ("start", started.map(|o| format!("{o:?}"))),
        ] {
            assert!(
                matches!(&out, Err(LifecycleError::RunningUnserved { pids, .. }) if *pids == [d.pid()]),
                "{mode}: {call}: expected RunningUnserved naming pid {}, got {out:?}",
                d.pid()
            );
        }
        assert!(rec.sent().is_empty(), "{mode}: no signal may be sent");
        assert!(is_alive(d.pid()), "{mode}: the daemon is untouched");
    }
}

// #11098: a stdio bridge runs the daemon's executable but not its arguments.
// The shortcut "match on the executable alone" reads it as the daemon.
#[tokio::test]
async fn stop_returns_not_running_while_only_a_bridge_of_the_same_binary_runs() {
    let fx = Fixture::new();
    let marker = fx.path("marker");
    let bridge = fx.launch_as(
        ROLE_BRIDGE,
        DAEMON,
        "never-bind",
        &fx.socket,
        &[format!("marker={}", marker.display())],
    );
    assert!(
        wait_for(|| !pid_lines(&marker).is_empty()),
        "the bridge never started"
    );
    let rec = Recording::new(true);
    let out = stop_with(&fx.descriptor(StoppedSignal::SocketRefuses), &rec).await;
    assert_eq!(
        out.expect("a bridge alone is not the daemon"),
        StopOutcome::NotRunning
    );
    assert!(rec.sent().is_empty(), "no signal may be sent");
    assert!(is_alive(bridge.pid()), "the bridge is untouched");
}

// #11098: `start` called from a checkout with a `.env.local`, and with a
// sentinel exported, must not run the daemon in that checkout or hand it the
// sentinel. A separate runner calls `start`, so this process keeps its cwd.
#[test]
fn start_runs_the_daemon_in_its_working_dir_with_only_the_allowed_environment() {
    let fx = Fixture::new();
    let checkout = fx.path("checkout");
    fs::create_dir(&checkout).expect("checkout dir");
    fs::write(checkout.join(".env.local"), "TC_LC_FROM_DOTENV=1\n").expect(".env.local");
    let checkout = fs::canonicalize(&checkout).expect("canonical checkout");
    let out = fx.path("out");
    let marker = fx.path("marker");
    let status = Command::new(std::env::current_exe().expect("current_exe"))
        .args([
            "--exact",
            "child_entry",
            "-q",
            ROLE_RUNNER,
            "allow=TC_LC_ALLOWED",
        ])
        .arg(format!("install={}", fx.install.display()))
        .arg(format!("socket={}", fx.socket.display()))
        .arg(format!("marker={}", marker.display()))
        .arg(format!("out={}", out.display()))
        .current_dir(&checkout)
        .env("TC_LC_SENTINEL", "leaked")
        .env("TC_LC_ALLOWED", "kept")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run runner");
    let _reaps = fx.reap_reported(&marker);
    let report = fs::read_to_string(&out).unwrap_or_default();
    assert!(status.success(), "runner failed: {report}");
    let pid: u32 = report.trim().parse().expect("runner pid");
    let cwd = fs::read_link(format!("/proc/{pid}/cwd")).expect("daemon cwd");
    let environ = fs::read(format!("/proc/{pid}/environ")).expect("daemon environ");
    let vars: Vec<String> = environ
        .split(|b| *b == 0)
        .map(|v| String::from_utf8_lossy(v).into_owned())
        .collect();
    // Names only in a failure message: a value can be a credential.
    let names: Vec<&str> = vars.iter().filter_map(|v| v.split('=').next()).collect();

    assert_ne!(cwd, checkout, "the daemon runs in the caller's cwd");
    assert_eq!(cwd, Path::new("/"), "the default working_dir is /");
    assert!(
        !names.contains(&"TC_LC_SENTINEL"),
        "the caller's sentinel reached the daemon; its names: {names:?}"
    );
    assert!(
        vars.iter().any(|v| v == "TC_LC_ALLOWED=kept"),
        "the allowlisted variable is missing; names: {names:?}"
    );
    if let Some(home) = std::env::var_os("HOME") {
        let want = format!("HOME={}", home.to_string_lossy());
        assert!(
            vars.contains(&want),
            "the base HOME is missing; names: {names:?}"
        );
    }
}

// ─── #11098: sibling instances of the same binary and argv ──────────────────

fn cmdline(pid: u32) -> Vec<u8> {
    fs::read(format!("/proc/{pid}/cmdline")).expect("cmdline")
}

/// Two instances, A and B, of one binary with identical argv; only their
/// `SOCKET_ENV` values (the stand-in for a data-dir variable) differ.
struct Siblings {
    fx: Fixture,
    marker: PathBuf,
    extra: Vec<String>,
    sock_a: PathBuf,
    sock_b: PathBuf,
}

impl Siblings {
    fn new() -> Self {
        let fx = Fixture::new();
        let marker = fx.path("marker");
        let extra = vec![format!("marker={}", marker.display())];
        let (sock_a, sock_b) = (fx.instance_socket("a"), fx.instance_socket("b"));
        Self {
            fx,
            marker,
            extra,
            sock_a,
            sock_b,
        }
    }

    fn run(&self, socket: &Path) -> Running {
        self.fx.spawn_at(DAEMON, "serve", socket, &self.extra)
    }

    /// The descriptor for instance A, spawning A with B's argv.
    fn desc_a(&self) -> DaemonDescriptor {
        let mut desc = self
            .fx
            .descriptor_at(StoppedSignal::SocketRefuses, &self.sock_a);
        desc.spawn_args = daemon_args("serve", &self.extra);
        desc
    }

    fn assert_b_untouched(&self, b: &Running) {
        assert!(is_alive(b.pid()), "sibling B was stopped");
        assert!(UnixStream::connect(&self.sock_b).is_ok(), "B still serves");
    }
}

// #11098: a sandbox daemon runs the production argv; only its data-dir
// variable differs. The shortcut "binary + argv is this daemon" stops A, then
// reads B as A still running, so `start` refuses and A stays down.
#[tokio::test]
async fn restart_of_one_instance_leaves_a_sibling_with_the_same_argv_serving() {
    let s = Siblings::new();
    let a = s.run(&s.sock_a);
    let b = s.run(&s.sock_b);
    assert_eq!(
        cmdline(a.pid()),
        cmdline(b.pid()),
        "precondition: same argv"
    );
    let desc = s.desc_a();
    let out = restart(&desc).await;
    let _reaps = s.fx.reap_reported(&s.marker);
    let out = out.expect("restart of A beside sibling B");
    assert_eq!(
        out.stop,
        StopOutcome::Stopped {
            pid: a.pid(),
            forced: false
        }
    );
    let new = out.start.pid;
    assert!(UnixStream::connect(&s.sock_a).is_ok(), "A serves again");
    assert_eq!(cmdline(new), cmdline(b.pid()), "the new A has B's argv");
    s.assert_b_untouched(&b);
    assert_eq!(
        stop(&desc).await.expect("cleanup stop"),
        StopOutcome::Stopped {
            pid: new,
            forced: false
        }
    );
    s.assert_b_untouched(&b);
}

// #11098: owner ruling — a daemon that is not running is started, even while
// a sibling instance of it runs.
#[tokio::test]
async fn start_and_restart_start_a_stopped_instance_while_a_sibling_runs() {
    let s = Siblings::new();
    let b = s.run(&s.sock_b);
    let desc = s.desc_a();
    let started = start(&desc).await;
    let mut reaps = s.fx.reap_reported(&s.marker);
    let pid = started.expect("start of A beside sibling B").pid;
    assert!(UnixStream::connect(&s.sock_a).is_ok(), "A serves");
    assert_eq!(cmdline(pid), cmdline(b.pid()), "A has B's argv");
    assert_eq!(
        stop(&desc).await.expect("stop A"),
        StopOutcome::Stopped { pid, forced: false }
    );
    s.assert_b_untouched(&b);

    let restarted = restart(&desc).await;
    reaps.extend(s.fx.reap_reported(&s.marker));
    let restarted = restarted.expect("restart of a stopped A beside B");
    assert_eq!(restarted.stop, StopOutcome::NotRunning);
    assert!(UnixStream::connect(&s.sock_a).is_ok(), "A serves again");
    assert_eq!(
        stop(&desc).await.expect("cleanup stop"),
        StopOutcome::Stopped {
            pid: restarted.start.pid,
            forced: false
        }
    );
    s.assert_b_untouched(&b);
}

// #11098: a process that matches the binary and argv but hides its
// environment (undumpable) cannot be told from this instance, so it fails
// closed. It runs as `OTHER`, which no other test's descriptor names.
#[tokio::test]
async fn stop_and_start_fail_closed_when_a_matching_process_hides_its_environment() {
    let fx = Fixture::new();
    let marker = fx.path("marker");
    let marker_arg = format!("marker={}", marker.display());
    let hidden = fx.launch(
        OTHER,
        "never-bind",
        &fx.socket,
        &[marker_arg.clone(), "undumpable".to_string()],
    );
    assert!(
        wait_for(|| !pid_lines(&marker).is_empty()),
        "the fixture never started"
    );
    let mut desc = fx.descriptor(StoppedSignal::SocketRefuses);
    desc.binary = OTHER.to_string();
    desc.spawn_args = daemon_args("serve", &[marker_arg]);
    let rec = Recording::new(true);
    let stopped = stop_with(&desc, &rec).await;
    let started = start(&desc).await;
    std::thread::sleep(SPAWN_SETTLE);
    let _reaps = fx.reap_reported(&marker);

    for (call, out) in [
        ("stop", stopped.map(|o| format!("{o:?}"))),
        ("start", started.map(|o| format!("{o:?}"))),
    ] {
        assert!(
            matches!(&out, Err(LifecycleError::InstanceUnreadable { pid, .. }) if *pid == hidden.pid()),
            "{call}: expected InstanceUnreadable naming pid {}, got {out:?}",
            hidden.pid()
        );
    }
    assert_eq!(
        pid_lines(&marker),
        vec![hidden.pid().to_string()],
        "start spawned a second process"
    );
    assert!(rec.sent().is_empty(), "no signal may be sent");
    assert!(is_alive(hidden.pid()), "the process is untouched");
}

// #11098: a `/proc` read that fails for a reason other than a vanished
// process (here EMFILE) once read every process as absent, so `stop` said
// `NotRunning` and `restart` started a second daemon.
#[test]
fn stop_fails_closed_when_the_scan_cannot_open_proc_files() {
    let fx = Fixture::new();
    let out = fx.path("out");
    let status = Command::new(std::env::current_exe().expect("current_exe"))
        .args(["--exact", "child_entry", "-q", ROLE_RUNNER, "emfile"])
        .arg(format!("install={}", fx.install.display()))
        .arg(format!("socket={}", fx.socket.display()))
        .arg(format!("marker={}", fx.path("marker").display()))
        .arg(format!("out={}", out.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run runner");
    let report = fs::read_to_string(&out).unwrap_or_default();
    assert!(status.success(), "runner failed: {report}");
    assert!(
        report.starts_with("Err(Io") && report.contains("/status"),
        "expected an Io error on a status read, got {report}"
    );
}
