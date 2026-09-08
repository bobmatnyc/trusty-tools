//! Parent-death linkage: a spawned process that exits when its spawner dies.
//!
//! Why (#7085, #3734): a `Child` handle only reaps in `Drop`, and `Drop` runs
//! only while the parent is still executing — a panic unwind, a clean return.
//! SIGKILL the parent and no destructor runs at all, so the child is reparented
//! to pid 1 and keeps running forever. That is not hypothetical: a census on
//! 2026-09-08 found 102 orphaned `trusty-memory serve --foreground` daemons from
//! `cargo test` runs holding 12.6 GB, each against a temp data dir its spawner
//! had long since deleted. A killed parent cannot clean up after itself, so the
//! only mechanism that closes the whole class has to live INSIDE the child.
//!
//! What: [`exit_with_parent`] stamps [`ENV_EXIT_WITH_PARENT`] onto a `Command`
//! with the spawner's own IDENTITY — its pid and its process start time; the
//! spawned program calls [`arm_from_env`] early in its startup and self-exits
//! once that parent is gone. [`arm_for_named_parent`] is the variant for a
//! parent named out of band — a `--parent-pid` flag rather than the
//! environment — where the named pid need not be the direct spawner. Both
//! arming entry points are unix-only and compile to no-ops elsewhere; stamping
//! the environment is portable and harmless.
//!
//! Mechanism: a polled `getppid()` comparison, a `kill(pid, 0)` liveness probe,
//! and the parent's process START TIME, on a dedicated OS thread.
//! `prctl(PR_SET_PDEATHSIG)` is Linux-only and this workspace's daemons run on
//! macOS first. A pipe held open by the parent would report EOF, but only while
//! no other process holds the write end — every grandchild inherits it, so a
//! daemon that spawns anything at all defeats the signal. `getppid()` is
//! reparent-detection, which no descendant can hold open, and it behaves
//! identically on macOS and Linux.
//!
//! Why the start time (#7085 review): a pid alone is not an identity. A daemon
//! watching a grandparent has no reparent signal to fall back on, so a pid the
//! OS recycles onto some unrelated process would read as "parent still alive"
//! forever — the exact permanent orphan this module exists to prevent.
//! [`ParentIdentity`] pairs the pid with a process start time, and a changed
//! start time is death. macOS reads it with
//! `proc_pidinfo(PROC_PIDTASKALLINFO)`, Linux from `/proc/<pid>/stat` field 22;
//! a platform where neither works degrades to the liveness probe alone rather
//! than to a false "gone".
//!
//! Why the SPAWNER reads that start time (#7085 round-2 review): reading it in
//! the child, at arm time, is a TOCTOU. Between `Command::spawn()` and the
//! child reaching `arm_from_env` the spawner can die and the OS can hand its
//! pid to something unrelated; the child would then record the IMPOSTOR's start
//! time and watch a process nobody asked it to watch — firing only when that
//! process happens to exit, which may be never. A process cannot be recycled
//! while it is running, so the spawner reading its OWN start time has no such
//! window. The stamp therefore carries the pair, `<pid>:<start>`, and the child
//! only ever parses it. A stamped start time that does not match the live pid's
//! means the spawner is already gone and the child exits at arm.
//!
//! Why the watchdog waits for a SIGTERM handler (#7085 round-2 review): the
//! graceful path is `raise(SIGTERM)` into the daemon's own handler, and
//! `arm_from_env` runs before that handler is installed. Raising into the
//! default disposition would kill the process outright, skipping the socket
//! unlink and the index flush that make the exit graceful — most visibly when
//! the parent is already dead at arm and the watchdog fires on its first tick.
//! [`wait_for_term_handler`] holds the raise until SIGTERM is no longer on
//! `SIG_DFL`, bounded by [`HANDLER_WAIT`].
//!
//! Opt-in by construction: nothing arms unless [`ENV_EXIT_WITH_PARENT`] is set,
//! and arming always announces itself on stderr, so a daemon that self-exits is
//! never doing so silently.
//!
//! Test: `the_stamp_round_trips_this_process_identity`,
//! `a_stamped_start_time_that_does_not_match_the_live_pid_reads_as_gone`,
//! `a_stamp_without_a_start_time_degrades_to_liveness`,
//! `an_unwatchable_stamp_arms_nothing`, `parent_is_gone_on_reparent`,
//! `parent_is_gone_when_named_parent_exits`, `grandchild_ignores_its_own_reparent`,
//! `recycled_pid_reads_as_gone`, `an_unreadable_start_time_favours_alive`,
//! `start_time_is_readable_for_this_process`,
//! `an_installed_sigterm_handler_is_distinguishable_from_the_default`,
//! `watch_parent_returns_once_parent_dies`.

use std::process::Command;
use std::time::Duration;

/// Names the pid a spawned process must not outlive (`TRUSTY_EXIT_WITH_PARENT`).
///
/// Why: the value has to cross an `exec`, and the environment is the one channel
/// that reaches a program whose argument vector the spawner does not own.
/// What: the exact env-var name. The value is `<pid>:<start>` — the spawner's
/// pid in decimal and the spawner's own process start time, which is what makes
/// a recycled pid distinguishable from the real parent. A bare `<pid>` is
/// accepted and degrades to a liveness-only watch; that is what a spawner on a
/// platform with no start-time source writes. Set it through
/// [`exit_with_parent`] rather than by hand, so the name and both halves of the
/// value are written in one place.
/// Test: `the_stamp_round_trips_this_process_identity`.
pub const ENV_EXIT_WITH_PARENT: &str = "TRUSTY_EXIT_WITH_PARENT";

/// Separates the pid from the start time inside [`ENV_EXIT_WITH_PARENT`].
///
/// Why a colon: neither half can contain one, so the split is unambiguous
/// without escaping, and the value stays readable in a `ps -E` dump.
const STAMP_SEPARATOR: char = ':';

/// How often the watchdog re-checks its parent.
///
/// Why: 250 ms bounds an orphan's lifetime to about a second while costing two
/// syscalls per tick, which is free next to a daemon's idle work.
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// How long a graceful shutdown gets before the watchdog exits the hard way.
///
/// Why: the point of routing through SIGTERM is that the daemon's own handler
/// flushes its indexes and unlinks its socket. A daemon that is wedged must
/// still not survive its parent, so the grace window is bounded rather than
/// open-ended. 5 s clears trusty-memory's BM25 exit flush on a temp data dir
/// while keeping the worst-case orphan lifetime in single-digit seconds.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// How long the watchdog waits for the process to install a SIGTERM handler
/// before giving up on a graceful exit.
///
/// Why: `arm_from_env` is called at the top of a daemon's serve path, and the
/// handler goes in much later — after the config loads, the store opens, and
/// the socket binds. A parent that is already dead at arm makes the watchdog
/// fire on its first tick, well inside that window. 10 s covers a cold
/// trusty-memory boot on a loaded box; past it, waiting longer only extends the
/// orphan's life, so the watchdog exits by itself instead.
const HANDLER_WAIT: Duration = Duration::from_secs(10);

/// Exit status a self-exiting watchdog reports when it had to force the exit.
///
/// Why: an operator reading a crash log has to be able to tell "my parent died
/// and I refused to become an orphan" from a panic or an OOM kill. A distinct,
/// documented code plus the stderr line above it says which happened.
/// What: 0 for the graceful path (launchd's `KeepAlive { SuccessfulExit: false }`
/// must not respawn a daemon that exited on purpose); this code only when the
/// grace window expired with the process still running.
pub const EXIT_CODE_PARENT_DIED_UNGRACEFUL: i32 = 87;

/// Stamp `cmd` so the process it spawns exits when THIS process dies.
///
/// Why: the spawner is the only one that knows its own identity, and it must
/// record it before the fork — after a SIGKILL there is nobody left to tell the
/// child anything. Reading its own start time here rather than letting the
/// child read one from the pid is what closes the recycled-pid window described
/// in the module doc. Callers pair this with [`arm_from_env`] in the spawned
/// program.
/// What: sets [`ENV_EXIT_WITH_PARENT`] to [`own_stamp`]. Portable: the stamp is
/// inert in a program that never arms, and on a non-unix target.
/// Test: `the_stamp_round_trips_this_process_identity`.
pub fn exit_with_parent(cmd: &mut Command) -> &mut Command {
    cmd.env(ENV_EXIT_WITH_PARENT, own_stamp())
}

/// This process's own stamp value: `<pid>:<start>`, or `<pid>` alone where the
/// platform reports no start time.
///
/// Why it is safe to read here and not in the child: this process holds the pid
/// it is asking about, so nothing can recycle it out from under the read.
/// What: pid and start time joined by [`STAMP_SEPARATOR`].
/// Test: `the_stamp_round_trips_this_process_identity`.
#[cfg(unix)]
fn own_stamp() -> String {
    let me = identify(std::process::id());
    match me.start {
        Some(start) => format!("{}{STAMP_SEPARATOR}{start}", me.pid),
        None => me.pid.to_string(),
    }
}

/// Non-unix stub: no start-time source, so the stamp is the pid alone.
#[cfg(not(unix))]
fn own_stamp() -> String {
    std::process::id().to_string()
}

/// Tokio counterpart of [`exit_with_parent`], for an async spawn site.
///
/// Why: `tokio::process::Command` is a distinct type from `std`'s, and a test
/// that spawns a bridge asynchronously owes its child the same linkage. Both
/// entry points write the same name and the same value, so a caller never has to
/// know which one the other used.
/// What: sets [`ENV_EXIT_WITH_PARENT`] to [`own_stamp`].
/// Test: `the_stamp_round_trips_this_process_identity` pins the value both share.
pub fn exit_with_parent_tokio(cmd: &mut tokio::process::Command) -> &mut tokio::process::Command {
    cmd.env(ENV_EXIT_WITH_PARENT, own_stamp())
}

/// This process's own parent-death stamp, if it carries one.
///
/// Why (#7085 review): a detached spawn scrubs the stamp by default, so a caller
/// that deliberately wants its grandchild linked has to hand the value over
/// explicitly. Reading it through this function keeps the env-var name in one
/// place and makes every forwarding site greppable.
/// What: the raw value of [`ENV_EXIT_WITH_PARENT`] in this process, or `None`.
/// Test: `daemon_guard::tests::detached_command_scrubs_the_parent_stamp`.
pub fn inherited_stamp() -> Option<String> {
    std::env::var(ENV_EXIT_WITH_PARENT).ok()
}

/// The pid a watchdog is watching, plus the identity of the process that held
/// that pid when the watchdog armed.
///
/// Why: see the module doc — a pid the OS recycles would otherwise read as the
/// parent still being alive, forever.
/// What: `start` is the process start time in whole seconds, or `None` where
/// this platform cannot report it. Two `ParentIdentity` values with the same
/// pid and different `start` are different processes.
/// Test: `recycled_pid_reads_as_gone`, `start_time_is_readable_for_this_process`.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ParentIdentity {
    pid: u32,
    start: Option<u64>,
}

/// Arm the watchdog from [`ENV_EXIT_WITH_PARENT`], if the spawner set it.
///
/// Why (#7085): this is the half that runs inside the child, and it is what a
/// SIGKILL of the parent cannot skip. Call it once, early, on the long-running
/// path of a program a test may spawn.
/// What: reads the env var. Absent → returns `None` and arms nothing, which is
/// every production invocation. Present and naming a pid above 1 → takes the
/// identity STRAIGHT FROM THE STAMP, spawns the watchdog thread, announces the
/// arming on stderr, and returns the pid. Unparseable or `<= 1` → warns on
/// stderr and arms nothing; a typo must not silently disable the linkage.
///
/// The start time is never re-derived from the stamped pid here. Deriving it
/// would read whatever process holds that pid NOW, which after a recycle is an
/// impostor the watchdog would then wait on forever — see the module doc. A
/// stamped start time that disagrees with the live pid's therefore means the
/// spawner is already gone, and the watchdog's first tick takes the graceful
/// exit path immediately.
///
/// The reparent prong is armed only when the stamped pid IS this process's
/// direct parent. A daemon started detached by an intermediate CLI is a
/// grandchild: that CLI exits by design, so its own ppid moving says nothing
/// about whether the stamped process is still alive, and watching for it would
/// kill the daemon seconds after it started. Such a daemon watches the stamped
/// process's identity — pid plus start time — instead.
/// Test: `a_stamped_start_time_that_does_not_match_the_live_pid_reads_as_gone`,
/// `an_unwatchable_stamp_arms_nothing`;
/// `crates/trusty-memory/tests/orphan_reap_7085.rs` proves both the direct-child
/// and the detached-grandchild loops against a real SIGKILLed spawner.
#[cfg(unix)]
pub fn arm_from_env(tag: &str) -> Option<u32> {
    let raw = std::env::var(ENV_EXIT_WITH_PARENT).ok()?;
    let parent = parent_from_stamp(&raw, tag)?;
    // SAFETY: `getppid` takes no arguments and cannot fail.
    let ppid = unsafe { libc::getppid() } as u32;
    arm(parent, (ppid == parent.pid).then_some(ppid), tag);
    Some(parent.pid)
}

/// Parse an [`ENV_EXIT_WITH_PARENT`] value into the identity to watch.
///
/// Why it is a separate function: this is the half of #7085's round-2 finding
/// that can be proven without spawning a daemon or signalling the test runner.
/// What: accepts `<pid>` or `<pid>:<start>`. A pid of 0 or 1, or one that does
/// not parse, warns and yields `None` — nothing arms. A start time that does not
/// parse warns and degrades the identity to liveness-only rather than reporting
/// a false "gone", which would kill a healthy daemon on a malformed stamp.
/// Test: `the_stamp_round_trips_this_process_identity`,
/// `a_stamped_start_time_that_does_not_match_the_live_pid_reads_as_gone`,
/// `a_stamp_without_a_start_time_degrades_to_liveness`,
/// `an_unwatchable_stamp_arms_nothing`.
#[cfg(unix)]
fn parent_from_stamp(raw: &str, tag: &str) -> Option<ParentIdentity> {
    let trimmed = raw.trim();
    let (pid_text, start_text) = match trimmed.split_once(STAMP_SEPARATOR) {
        Some((pid, start)) => (pid, Some(start)),
        None => (trimmed, None),
    };
    let pid = match pid_text.parse::<u32>() {
        Ok(pid) if pid > 1 => pid,
        _ => {
            eprintln!(
                "[{tag}] {ENV_EXIT_WITH_PARENT}={raw:?} is not a watchable pid; \
                 parent-death watchdog not armed"
            );
            return None;
        }
    };
    let start = match start_text {
        None => None,
        Some(text) => match text.parse::<u64>() {
            Ok(start) => Some(start),
            Err(_) => {
                eprintln!(
                    "[{tag}] {ENV_EXIT_WITH_PARENT}={raw:?} carries no readable start \
                     time; watching pid {pid} by liveness alone"
                );
                None
            }
        },
    };
    Some(ParentIdentity { pid, start })
}

/// Non-unix stub: the watchdog needs `getppid`/`kill`, so arming is a no-op.
#[cfg(not(unix))]
pub fn arm_from_env(_tag: &str) -> Option<u32> {
    None
}

/// Arm the watchdog for a parent named out of band rather than by environment.
///
/// Why (#3734): the desktop GUI passes its pid to the `tagent --api` sidecar as
/// `--parent-pid`, and that pid is not required to be the sidecar's direct
/// spawner. Anchoring the reparent check to `getppid()` AT ARM TIME rather than
/// to `parent_pid` keeps the check correct in that case, while the identity
/// probe on `parent_pid` covers the parent this process was told about.
///
/// Where the start time comes from, and why it differs from [`arm_from_env`]:
/// a `--parent-pid` flag carries a pid and nothing else, so there is no stamped
/// start time to parse and [`identify`] reads one from the live pid here. That
/// leaves the narrow recycle window `arm_from_env` closes — but it is covered
/// from the other side: this entry point always arms the reparent prong, and
/// `getppid()` moving is immune to pid reuse. A GUI that wants the stronger
/// guarantee stamps [`ENV_EXIT_WITH_PARENT`] with [`exit_with_parent`] instead
/// of passing a flag.
/// What: reads `getppid()` as the expected ppid, then arms as [`arm_from_env`]
/// does. A `parent_pid` of 0 or 1 is not watchable and arms nothing.
/// Test: `parent_is_gone_when_named_parent_exits` covers the identity prong this
/// entry point depends on.
#[cfg(unix)]
pub fn arm_for_named_parent(parent_pid: u32, tag: &str) {
    if parent_pid <= 1 {
        eprintln!(
            "[{tag}] parent-death watchdog not armed: parent pid {parent_pid} is not watchable"
        );
        return;
    }
    // SAFETY: `getppid` takes no arguments and cannot fail.
    let armed_ppid = unsafe { libc::getppid() } as u32;
    arm(identify(parent_pid), Some(armed_ppid), tag);
}

/// Non-unix stub: see [`arm_from_env`].
#[cfg(not(unix))]
pub fn arm_for_named_parent(_parent_pid: u32, _tag: &str) {}

/// Pair a pid with its current start time.
#[cfg(unix)]
fn identify(pid: u32) -> ParentIdentity {
    ParentIdentity {
        pid,
        start: process_start_secs(pid),
    }
}

/// Spawn the watchdog thread that ends this process once the parent goes.
///
/// Why a dedicated OS thread and not an async task: this must work in a program
/// with no runtime, and it must keep ticking while the runtime is saturated or
/// mid-shutdown — the whole point is that it fires when other machinery cannot.
///
/// Why SIGTERM rather than a bare `process::exit` (#7085 review): every trusty-*
/// daemon already handles SIGTERM through `shutdown::shutdown_signal`, which is
/// what flushes BM25 and unlinks the socket. Raising it on ourselves reuses that
/// tested path instead of a second, weaker one. A daemon that ignores SIGTERM,
/// or is wedged, must still not outlive its parent, so the graceful window is
/// bounded by [`SHUTDOWN_GRACE`] and then forced.
///
/// Why it waits before raising (#7085 round-2 review): arming happens before the
/// daemon installs its SIGTERM handler, so a parent that is already dead at arm
/// would otherwise have the first tick raise SIGTERM into the DEFAULT
/// disposition — which kills the process outright and skips the socket unlink
/// and index flush that make the exit graceful. [`wait_for_term_handler`] holds
/// the raise until a handler exists.
/// What: blocks in [`watch_parent`], announces the reason on stderr, waits up to
/// [`HANDLER_WAIT`] for a SIGTERM handler, raises SIGTERM on this process, and
/// waits up to [`SHUTDOWN_GRACE`]. If the process is still running when that
/// expires — or no handler ever appeared, so the raise was never worth making —
/// it exits [`EXIT_CODE_PARENT_DIED_UNGRACEFUL`], which distinguishes this from
/// a crash.
#[cfg(unix)]
fn arm(parent: ParentIdentity, armed_ppid: Option<u32>, tag: &str) {
    let owned_tag = tag.to_string();
    let watching = if armed_ppid.is_some() {
        "as a direct child"
    } else {
        "as a detached grandchild (liveness and start time only)"
    };
    eprintln!(
        "[{tag}] parent-death watchdog armed for pid {} {watching}",
        parent.pid
    );
    let spawned = std::thread::Builder::new()
        .name("parent-death-watchdog".to_string())
        .spawn(move || {
            watch_parent(parent, armed_ppid, POLL_INTERVAL);
            eprintln!(
                "[{owned_tag}] parent pid {} is gone; shutting down rather than \
                 becoming an orphan",
                parent.pid
            );
            if wait_for_term_handler(HANDLER_WAIT) {
                // SAFETY: `raise` delivers to this process and takes a signal number.
                unsafe { libc::raise(libc::SIGTERM) };
                std::thread::sleep(SHUTDOWN_GRACE);
                eprintln!(
                    "[{owned_tag}] graceful shutdown did not complete within \
                     {SHUTDOWN_GRACE:?}; forcing exit {EXIT_CODE_PARENT_DIED_UNGRACEFUL}"
                );
            } else {
                eprintln!(
                    "[{owned_tag}] no SIGTERM handler was installed within \
                     {HANDLER_WAIT:?}, so raising it would kill this process on the \
                     default disposition; exiting {EXIT_CODE_PARENT_DIED_UNGRACEFUL} \
                     instead"
                );
            }
            std::process::exit(EXIT_CODE_PARENT_DIED_UNGRACEFUL);
        });
    if let Err(e) = spawned {
        eprintln!("[{tag}] could not start the parent-death watchdog thread: {e}");
    }
}

/// Block until the parent is gone, polling every `interval`.
///
/// What: pure control flow with no process-global effect, so the detection logic
/// is testable without signalling or exiting the test runner.
/// Test: `watch_parent_returns_once_parent_dies`.
#[cfg(unix)]
fn watch_parent(parent: ParentIdentity, armed_ppid: Option<u32>, interval: Duration) {
    while !parent_is_gone(parent, armed_ppid) {
        std::thread::sleep(interval);
    }
}

/// Block until this process has a SIGTERM handler, or `budget` expires.
///
/// What: polls [`term_handler_installed`] every [`POLL_INTERVAL`]. Returns
/// whether a handler appeared — `false` means the caller must not raise SIGTERM,
/// because the default disposition would kill the process outright instead of
/// running its shutdown path.
/// Test: `an_installed_sigterm_handler_is_distinguishable_from_the_default`.
#[cfg(unix)]
fn wait_for_term_handler(budget: Duration) -> bool {
    let deadline = std::time::Instant::now() + budget;
    while !term_handler_installed() {
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    true
}

/// Has anything replaced SIGTERM's default disposition?
///
/// Why this and not a flag the daemon sets: the disposition is the thing that
/// actually decides whether a raise is graceful, and asking the kernel needs no
/// cooperation from — and no process-global state shared with — the daemon.
/// tokio's signal driver installs a real handler function, so a daemon awaiting
/// [`crate::shutdown_signal`] answers `true` once that future is first polled.
/// What: queries `sigaction` with a null new-action, which only reads. `true`
/// when the current handler is anything other than `SIG_DFL`; a failed query
/// reads as `false`, the conservative answer.
/// Test: `an_installed_sigterm_handler_is_distinguishable_from_the_default`.
#[cfg(unix)]
fn term_handler_installed() -> bool {
    let mut current = std::mem::MaybeUninit::<libc::sigaction>::zeroed();
    // SAFETY: a null `act` makes this a query; `current` is a correctly sized,
    // zeroed buffer of exactly the type the kernel fills.
    let rc = unsafe { libc::sigaction(libc::SIGTERM, std::ptr::null(), current.as_mut_ptr()) };
    if rc != 0 {
        return false;
    }
    // SAFETY: a zero return means the kernel initialised the struct.
    unsafe { current.assume_init() }.sa_sigaction != libc::SIG_DFL
}

/// Has the parent gone, by any of the three signals?
///
/// Why three: being reparented is immune to pid reuse — only our real parent
/// dying changes `getppid()` — but it is available only when we are that
/// parent's direct child. `armed_ppid` is `None` for a detached grandchild,
/// which leaves the liveness probe, and a liveness probe alone cannot tell a
/// live parent from a recycled pid. The start-time comparison closes that.
/// What: `true` when `getppid()` has moved off `armed_ppid`, when
/// `parent.pid` no longer exists, or when the process now holding `parent.pid`
/// started at a different time than the one we armed on. A platform that cannot
/// report start times compares nothing and falls back to the first two.
/// Test: `parent_is_gone_on_reparent`, `parent_is_gone_when_named_parent_exits`,
/// `grandchild_ignores_its_own_reparent`, `recycled_pid_reads_as_gone`.
#[cfg(unix)]
fn parent_is_gone(parent: ParentIdentity, armed_ppid: Option<u32>) -> bool {
    if let Some(armed) = armed_ppid {
        // SAFETY: `getppid` takes no arguments and cannot fail.
        if unsafe { libc::getppid() } as u32 != armed {
            return true;
        }
    }
    if !pid_alive(parent.pid) {
        return true;
    }
    match (parent.start, process_start_secs(parent.pid)) {
        (Some(armed), Some(now)) => armed != now,
        // Either we could not read a start time when arming, or cannot read one
        // now. Reporting "gone" on a missing reading would kill a healthy daemon
        // on any platform this cannot see into, so liveness stands alone there.
        _ => false,
    }
}

/// True while a process with `pid` still exists.
///
/// What: `kill` with signal 0 delivers nothing and reports only whether the
/// target is signalable. `EPERM` means it exists but is not ours, which is still
/// alive; `ESRCH` means gone.
/// Test: `parent_is_gone_when_named_parent_exits`.
#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }
    // SAFETY: `kill` with signal 0 performs existence and permission checks
    // only, delivering no signal; every return value is safe to observe.
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if rc == 0 {
        return true;
    }
    matches!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EPERM)
    )
}

/// The start time of the process currently holding `pid`, in whole seconds.
///
/// What: on macOS, `pbi_start_tvsec` from `proc_pidinfo(PROC_PIDTASKALLINFO)` —
/// wall-clock seconds. On Linux, field 22 of `/proc/<pid>/stat`, which is clock
/// ticks since boot; the unit does not matter because the value is only ever
/// compared against another reading from the same host. `None` when the process
/// is gone, is not ours to inspect, or the platform has neither source.
/// Test: `start_time_is_readable_for_this_process`, `recycled_pid_reads_as_gone`.
#[cfg(target_os = "macos")]
fn process_start_secs(pid: u32) -> Option<u64> {
    if pid == 0 || pid > i32::MAX as u32 {
        return None;
    }
    let mut info = std::mem::MaybeUninit::<libc::proc_taskallinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_taskallinfo>() as libc::c_int;
    // SAFETY: `info` is a correctly sized, zeroed buffer of exactly the type
    // PROC_PIDTASKALLINFO fills, and `size` describes it. A short or failed read
    // returns a value below `size`, which is checked before the buffer is read.
    let written = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTASKALLINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if written < size {
        return None;
    }
    // SAFETY: the kernel wrote `size` bytes, so the struct is initialised.
    Some(unsafe { info.assume_init() }.pbsd.pbi_start_tvsec)
}

/// Linux counterpart of the macOS reader above: `/proc/<pid>/stat` field 22.
#[cfg(all(unix, target_os = "linux"))]
fn process_start_secs(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name in field 2 is parenthesised and may contain spaces, so
    // fields are counted from after the closing paren rather than by splitting
    // the whole line.
    let after_comm = stat.rsplit_once(") ")?.1;
    after_comm.split_whitespace().nth(19)?.parse().ok()
}

/// Every other unix: no start-time source, so identity degrades to the pid.
#[cfg(all(unix, not(target_os = "macos"), not(target_os = "linux")))]
fn process_start_secs(_pid: u32) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read the stamp `exit_with_parent` put on a `Command`.
    fn stamped_value(cmd: &Command) -> String {
        cmd.get_envs()
            .find(|(k, _)| *k == std::ffi::OsStr::new(ENV_EXIT_WITH_PARENT))
            .and_then(|(_, v)| v)
            .expect("exit_with_parent must set the env var")
            .to_string_lossy()
            .into_owned()
    }

    /// The spawner half of the contract: the stamp must carry THIS process's
    /// full identity under the documented name, and the child's parser must
    /// recover exactly that — pid and start time both.
    #[cfg(unix)]
    #[test]
    fn the_stamp_round_trips_this_process_identity() {
        let mut cmd = Command::new("true");
        exit_with_parent(&mut cmd);
        let parsed = parent_from_stamp(&stamped_value(&cmd), "test")
            .expect("the stamp this crate writes must parse");
        assert_eq!(
            parsed,
            identify(std::process::id()),
            "the stamp must name this process's pid and start time"
        );
    }

    /// The round-2 finding (#7085): the child must trust the STAMPED start time
    /// and never re-derive one from the pid. A stamp whose start time disagrees
    /// with the process now holding that pid is exactly what a recycle leaves
    /// behind between `Command::spawn()` and the child reaching `arm_from_env`,
    /// and the child must read it as "parent already gone" at arm — not adopt
    /// the impostor and wait for it to exit.
    ///
    /// This test is red against the pre-fix `arm_from_env`, which called
    /// `identify(parent)` in the child: re-deriving always agrees with itself,
    /// so the impostor reads as alive.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn a_stamped_start_time_that_does_not_match_the_live_pid_reads_as_gone() {
        let me = std::process::id();
        let real = process_start_secs(me).expect("this platform must report a start time");
        let impostor = format!("{me}{STAMP_SEPARATOR}{}", real.wrapping_add(1));

        let parsed = parent_from_stamp(&impostor, "test").expect("a well-formed stamp must parse");
        assert_eq!(parsed.pid, me, "the pid half must survive parsing");
        assert_eq!(
            parsed.start,
            Some(real.wrapping_add(1)),
            "the STAMPED start time must be kept verbatim, not re-read from the pid"
        );
        assert!(
            parent_is_gone(parsed, None),
            "a live pid whose stamped start time does not match it is a RECYCLED \
             pid; arming on it would watch a process nobody asked for and might \
             never fire"
        );
    }

    /// A stamp with no start time — what a spawner on a platform with no
    /// start-time source writes, and what the pre-#7085 stamp format looked
    /// like — must still arm, watching by liveness alone.
    #[cfg(unix)]
    #[test]
    fn a_stamp_without_a_start_time_degrades_to_liveness() {
        let parsed = parent_from_stamp("4242", "test").expect("a bare pid must parse");
        assert_eq!(
            parsed,
            ParentIdentity {
                pid: 4242,
                start: None
            }
        );
        let malformed = parent_from_stamp("4242:not-a-time", "test")
            .expect("a malformed start time must degrade, not refuse");
        assert_eq!(
            malformed.start, None,
            "an unparseable start time must never be treated as a mismatch"
        );
    }

    /// A stamp that names nothing watchable must arm nothing rather than watch
    /// pid 0 or pid 1 — pid 1 never exits, so arming on it is a watchdog that
    /// can never fire.
    #[cfg(unix)]
    #[test]
    fn an_unwatchable_stamp_arms_nothing() {
        for raw in ["0", "1", "", "not-a-pid", ":123"] {
            assert!(
                parent_from_stamp(raw, "test").is_none(),
                "{raw:?} must not arm a watchdog"
            );
        }
    }

    /// The graceful path depends on telling an installed SIGTERM handler from
    /// the default disposition; if that query could not, the watchdog would
    /// either raise into a hard kill or never raise at all.
    #[cfg(unix)]
    #[test]
    fn an_installed_sigterm_handler_is_distinguishable_from_the_default() {
        // SAFETY: a null `act` queries the current disposition; `saved` is a
        // correctly sized, zeroed buffer of the type the kernel fills.
        let mut saved = std::mem::MaybeUninit::<libc::sigaction>::zeroed();
        assert_eq!(
            unsafe { libc::sigaction(libc::SIGTERM, std::ptr::null(), saved.as_mut_ptr()) },
            0,
            "querying SIGTERM's disposition must succeed"
        );
        // SAFETY: the query returned 0, so the struct is initialised.
        let saved = unsafe { saved.assume_init() };

        // SAFETY: `libc::signal` sets SIGTERM's disposition for this process;
        // the original is captured above and restored below, and no test in this
        // binary sends itself SIGTERM.
        unsafe { libc::signal(libc::SIGTERM, libc::SIG_IGN) };
        let with_handler = term_handler_installed();
        // SAFETY: restores exactly the sigaction read above.
        unsafe { libc::sigaction(libc::SIGTERM, &saved, std::ptr::null_mut()) };

        assert!(
            with_handler,
            "a disposition other than SIG_DFL must read as an installed handler"
        );
        assert_eq!(
            term_handler_installed(),
            saved.sa_sigaction != libc::SIG_DFL,
            "the restored disposition must read back as whatever it was"
        );
    }

    /// The start-time source must actually work on the platforms this runs on.
    /// Without this the identity check degrades to liveness silently, which is
    /// the pid-reuse hole it exists to close.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn start_time_is_readable_for_this_process() {
        assert!(
            process_start_secs(std::process::id()).is_some(),
            "this platform must report a process start time"
        );
        assert!(
            process_start_secs(u32::MAX - 1).is_none(),
            "a pid nothing holds must report no start time"
        );
    }

    /// The reparent prong: a ppid that no longer matches the one we armed with
    /// means our spawner died, whatever the identity says.
    #[cfg(unix)]
    #[test]
    fn parent_is_gone_on_reparent() {
        // SAFETY: `getppid` takes no arguments and cannot fail.
        let real_ppid = unsafe { libc::getppid() } as u32;
        let me = identify(std::process::id());
        assert!(
            !parent_is_gone(me, Some(real_ppid)),
            "our own live pid under our real ppid must read as present"
        );
        assert!(
            parent_is_gone(me, Some(real_ppid.wrapping_add(1))),
            "a ppid that no longer matches the armed one must read as gone"
        );
    }

    /// The liveness prong: a named parent that exits and is reaped must read as
    /// gone even though our own `getppid()` never moved.
    #[cfg(unix)]
    #[test]
    fn parent_is_gone_when_named_parent_exits() {
        // SAFETY: `getppid` takes no arguments and cannot fail.
        let armed_ppid = unsafe { libc::getppid() } as u32;
        let mut fake_parent = Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn fake parent");
        let parent = identify(fake_parent.id());
        assert!(
            !parent_is_gone(parent, Some(armed_ppid)),
            "a live named parent must read as present"
        );
        fake_parent.kill().expect("kill fake parent");
        fake_parent.wait().expect("reap fake parent");
        assert!(
            parent_is_gone(parent, Some(armed_ppid)),
            "a reaped named parent must read as gone"
        );
    }

    /// A detached grandchild must NOT read its own reparent as its stamped
    /// parent dying: the intermediate CLI that spawned it exits immediately by
    /// design, so only the identity prong may speak for it.
    #[cfg(unix)]
    #[test]
    fn grandchild_ignores_its_own_reparent() {
        let mut live_parent = Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn stand-in parent");
        let parent = identify(live_parent.id());
        assert!(
            !parent_is_gone(parent, None),
            "with no reparent prong armed, a live stamped pid must read as present"
        );
        live_parent.kill().expect("kill stand-in parent");
        live_parent.wait().expect("reap stand-in parent");
        assert!(
            parent_is_gone(parent, None),
            "the identity prong must still fire once the stamped pid is gone"
        );
    }

    /// Pid reuse, which is the only way a grandchild's liveness probe can lie:
    /// the pid is alive and signalable, but the process holding it is not the
    /// one we armed on. A recycled pid is faked here by arming on a start time
    /// that does not match the live process's — exactly the state the kernel
    /// leaves behind when it hands a freed pid to something new.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn recycled_pid_reads_as_gone() {
        let live = identify(std::process::id());
        assert!(
            !parent_is_gone(live, None),
            "the real start time must read as present"
        );
        let recycled = ParentIdentity {
            pid: live.pid,
            start: live.start.map(|s| s.wrapping_add(1)),
        };
        assert!(
            parent_is_gone(recycled, None),
            "a live pid whose start time moved is a DIFFERENT process and must \
             read as gone; without this a recycled pid keeps a grandchild alive \
             forever"
        );
    }

    /// The other side of `recycled_pid_reads_as_gone`: when either start-time
    /// reading is missing, the comparison must abstain and let liveness decide.
    /// Reporting "gone" on a missing reading would make the watchdog kill a
    /// perfectly healthy daemon on any platform it cannot see into.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_start_time_favours_alive() {
        let unreadable = ParentIdentity {
            pid: std::process::id(),
            start: None,
        };
        assert!(
            !parent_is_gone(unreadable, None),
            "a live pid with no armed start time must read as present, not gone"
        );
        let dead = ParentIdentity {
            pid: u32::MAX - 1,
            start: None,
        };
        assert!(
            parent_is_gone(dead, None),
            "abstaining on the start time must not also suppress the liveness probe"
        );
    }

    /// The loop the watchdog thread awaits: it must keep waiting while the
    /// parent lives and return promptly once it dies. Watching a real `sleep`
    /// child by pid exercises the same code the daemon runs without signalling
    /// the test runner.
    #[cfg(unix)]
    #[test]
    fn watch_parent_returns_once_parent_dies() {
        // SAFETY: `getppid` takes no arguments and cannot fail.
        let armed_ppid = unsafe { libc::getppid() } as u32;
        let mut fake_parent = Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn fake parent");
        let parent = identify(fake_parent.id());

        let handle = std::thread::spawn(move || {
            watch_parent(parent, Some(armed_ppid), Duration::from_millis(25));
        });

        std::thread::sleep(Duration::from_millis(150));
        assert!(
            !handle.is_finished(),
            "the watchdog must keep waiting while its parent is alive"
        );

        fake_parent.kill().expect("kill fake parent");
        fake_parent.wait().expect("reap fake parent");

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !handle.is_finished() {
            assert!(
                std::time::Instant::now() < deadline,
                "the watchdog must return within 5s of its parent dying"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
        handle.join().expect("watchdog thread must not panic");
    }
}
