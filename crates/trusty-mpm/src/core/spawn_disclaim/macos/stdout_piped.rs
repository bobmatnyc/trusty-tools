//! Synchronous piped-stdout disclaimed spawn — the macOS implementation
//! behind [`super::super::disclaimed_stdout_piped_spawn`] (issue #3267,
//! #2997 part 6).
//!
//! Why: `formatters::info_box::probes::run_git_log` (in the `tm` binary)
//! spawns `git log` and arms a 3-second SIGKILL watchdog on the child's pid
//! BEFORE blocking on a read-to-EOF-then-wait, so it needs the pid available
//! immediately after spawn — [`super::capture`]'s capture-to-completion
//! contract only returns once the child has already exited, exposing no pid
//! to arm the watchdog with. Its raw `Command::new("git")...spawn()` call
//! previously did not disclaim, reproducing the same #2819/#2721/#2997
//! mis-attribution on the welcome-panel git-log probe path.
//! What: [`spawn_stdout_piped_disclaimed`] re-derives argv/envp/cwd from the
//! given `Command` via the same stable accessors [`super::status`] uses,
//! builds posix_spawn file actions (stdout → a pipe, stderr → `/dev/null`,
//! stdin left untouched — inherited, matching the pre-existing `Command`'s
//! implicit default), sets the disclaim attribute when the private SPI
//! resolves, and spawns via `posix_spawnp`. The pid and the parent-held
//! stdout pipe read end (wrapped in a plain synchronous `std::fs::File`) are
//! returned immediately, UNWAITED — the caller arms its watchdog on the pid,
//! then reads stdout to EOF and reaps via
//! [`super::super::StdoutPipedSpawn::wait_with_output`].
//! Test: `super::super::tests::disclaimed_stdout_piped_spawn_*` (macOS-only);
//! `super::super::piped_native_tests::disclaimed_stdout_piped_spawn_native_path_round_trips`
//! (any OS).

use std::ffi::{CString, OsString};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::FromRawFd;
use std::process::Command;

use super::super::stdout_piped::{StdoutPipedHandle, StdoutPipedSpawn};
use super::{OwnedFd, pipe_cloexec, posix_spawn_file_actions_addchdir_np, resolve_disclaim_fn};

/// Spawn `cmd` with stdout piped, stderr discarded, stdin inherited, and TCC
/// responsibility disclaimed — returns immediately, unwaited.
///
/// Why/What: see the module docs.
/// Test: `super::super::tests::disclaimed_stdout_piped_spawn_*`.
pub(crate) fn spawn_stdout_piped_disclaimed(cmd: &Command) -> io::Result<StdoutPipedSpawn> {
    let prog_c = CString::new(cmd.get_program().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "program contains NUL"))?;
    let mut argv_c: Vec<CString> = vec![prog_c.clone()];
    for a in cmd.get_args() {
        argv_c.push(
            CString::new(a.as_bytes()).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "argument contains NUL")
            })?,
        );
    }
    let mut argv_ptr: Vec<*mut libc::c_char> = argv_c
        .iter()
        .map(|c| c.as_ptr() as *mut libc::c_char)
        .collect();
    argv_ptr.push(std::ptr::null_mut());

    // envp = current process environment with cmd's explicit overlay applied
    // — matches what `Command` hands the child by default.
    let mut env_map: std::collections::BTreeMap<OsString, OsString> = std::env::vars_os().collect();
    for (k, v) in cmd.get_envs() {
        match v {
            Some(val) => {
                env_map.insert(k.to_os_string(), val.to_os_string());
            }
            None => {
                env_map.remove(k);
            }
        }
    }
    let mut envp_c: Vec<CString> = Vec::with_capacity(env_map.len());
    for (k, v) in &env_map {
        let mut buf = Vec::with_capacity(k.as_bytes().len() + v.as_bytes().len() + 1);
        buf.extend_from_slice(k.as_bytes());
        buf.push(b'=');
        buf.extend_from_slice(v.as_bytes());
        envp_c.push(CString::new(buf).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "environment variable contains NUL",
            )
        })?);
    }
    let mut envp_ptr: Vec<*mut libc::c_char> = envp_c
        .iter()
        .map(|c| c.as_ptr() as *mut libc::c_char)
        .collect();
    envp_ptr.push(std::ptr::null_mut());

    let cwd_c = cmd
        .get_current_dir()
        .map(|p| {
            CString::new(p.as_os_str().as_bytes())
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "cwd contains NUL"))
        })
        .transpose()?;

    let (out_rd, out_wr) = pipe_cloexec()?;
    let mut out_rd = OwnedFd(out_rd);
    let out_wr = OwnedFd(out_wr);

    let dev_null = c"/dev/null";
    let pid = {
        // SAFETY: standard posix_spawn setup. All pointers passed are valid
        // for the duration of the call (argv_c/envp_c/cwd_c/prog_c outlive
        // it).
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

            if let Some(dir_c) = &cwd_c {
                let rc = posix_spawn_file_actions_addchdir_np(&mut file_actions, dir_c.as_ptr());
                if rc != 0 {
                    libc::posix_spawnattr_destroy(&mut attr);
                    libc::posix_spawn_file_actions_destroy(&mut file_actions);
                    return Err(io::Error::from_raw_os_error(rc));
                }
            }

            // stdout → pipe (matches `cmd.stdout(Stdio::piped())`).
            let rc = libc::posix_spawn_file_actions_adddup2(&mut file_actions, out_wr.0, 1);
            if rc != 0 {
                libc::posix_spawnattr_destroy(&mut attr);
                libc::posix_spawn_file_actions_destroy(&mut file_actions);
                return Err(io::Error::from_raw_os_error(rc));
            }
            // stderr → /dev/null (matches `cmd.stderr(Stdio::null())`). No
            // stdin action: fd 0 is left untouched, i.e. inherited.
            let rc = libc::posix_spawn_file_actions_addopen(
                &mut file_actions,
                2,
                dev_null.as_ptr(),
                libc::O_WRONLY,
                0,
            );
            if rc != 0 {
                libc::posix_spawnattr_destroy(&mut attr);
                libc::posix_spawn_file_actions_destroy(&mut file_actions);
                return Err(io::Error::from_raw_os_error(rc));
            }

            // Disclaim TCC responsibility when the SPI is available.
            if let Some(disclaim) = resolve_disclaim_fn() {
                let _ = disclaim(&mut attr, 1);
            }

            let mut pid: libc::pid_t = 0;
            let rc = libc::posix_spawnp(
                &mut pid,
                prog_c.as_ptr(),
                &file_actions,
                &attr,
                argv_ptr.as_ptr(),
                envp_ptr.as_ptr(),
            );
            libc::posix_spawnattr_destroy(&mut attr);
            libc::posix_spawn_file_actions_destroy(&mut file_actions);
            if rc != 0 {
                return Err(io::Error::from_raw_os_error(rc));
            }
            pid
        }
    };

    // Close our copy of the write end so the read end sees EOF once the
    // child (and anything that inherited fd 1) exits.
    drop(out_wr);

    let out_fd = std::mem::replace(&mut out_rd, OwnedFd(-1)).into_raw();
    // SAFETY: `out_fd` is an owned, open pipe read end; File closes it on
    // drop. Nothing else touches it after this point.
    let out_file = unsafe { std::fs::File::from_raw_fd(out_fd) };

    Ok(StdoutPipedSpawn {
        id: pid as u32,
        stdout: Box::new(out_file),
        handle: StdoutPipedHandle::Disclaimed(pid),
    })
}
