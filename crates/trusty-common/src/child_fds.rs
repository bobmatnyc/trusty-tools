//! Keep a spawned child from inheriting the parent's sockets (#10083).
//!
//! Why: on macOS there is no `SOCK_CLOEXEC`, so `mio` (and std) create a
//! socket and set `FD_CLOEXEC` in a second step. A child forked between the
//! two steps holds the socket after the parent drops it; a standalone model
//! kept 511 of 20,000 dropped connections open with four spawner threads.
//! std's spawn does not use `POSIX_SPAWN_CLOEXEC_DEFAULT`, and no code we own
//! creates every socket, so the fix has to act on the child.
//! What: `seal_inherited` (and the `_except` / `_tokio` forms) install a
//! `pre_exec` hook that sets `FD_CLOEXEC` on every descriptor from 3 up to
//! the process fd limit, so `exec` closes them. Setting the flag rather than
//! calling `close` keeps std's own close-on-exec error pipe working: closing
//! it would report a failed `exec` as a successful spawn. Active on macOS
//! only; elsewhere the kernel creates sockets close-on-exec atomically and
//! the helpers do nothing. A `pre_exec` hook takes std off `posix_spawn`
//! onto the fork path, so a spawn costs slightly more.
//! Test: `cargo test -p trusty-common --features unconditional-only child_fds`.

#[cfg(all(unix, any(target_os = "macos", test)))]
use std::io;

/// A raw descriptor number (`std::os::fd::RawFd` exists on unix only).
type RawFd = i32;

/// The first descriptor the hook touches; 0-2 are the child's stdio.
#[cfg(all(unix, any(target_os = "macos", test)))]
const FIRST_FD: RawFd = 3;

/// Upper bound on descriptors the hook visits, whatever the fd limit says.
#[cfg(all(unix, any(target_os = "macos", test)))]
const MAX_SCAN: RawFd = 1 << 16;

/// Set `FD_CLOEXEC` on every inherited descriptor of the child `cmd` spawns.
///
/// Why: see the module doc (#10083).
/// What: [`seal_inherited_except`] with an empty allowlist.
/// Test: `a_sealed_child_does_not_inherit_a_leaked_socket`.
pub fn seal_inherited(cmd: &mut std::process::Command) -> &mut std::process::Command {
    seal_inherited_except(cmd, &[])
}

/// [`seal_inherited`], leaving the descriptors in `keep` as they are.
///
/// Why: a child that is meant to receive a descriptor (the `tm secrets exec`
/// token pipe) must still get it.
/// What: the hook skips every fd in `keep`. The caller clears `FD_CLOEXEC`
/// on those itself; the hook neither sets nor clears it.
/// Test: `a_kept_descriptor_still_reaches_the_child`.
pub fn seal_inherited_except<'a>(
    cmd: &'a mut std::process::Command,
    keep: &[RawFd],
) -> &'a mut std::process::Command {
    #[cfg(target_os = "macos")]
    install(cmd, keep);
    #[cfg(not(target_os = "macos"))]
    let _ = keep;
    cmd
}

/// [`seal_inherited`] for a `tokio::process::Command`.
///
/// Test: `a_sealed_tokio_child_does_not_inherit_a_leaked_socket`.
pub fn seal_inherited_tokio(cmd: &mut tokio::process::Command) -> &mut tokio::process::Command {
    #[cfg(target_os = "macos")]
    install_tokio(cmd, &[]);
    cmd
}

/// Install the sealing hook on a std command, on every unix target.
///
/// Why: the public helpers are macOS-only, but the hook body is plain POSIX;
/// tests on Linux CI call this directly so the code is exercised there too.
#[cfg(all(unix, any(target_os = "macos", test)))]
pub(crate) fn install(cmd: &mut std::process::Command, keep: &[RawFd]) {
    use std::os::unix::process::CommandExt as _;
    // SAFETY: the hook runs between fork and exec and calls only `fcntl`
    // (async-signal-safe) over data prepared before the fork: no allocation,
    // no lock, no access to parent state beyond the captured `Vec`.
    unsafe {
        cmd.pre_exec(sealing_hook(keep));
    }
}

/// [`install`] for a tokio command.
#[cfg(all(unix, any(target_os = "macos", test)))]
pub(crate) fn install_tokio(cmd: &mut tokio::process::Command, keep: &[RawFd]) {
    // SAFETY: as in [`install`].
    unsafe {
        cmd.pre_exec(sealing_hook(keep));
    }
}

/// The `pre_exec` body: everything allocated here, before the fork.
#[cfg(all(unix, any(target_os = "macos", test)))]
fn sealing_hook(keep: &[RawFd]) -> impl FnMut() -> io::Result<()> + Send + Sync + 'static {
    let keep = keep.to_vec();
    // SAFETY: `getdtablesize` takes no arguments and reads a process limit.
    let limit = unsafe { libc::getdtablesize() }.clamp(FIRST_FD, MAX_SCAN);
    move || {
        seal_range(limit, &keep);
        Ok(())
    }
}

/// Set `FD_CLOEXEC` on every open fd in `FIRST_FD..limit` not in `keep`.
///
/// What: `fcntl` only. A closed fd (`EBADF`) and a flag already set are skipped.
#[cfg(all(unix, any(target_os = "macos", test)))]
fn seal_range(limit: RawFd, keep: &[RawFd]) {
    for fd in FIRST_FD..limit {
        if keep.contains(&fd) {
            continue;
        }
        // SAFETY: `fcntl` on a plain fd number; an invalid fd returns -1.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if flags >= 0 && flags & libc::FD_CLOEXEC == 0 {
            // SAFETY: as above; the result is deliberately ignored.
            unsafe {
                libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
            }
        }
    }
}

#[cfg(all(test, unix))]
#[path = "child_fds_tests.rs"]
mod tests;
