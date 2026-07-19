//! macOS-only spawn primitives shared by [`super::disclaimed_output`] (the
//! capture-to-completion shape, [`capture`]), [`super::disclaimed_status`]
//! and [`super::disclaimed_spawn_detached`] (the inherited-stdio shapes,
//! [`status`]), and [`super::disclaimed_piped_spawn`] (the long-lived async
//! piped shape, [`piped`]).
//!
//! Why: all three spawn shapes need the SAME low-level building blocks —
//! resolving the private disclaim SPI, reaping a pid, and (for two of the
//! three) a CLOEXEC pipe pair or the Darwin chdir file-action extension — so
//! they live here once rather than being copy-pasted into each shape's own
//! file.
//! What: [`resolve_disclaim_fn`] (dlsym lookup for
//! `responsibility_spawnattrs_setdisclaim`), [`pipe_cloexec`] (a pipe with
//! both ends `FD_CLOEXEC`), [`OwnedFd`] (an RAII raw-fd guard), and
//! [`wait_for`] (blocking `waitpid` on a specific pid). `environ` is declared
//! here since [`capture`] passes it straight through to `posix_spawnp`, and
//! `posix_spawn_file_actions_addchdir_np` since both [`status`] and [`piped`]
//! need to reproduce a `Command::current_dir()`.
//! Test: exercised transitively by `capture`'s, `status`'s, and `piped`'s own
//! test modules; there is no direct unit test of these helpers in isolation
//! (they have no externally observable behavior without a real spawn).

mod capture;
mod piped;
mod status;

pub(crate) use capture::spawn_capture_disclaimed;
pub(crate) use piped::spawn_piped_disclaimed;
pub(crate) use status::{spawn_detached_disclaimed, spawn_status_inherit_disclaimed};

use std::io;
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;

// The current process environment block, passed through to a disclaimed
// child so it inherits our environment exactly as `Command` would.
unsafe extern "C" {
    pub(super) static environ: *const *mut libc::c_char;
}

// `posix_spawn_file_actions_addchdir_np` — a Darwin non-portable ("_np")
// extension declared in <spawn.h> (public libSystem symbol, unlike the
// private disclaim SPI below, so it is linked directly rather than resolved
// via `dlsym`). Available since macOS 10.15; sets the child's working
// directory as a posix_spawn file action. Shared by `status` and `piped`,
// both of which need to reproduce a `Command::current_dir()` set on the
// `std::process::Command` they're given.
unsafe extern "C" {
    pub(super) fn posix_spawn_file_actions_addchdir_np(
        file_actions: *mut libc::posix_spawn_file_actions_t,
        path: *const libc::c_char,
    ) -> libc::c_int;
}

// Signature of the private libSystem SPI that marks a posix_spawn attribute
// set as "disclaim responsibility". Resolved via `dlsym` so its absence on
// any future macOS degrades gracefully to a normal spawn.
type DisclaimFn = unsafe extern "C" fn(*mut libc::posix_spawnattr_t, libc::c_int) -> libc::c_int;

/// Resolve `responsibility_spawnattrs_setdisclaim` at runtime, or `None`.
///
/// Why: the symbol is a private SPI; linking it directly would make the
/// whole binary fail to load if it ever disappeared. `dlsym` turns that into
/// a graceful "spawn without disclaim" fallback.
/// What: looks the symbol up in the global namespace (it lives in libSystem,
/// always loaded) and transmutes it to a typed fn pointer.
/// Test: covered indirectly — the capture/status/piped tests exercise the
/// whole spawn path; this returns `Some` on any TCC-era macOS.
pub(super) fn resolve_disclaim_fn() -> Option<DisclaimFn> {
    // SAFETY: `dlsym(RTLD_DEFAULT, name)` is a read-only symbol lookup. The
    // returned pointer, if non-null, is a valid function with the C ABI we
    // transmute to (matching the documented SPI signature).
    unsafe {
        let name = c"responsibility_spawnattrs_setdisclaim";
        let sym = libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr());
        if sym.is_null() {
            None
        } else {
            Some(std::mem::transmute::<*mut libc::c_void, DisclaimFn>(sym))
        }
    }
}

/// Reap `pid`, returning its `ExitStatus`.
///
/// Why: a spawned child must be waited on to avoid a zombie and to learn its
/// exit status. `piped` also uses this on a background `spawn_blocking` task
/// so the long-lived disclaimed child is always eventually reaped even if
/// nobody calls `wait()` explicitly.
/// What: loops over `waitpid` through `EINTR`, converting the raw wait status
/// into an `ExitStatus`.
/// Test: exercised by every `disclaimed_output_*`/`disclaimed_status_*`
/// capture test and the piped lifecycle tests.
pub(super) fn wait_for(pid: libc::pid_t) -> io::Result<ExitStatus> {
    loop {
        let mut status: libc::c_int = 0;
        // SAFETY: `waitpid` writes only into `status`; `pid` is our child.
        let rc = unsafe { libc::waitpid(pid, &mut status, 0) };
        if rc == -1 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(e);
        }
        return Ok(ExitStatus::from_raw(status));
    }
}

/// Create a pipe whose BOTH ends are `FD_CLOEXEC`.
///
/// Why: the read (or write) end kept by the parent must not leak into
/// unrelated spawns; the end handed to the child is re-established onto a
/// standard fd by dup2 (which clears CLOEXEC on the target), so marking the
/// sources CLOEXEC means the child keeps only its dup2'd fds, no strays.
/// What: `pipe()` then `fcntl(F_SETFD, FD_CLOEXEC)` on each end.
/// Test: exercised by the capture and piped tests (correct EOF/no-leak
/// behaviour).
pub(super) fn pipe_cloexec() -> io::Result<(libc::c_int, libc::c_int)> {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: `pipe` writes exactly two fds into the array.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    for fd in fds {
        // SAFETY: `fd` is a freshly created, open fd we own.
        if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
            let e = io::Error::last_os_error();
            unsafe {
                libc::close(fds[0]);
                libc::close(fds[1]);
            }
            return Err(e);
        }
    }
    Ok((fds[0], fds[1]))
}

/// A raw fd that is closed on drop unless its value is `-1`.
///
/// Why: spawn setup has several fallible steps; an RAII guard guarantees no
/// pipe fd leaks on an early return without hand-writing close on every
/// error path.
/// What: wraps a `c_int`; `into_raw` releases ownership (sets the guard to
/// `-1`) when the fd is handed off (to a `File` or converted to a
/// `std::os::fd::OwnedFd`).
/// Test: exercised by the capture and piped tests.
pub(super) struct OwnedFd(pub(super) libc::c_int);

impl OwnedFd {
    pub(super) fn into_raw(mut self) -> libc::c_int {
        let fd = self.0;
        self.0 = -1;
        fd
    }
}

impl Drop for OwnedFd {
    fn drop(&mut self) {
        if self.0 >= 0 {
            // SAFETY: we own this fd and it is closed exactly once.
            unsafe { libc::close(self.0) };
        }
    }
}
