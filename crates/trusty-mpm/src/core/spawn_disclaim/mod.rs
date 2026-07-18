//! macOS TCC "responsible process" disclaim for the children tm spawns.
//!
//! Why: trusty-mpm launches every managed agent as a `claude` process — via a
//! `tmux` session ([`crate::core::tmux`]), as an inherited-stdio attended
//! process (`tm run`/`tm login`), or, for the daemon's default actor-managed
//! session backend, as a direct long-lived piped child
//! ([`crate::control::backend::stream_json`]). On macOS the consent system
//! (TCC) attributes a child's access request to its *responsible process*,
//! which it resolves by walking the spawn chain up to the first ancestor that
//! took responsibility. Because the signed `trusty-mpm` daemon forked all
//! three of these WITHOUT disclaiming, that responsible process resolved back
//! to `trusty-mpm` itself. So when an agent (or a shell/build-script it runs,
//! e.g. a `cargo build` touching `~/Library/CloudStorage`) touches a
//! TCC-protected resource, macOS pops "trusty-mpm would like to access data
//! from other apps" / "…Apple Music…" — repeatedly, one storm per requesting
//! child, because each transient child has no stable code identity for TCC to
//! remember the decision against (issues #2819, #2721, #2997). tm's own
//! filesystem traversal is already fenced off from those paths
//! ([`crate::core::protected_dirs`], #2760); this remaining prompt class is
//! child-initiated access mis-attributed to the signed parent.
//! What: [`disclaimed_output`] spawns a command exactly like
//! `Command::new(program).args(args).output()` but, on macOS, sets the
//! `responsibility_spawnattrs_setdisclaim` posix_spawn attribute so the child
//! — and everything it forks, including the persistent tmux server — becomes
//! its OWN responsible process (used by the tmux-hosted managed-session path,
//! [`crate::core::tmux::run_tmux_with_bin`]). [`disclaimed_status`] does the
//! same for an inherited-stdio child (used by `tm run`/`tm login`).
//! [`disclaimed_piped_spawn`] does the same for a long-lived, piped-I/O child
//! (used by [`crate::control::backend::stream_json::StreamJsonBackend`]'s
//! default actor-managed session — issue #2997's fix). All three resolve the
//! private SPI with `dlsym` at call time: if it is ever absent the spawn
//! proceeds normally (no disclaim, no regression, no re-run). Setting
//! `TM_DISABLE_SPAWN_DISCLAIM=1` forces the plain path for any of them as an
//! operational safety valve. On non-macOS all three are a thin pass-through
//! to the standard library's own spawn (`Command::output` / `Command::status`
//! / `tokio::process::Command::spawn`).
//!
//! Known limitation: tmux uses one shared server. The disclaim takes effect
//! only when *tm* forks the server; if a server was already running (e.g. the
//! user's own `tmux`), `new-session -A` attaches to it and that server's
//! responsibility is unchanged until the next server fork (`tmux
//! kill-server` / reboot).
//!
//! Scope: as of #2997 this covers the tmux-hosted managed-session path
//! ([`crate::core::tmux::run_tmux_with_bin`]), the `tm run`/`tm login`
//! inherited-stdio path (`standalone::run::build_launch_command` /
//! `build_login_command`), AND the daemon's default actor-managed session
//! backend ([`crate::control::backend::stream_json::StreamJsonBackend`]) —
//! every known `claude`-spawning call site in the crate now disclaims.
//! Test: `disclaimed_output_captures_stdout`,
//! `disclaimed_output_captures_stderr_and_nonzero_exit`,
//! `disclaimed_output_saturates_stdout_and_stderr_without_deadlock`,
//! `disclaimed_output_reports_spawn_error_for_missing_binary`,
//! `disclaimed_status_inherits_and_reports_exit_code`,
//! `disclaimed_status_applies_cwd_and_env_override`,
//! `disclaimed_status_reports_spawn_error_for_missing_binary` (all macOS-only,
//! in this module's `tests`); `spawn_piped_disclaimed_writes_and_reads_via_cat`
//! and its siblings in `macos::piped` (piped path, macOS-only);
//! `disclaimed_piped_spawn_native_path_round_trips` (piped path, any OS —
//! exercises the exact code path Linux always takes).

/// Environment variable that, when set to any value, forces
/// [`disclaimed_output`]/[`disclaimed_status`]/[`disclaimed_piped_spawn`]
/// onto the plain (non-disclaimed) spawn path. Operational safety valve for
/// the private-SPI-based spawn.
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

/// Spawn an already-built `Command` connected to the current process's
/// stdin/stdout/stderr (i.e. one built with `Stdio::inherit()` on all three,
/// the shape `tm run`/`tm login` use for attended sessions) and — on macOS —
/// disclaim TCC responsibility exactly like [`disclaimed_output`]. Returns
/// once the child exits, mirroring `Command::status()`.
///
/// Why: `standalone::run::build_launch_command`/`build_login_command` (the
/// `tm run <alias>`/`tm login` interactive drivers) spawn `claude` directly
/// via `Command::status()`, bypassing the tmux-hosted disclaim in
/// [`disclaimed_output`]/[`crate::core::tmux::run_tmux_with_bin`] entirely —
/// so this session-launch path still reproduced the #2819/#2721 TCC
/// mis-attribution storm (issue #2997). This routes it through the same
/// disclaim.
/// What: reconstructs the program, args, working directory, and environment
/// overrides from the already-built `cmd` via `Command`'s public accessors
/// (`get_program`/`get_args`/`get_current_dir`/`get_envs`) — so callers keep
/// building and unit-testing plain `Command`s exactly as before (see
/// `standalone::run::build_launch_command`'s own tests) — then, on macOS,
/// spawns via `posix_spawnp` with NO stdio file actions (fds 0/1/2 pass
/// through unmodified: `Stdio::inherit()` semantics), a `chdir` file action
/// when `cmd` has a working directory set, an environment block equal to the
/// current process environment with `cmd`'s env overlay applied (same
/// override/remove semantics `Command::get_envs` documents — this does NOT
/// support a prior `Command::env_clear()`, which none of tm's callers use),
/// and the disclaim posix_spawn attribute when the private SPI resolves. On
/// non-macOS, or when `TM_DISABLE_SPAWN_DISCLAIM` is set, delegates straight
/// to `cmd.status()`.
/// Test: `disclaimed_status_inherits_and_reports_exit_code`,
/// `disclaimed_status_applies_cwd_and_env_override`,
/// `disclaimed_status_reports_spawn_error_for_missing_binary`,
/// `disclaimed_status_disable_env_forces_plain_path`.
pub fn disclaimed_status(
    cmd: &mut std::process::Command,
) -> std::io::Result<std::process::ExitStatus> {
    #[cfg(target_os = "macos")]
    {
        if std::env::var_os(DISABLE_ENV).is_none() {
            return macos::spawn_status_inherit_disclaimed(cmd);
        }
    }
    cmd.status()
}

/// Trait-object alias for a spawned child's writable stdin, uniform across
/// the native (`tokio::process::ChildStdin`) and macOS-disclaimed
/// (`tokio::net::unix::pipe::Sender`) spawn paths so callers ([`PipedSpawn`])
/// don't need to match on which path produced the child.
pub type DynChildStdin = Box<dyn tokio::io::AsyncWrite + Unpin + Send>;

/// Trait-object alias for a spawned child's readable stdout — see
/// [`DynChildStdin`].
pub type DynChildStdout = Box<dyn tokio::io::AsyncRead + Unpin + Send>;

/// A handle to a spawned child's lifecycle (kill/wait), uniform across the
/// native and macOS-disclaimed spawn paths.
///
/// Why: [`StreamJsonBackend`](crate::control::backend::stream_json::StreamJsonBackend)
/// needs `start_kill`/`wait` regardless of which path spawned the child; this
/// enum keeps that call surface identical to `tokio::process::Child`'s own,
/// so callers migrating from a bare `tokio::process::Child` change only the
/// type name.
/// What: `Native` wraps a real `tokio::process::Child` (used on non-macOS,
/// and on macOS when [`DISABLE_ENV`] forces the fallback). `Disclaimed`
/// (macOS only) holds the raw pid, a `spawn_blocking` reaper task (spawned
/// eagerly at spawn time so the child is reaped even if `wait` is never
/// called), and the still-open-but-undrained stderr read end (kept alive
/// only for RAII — matches `StreamJsonBackend`'s pre-existing "captured but
/// never forwarded" behavior).
/// Test: exercised by every `spawn_piped_disclaimed_*` test (macOS) and
/// `disclaimed_piped_spawn_native_path_round_trips` (any OS).
pub enum ChildHandle {
    /// The plain `tokio::process`-spawned child (non-macOS, or the
    /// [`DISABLE_ENV`] escape hatch).
    Native(tokio::process::Child),
    /// The macOS posix_spawn-based, TCC-disclaimed child.
    #[cfg(target_os = "macos")]
    Disclaimed {
        pid: libc::pid_t,
        reaper: Option<tokio::task::JoinHandle<std::io::Result<std::process::ExitStatus>>>,
        _stderr: std::os::fd::OwnedFd,
    },
}

impl ChildHandle {
    /// Send SIGKILL to the child (non-blocking), mirroring
    /// `tokio::process::Child::start_kill`.
    ///
    /// Why/What: see [`ChildHandle`]'s docs.
    /// Test: `spawn_piped_disclaimed_kill_and_wait_reaps_child`,
    /// `disclaimed_piped_spawn_native_path_round_trips`.
    pub fn start_kill(&mut self) -> std::io::Result<()> {
        match self {
            ChildHandle::Native(child) => child.start_kill(),
            #[cfg(target_os = "macos")]
            ChildHandle::Disclaimed { pid, .. } => {
                // SAFETY: `pid` is our own spawned child; SIGKILL is always a
                // valid signal to send to a live pid we own.
                if unsafe { libc::kill(*pid, libc::SIGKILL) } == 0 {
                    Ok(())
                } else {
                    Err(std::io::Error::last_os_error())
                }
            }
        }
    }

    /// Wait for the child to exit, mirroring `tokio::process::Child::wait`.
    ///
    /// Why/What: see [`ChildHandle`]'s docs. Panics if called twice on a
    /// `Disclaimed` handle (the reaper task is consumed on the first call) —
    /// callers never do this today (`StreamJsonBackend::stop` and its `Drop`
    /// impl are mutually exclusive by construction).
    /// Test: `spawn_piped_disclaimed_writes_and_reads_via_cat`,
    /// `spawn_piped_disclaimed_kill_and_wait_reaps_child`,
    /// `disclaimed_piped_spawn_native_path_round_trips`.
    pub async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        match self {
            ChildHandle::Native(child) => child.wait().await,
            #[cfg(target_os = "macos")]
            ChildHandle::Disclaimed { reaper, .. } => {
                let handle = reaper
                    .take()
                    .expect("ChildHandle::wait called more than once");
                match handle.await {
                    Ok(result) => result,
                    Err(e) => Err(std::io::Error::other(e)),
                }
            }
        }
    }
}

/// The result of a piped, disclaimed spawn: a lifecycle [`ChildHandle`] plus
/// the child's stdin/stdout pipes as uniform async trait objects.
///
/// Why: [`disclaimed_piped_spawn`]'s return shape — see that function's docs.
/// What: three public fields; the caller destructures them (stderr is
/// intentionally not exposed — see [`ChildHandle`]'s docs on why it stays
/// open-but-undrained rather than surfaced).
/// Test: see [`disclaimed_piped_spawn`].
pub struct PipedSpawn {
    pub handle: ChildHandle,
    pub stdin: DynChildStdin,
    pub stdout: DynChildStdout,
}

/// Spawn a long-lived child with piped stdin/stdout and — on macOS — disclaim
/// TCC responsibility, exactly like [`disclaimed_output`] but for a
/// long-lived, async, piped-I/O child instead of a capture-to-completion one.
///
/// Why: fixes issue #2997 — `StreamJsonBackend`'s `claude -p --output-format
/// stream-json` child (the daemon's default actor-managed session backend)
/// previously spawned via plain `tokio::process::Command`, so its TCC access
/// (and that of anything IT forks, e.g. an agent's `cargo build`) rolled up
/// to the signed `trusty-mpm` binary. This is the same disclaim fix as #2819
/// applied to the long-lived piped spawn shape.
/// What: takes an already-built `std::process::Command` (e.g.
/// `build_claude_command`'s output, `ANTHROPIC_API_KEY` already removed) and,
/// on macOS, re-derives argv/envp/cwd from it via the stable
/// `get_program`/`get_args`/`get_envs`/`get_current_dir` accessors and spawns
/// via `posix_spawnp` with three pipes and the disclaim attribute set,
/// wrapping the parent-held pipe ends in `tokio::net::unix::pipe`. On
/// non-macOS (or with [`DISABLE_ENV`] set) it spawns `cmd` through
/// `tokio::process::Command` exactly as `StreamJsonBackend` did before this
/// fix — a pure pass-through with no behavior change.
/// Test: `spawn_piped_disclaimed_writes_and_reads_via_cat`,
/// `spawn_piped_disclaimed_preserves_cwd`,
/// `spawn_piped_disclaimed_removes_env_and_keeps_rest`,
/// `spawn_piped_disclaimed_reports_spawn_error_for_missing_binary`,
/// `spawn_piped_disclaimed_kill_and_wait_reaps_child` (macOS-only, in
/// `macos::piped`); `disclaimed_piped_spawn_native_path_round_trips` (any OS).
pub fn disclaimed_piped_spawn(cmd: std::process::Command) -> std::io::Result<PipedSpawn> {
    #[cfg(target_os = "macos")]
    {
        if std::env::var_os(DISABLE_ENV).is_none() {
            return macos::spawn_piped_disclaimed(cmd);
        }
    }
    spawn_piped_native(cmd)
}

/// The non-disclaimed fallback for [`disclaimed_piped_spawn`]: spawn `cmd`
/// through plain `tokio::process::Command`.
///
/// Why: used unconditionally on non-macOS (there is no TCC there) and as the
/// [`DISABLE_ENV`] escape hatch on macOS — identical to
/// `StreamJsonBackend::spawn`'s pre-#2997 behavior.
/// What: converts `cmd` to a `tokio::process::Command`, spawns it, and takes
/// its stdin/stdout pipes (the caller's `cmd` must already have configured
/// `Stdio::piped()` on both — `build_claude_command` does). stderr is left
/// untouched (never `.take()`n), matching `StreamJsonBackend`'s pre-existing
/// "captured but never forwarded" behavior.
/// Test: `disclaimed_piped_spawn_native_path_round_trips`.
fn spawn_piped_native(cmd: std::process::Command) -> std::io::Result<PipedSpawn> {
    let mut child = tokio::process::Command::from(cmd).spawn()?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| std::io::Error::other("child stdin pipe was not opened"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("child stdout pipe was not opened"))?;
    Ok(PipedSpawn {
        handle: ChildHandle::Native(child),
        stdin: Box::new(stdin),
        stdout: Box::new(stdout),
    })
}

#[cfg(target_os = "macos")]
mod macos;

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

    // --- disclaimed_status (issue #2997: tm run/tm login session-launch path) ---

    #[test]
    fn disclaimed_status_inherits_and_reports_exit_code() {
        // No explicit stdio set on the Command — matches build_launch_command's
        // Stdio::inherit() shape (Command::status()'s implicit default is also
        // inherit, so this exercises the same fd-passthrough contract).
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.args(["-c", "exit 7"]);
        let status = disclaimed_status(&mut cmd).unwrap();
        assert_eq!(status.code(), Some(7));
    }

    #[test]
    fn disclaimed_status_applies_cwd_and_env_override() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tmp_path = std::fs::canonicalize(tmp.path()).unwrap();

        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.current_dir(&tmp_path);
        cmd.env("SPAWN_DISCLAIM_TEST_VAR_2997", "hello-2997");
        cmd.args([
            "-c",
            "printf '%s' \"$SPAWN_DISCLAIM_TEST_VAR_2997\" > out.txt && pwd > cwd.txt",
        ]);
        let status = disclaimed_status(&mut cmd).unwrap();
        assert!(status.success());

        let out_contents = std::fs::read_to_string(tmp_path.join("out.txt")).unwrap();
        assert_eq!(
            out_contents, "hello-2997",
            "expected the Command's env override to reach the child"
        );

        let cwd_contents = std::fs::read_to_string(tmp_path.join("cwd.txt")).unwrap();
        assert_eq!(
            std::fs::canonicalize(cwd_contents.trim()).unwrap(),
            tmp_path,
            "expected the Command's current_dir to reach the child via chdir_np"
        );
    }

    #[test]
    fn disclaimed_status_reports_spawn_error_for_missing_binary() {
        let mut cmd = std::process::Command::new("/nonexistent/definitely-not-a-real-binary-2997");
        let err = disclaimed_status(&mut cmd)
            .expect_err("spawning a missing binary must error, not hang or panic");
        assert!(matches!(
            err.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::Other
        ));
    }

    #[test]
    fn disclaimed_status_disable_env_forces_plain_path() {
        // SAFETY: single-threaded test setup around the env toggle.
        unsafe { std::env::set_var(DISABLE_ENV, "1") };
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.args(["-c", "exit 3"]);
        let status = disclaimed_status(&mut cmd).unwrap();
        unsafe { std::env::remove_var(DISABLE_ENV) };
        assert_eq!(status.code(), Some(3));
    }
}

#[cfg(test)]
mod piped_native_tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    /// Exercises the exact code path Linux always takes for
    /// [`disclaimed_piped_spawn`] (and that macOS takes under
    /// [`DISABLE_ENV`]) — no `cfg(target_os = "macos")` gate, so this runs on
    /// every CI platform and proves the fallback is a correct, unaffected
    /// no-op wrapper around `tokio::process::Command`.
    #[tokio::test]
    async fn disclaimed_piped_spawn_native_path_round_trips() {
        if cfg!(target_os = "windows") {
            return; // no portable `cat` equivalent; skip on Windows
        }
        // SAFETY: no other test in this binary reads/writes DISABLE_ENV
        // concurrently with this one.
        unsafe { std::env::set_var(DISABLE_ENV, "1") };
        let mut cmd = std::process::Command::new("/bin/cat");
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut spawned = disclaimed_piped_spawn(cmd).unwrap();
        unsafe { std::env::remove_var(DISABLE_ENV) };

        spawned.stdin.write_all(b"native path\n").await.unwrap();
        drop(spawned.stdin);
        let mut reader = BufReader::new(spawned.stdout);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        assert_eq!(line, "native path\n");
        let status = spawned.handle.wait().await.unwrap();
        assert!(status.success());
    }
}
