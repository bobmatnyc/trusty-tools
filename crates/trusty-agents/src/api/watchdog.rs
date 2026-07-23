//! Parent-death watchdog for the `tagent --api` sidecar (#3734).
//!
//! Why: When the trusty-agents desktop GUI spawns `tagent --api` as a sidecar,
//! the sidecar must not outlive its parent. PR #3728 reaps it on the Tauri
//! quit event, but that path does not fire for every GUI death: an external
//! SIGTERM/SIGKILL, a crash, or macOS's synchronous Cmd+Q teardown can all
//! exit the GUI without the reap running, leaving `tagent --api` orphaned
//! (reparented to launchd, pid 1) and still holding its fixed port. A watchdog
//! INSIDE the sidecar closes the whole orphan class: whatever kills the parent,
//! the sidecar notices and self-exits, releasing the port.
//! What: `arm_parent_death_watchdog(parent_pid)` spawns a background task that
//! self-exits the process once `parent_pid` (the GUI, passed via `--parent-pid`)
//! is gone. `watch_parent` is the pure, testable detection loop and `pid_alive`
//! the liveness primitive. Unix-only (macOS/Linux); a no-op elsewhere.
//! Test: `watchdog_exits_when_named_parent_dies`, `pid_alive_tracks_process`.

#[cfg(unix)]
use std::time::Duration;

/// Poll interval for the parent-liveness loop. 2s keeps the watchdog cheap
/// (a `getppid`/`kill(0)` pair costs effectively nothing) while bounding the
/// worst-case orphan lifetime after a parent death to a couple of seconds —
/// well within demo tolerances and far better than "forever".
#[cfg(unix)]
const WATCH_INTERVAL: Duration = Duration::from_secs(2);

/// True while a process with `pid` still exists (we can signal it, or are
/// merely denied permission); false once it is gone (`ESRCH`).
///
/// Why: The fallback death signal is "the named parent pid vanished". `kill`
/// with signal 0 is the canonical existence probe — it delivers nothing and
/// only reports whether the target is signalable.
/// What: Returns `true` for `rc == 0` (alive) or `EPERM` (exists, not ours to
/// signal); `false` for `ESRCH` (no such process) or any other error.
/// Test: `pid_alive_tracks_process`.
#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    // SAFETY: `kill` with signal 0 performs existence/permission checks only,
    // delivering no signal; every return value is memory-safe to observe.
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if rc == 0 {
        return true;
    }
    matches!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EPERM)
    )
}

/// Block until the parent identified by `parent_pid` is gone, polling every
/// `interval`.
///
/// Why: Two independent death signals make this robust against pid reuse. The
/// primary one — being reparented away from whoever spawned us — is reuse-proof:
/// our real parent dying is the only thing that changes our `getppid()` (on
/// macOS we are reparented to launchd, pid 1). We capture our parent at entry
/// (`armed_ppid`) and watch for it to change, rather than comparing against the
/// passed `parent_pid`, so the check is correct even if `--parent-pid` names a
/// pid that is not literally our direct spawner (in the GUI-sidecar case the two
/// are identical). The secondary signal — the named `parent_pid` no longer
/// existing — covers exactly that not-our-spawner case.
/// What: Returns when EITHER `getppid()` changes from its entry value OR
/// `!pid_alive(parent_pid)`. Pure control flow with no process-global side
/// effects, so it is unit-testable without booting the API server or calling
/// `process::exit`.
/// Test: `watchdog_exits_when_named_parent_dies`.
#[cfg(unix)]
async fn watch_parent(parent_pid: u32, interval: Duration) {
    // SAFETY: `getppid` takes no arguments and is always safe.
    let armed_ppid = unsafe { libc::getppid() } as u32;
    loop {
        // Reparented away from our spawner? Fast and immune to pid reuse.
        // SAFETY: `getppid` takes no arguments and is always safe.
        if unsafe { libc::getppid() } as u32 != armed_ppid {
            return;
        }
        // Belt-and-suspenders: the named parent pid vanished outright.
        if !pid_alive(parent_pid) {
            return;
        }
        tokio::time::sleep(interval).await;
    }
}

/// Arm the parent-death watchdog: spawn a background task that self-exits this
/// process once `parent_pid` is gone.
///
/// Why: #3734 — no GUI failure mode (Cmd+Q, crash, external SIGTERM/SIGKILL)
/// may leave the `--api` sidecar orphaned on its fixed port. This is the
/// guaranteed backstop that runs regardless of whether the GUI's own Tauri-side
/// reap fires.
/// What: Only arms for a watchable `parent_pid` (> 1). Spawns a Tokio task that
/// awaits `watch_parent` and then `process::exit(0)`s — abrupt but reliable, so
/// the listener/port is released immediately (the GUI is already gone, so there
/// is no in-flight work to drain). Call once, only when running as a sidecar
/// (i.e. the GUI passed `--parent-pid`).
/// Test: `watchdog_exits_when_named_parent_dies` covers the detection loop the
/// spawned task awaits; the `process::exit` shell is deliberately not unit-
/// tested (it would terminate the test runner).
#[cfg(unix)]
pub fn arm_parent_death_watchdog(parent_pid: u32) {
    if parent_pid <= 1 {
        eprintln!(
            "[tagent] parent-death watchdog not armed: parent pid {parent_pid} is not watchable"
        );
        return;
    }
    eprintln!("[tagent] parent-death watchdog armed for parent pid {parent_pid}");
    tokio::spawn(async move {
        watch_parent(parent_pid, WATCH_INTERVAL).await;
        eprintln!(
            "[tagent] parent pid {parent_pid} exited; shutting down --api sidecar to release its port"
        );
        std::process::exit(0);
    });
}

/// Non-unix stub: the watchdog relies on `getppid`/`kill`, so arming is a no-op
/// off unix. The desktop GUI ships on macOS; this only keeps the crate
/// compiling on other targets.
#[cfg(not(unix))]
pub fn arm_parent_death_watchdog(_parent_pid: u32) {}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// `pid_alive` must read a live child as alive and a reaped one as gone —
    /// the liveness primitive the fallback death signal depends on.
    #[tokio::test]
    async fn pid_alive_tracks_process() {
        let mut child = tokio::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep");
        let pid = child.id().expect("child pid");
        assert!(pid_alive(pid), "freshly spawned child must read as alive");
        child.start_kill().expect("kill child");
        child.wait().await.expect("reap child");
        // After reap the pid is freed and `kill(pid, 0)` returns ESRCH.
        assert!(!pid_alive(pid), "reaped child must read as gone");
    }

    /// The headless equivalent of "spawn `tagent --api` under a short-lived
    /// fake parent, assert self-exit": the watchdog loop must keep running
    /// while its named parent is alive and return promptly once it dies. We
    /// watch a real `sleep` child by pid (rather than boot the API server or
    /// call `process::exit`, which would kill the test runner). Our own
    /// `getppid()` never changes during the test, so only the pid-liveness
    /// branch of `watch_parent` can fire — exactly the branch a fake parent
    /// exercises.
    #[tokio::test]
    async fn watchdog_exits_when_named_parent_dies() {
        let mut fake_parent = tokio::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn fake parent");
        let pid = fake_parent.id().expect("fake parent pid");

        let handle = tokio::spawn(watch_parent(pid, Duration::from_millis(50)));

        // While the parent lives, the watchdog must not resolve.
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            !handle.is_finished(),
            "watchdog must keep running while its parent is alive"
        );

        // Kill the fake parent; the watchdog must detect it and return.
        fake_parent.start_kill().expect("kill fake parent");
        fake_parent.wait().await.expect("reap fake parent");

        tokio::time::timeout(Duration::from_secs(3), handle)
            .await
            .expect("watchdog must detect parent death within 3s")
            .expect("watchdog task must not panic");
    }
}
