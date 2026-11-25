//! macOS-only spawn primitives shared by [`super::disclaimed_output`] (the
//! capture-to-completion shape, [`capture`]), [`super::disclaimed_status`]
//! and [`super::disclaimed_spawn_detached`] (the inherited-stdio shapes,
//! [`status`]), [`super::disclaimed_piped_spawn`] (the long-lived async
//! piped shape, [`piped`]), and [`super::disclaimed_stderr_piped_spawn`]/
//! [`super::disclaimed_stdout_piped_spawn`] (the synchronous single-stream
//! piped shapes, [`stderr_piped`]/[`stdout_piped`] — issue #3267).
//!
//! Why: all five spawn shapes need the SAME low-level building blocks —
//! resolving the private disclaim SPI, reaping a pid, and (for most of the
//! five) a CLOEXEC pipe pair or the Darwin chdir file-action extension — so
//! they live here once rather than being copy-pasted into each shape's own
//! file.
//! What: [`resolve_disclaim_fn`] (dlsym lookup for
//! `responsibility_spawnattrs_setdisclaim`), [`cloexec_default`] (the
//! `POSIX_SPAWN_CLOEXEC_DEFAULT` setup), [`pipe_cloexec`] (a pipe with
//! both ends `FD_CLOEXEC`), [`OwnedFd`] (an RAII raw-fd guard), and
//! [`wait_for`] (blocking `waitpid` on a specific pid). `environ` is declared
//! here since [`capture`] passes it straight through to `posix_spawnp`, and
//! `posix_spawn_file_actions_addchdir_np` since [`status`], [`piped`],
//! [`stderr_piped`], and [`stdout_piped`] all need to reproduce a
//! `Command::current_dir()`.
//! Test: exercised transitively by `capture`'s, `status`'s, `piped`'s,
//! `stderr_piped`'s, and `stdout_piped`'s own test modules; there is no
//! direct unit test of these helpers in isolation (they have no externally
//! observable behavior without a real spawn).

mod capture;
mod piped;
mod status;
mod stderr_piped;
mod stdout_piped;

pub(crate) use capture::spawn_capture_disclaimed;
pub(crate) use piped::spawn_piped_disclaimed;
pub(crate) use status::{spawn_detached_disclaimed, spawn_status_inherit_disclaimed};
pub(crate) use stderr_piped::spawn_stderr_piped_disclaimed;
pub(crate) use stdout_piped::spawn_stdout_piped_disclaimed;

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

/// Poll `pid` without blocking: `Some(status)` once it has exited.
///
/// Why: [`wait_for`] blocks with no bound, so a wedged child hangs its caller
/// forever — a `claude --version` probe stalled a live `tm` session launch
/// (#5969). A `WNOHANG` variant lets the `timeout` module bound the wait and
/// kill the child when the deadline passes.
/// What: `waitpid(pid, WNOHANG)` through `EINTR`; `rc == 0` means the child is
/// still running, so `Ok(None)`.
/// Test: `spawn_disclaim::timeout`'s `disclaimed_stdout_with_timeout_*` tests.
pub(super) fn try_wait_for(pid: libc::pid_t) -> io::Result<Option<ExitStatus>> {
    loop {
        let mut status: libc::c_int = 0;
        // SAFETY: `waitpid` writes only into `status`; `pid` is our child.
        let rc = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if rc == -1 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(e);
        }
        if rc == 0 {
            return Ok(None);
        }
        return Ok(Some(ExitStatus::from_raw(status)));
    }
}

// `posix_spawn_file_actions_addinherit_np` — public Darwin <spawn.h>
// extension: under `POSIX_SPAWN_CLOEXEC_DEFAULT`, keeps `fd` open in the child.
unsafe extern "C" {
    fn posix_spawn_file_actions_addinherit_np(
        file_actions: *mut libc::posix_spawn_file_actions_t,
        fd: libc::c_int,
    ) -> libc::c_int;
}

/// Close every descriptor in the child except those the spawn sets up.
///
/// Why: #10083 — a parent fd created without atomic CLOEXEC (mio sockets,
/// std pipes) must not reach a disclaimed child.
/// What: ORs `POSIX_SPAWN_CLOEXEC_DEFAULT` into `attr`'s flags, keeping any
/// flag already set, and marks each fd in `inherit` (stdio the shape leaves
/// untouched) inheritable. Fds produced by dup2/open file actions survive.
/// Test: `a_disclaimed_capture_child_does_not_inherit_a_stray_fd`,
/// `a_disclaimed_status_child_does_not_inherit_a_stray_fd_but_keeps_stdio`.
///
/// # Safety
/// `attr` and `file_actions` must be initialized and not yet destroyed.
pub(super) unsafe fn cloexec_default(
    attr: &mut libc::posix_spawnattr_t,
    file_actions: &mut libc::posix_spawn_file_actions_t,
    inherit: &[libc::c_int],
) -> io::Result<()> {
    // SAFETY: the caller guarantees both structures are initialized.
    unsafe {
        let mut flags: libc::c_short = 0;
        let rc = libc::posix_spawnattr_getflags(attr, &mut flags);
        if rc != 0 {
            return Err(io::Error::from_raw_os_error(rc));
        }
        flags |= libc::POSIX_SPAWN_CLOEXEC_DEFAULT as libc::c_short;
        let rc = libc::posix_spawnattr_setflags(attr, flags);
        if rc != 0 {
            return Err(io::Error::from_raw_os_error(rc));
        }
        for &fd in inherit {
            let rc = posix_spawn_file_actions_addinherit_np(file_actions, fd);
            if rc != 0 {
                return Err(io::Error::from_raw_os_error(rc));
            }
        }
    }
    Ok(())
}

/// Create a pipe whose BOTH ends are `FD_CLOEXEC`.
///
/// Why: the parent-held end must not leak into unrelated spawns; the end
/// handed to the child is re-established onto a standard fd by dup2, so the
/// child keeps only its dup2'd fds.
/// What: `pipe()` then `fcntl(F_SETFD, FD_CLOEXEC)` on each end. Darwin has
/// no `pipe2`, so a window remains between the two calls in which a fork in
/// another thread inherits both ends. A disclaimed child cannot: every shape
/// spawns with [`cloexec_default`]. A FIFO opened `O_CLOEXEC` would close the
/// window but never reports EOF through tokio's kqueue reader (#10083).
/// Test: `pipe_cloexec_ends_are_close_on_exec`; the capture and piped tests.
pub(super) fn pipe_cloexec() -> io::Result<(libc::c_int, libc::c_int)> {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: `pipe` writes exactly two fds into the array.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let (rd, wr) = (OwnedFd(fds[0]), OwnedFd(fds[1]));
    for fd in [rd.0, wr.0] {
        // SAFETY: `fd` is a freshly created, open fd we own.
        if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok((rd.into_raw(), wr.into_raw()))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Open `/dev/null` with `FD_CLOEXEC` cleared: a descriptor nobody passed
    /// to the child on purpose, but which a plain exec lets it inherit.
    fn inheritable_stray_fd() -> OwnedFd {
        // SAFETY: `open` on a NUL-terminated literal path; the guard owns it.
        let fd = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDONLY) };
        assert!(fd >= 0, "open /dev/null: {}", io::Error::last_os_error());
        // SAFETY: `fd` is open and ours; F_SETFD 0 clears FD_CLOEXEC.
        assert_ne!(unsafe { libc::fcntl(fd, libc::F_SETFD, 0) }, -1);
        OwnedFd(fd)
    }

    /// #10083: a capture-shape child sees no stray parent descriptor.
    #[test]
    fn a_disclaimed_capture_child_does_not_inherit_a_stray_fd() {
        let stray = inheritable_stray_fd();
        let script = format!(
            "if [ -e /dev/fd/{} ]; then echo leaked; else echo clean; fi",
            stray.0
        );
        let out = spawn_capture_disclaimed("/bin/sh", &["-c".to_string(), script])
            .expect("spawn /bin/sh");
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "clean");
    }

    /// #10083: an inherited-stdio child sees no stray descriptor, yet still
    /// holds its stdin, stdout and stderr.
    #[test]
    fn a_disclaimed_status_child_does_not_inherit_a_stray_fd_but_keeps_stdio() {
        let stray = inheritable_stray_fd();
        let script = format!(
            "[ -e /dev/fd/0 ] && [ -e /dev/fd/1 ] && [ -e /dev/fd/2 ] && [ ! -e /dev/fd/{} ]",
            stray.0
        );
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.arg("-c").arg(script);
        let status = spawn_status_inherit_disclaimed(&mut cmd).expect("spawn /bin/sh");
        assert!(
            status.success(),
            "child saw a stray fd or lost stdio: {status:?}"
        );
    }

    /// #10083: both pipe ends are close-on-exec and form a blocking pipe.
    #[test]
    fn pipe_cloexec_ends_are_close_on_exec() {
        let (rd, wr) = pipe_cloexec().expect("pipe_cloexec");
        let (rd, wr) = (OwnedFd(rd), OwnedFd(wr));
        for fd in [rd.0, wr.0] {
            // SAFETY: F_GETFD on an fd we own.
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            assert_eq!(flags & libc::FD_CLOEXEC, libc::FD_CLOEXEC);
        }
        // SAFETY: valid fds and a 1-byte buffer.
        unsafe {
            assert_eq!(libc::write(wr.0, b"x".as_ptr().cast(), 1), 1);
            let mut b = [0u8; 1];
            assert_eq!(libc::read(rd.0, b.as_mut_ptr().cast(), 1), 1);
            assert_eq!(libc::fcntl(rd.0, libc::F_GETFL) & libc::O_NONBLOCK, 0);
        }
    }
}
