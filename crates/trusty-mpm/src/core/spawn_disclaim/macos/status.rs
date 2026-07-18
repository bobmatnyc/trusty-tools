//! Inherited-stdio disclaimed spawn — the macOS implementation behind
//! [`super::super::disclaimed_status`] (issue #2997, `tm run`/`tm login`).
//!
//! Why: `standalone::run::build_launch_command`/`build_login_command` (the
//! `tm run <alias>`/`tm login` interactive drivers) spawn `claude` directly
//! via `Command::status()` with stdio inherited from the terminal, bypassing
//! the tmux-hosted disclaim in [`super::capture`]/
//! [`crate::core::tmux::run_tmux_with_bin`] entirely — so this session-launch
//! path still reproduced the #2819/#2721 TCC mis-attribution storm.
//! What: [`spawn_status_inherit_disclaimed`] reconstructs argv from
//! `cmd.get_program()`/`cmd.get_args()`, chdirs to `cmd.get_current_dir()`
//! when set (via [`super::posix_spawn_file_actions_addchdir_np`]), and
//! rebuilds envp as the current process environment with `cmd.get_envs()`'s
//! overrides/removals layered on top — matching what `Command` itself would
//! hand the child. No stdin/stdout/stderr file actions are added, so the
//! child inherits the caller's fds directly (`Stdio::inherit()` semantics —
//! this fn assumes `cmd` was built that way; it does not read back `cmd`'s
//! stdio config, since `std::process::Command` exposes no accessor for it).
//! Sets the disclaim attribute when the private SPI resolves (via
//! [`super::resolve_disclaim_fn`]), spawns via `posix_spawnp`, and reaps the
//! child via [`super::wait_for`].
//! Test: `disclaimed_status_*` in `super::super`'s test module (this module
//! has no tests of its own — see that module's doc for why).

use std::collections::HashMap;
use std::ffi::{CString, OsString};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::process::{Command, ExitStatus};

use super::{posix_spawn_file_actions_addchdir_np, resolve_disclaim_fn, wait_for};

/// Spawn `cmd` with stdio inherited (fds 0/1/2 left untouched) and TCC
/// responsibility disclaimed.
///
/// Why: see the module docs.
/// What: see the module docs.
/// Test: `super::super::tests::disclaimed_status_*`.
pub(crate) fn spawn_status_inherit_disclaimed(cmd: &mut Command) -> io::Result<ExitStatus> {
    let prog_c = CString::new(cmd.get_program().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "program contains NUL"))?;

    let mut argv_c: Vec<CString> = Vec::with_capacity(1);
    argv_c.push(prog_c.clone());
    for a in cmd.get_args() {
        argv_c.push(
            CString::new(a.as_bytes())
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "argument contains NUL"))?,
        );
    }
    let mut argv_ptr: Vec<*mut libc::c_char> = argv_c
        .iter()
        .map(|c| c.as_ptr() as *mut libc::c_char)
        .collect();
    argv_ptr.push(std::ptr::null_mut());

    // envp = current process environment with cmd's explicit overlay
    // applied (Some(v) => set/override, None => remove) — matches what
    // `Command` hands the child when built by inheriting the parent env
    // (no prior `env_clear()`, true of every tm caller of this fn).
    let mut env_map: HashMap<OsString, OsString> = std::env::vars_os().collect();
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
            io::Error::new(io::ErrorKind::InvalidInput, "environment variable contains NUL")
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

    let pid = {
        // SAFETY: standard posix_spawn setup. All pointers passed are
        // valid for the duration of the call (argv_c/envp_c/cwd_c/prog_c
        // outlive it).
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

            if let Some(ref cwd) = cwd_c {
                let rc = posix_spawn_file_actions_addchdir_np(&mut file_actions, cwd.as_ptr());
                if rc != 0 {
                    libc::posix_spawnattr_destroy(&mut attr);
                    libc::posix_spawn_file_actions_destroy(&mut file_actions);
                    return Err(io::Error::from_raw_os_error(rc));
                }
            }

            // Disclaim TCC responsibility when the SPI is available.
            if let Some(disclaim) = resolve_disclaim_fn() {
                // A non-zero return here is non-fatal: worst case the
                // child is not disclaimed (today's behaviour), so we
                // ignore it.
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

    wait_for(pid)
}
