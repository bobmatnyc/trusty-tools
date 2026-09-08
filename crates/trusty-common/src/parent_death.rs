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
//! with the spawner's own pid; the spawned program calls [`arm_from_env`] early
//! in its startup and self-exits once that parent is gone.
//! [`arm_for_named_parent`] is the variant for a parent named out of band — a
//! `--parent-pid` flag rather than the environment — where the named pid need
//! not be the direct spawner. Both arming entry points are unix-only and compile
//! to no-ops elsewhere; stamping the environment is portable and harmless.
//!
//! Mechanism: a polled `getppid()` comparison plus a `kill(pid, 0)` liveness
//! probe, on a dedicated OS thread. `prctl(PR_SET_PDEATHSIG)` is Linux-only and
//! this workspace's daemons run on macOS first. A pipe held open by the parent
//! would report EOF, but only while no other process holds the write end — every
//! grandchild inherits it, so a daemon that spawns anything at all defeats the
//! signal. `getppid()` is reparent-detection, which no descendant can hold open,
//! and it behaves identically on macOS and Linux.
//!
//! Opt-in by construction: nothing arms unless [`ENV_EXIT_WITH_PARENT`] is set,
//! so a launchd-supervised daemon (which launchd starts with the plist's
//! environment, never a test's) is untouched.
//!
//! Test: `stamps_this_process_pid`, `parent_is_gone_on_reparent`,
//! `parent_is_gone_when_named_parent_exits`, `grandchild_ignores_its_own_reparent`,
//! `watch_parent_returns_once_parent_dies`.

use std::process::Command;

/// Names the pid a spawned process must not outlive (`TRUSTY_EXIT_WITH_PARENT`).
///
/// Why: the value has to cross an `exec`, and the environment is the one channel
/// that reaches a program whose argument vector the spawner does not own.
/// What: the exact env-var name; the value is the spawner's pid in decimal. Set
/// it through [`exit_with_parent`] rather than by hand, so the pid and the name
/// are written in one place.
/// Test: `stamps_this_process_pid`.
pub const ENV_EXIT_WITH_PARENT: &str = "TRUSTY_EXIT_WITH_PARENT";

/// How often the watchdog re-checks its parent.
///
/// Why: 250 ms bounds an orphan's lifetime to under a second while costing two
/// syscalls per tick, which is free next to a daemon's idle work.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

/// Stamp `cmd` so the process it spawns exits when THIS process dies.
///
/// Why: the spawner is the only one that knows its own pid, and it must record
/// it before the fork — after a SIGKILL there is nobody left to tell the child
/// anything. Callers pair this with [`arm_from_env`] in the spawned program.
/// What: sets [`ENV_EXIT_WITH_PARENT`] to `std::process::id()`. Portable: the
/// stamp is inert in a program that never arms, and on a non-unix target.
/// Test: `stamps_this_process_pid`.
pub fn exit_with_parent(cmd: &mut Command) -> &mut Command {
    cmd.env(ENV_EXIT_WITH_PARENT, std::process::id().to_string())
}

/// Tokio counterpart of [`exit_with_parent`], for an async spawn site.
///
/// Why: `tokio::process::Command` is a distinct type from `std`'s, and a test
/// that spawns a bridge asynchronously owes its child the same linkage. Both
/// entry points write the same name and the same value, so a caller never has to
/// know which one the other used.
/// What: sets [`ENV_EXIT_WITH_PARENT`] to `std::process::id()`.
/// Test: `stamps_this_process_pid` pins the value both share.
pub fn exit_with_parent_tokio(cmd: &mut tokio::process::Command) -> &mut tokio::process::Command {
    cmd.env(ENV_EXIT_WITH_PARENT, std::process::id().to_string())
}

/// Arm the watchdog from [`ENV_EXIT_WITH_PARENT`], if the spawner set it.
///
/// Why (#7085): this is the half that runs inside the child, and it is what a
/// SIGKILL of the parent cannot skip. Call it once, early, on the long-running
/// path of a program a test may spawn.
/// What: reads the env var. Absent → returns `None` and arms nothing, which is
/// every production invocation. Present and naming a pid above 1 → spawns the
/// watchdog thread and returns that pid. Unparseable or `<= 1` → warns on stderr
/// and arms nothing; a typo must not silently disable the linkage.
///
/// The reparent prong is armed only when the stamped pid IS this process's
/// direct parent. A daemon started detached by an intermediate CLI inherits the
/// stamp but is a grandchild: that CLI exits by design, so its own ppid moving
/// says nothing about whether the stamped process is still alive, and watching
/// for it would kill the daemon seconds after it started. Such a daemon watches
/// the stamped pid's liveness only.
/// Test: `stamps_this_process_pid` pins the contract's spawner half;
/// `crates/trusty-memory/tests/orphan_reap_7085.rs` proves the whole loop end to
/// end against a real SIGKILLed spawner.
#[cfg(unix)]
pub fn arm_from_env(tag: &str) -> Option<u32> {
    let raw = std::env::var(ENV_EXIT_WITH_PARENT).ok()?;
    let parent = match raw.trim().parse::<u32>() {
        Ok(pid) if pid > 1 => pid,
        _ => {
            eprintln!(
                "[{tag}] {ENV_EXIT_WITH_PARENT}={raw:?} is not a watchable pid; \
                 parent-death watchdog not armed"
            );
            return None;
        }
    };
    // SAFETY: `getppid` takes no arguments and cannot fail.
    let ppid = unsafe { libc::getppid() } as u32;
    arm(parent, (ppid == parent).then_some(ppid), tag);
    Some(parent)
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
/// to `parent_pid` keeps the check correct in that case, while the liveness
/// probe on `parent_pid` covers the parent this process was told about.
/// What: reads `getppid()` as the expected ppid, then arms as [`arm_from_env`]
/// does. A `parent_pid` of 0 or 1 is not watchable and arms nothing.
/// Test: `parent_is_gone_when_named_parent_exits` covers the liveness prong this
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
    arm(parent_pid, Some(armed_ppid), tag);
}

/// Non-unix stub: see [`arm_from_env`].
#[cfg(not(unix))]
pub fn arm_for_named_parent(_parent_pid: u32, _tag: &str) {}

/// Spawn the watchdog thread that self-exits this process once the parent goes.
///
/// Why a dedicated OS thread and not an async task: this must work in a program
/// with no runtime, and it must keep ticking while the runtime is saturated or
/// mid-shutdown — the whole point is that it fires when other machinery cannot.
/// What: blocks in [`watch_parent`], then `process::exit(0)`. Abrupt is correct:
/// the parent is already gone, so there is no work to drain and no reader for a
/// graceful shutdown's result. Exit 0 keeps launchd's `KeepAlive
/// { SuccessfulExit: false }` from respawning, which matters if this ever arms
/// under a supervisor.
#[cfg(unix)]
fn arm(named_parent: u32, armed_ppid: Option<u32>, tag: &str) {
    let owned_tag = tag.to_string();
    let spawned = std::thread::Builder::new()
        .name("parent-death-watchdog".to_string())
        .spawn(move || {
            watch_parent(named_parent, armed_ppid, POLL_INTERVAL);
            eprintln!(
                "[{owned_tag}] parent pid {named_parent} is gone; exiting rather \
                 than becoming an orphan"
            );
            std::process::exit(0);
        });
    if let Err(e) = spawned {
        eprintln!("[{tag}] could not start the parent-death watchdog thread: {e}");
    }
}

/// Block until the parent is gone, polling every `interval`.
///
/// What: pure control flow with no process-global effect, so the detection logic
/// is testable without calling `process::exit` on the test runner.
/// Test: `watch_parent_returns_once_parent_dies`.
#[cfg(unix)]
fn watch_parent(named_parent: u32, armed_ppid: Option<u32>, interval: std::time::Duration) {
    while !parent_is_gone(named_parent, armed_ppid) {
        std::thread::sleep(interval);
    }
}

/// Has the parent gone, by either signal?
///
/// Why two signals: being reparented is immune to pid reuse — only our real
/// parent dying changes `getppid()` — but it is available only when we are that
/// parent's direct child. `armed_ppid` is `None` for a detached grandchild,
/// which leaves the liveness probe as the sole signal. Either firing is
/// sufficient.
/// What: `true` when `getppid()` has moved off `armed_ppid`, or when
/// `named_parent` no longer exists.
/// Test: `parent_is_gone_on_reparent`, `parent_is_gone_when_named_parent_exits`,
/// `grandchild_ignores_its_own_reparent`.
#[cfg(unix)]
fn parent_is_gone(named_parent: u32, armed_ppid: Option<u32>) -> bool {
    if let Some(armed) = armed_ppid {
        // SAFETY: `getppid` takes no arguments and cannot fail.
        if unsafe { libc::getppid() } as u32 != armed {
            return true;
        }
    }
    !pid_alive(named_parent)
}

/// True while a process with `pid` still exists.
///
/// What: `kill` with signal 0 delivers nothing and reports only whether the
/// target is signalable. `EPERM` means it exists but is not ours, which is still
/// alive; `ESRCH` means gone.
/// Test: `parent_is_gone_when_named_parent_exits`.
#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The spawner half of the contract: the stamp must carry THIS process's
    /// pid under the documented name, because that pid is what the child polls.
    #[test]
    fn stamps_this_process_pid() {
        let mut cmd = Command::new("true");
        exit_with_parent(&mut cmd);
        let stamped = cmd
            .get_envs()
            .find(|(k, _)| *k == std::ffi::OsStr::new(ENV_EXIT_WITH_PARENT))
            .and_then(|(_, v)| v)
            .expect("exit_with_parent must set the env var");
        assert_eq!(
            stamped,
            std::ffi::OsStr::new(&std::process::id().to_string())
        );
    }

    /// The reparent prong: a ppid that no longer matches the one we armed with
    /// means our spawner died, whatever the named pid says.
    #[cfg(unix)]
    #[test]
    fn parent_is_gone_on_reparent() {
        // SAFETY: `getppid` takes no arguments and cannot fail.
        let real_ppid = unsafe { libc::getppid() } as u32;
        assert!(
            !parent_is_gone(std::process::id(), Some(real_ppid)),
            "our own live pid under our real ppid must read as present"
        );
        assert!(
            parent_is_gone(std::process::id(), Some(real_ppid.wrapping_add(1))),
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
        let pid = fake_parent.id();
        assert!(
            !parent_is_gone(pid, Some(armed_ppid)),
            "a live named parent must read as present"
        );
        fake_parent.kill().expect("kill fake parent");
        fake_parent.wait().expect("reap fake parent");
        assert!(
            parent_is_gone(pid, Some(armed_ppid)),
            "a reaped named parent must read as gone"
        );
    }

    /// A detached grandchild must NOT read its own reparent as its stamped
    /// parent dying: the intermediate CLI that spawned it exits immediately by
    /// design, so only the liveness prong may speak for it.
    #[cfg(unix)]
    #[test]
    fn grandchild_ignores_its_own_reparent() {
        let mut live_parent = Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn stand-in parent");
        let pid = live_parent.id();
        assert!(
            !parent_is_gone(pid, None),
            "with no reparent prong armed, a live stamped pid must read as present"
        );
        live_parent.kill().expect("kill stand-in parent");
        live_parent.wait().expect("reap stand-in parent");
        assert!(
            parent_is_gone(pid, None),
            "the liveness prong must still fire once the stamped pid is gone"
        );
    }

    /// The loop the watchdog thread awaits: it must keep waiting while the
    /// parent lives and return promptly once it dies. Watching a real `sleep`
    /// child by pid exercises the same code the daemon runs without calling
    /// `process::exit`, which would kill the test runner.
    #[cfg(unix)]
    #[test]
    fn watch_parent_returns_once_parent_dies() {
        // SAFETY: `getppid` takes no arguments and cannot fail.
        let armed_ppid = unsafe { libc::getppid() } as u32;
        let mut fake_parent = Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn fake parent");
        let pid = fake_parent.id();

        let handle = std::thread::spawn(move || {
            watch_parent(pid, Some(armed_ppid), std::time::Duration::from_millis(25));
        });

        std::thread::sleep(std::time::Duration::from_millis(150));
        assert!(
            !handle.is_finished(),
            "the watchdog must keep waiting while its parent is alive"
        );

        fake_parent.kill().expect("kill fake parent");
        fake_parent.wait().expect("reap fake parent");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !handle.is_finished() {
            assert!(
                std::time::Instant::now() < deadline,
                "the watchdog must return within 5s of its parent dying"
            );
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        handle.join().expect("watchdog thread must not panic");
    }
}
