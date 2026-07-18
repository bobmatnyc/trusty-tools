//! Long-lived, async, piped disclaimed spawn — the macOS implementation
//! behind [`super::super::disclaimed_piped_spawn`] (issue #2997).
//!
//! Why: [`super::capture`]'s `spawn_capture_disclaimed` captures-to-completion
//! and [`super::status`]'s `spawn_status_inherit_disclaimed` inherits stdio —
//! neither fits `control::backend::stream_json::StreamJsonBackend`'s
//! long-lived `claude -p --output-format stream-json` child, which stays
//! alive for the whole session, fed newline-JSON on stdin and emitting
//! newline-JSON on stdout. Because `StreamJsonBackend::spawn` previously used
//! a plain `tokio::process::Command::new("claude")`, it never disclaimed TCC
//! responsibility, so a `claude`-invoked child (e.g. an agent's `cargo
//! build`, whose `libgit2-sys` build script touches `~/Library/CloudStorage`
//! FileProvider paths) had its access request attributed all the way up to
//! the signed `trusty-mpm` binary — reproducing the #2819/#2721 App-Data
//! prompt class on the daemon's default actor-managed session backend.
//! What: [`spawn_piped_disclaimed`] takes an already-built
//! `std::process::Command` (so the caller's argument/env construction, e.g.
//! `build_claude_command`'s §9.1 `ANTHROPIC_API_KEY` removal, is completely
//! untouched), re-derives argv/envp/cwd from it via the stable
//! `get_program`/`get_args`/`get_envs`/`get_current_dir` accessors, and spawns
//! via `posix_spawnp` with three pipes (stdin/stdout/stderr) and the disclaim
//! attribute set (via [`super::resolve_disclaim_fn`]) exactly like
//! [`super::capture`]/[`super::status`]. The parent-held pipe ends are handed
//! to `tokio::net::unix::pipe::{Sender, Receiver}` (which validates each fd is
//! a pipe with the right access mode and sets it non-blocking) so the caller
//! gets normal async `AsyncWrite`/`AsyncRead` handles. The stderr read end is
//! kept open but never drained — matching `StreamJsonBackend`'s pre-existing
//! "captured but never forwarded" behavior — rather than closed (closing it
//! would deliver `SIGPIPE`/`EPIPE` to the child on its next stderr write, a
//! behavior change out of scope for this fix). A `spawn_blocking` task reaps
//! the child via [`super::wait_for`] immediately, so the pid is never left a
//! zombie even if the caller never calls `ChildHandle::wait` — and it is
//! spawned BEFORE the `Sender`/`Receiver::from_owned_fd` pipe conversions
//! (deliberately, not incidentally: `posix_spawnp` has already succeeded by
//! that point, so the child is alive and running regardless of whether those
//! conversions do; spawning the reaper first means a conversion error's `?`
//! early-return still leaves the pid supervised rather than leaking it
//! unsupervised — mirrors [`super::capture`]'s "reap the child
//! UNCONDITIONALLY before propagating any pipe-read error" discipline).
//! Test: `spawn_piped_disclaimed_writes_and_reads_via_cat`,
//! `spawn_piped_disclaimed_preserves_cwd`,
//! `spawn_piped_disclaimed_removes_env_and_keeps_rest`,
//! `spawn_piped_disclaimed_reports_spawn_error_for_missing_binary`,
//! `spawn_piped_disclaimed_kill_and_wait_reaps_child`. The reaper-before-
//! conversion ordering itself has no dedicated test: triggering a real
//! `Sender`/`Receiver::from_owned_fd` failure needs a non-pipe fd or a
//! deliberately-broken one, which this suite has no fault-injection seam
//! for (every path here spawns a live process); the ordering is verified by
//! code inspection instead — the reaper spawn is the last statement before
//! the two conversions run, so it always executes first.

use std::collections::BTreeMap;
use std::ffi::{CString, OsString};
use std::io;
use std::os::fd::{FromRawFd, OwnedFd as StdOwnedFd};
use std::os::unix::ffi::OsStrExt;

use super::super::{ChildHandle, PipedSpawn};
use super::{
    OwnedFd, pipe_cloexec, posix_spawn_file_actions_addchdir_np, resolve_disclaim_fn, wait_for,
};

/// Spawn `cmd` with piped stdin/stdout/stderr and TCC responsibility
/// disclaimed, wiring the parent's pipe ends into async tokio I/O.
///
/// Why: see the module docs.
/// What: see the module docs. Note the reaper is spawned before the
/// `Sender`/`Receiver::from_owned_fd` conversions below, not after — see the
/// module docs for why.
/// Test: see the module docs.
pub(crate) fn spawn_piped_disclaimed(cmd: std::process::Command) -> io::Result<PipedSpawn> {
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

    // Re-derive the child's environment: inherit the parent's, then replay
    // `cmd`'s explicit mutations (Some = set/override, None = env_remove) —
    // this is the path that honors §9.1's `ANTHROPIC_API_KEY` removal, since
    // we bypass `Command`'s own spawn entirely.
    let mut env_map: BTreeMap<OsString, OsString> = std::env::vars_os().collect();
    for (key, val) in cmd.get_envs() {
        match val {
            Some(v) => {
                env_map.insert(key.to_os_string(), v.to_os_string());
            }
            None => {
                env_map.remove(key);
            }
        }
    }
    let mut envp_c: Vec<CString> = Vec::with_capacity(env_map.len());
    for (k, v) in &env_map {
        let mut entry = k.as_bytes().to_vec();
        entry.push(b'=');
        entry.extend_from_slice(v.as_bytes());
        envp_c.push(
            CString::new(entry).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "env entry contains NUL")
            })?,
        );
    }
    let mut envp_ptr: Vec<*mut libc::c_char> = envp_c
        .iter()
        .map(|c| c.as_ptr() as *mut libc::c_char)
        .collect();
    envp_ptr.push(std::ptr::null_mut());

    let cwd_c = cmd
        .get_current_dir()
        .map(|dir| {
            CString::new(dir.as_os_str().as_bytes())
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "cwd contains NUL"))
        })
        .transpose()?;

    // Three pipes: parent writes stdin, parent reads stdout/stderr. The
    // CLOEXEC source fds close at exec; the dup2 targets (0/1/2) survive.
    let (in_rd, in_wr) = pipe_cloexec()?;
    let (out_rd, out_wr) = pipe_cloexec()?;
    let (err_rd, err_wr) = pipe_cloexec()?;
    let in_rd = OwnedFd(in_rd);
    let in_wr = OwnedFd(in_wr);
    let out_rd = OwnedFd(out_rd);
    let out_wr = OwnedFd(out_wr);
    let err_rd = OwnedFd(err_rd);
    let err_wr = OwnedFd(err_wr);

    let pid = {
        // SAFETY: standard posix_spawn setup; all pointers passed are valid
        // for the duration of the call and outlive it (argv_c/envp_c/cwd_c
        // are not dropped until after `posix_spawnp` returns below).
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
            let rc = libc::posix_spawn_file_actions_adddup2(&mut file_actions, in_rd.0, 0);
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

            // Disclaim TCC responsibility when the SPI is available — the fix
            // for #2997: the descendant chain (this `claude` process and
            // everything IT forks) becomes its own responsible unit instead
            // of rolling attribution up to the signed `trusty-mpm` binary.
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

    // Close our copies of the child-side fds; keep the parent-side ends.
    drop(in_rd);
    drop(out_wr);
    drop(err_wr);

    // Reap unconditionally in the background so the pid is never left a
    // zombie — whether or not the caller ever calls `ChildHandle::wait`, AND
    // whether or not the pipe-fd conversions below succeed. `posix_spawnp`
    // above already succeeded, so the child is alive and running from this
    // point on regardless of what happens next; spawning the reaper BEFORE
    // the `Sender`/`Receiver::from_owned_fd` conversions (rather than after,
    // as originally written) means a conversion failure's `?` early-return
    // still leaves this task waiting on `pid` in the background instead of
    // leaking it unsupervised (mirrors `capture::spawn_capture_disclaimed`'s
    // "reap the child UNCONDITIONALLY before propagating any pipe-read
    // error" discipline).
    let reaper = tokio::task::spawn_blocking(move || wait_for(pid));

    // SAFETY: each raw fd below is an owned, open pipe end whose `OwnedFd`
    // guard released it via `into_raw()` immediately beforehand, so there is
    // exactly one owner at all times; wrapping it in `std::os::fd::OwnedFd`
    // hands that ownership to the `tokio::net::unix::pipe` constructor, which
    // validates the fd is actually a pipe with the expected access mode.
    let stdin_owned = unsafe { StdOwnedFd::from_raw_fd(in_wr.into_raw()) };
    let stdout_owned = unsafe { StdOwnedFd::from_raw_fd(out_rd.into_raw()) };
    let stderr_owned = unsafe { StdOwnedFd::from_raw_fd(err_rd.into_raw()) };

    let sender = tokio::net::unix::pipe::Sender::from_owned_fd(stdin_owned)?;
    let receiver = tokio::net::unix::pipe::Receiver::from_owned_fd(stdout_owned)?;

    Ok(PipedSpawn {
        handle: ChildHandle::Disclaimed {
            pid,
            reaper: Some(reaper),
            _stderr: stderr_owned,
        },
        stdin: Box::new(sender),
        stdout: Box::new(receiver),
    })
}

#[cfg(test)]
mod tests {
    use super::super::super::disclaimed_piped_spawn;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    #[tokio::test]
    #[serial_test::serial]
    async fn spawn_piped_disclaimed_writes_and_reads_via_cat() {
        let cmd = std::process::Command::new("/bin/cat");
        let mut spawned = disclaimed_piped_spawn(cmd).unwrap();
        spawned
            .stdin
            .write_all(b"hello disclaimed\n")
            .await
            .unwrap();
        drop(spawned.stdin); // EOF so `cat` exits
        let mut reader = BufReader::new(spawned.stdout);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        assert_eq!(line, "hello disclaimed\n");
        let status = spawned.handle.wait().await.unwrap();
        assert!(status.success());
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn spawn_piped_disclaimed_preserves_cwd() {
        let dir = std::env::temp_dir();
        let mut cmd = std::process::Command::new("/bin/pwd");
        cmd.current_dir(&dir);
        let mut spawned = disclaimed_piped_spawn(cmd).unwrap();
        drop(spawned.stdin);
        let mut reader = BufReader::new(spawned.stdout);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        // macOS's /tmp is a symlink to /private/tmp; pwd resolves the real
        // path, so compare canonicalized paths rather than raw strings.
        let got = std::path::Path::new(line.trim())
            .canonicalize()
            .unwrap_or_else(|_| line.trim().into());
        let want = dir.canonicalize().unwrap_or(dir);
        assert_eq!(got, want);
        spawned.handle.wait().await.unwrap();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn spawn_piped_disclaimed_removes_env_and_keeps_rest() {
        // SAFETY: single-threaded-relative-to-this-test env mutation; no
        // other test reads this specific key.
        unsafe { std::env::set_var("SPAWN_DISCLAIM_PIPED_TEST_KEEP", "kept") };
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.arg("-c").arg("printf '%s\\n' \"$SPAWN_DISCLAIM_PIPED_TEST_KEEP:${SPAWN_DISCLAIM_PIPED_TEST_REMOVE-unset}\"");
        cmd.env("SPAWN_DISCLAIM_PIPED_TEST_REMOVE", "should-not-leak");
        cmd.env_remove("SPAWN_DISCLAIM_PIPED_TEST_REMOVE");
        let mut spawned = disclaimed_piped_spawn(cmd).unwrap();
        drop(spawned.stdin);
        let mut reader = BufReader::new(spawned.stdout);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        unsafe { std::env::remove_var("SPAWN_DISCLAIM_PIPED_TEST_KEEP") };
        assert_eq!(line, "kept:unset\n");
        spawned.handle.wait().await.unwrap();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn spawn_piped_disclaimed_reports_spawn_error_for_missing_binary() {
        let cmd = std::process::Command::new("/nonexistent/definitely-not-a-real-binary-2997");
        // `PipedSpawn` holds `Box<dyn AsyncWrite/AsyncRead>` trait objects, so
        // it isn't `Debug` and can't go through `expect_err`; `.err()` avoids
        // needing a `Debug` bound on the `Ok` side entirely.
        let err = disclaimed_piped_spawn(cmd)
            .err()
            .expect("spawning a missing binary must error, not hang or panic");
        assert!(matches!(
            err.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::Other
        ));
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn spawn_piped_disclaimed_kill_and_wait_reaps_child() {
        let mut cmd = std::process::Command::new("/bin/sleep");
        cmd.arg("30");
        let mut spawned = disclaimed_piped_spawn(cmd).unwrap();
        spawned.handle.start_kill().unwrap();
        let status = spawned.handle.wait().await.unwrap();
        assert!(!status.success());
    }
}
