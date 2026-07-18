//! Capture-to-completion disclaimed spawn — the macOS implementation behind
//! [`super::super::disclaimed_output`].
//!
//! Why: `run_tmux_with_bin`'s tmux commands are all "spawn, capture stdout +
//! stderr, wait" shaped, so [`spawn_capture_disclaimed`] mirrors
//! `Command::output()`'s contract exactly, just with TCC responsibility
//! disclaimed on the child.
//! What: [`spawn_capture_disclaimed`] creates two `CLOEXEC` pipes, builds
//! posix_spawn file actions (stdin←/dev/null, stdout→pipe, stderr→pipe), sets
//! the disclaim attribute when the SPI is available (via
//! [`super::resolve_disclaim_fn`]), spawns via `posix_spawnp` (PATH search,
//! like `Command`), drains both pipes concurrently to avoid the classic
//! large-output deadlock, and reaps the child via [`super::wait_for`]. Any
//! pre-spawn failure surfaces as an `io::Error` exactly as `Command::output`
//! would; the child is never spawned more than once.
//! Test: `disclaimed_output_*` in `super::super`'s test module (this module
//! has no tests of its own — see that module's doc for why).

use std::ffi::CString;
use std::io::{self, Read};
use std::os::unix::io::FromRawFd;
use std::process::Output;

use super::{OwnedFd, environ, pipe_cloexec, resolve_disclaim_fn, wait_for};

/// Spawn with stdout/stderr captured and TCC responsibility disclaimed.
///
/// Why: see the module docs.
/// What: see the module docs.
/// Test: `super::super::tests::disclaimed_output_*`.
pub(crate) fn spawn_capture_disclaimed(program: &str, args: &[String]) -> io::Result<Output> {
    let prog_c = CString::new(program)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "program contains NUL"))?;
    let mut argv_c: Vec<CString> = Vec::with_capacity(args.len() + 1);
    argv_c.push(prog_c.clone());
    for a in args {
        argv_c.push(
            CString::new(a.as_str()).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "argument contains NUL")
            })?,
        );
    }
    // Null-terminated `char *const []`. posix_spawn does not mutate these.
    let mut argv_ptr: Vec<*mut libc::c_char> = argv_c
        .iter()
        .map(|c| c.as_ptr() as *mut libc::c_char)
        .collect();
    argv_ptr.push(std::ptr::null_mut());

    let (out_rd, out_wr) = pipe_cloexec()?;
    let (err_rd, err_wr) = pipe_cloexec()?;
    // Ensure every fd is accounted for even on the error paths below.
    let mut out_rd = OwnedFd(out_rd);
    let out_wr = OwnedFd(out_wr);
    let mut err_rd = OwnedFd(err_rd);
    let err_wr = OwnedFd(err_wr);

    let dev_null = c"/dev/null";
    let pid = {
        // SAFETY: standard posix_spawn setup. All pointers passed are valid
        // for the duration of the call; `argv_c`/`prog_c` outlive it.
        unsafe {
            let mut file_actions: libc::posix_spawn_file_actions_t = std::mem::zeroed();
            let mut attr: libc::posix_spawnattr_t = std::mem::zeroed();
            if libc::posix_spawn_file_actions_init(&mut file_actions) != 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::posix_spawnattr_init(&mut attr) != 0 {
                libc::posix_spawn_file_actions_destroy(&mut file_actions);
                return Err(io::Error::last_os_error());
            }

            // stdin ← /dev/null; stdout → out_wr (fd 1); stderr → err_wr (fd 2).
            // The dup2 targets (0/1/2) are created without CLOEXEC (dup2
            // clears it), so they survive exec; the CLOEXEC source pipe fds
            // close at exec, so the child holds no extra copies. Each call's
            // return code is checked: a non-zero rc means the file-actions
            // list is malformed (e.g. an invalid fd), so `posix_spawnp` would
            // either exec with the wrong fds wired up or fail outright — we
            // must not proceed to spawn on any of these.
            let rc = libc::posix_spawn_file_actions_addopen(
                &mut file_actions,
                0,
                dev_null.as_ptr(),
                libc::O_RDONLY,
                0,
            );
            if rc != 0 {
                libc::posix_spawnattr_destroy(&mut attr);
                libc::posix_spawn_file_actions_destroy(&mut file_actions);
                return Err(io::Error::from_raw_os_error(rc));
            }
            let rc = libc::posix_spawn_file_actions_adddup2(&mut file_actions, out_wr.0, 1);
            if rc != 0 {
                libc::posix_spawnattr_destroy(&mut attr);
                libc::posix_spawn_file_actions_destroy(&mut file_actions);
                return Err(io::Error::from_raw_os_error(rc));
            }
            let rc = libc::posix_spawn_file_actions_adddup2(&mut file_actions, err_wr.0, 2);
            if rc != 0 {
                libc::posix_spawnattr_destroy(&mut attr);
                libc::posix_spawn_file_actions_destroy(&mut file_actions);
                return Err(io::Error::from_raw_os_error(rc));
            }

            // Disclaim TCC responsibility when the SPI is available.
            if let Some(disclaim) = resolve_disclaim_fn() {
                // A non-zero return here is non-fatal: worst case the child
                // is not disclaimed (today's behaviour), so we ignore it.
                let _ = disclaim(&mut attr, 1);
            }

            let mut pid: libc::pid_t = 0;
            let rc = libc::posix_spawnp(
                &mut pid,
                prog_c.as_ptr(),
                &file_actions,
                &attr,
                argv_ptr.as_ptr(),
                environ,
            );
            libc::posix_spawnattr_destroy(&mut attr);
            libc::posix_spawn_file_actions_destroy(&mut file_actions);
            if rc != 0 {
                // Mirrors `Command::output`'s "binary not found" error shape.
                return Err(io::Error::from_raw_os_error(rc));
            }
            pid
        }
    };

    // Close our copies of the write ends so the reads see EOF once the child
    // (and any process that inherited fd 1/2) exits.
    drop(out_wr);
    drop(err_wr);

    // Drain both pipes concurrently — reading them sequentially would
    // deadlock whenever the child fills one pipe buffer while we block on the
    // other (tmux `capture-pane` can emit 100k lines of scrollback).
    let out_fd = std::mem::replace(&mut out_rd, OwnedFd(-1)).into_raw();
    let reader = std::thread::spawn(move || {
        // SAFETY: `out_fd` is an owned, open pipe read end handed to this
        // File, which closes it on drop. Nothing else touches it.
        let mut f = unsafe { std::fs::File::from_raw_fd(out_fd) };
        let mut buf = Vec::new();
        let _ = f.read_to_end(&mut buf);
        buf
    });

    let err_fd = std::mem::replace(&mut err_rd, OwnedFd(-1)).into_raw();
    // SAFETY: `err_fd` is an owned, open pipe read end; File closes it on drop.
    let mut err_file = unsafe { std::fs::File::from_raw_fd(err_fd) };
    let mut stderr_buf = Vec::new();
    let err_read = err_file.read_to_end(&mut stderr_buf);

    let stdout_buf = reader.join().unwrap_or_default();

    // Reap the child UNCONDITIONALLY before propagating any pipe-read
    // error. `err_read?` returning early here — before the child is
    // waited on — would leak a zombie in this long-lived daemon on every
    // stderr-read failure, since nothing else ever reaps this pid.
    let status = wait_for(pid);
    err_read?;
    let status = status?;
    Ok(Output {
        status,
        stdout: stdout_buf,
        stderr: stderr_buf,
    })
}
