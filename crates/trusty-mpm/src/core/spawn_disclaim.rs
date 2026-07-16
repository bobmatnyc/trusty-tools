//! macOS TCC "responsible process" disclaim for the children tm spawns.
//!
//! Why: trusty-mpm launches every managed agent as a `claude` process hosted
//! inside a `tmux` session ([`crate::core::tmux`]). On macOS the consent system
//! (TCC) attributes a child's access request to its *responsible process*, which
//! it resolves by walking the spawn chain up to the first ancestor that took
//! responsibility. Because the signed `trusty-mpm` daemon forks the tmux server
//! — and thus every `claude`/agent descendant — WITHOUT disclaiming, that
//! responsible process resolves back to `trusty-mpm` itself. So when an agent
//! (or a shell it runs) touches the Apple Music / media library
//! (`kTCCServiceMediaLibrary`, e.g. anything under `~/Music/Music`), macOS pops
//! "trusty-mpm would like to access Apple Music, your music and video activity,
//! and your media library" — repeatedly, one storm per requesting child, because
//! each transient child has no stable code identity for TCC to remember the
//! decision against (issue #2819). tm's own filesystem traversal is already
//! fenced off from those paths ([`crate::core::protected_dirs`], #2760); this
//! remaining prompt class is child-initiated access mis-attributed to the signed
//! parent. The disclaim is service-agnostic: the same rollup also drives the
//! "trusty-mpm would like to access data from other apps" App-Data prompt
//! (`kTCCServiceSystemPolicyAppData`) when a child reads another app's
//! `~/Library/Containers` / `~/Library/Application Support` data (#2721), so
//! disclaiming responsibility fixes every child-initiated prompt class at once,
//! not just the media library. The installed binary is correctly Developer-ID
//! signed with a stable identifier-anchored designated requirement, so these
//! recurrences are NOT signing drift — they are the attribution rollup.
//! What: [`disclaimed_output`] spawns a command exactly like
//! `Command::new(program).args(args).output()` but, on macOS, sets the
//! `responsibility_spawnattrs_setdisclaim` posix_spawn attribute so the child —
//! and everything it forks, including the persistent tmux server — becomes its
//! OWN responsible process. The consent dialog then names the actual requester
//! (`tmux`/`claude`) with a stable code identity TCC can remember, instead of the
//! flagship binary, so the storm stops after a single per-child decision. The
//! private SPI is resolved with `dlsym` at call time: if it is ever absent the
//! spawn proceeds normally (no disclaim, no regression, no re-run). Setting
//! `TM_DISABLE_SPAWN_DISCLAIM=1` forces the plain path as an operational safety
//! valve. On non-macOS this is a thin pass-through to `Command::output`.
//!
//! Known limitation: tmux uses one shared server. The disclaim takes effect only
//! when *tm* forks the server; if a server was already running (e.g. the user's
//! own `tmux`), `new-session -A` attaches to it and that server's responsibility
//! is unchanged until the next server fork (`tmux kill-server` / reboot).
//!
//! Scope: this fixes the **tmux-hosted managed-session path**
//! ([`crate::core::tmux::run_tmux_with_bin`]) — the dominant fleet path where
//! every `tm session create`/daemon-managed `claude` runs. Two other spawn
//! shapes still call `claude` directly and bypass this disclaim, so they can
//! still reproduce the prompt storm: `standalone::run::build_launch_command`
//! (the interactive `tm run <alias>` driver, `Stdio::inherit()`) and
//! `control::backend::stream_json::build_claude_command` (the alpha-1 default
//! `StreamJsonBackend`, `tokio::process` piped I/O). Extending the disclaim to
//! those two shapes is tracked in issue #2822 rather than folded into this
//! module, since the interactive-inherit and tokio-piped spawn shapes need
//! their own posix_spawn-based reimplementation (a `pre_exec` shortcut was
//! considered and is NOT confirmed feasible — see #2822 for why).
//! Test: `disclaimed_output_captures_stdout`,
//! `disclaimed_output_captures_stderr_and_nonzero_exit`,
//! `disclaimed_output_saturates_stdout_and_stderr_without_deadlock`,
//! `disclaimed_output_reports_spawn_error_for_missing_binary`.

/// Environment variable that, when set to any value, forces
/// [`disclaimed_output`] onto the plain (non-disclaimed) `Command::output`
/// path. Operational safety valve for the private-SPI-based spawn.
pub const DISABLE_ENV: &str = "TM_DISABLE_SPAWN_DISCLAIM";

/// Spawn `program` with `args`, capture stdout/stderr, and — on macOS —
/// disclaim TCC responsibility so the child is its own responsible process.
///
/// Why: see the module docs — this is tm's fix for the media-library/App-Data
/// consent storm (issue #2819) on the tmux-hosted managed-session path, which
/// arises because a `claude`/`tmux` child's TCC access is otherwise attributed
/// to the signed `trusty-mpm` binary.
/// What: behaves like `Command::new(program).args(args).output()` (stdin from
/// `/dev/null`, stdout+stderr captured), but on macOS spawns via `posix_spawnp`
/// with the `responsibility_spawnattrs_setdisclaim` attribute set. There is
/// exactly ONE spawn regardless of whether the disclaim SPI is present, so a
/// missing SPI never causes a double-run. On non-macOS it delegates straight to
/// `Command::output`.
/// Test: `disclaimed_output_captures_stdout`,
/// `disclaimed_output_captures_stderr_and_nonzero_exit`,
/// `disclaimed_output_saturates_stdout_and_stderr_without_deadlock`,
/// `disclaimed_output_reports_spawn_error_for_missing_binary`.
pub fn disclaimed_output(program: &str, args: &[String]) -> std::io::Result<std::process::Output> {
    #[cfg(target_os = "macos")]
    {
        if std::env::var_os(DISABLE_ENV).is_none() {
            return macos::spawn_capture_disclaimed(program, args);
        }
    }
    std::process::Command::new(program).args(args).output()
}

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::CString;
    use std::io::{self, Read};
    use std::os::unix::io::FromRawFd;
    use std::os::unix::process::ExitStatusExt;
    use std::process::{ExitStatus, Output};

    // The current process environment block, passed through to the child so it
    // inherits our environment exactly as `Command` would.
    unsafe extern "C" {
        static environ: *const *mut libc::c_char;
    }

    // Signature of the private libSystem SPI that marks a posix_spawn attribute
    // set as "disclaim responsibility". Resolved via `dlsym` so its absence on
    // any future macOS degrades gracefully to a normal spawn.
    type DisclaimFn =
        unsafe extern "C" fn(*mut libc::posix_spawnattr_t, libc::c_int) -> libc::c_int;

    /// Resolve `responsibility_spawnattrs_setdisclaim` at runtime, or `None`.
    ///
    /// Why: the symbol is a private SPI; linking it directly would make the
    /// whole binary fail to load if it ever disappeared. `dlsym` turns that into
    /// a graceful "spawn without disclaim" fallback.
    /// What: looks the symbol up in the global namespace (it lives in libSystem,
    /// always loaded) and transmutes it to a typed fn pointer.
    /// Test: covered indirectly — the capture tests exercise the whole spawn
    /// path; this returns `Some` on any TCC-era macOS.
    fn resolve_disclaim_fn() -> Option<DisclaimFn> {
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

    /// Spawn with stdout/stderr captured and TCC responsibility disclaimed.
    ///
    /// Why: the single macOS implementation behind [`super::disclaimed_output`].
    /// What: creates two `CLOEXEC` pipes, builds posix_spawn file actions
    /// (stdin←/dev/null, stdout→pipe, stderr→pipe), sets the disclaim attribute
    /// when the SPI is available, spawns via `posix_spawnp` (PATH search, like
    /// `Command`), drains both pipes concurrently to avoid the classic
    /// large-output deadlock, and reaps the child. Any pre-spawn failure surfaces
    /// as an `io::Error` exactly as `Command::output` would; the child is never
    /// spawned more than once.
    /// Test: `super::tests::disclaimed_output_*`.
    pub(super) fn spawn_capture_disclaimed(program: &str, args: &[String]) -> io::Result<Output> {
        let prog_c = CString::new(program)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "program contains NUL"))?;
        let mut argv_c: Vec<CString> = Vec::with_capacity(args.len() + 1);
        argv_c.push(prog_c.clone());
        for a in args {
            argv_c.push(CString::new(a.as_str()).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "argument contains NUL")
            })?);
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

    /// Reap `pid`, returning its `ExitStatus`.
    ///
    /// Why: a spawned child must be waited on to avoid a zombie and to learn its
    /// exit status.
    /// What: loops over `waitpid` through `EINTR`, converting the raw wait status
    /// into an `ExitStatus`.
    /// Test: exercised by every `disclaimed_output_*` capture test.
    fn wait_for(pid: libc::pid_t) -> io::Result<ExitStatus> {
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
    /// Why: the read ends must not leak into unrelated spawns; the write ends are
    /// re-established onto fd 1/2 by dup2 (which clears CLOEXEC on the target) so
    /// marking the sources CLOEXEC means the child keeps only fd 1/2, no strays.
    /// What: `pipe()` then `fcntl(F_SETFD, FD_CLOEXEC)` on each end.
    /// Test: exercised by the capture tests (correct EOF/no-leak behaviour).
    fn pipe_cloexec() -> io::Result<(libc::c_int, libc::c_int)> {
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
    /// Why: the spawn setup has several fallible steps; an RAII guard guarantees
    /// no pipe fd leaks on an early return without hand-writing close on every
    /// error path.
    /// What: wraps a `c_int`; `into_raw` releases ownership (sets the guard to
    /// `-1`) when the fd is handed to a `File`.
    /// Test: exercised by the capture tests.
    struct OwnedFd(libc::c_int);

    impl OwnedFd {
        fn into_raw(mut self) -> libc::c_int {
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
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn disclaimed_output_captures_stdout() {
        let out = disclaimed_output("/bin/echo", &["hello world".to_string()]).unwrap();
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout), "hello world\n");
        assert!(out.stderr.is_empty());
    }

    #[test]
    fn disclaimed_output_captures_stderr_and_nonzero_exit() {
        // `sh -c 'echo boom >&2; exit 3'` → stderr captured, exit code 3.
        let out = disclaimed_output(
            "/bin/sh",
            &["-c".to_string(), "echo boom >&2; exit 3".to_string()],
        )
        .unwrap();
        assert!(!out.status.success());
        assert_eq!(out.status.code(), Some(3));
        assert_eq!(String::from_utf8_lossy(&out.stderr), "boom\n");
        assert!(out.stdout.is_empty());
    }

    #[test]
    fn disclaimed_output_saturates_stdout_and_stderr_without_deadlock() {
        // Fill BOTH stdout and stderr with ~2 MiB CONCURRENTLY (the stdout
        // producer is backgrounded so it races the stderr producer) — this is
        // the shape that actually stresses the concurrent-drain design.
        // Draining stdout to completion before touching stderr (or vice versa)
        // would deadlock as soon as the other pipe's kernel buffer fills while
        // its producer keeps writing; a single-stream test can't catch that.
        let out = disclaimed_output(
            "/bin/sh",
            &[
                "-c".to_string(),
                "head -c 2097152 /dev/zero | tr '\\0' 'x' & \
                 head -c 2097152 /dev/zero | tr '\\0' 'y' >&2; \
                 wait"
                    .to_string(),
            ],
        )
        .unwrap();
        assert!(out.status.success());
        assert_eq!(out.stdout.len(), 2_097_152);
        assert!(out.stdout.iter().all(|&b| b == b'x'));
        assert_eq!(out.stderr.len(), 2_097_152);
        assert!(out.stderr.iter().all(|&b| b == b'y'));
    }

    #[test]
    fn disclaimed_output_reports_spawn_error_for_missing_binary() {
        let err = disclaimed_output("/nonexistent/definitely-not-a-real-binary-2819", &[])
            .expect_err("spawning a missing binary must error, not hang or panic");
        // Same class of error `Command::output` yields for a missing program.
        assert!(matches!(
            err.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::Other
        ));
    }

    #[test]
    fn disable_env_forces_plain_path_still_captures() {
        // With the safety valve set we go through Command::output; behaviour
        // (captured stdout) must be identical.
        // SAFETY: single-threaded test setup around the env toggle.
        unsafe { std::env::set_var(DISABLE_ENV, "1") };
        let out = disclaimed_output("/bin/echo", &["valve".to_string()]).unwrap();
        unsafe { std::env::remove_var(DISABLE_ENV) };
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout), "valve\n");
    }
}
