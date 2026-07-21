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
//! every known `claude`-spawning call site in the crate now disclaims. As of
//! #3126 [`disclaimed_spawn_detached`] adds the TUI health screen's `[S]` key
//! (`tui::event_loop::health_start`, a detached `cargo run` child never
//! waited on) — the last undisclaimed spawn site found by the #2997 part 5
//! sweep. As of #3267 (#2997 part 6) [`disclaimed_stderr_piped_spawn`] and
//! [`disclaimed_stdout_piped_spawn`] add the two non-`claude` spawn sites the
//! #3261 code-critic review flagged: `provisioner::clone_progress::clone_with_progress`
//! (a daemon-initiated `git clone`) and
//! `formatters::info_box::probes::run_git_log` (the `tm` binary's welcome-panel
//! `git log` probe).
//! Test: `disclaimed_output_captures_stdout`,
//! `disclaimed_output_captures_stderr_and_nonzero_exit`,
//! `disclaimed_output_saturates_stdout_and_stderr_without_deadlock`,
//! `disclaimed_output_reports_spawn_error_for_missing_binary`,
//! `disclaimed_status_inherits_and_reports_exit_code`,
//! `disclaimed_status_applies_cwd_and_env_override`,
//! `disclaimed_status_reports_spawn_error_for_missing_binary`,
//! `disclaimed_second_kill_after_wait_is_noop` (all macOS-only,
//! in this module's `tests`); `spawn_piped_disclaimed_writes_and_reads_via_cat`
//! and its siblings in `macos::piped` (piped path, macOS-only);
//! `disclaimed_piped_spawn_native_path_round_trips` (piped path, any OS —
//! exercises the exact code path Linux always takes);
//! `disclaimed_spawn_detached_returns_ok_for_true`,
//! `disclaimed_spawn_detached_reports_spawn_error_for_missing_binary`,
//! `disclaimed_spawn_detached_disable_env_forces_plain_path` (macOS-only,
//! this module's `tests`); `disclaimed_stderr_piped_spawn_streams_and_waits`,
//! `disclaimed_stderr_piped_spawn_applies_cwd_and_env_override`,
//! `disclaimed_stderr_piped_spawn_reports_spawn_error_for_missing_binary`,
//! `disclaimed_stderr_piped_spawn_disable_env_forces_plain_path`,
//! `disclaimed_stdout_piped_spawn_captures_and_waits`,
//! `disclaimed_stdout_piped_spawn_id_allows_watchdog_kill_before_wait`,
//! `disclaimed_stdout_piped_spawn_reports_spawn_error_for_missing_binary`,
//! `disclaimed_stdout_piped_spawn_disable_env_forces_plain_path` (macOS-only,
//! this module's `tests`); `disclaimed_stderr_piped_spawn_native_path_round_trips`,
//! `disclaimed_stdout_piped_spawn_native_path_round_trips` (any OS,
//! `piped_native_tests`).

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

/// Spawn `program` with `args` detached from the current process — a
/// fire-and-forget spawn, matching `Command::spawn()`'s semantics of
/// returning immediately without waiting for the child — and, on macOS,
/// disclaim TCC responsibility exactly like [`disclaimed_output`]/
/// [`disclaimed_status`].
///
/// Why: the TUI health screen's `[S]` key (`tui::event_loop::health_start`)
/// launches a detached `cargo run -p trusty-search -- start` / `cargo run -p
/// trusty-memory` child and never waits on it, so it fit none of the other
/// three disclaim shapes (capture-to-completion, wait-for-status, or
/// long-lived piped) — the last undisclaimed spawn site in the TUI/
/// session-launch call graph (issue #2997 part 5, #3126). Two more
/// undisclaimed spawn sites (`provisioner::clone_progress`,
/// `formatters::info_box::probes::run_git_log`) were found by the #3261
/// code-critic review and are fixed by [`disclaimed_stderr_piped_spawn`]/
/// [`disclaimed_stdout_piped_spawn`] below (#2997 part 6, issue #3267).
/// What: on macOS, builds a `Command` for `program`/`args` with stdio left at
/// the default (inherited from the caller, matching the pre-existing
/// `.spawn()` call this replaces) and spawns it via `posix_spawnp` with the
/// disclaim attribute set, discarding the pid without waiting on it — the
/// child is reaped later either by an unrelated `waitpid` or, once `tm`
/// itself exits, by init/launchd, exactly as the un-awaited
/// `std::process::Child` this replaces already behaved (its caller dropped
/// the `Child` without calling `wait()`). On non-macOS, or with
/// [`DISABLE_ENV`] set, delegates straight to `Command::spawn()` and drops
/// the `Child` the same way.
/// Test: `disclaimed_spawn_detached_returns_ok_for_true`,
/// `disclaimed_spawn_detached_reports_spawn_error_for_missing_binary`,
/// `disclaimed_spawn_detached_disable_env_forces_plain_path`.
pub fn disclaimed_spawn_detached(program: &str, args: &[&str]) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        if std::env::var_os(DISABLE_ENV).is_none() {
            let mut cmd = std::process::Command::new(program);
            cmd.args(args);
            return macos::spawn_detached_disclaimed(&mut cmd);
        }
    }
    std::process::Command::new(program)
        .args(args)
        .spawn()
        .map(|_| ())
}

/// A synchronously-readable, piped-stderr child spawned by
/// [`disclaimed_stderr_piped_spawn`] — stdout discarded, stdin inherited.
///
/// Why: `provisioner::clone_progress::clone_with_progress` reads `git clone
/// --progress`'s stderr line-by-line WHILE THE CHILD RUNS
/// (to emit live percentage detail), so it needs a real-time stderr handle
/// rather than [`disclaimed_output`]'s capture-to-completion contract.
/// What: `stderr` is a boxed synchronous reader (the macOS-disclaimed pipe
/// read end, or a native `std::process::ChildStderr`) the caller drains to
/// EOF; [`Self::wait`] then reaps the child.
/// Test: see [`disclaimed_stderr_piped_spawn`].
pub struct StderrPipedSpawn {
    /// The child's stderr, readable synchronously while the process runs.
    pub stderr: Box<dyn std::io::Read + Send>,
    handle: StderrPipedHandle,
}

enum StderrPipedHandle {
    Native(std::process::Child),
    #[cfg(target_os = "macos")]
    Disclaimed(libc::pid_t),
}

impl StderrPipedSpawn {
    /// Wait for the child to exit, mirroring `Child::wait()`.
    ///
    /// Why: callers must drain `stderr` to EOF first (as
    /// `clone_with_progress` does) — the pipe's write end only closes once
    /// the child exits or closes fd 2 itself, so waiting before draining a
    /// large-output child can deadlock on a full pipe buffer.
    /// What: reaps the native `Child` or, on the macOS-disclaimed path, the
    /// raw pid via [`macos::wait_for`].
    /// Test: see [`disclaimed_stderr_piped_spawn`].
    pub fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        match &mut self.handle {
            StderrPipedHandle::Native(child) => child.wait(),
            #[cfg(target_os = "macos")]
            StderrPipedHandle::Disclaimed(pid) => macos::wait_for(*pid),
        }
    }
}

/// Spawn `cmd` with stdout discarded to `/dev/null`, stdin inherited, and
/// stderr piped for synchronous streaming — and, on macOS, disclaim TCC
/// responsibility exactly like [`disclaimed_output`]. Returns immediately
/// with a live stderr reader instead of blocking until the child exits.
///
/// Why: fixes one of the two spawn sites named by issue #3267 (#2997 part 6):
/// `clone_progress::clone_with_progress` previously called `cmd.spawn()`
/// directly, so the `git clone`'s (and anything a clone/checkout hook
/// forks') TCC access rolled up to the signed `trusty-mpm` binary — the same
/// mis-attribution shape as #2819/#2721/#2997/#3126, just on the
/// daemon-initiated workspace-provisioning path rather than a `claude`
/// launch. This is the "closest existing shape is the piped/long-lived
/// pattern" the issue called out, minimally adapted: unlike
/// [`disclaimed_piped_spawn`] this is synchronous (no tokio — `clone_with_progress`
/// is a blocking function called from a non-async trait method) and pipes
/// ONLY stderr rather than all three streams.
/// What: on macOS, re-derives argv/envp/cwd from `cmd` via the same stable
/// `Command` accessors [`macos::spawn_status_inherit_disclaimed`] uses, and
/// spawns via `posix_spawnp` with a `/dev/null` stdout file action, a piped
/// stderr, no stdin file action (inherited — matches the pre-existing
/// `Command`'s implicit default), and the disclaim attribute when the
/// private SPI resolves. On non-macOS (or with [`DISABLE_ENV`] set) sets
/// `cmd.stdout(Stdio::null()).stderr(Stdio::piped())` and delegates to
/// `cmd.spawn()`, taking the child's stderr pipe exactly as the pre-fix code
/// did.
/// Test: `disclaimed_stderr_piped_spawn_streams_and_waits`,
/// `disclaimed_stderr_piped_spawn_reports_spawn_error_for_missing_binary`,
/// `disclaimed_stderr_piped_spawn_disable_env_forces_plain_path` (macOS-only,
/// this module's `tests`); `disclaimed_stderr_piped_spawn_native_path_round_trips`
/// (any OS, `piped_native_tests`).
pub fn disclaimed_stderr_piped_spawn(
    mut cmd: std::process::Command,
) -> std::io::Result<StderrPipedSpawn> {
    #[cfg(target_os = "macos")]
    {
        if std::env::var_os(DISABLE_ENV).is_none() {
            return macos::spawn_stderr_piped_disclaimed(&cmd);
        }
    }
    cmd.stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn()?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| std::io::Error::other("child stderr pipe was not opened"))?;
    Ok(StderrPipedSpawn {
        stderr: Box::new(stderr),
        handle: StderrPipedHandle::Native(child),
    })
}

/// A piped-stdout child spawned by [`disclaimed_stdout_piped_spawn`] —
/// stderr discarded, stdin inherited — exposing the child's pid so a caller
/// can arm an external kill-watchdog BEFORE blocking on
/// [`Self::wait_with_output`].
///
/// Why: `formatters::info_box::probes::run_git_log` (in the `tm` binary)
/// spawns `git log` with a 3-second SIGKILL watchdog
/// that must be armed with the child's pid BEFORE the blocking
/// read-to-EOF-then-wait, so it needs the pid available immediately after
/// spawn — [`disclaimed_output`] only returns after the child has already
/// exited, so it cannot expose the pid in time.
/// What: `id` mirrors `std::process::Child::id()`; `wait_with_output`
/// consumes `self`, drains stdout, and reaps the child.
/// Test: see [`disclaimed_stdout_piped_spawn`].
pub struct StdoutPipedSpawn {
    /// The child's OS process id, valid immediately after spawn — used to
    /// arm an external kill-watchdog before [`Self::wait_with_output`] blocks.
    pub id: u32,
    stdout: Box<dyn std::io::Read + Send>,
    handle: StdoutPipedHandle,
}

enum StdoutPipedHandle {
    Native(std::process::Child),
    #[cfg(target_os = "macos")]
    Disclaimed(libc::pid_t),
}

impl StdoutPipedSpawn {
    /// Read stdout to EOF, then wait for exit — mirrors
    /// `Child::wait_with_output()`'s stdout+status contract. `stderr` is
    /// always empty (this spawn shape discards it to `/dev/null`).
    ///
    /// Why/What: see the struct docs.
    /// Test: see [`disclaimed_stdout_piped_spawn`].
    pub fn wait_with_output(mut self) -> std::io::Result<std::process::Output> {
        use std::io::Read as _;
        let mut stdout = Vec::new();
        self.stdout.read_to_end(&mut stdout)?;
        let status = match self.handle {
            StdoutPipedHandle::Native(mut child) => child.wait()?,
            #[cfg(target_os = "macos")]
            StdoutPipedHandle::Disclaimed(pid) => macos::wait_for(pid)?,
        };
        Ok(std::process::Output {
            status,
            stdout,
            stderr: Vec::new(),
        })
    }
}

/// Spawn `cmd` with stdout piped, stderr discarded to `/dev/null`, and stdin
/// inherited — and, on macOS, disclaim TCC responsibility exactly like
/// [`disclaimed_output`]. Returns immediately with the child's pid and a live
/// stdout reader instead of blocking until the child exits.
///
/// Why: fixes the second spawn site named by issue #3267 (#2997 part 6):
/// `formatters::info_box::probes::run_git_log` previously called
/// `std::process::Command::new("git")...spawn()` directly. Its 3-second
/// SIGKILL watchdog (guarding against a slow/network-mounted `.git`) needs
/// the pid immediately after spawn — before the blocking
/// read-then-wait — so this fits neither [`disclaimed_output`] (blocks
/// internally until the child exits, pid never exposed) nor
/// [`disclaimed_piped_spawn`] (async, three pipes); it needs its own thin
/// wrapper following the same argv/envp/cwd/disclaim-SPI reconstruction
/// pattern as [`macos::spawn_status_inherit_disclaimed`].
/// What: on macOS, re-derives argv/envp/cwd from `cmd`, spawns via
/// `posix_spawnp` with a piped stdout, a `/dev/null` stderr file action, no
/// stdin file action (inherited), and the disclaim attribute when the
/// private SPI resolves — returning the pid immediately, unwaited. On
/// non-macOS (or with [`DISABLE_ENV`] set) sets
/// `cmd.stdout(Stdio::piped()).stderr(Stdio::null())` and delegates to
/// `cmd.spawn()`, exposing `Child::id()` exactly as the pre-fix code did.
/// Test: `disclaimed_stdout_piped_spawn_captures_and_waits`,
/// `disclaimed_stdout_piped_spawn_reports_spawn_error_for_missing_binary`,
/// `disclaimed_stdout_piped_spawn_disable_env_forces_plain_path` (macOS-only,
/// this module's `tests`); `disclaimed_stdout_piped_spawn_native_path_round_trips`
/// (any OS, `piped_native_tests`).
pub fn disclaimed_stdout_piped_spawn(
    mut cmd: std::process::Command,
) -> std::io::Result<StdoutPipedSpawn> {
    #[cfg(target_os = "macos")]
    {
        if std::env::var_os(DISABLE_ENV).is_none() {
            return macos::spawn_stdout_piped_disclaimed(&cmd);
        }
    }
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let mut child = cmd.spawn()?;
    let id = child.id();
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("child stdout pipe was not opened"))?;
    Ok(StdoutPipedSpawn {
        id,
        stdout: Box::new(stdout),
        handle: StdoutPipedHandle::Native(child),
    })
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
/// `disclaimed_piped_spawn_native_path_round_trips` (any OS);
/// `disclaimed_second_kill_after_wait_is_noop` covers the
/// reap-then-re-kill sequence specifically.
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
    /// Why: `StreamJsonBackend::stop()` calls `start_kill()` then `wait()`
    /// (reaping the pid), and its `Drop` impl calls `start_kill()`
    /// unconditionally as a safety net on every path, including the normal
    /// post-`stop()` drop — so a naive `libc::kill(pid, …)` here fires a
    /// SECOND SIGKILL at an already-reaped pid on every graceful stop. Once a
    /// pid is reaped the OS is free to recycle it for an unrelated process,
    /// so that second signal is not a benign no-op — it can kill a stranger.
    /// `reaper` doubles as the "have we reaped yet" flag: [`Self::wait`]
    /// `take()`s it the first (and only allowed) time it runs, so
    /// `reaper.is_none()` here means the pid is already gone and `start_kill`
    /// must not touch it.
    /// What: no-ops to `Ok(())` when `reaper` is `None` (already reaped via
    /// `wait()`); otherwise sends SIGKILL as before. Safe to call before
    /// `wait()`, including multiple times before it (the pid is still live in
    /// both cases, so re-signalling it is the same benign repeat send
    /// `tokio::process::Child::start_kill` allows).
    /// Test: `disclaimed_second_kill_after_wait_is_noop` (this module),
    /// `spawn_piped_disclaimed_kill_and_wait_reaps_child`,
    /// `disclaimed_piped_spawn_native_path_round_trips`.
    pub fn start_kill(&mut self) -> std::io::Result<()> {
        match self {
            ChildHandle::Native(child) => child.start_kill(),
            #[cfg(target_os = "macos")]
            ChildHandle::Disclaimed { pid, reaper, .. } => {
                if reaper.is_none() {
                    // Already reaped by a prior `wait()` — the pid may have
                    // been recycled by the OS since; do not signal it.
                    return Ok(());
                }
                // SAFETY: `pid` is our own spawned child and, per the check
                // above, has not yet been reaped; SIGKILL is always a valid
                // signal to send to a live pid we own.
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
    /// impl are mutually exclusive by construction). Consuming `reaper` here
    /// via `take()` is also the "already reaped" signal [`Self::start_kill`]
    /// checks — the two behaviors share the one field deliberately, so they
    /// can never drift out of sync.
    /// Test: `disclaimed_second_kill_after_wait_is_noop`,
    /// `spawn_piped_disclaimed_writes_and_reads_via_cat`,
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

mod pane;
pub use pane::{PANE_DISCLAIM_SUBCOMMAND, disclaim_pane_command};

/// Whether the private `responsibility_spawnattrs_setdisclaim` SPI resolves on
/// this build.
///
/// Why: `tm doctor`'s TCC-taint check (issue #2997) needs to tell the operator
/// whether managed panes can actually disclaim — a future macOS that dropped
/// the SPI would silently degrade the whole fix to a no-op, and that should be
/// surfaced, not invisible.
/// What: `true` on macOS when [`macos::resolve_disclaim_fn`] finds the symbol;
/// `false` on every non-macOS build (there is no TCC there).
/// Test: `daemon::doctor`'s `tcc_taint_*` checks fold this into the verdict;
/// direct coverage lives in the capture/status/piped spawn tests that only
/// disclaim when it returns `Some`.
pub fn disclaim_spi_available() -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::resolve_disclaim_fn().is_some()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// Whether the [`DISABLE_ENV`] escape hatch is currently set.
///
/// Why: the same env var that forces every spawn shape onto the plain path
/// also disables the pane disclaim ([`disclaim_pane_command`]); `tm doctor`
/// reports when it is set so an operator who flipped it is reminded the #2997
/// attribution fix is off.
/// What: `true` when the process sees `TM_DISABLE_SPAWN_DISCLAIM` set to any
/// value.
/// Test: exercised via `daemon::doctor`'s verdict builder.
pub fn disclaim_disabled_by_env() -> bool {
    std::env::var_os(DISABLE_ENV).is_some()
}

#[cfg(target_os = "macos")]
mod macos;

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    // Every test in this module (and `piped_native_tests` below, and
    // `macos::piped`'s tests) spawns through `disclaimed_output`/
    // `disclaimed_status`/`disclaimed_piped_spawn`, all of which read the
    // process-global `DISABLE_ENV` var, and several of them toggle it via
    // `set_var`/`remove_var`. `cargo test` runs `#[test]` fns in parallel
    // threads within the same process by default, so an unserialized toggle
    // in one test can transiently flip the branch taken by a concurrently
    // running spawn in another test — exactly the race that produced the
    // intermittent "child stdin pipe was not opened" panic in
    // `spawn_piped_disclaimed_writes_and_reads_via_cat` under a full-suite
    // run. `#[serial_test::serial]` (unnamed key, matching this crate's
    // existing env-mutation convention — see `core::paths`/`standalone::load`)
    // puts every test below in the same mutual-exclusion group as the ones in
    // `piped_native_tests` and `macos::piped::tests`.

    #[test]
    #[serial_test::serial]
    fn disclaimed_output_captures_stdout() {
        let out = disclaimed_output("/bin/echo", &["hello world".to_string()]).unwrap();
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout), "hello world\n");
        assert!(out.stderr.is_empty());
    }

    #[test]
    #[serial_test::serial]
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
    #[serial_test::serial]
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
    #[serial_test::serial]
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
    #[serial_test::serial]
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
    #[serial_test::serial]
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
    #[serial_test::serial]
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
    #[serial_test::serial]
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
    #[serial_test::serial]
    fn disclaimed_status_disable_env_forces_plain_path() {
        // SAFETY: single-threaded test setup around the env toggle.
        unsafe { std::env::set_var(DISABLE_ENV, "1") };
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.args(["-c", "exit 3"]);
        let status = disclaimed_status(&mut cmd).unwrap();
        unsafe { std::env::remove_var(DISABLE_ENV) };
        assert_eq!(status.code(), Some(3));
    }

    // --- ChildHandle::start_kill double-kill-after-reap regression (issue: PR #3037 review) ---

    #[tokio::test]
    #[serial_test::serial]
    async fn disclaimed_second_kill_after_wait_is_noop() {
        // Reproduces the exact sequence `StreamJsonBackend::stop()` +
        // `Drop::drop()` run on every graceful stop: start_kill() -> wait()
        // (reaps the pid, consuming `reaper`) -> a second start_kill() from
        // `Drop`. Before the fix the second call unconditionally re-sent
        // SIGKILL to the already-reaped pid, risking a stray signal to a
        // recycled pid; it must now be a no-op.
        let mut cmd = std::process::Command::new("/bin/sleep");
        cmd.arg("30");
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut spawned = macos::spawn_piped_disclaimed(cmd).unwrap();

        spawned.handle.start_kill().unwrap();
        spawned.handle.wait().await.unwrap();

        // `wait()` consumes `reaper` — the same field `start_kill()` checks —
        // so this assertion pins the exact signal the no-op branch relies on.
        match &spawned.handle {
            ChildHandle::Disclaimed { reaper, .. } => {
                assert!(reaper.is_none(), "wait() must consume the reaper handle");
            }
            ChildHandle::Native(_) => {
                unreachable!("macos::spawn_piped_disclaimed always returns Disclaimed")
            }
        }

        // The second call must short-circuit to Ok(()) without re-signalling
        // the pid (the SAFETY comment in start_kill establishes this is the
        // only code path that could touch a reused pid).
        assert!(spawned.handle.start_kill().is_ok());
    }

    // --- disclaimed_spawn_detached (issue #3126: TUI health-screen `[S]` cargo spawn) ---

    #[test]
    #[serial_test::serial]
    fn disclaimed_spawn_detached_returns_ok_for_true() {
        // `/usr/bin/true` exits immediately; a detached spawn must report
        // `Ok(())` without blocking on the child's exit.
        assert!(disclaimed_spawn_detached("/usr/bin/true", &[]).is_ok());
    }

    #[test]
    #[serial_test::serial]
    fn disclaimed_spawn_detached_reports_spawn_error_for_missing_binary() {
        let err = disclaimed_spawn_detached("/nonexistent/definitely-not-a-real-binary-3126", &[])
            .expect_err("spawning a missing binary must error, not hang or panic");
        assert!(matches!(
            err.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::Other
        ));
    }

    #[test]
    #[serial_test::serial]
    fn disclaimed_spawn_detached_disable_env_forces_plain_path() {
        // SAFETY: single-threaded test setup around the env toggle.
        unsafe { std::env::set_var(DISABLE_ENV, "1") };
        let result = disclaimed_spawn_detached("/usr/bin/true", &[]);
        unsafe { std::env::remove_var(DISABLE_ENV) };
        assert!(result.is_ok());
    }

    // --- disclaimed_stderr_piped_spawn (issue #3267: clone_with_progress) ---

    #[test]
    #[serial_test::serial]
    fn disclaimed_stderr_piped_spawn_streams_and_waits() {
        // Writes to BOTH stdout and stderr, plus a distinct exit code — pins
        // the exact stdio shape `clone_with_progress` depends on: stdout
        // discarded (never observable), stderr captured, exit status
        // preserved.
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.args([
            "-c",
            "echo should-be-discarded; echo captured-line >&2; exit 5",
        ]);
        let mut spawned = disclaimed_stderr_piped_spawn(cmd).unwrap();
        let mut stderr_buf = Vec::new();
        std::io::Read::read_to_end(&mut spawned.stderr, &mut stderr_buf).unwrap();
        assert_eq!(String::from_utf8_lossy(&stderr_buf), "captured-line\n");
        let status = spawned.wait().unwrap();
        assert_eq!(status.code(), Some(5));
    }

    #[test]
    #[serial_test::serial]
    fn disclaimed_stderr_piped_spawn_applies_cwd_and_env_override() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tmp_path = std::fs::canonicalize(tmp.path()).unwrap();

        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.current_dir(&tmp_path);
        cmd.env("SPAWN_DISCLAIM_STDERR_TEST_VAR_3267", "hello-3267");
        cmd.args([
            "-c",
            "printf '%s' \"$SPAWN_DISCLAIM_STDERR_TEST_VAR_3267\" >&2 && pwd > cwd.txt",
        ]);
        let mut spawned = disclaimed_stderr_piped_spawn(cmd).unwrap();
        let mut stderr_buf = Vec::new();
        std::io::Read::read_to_end(&mut spawned.stderr, &mut stderr_buf).unwrap();
        let status = spawned.wait().unwrap();
        assert!(status.success());
        assert_eq!(String::from_utf8_lossy(&stderr_buf), "hello-3267");

        let cwd_contents = std::fs::read_to_string(tmp_path.join("cwd.txt")).unwrap();
        assert_eq!(
            std::fs::canonicalize(cwd_contents.trim()).unwrap(),
            tmp_path,
            "expected the Command's current_dir to reach the child via chdir_np"
        );
    }

    #[test]
    #[serial_test::serial]
    fn disclaimed_stderr_piped_spawn_reports_spawn_error_for_missing_binary() {
        let cmd =
            std::process::Command::new("/nonexistent/definitely-not-a-real-binary-3267-stderr");
        // `StderrPipedSpawn` holds a `Box<dyn Read>` trait object, so it
        // isn't `Debug` and can't go through `expect_err` (mirrors
        // `PipedSpawn`'s same constraint — see
        // `spawn_piped_disclaimed_reports_spawn_error_for_missing_binary`).
        let err = disclaimed_stderr_piped_spawn(cmd)
            .err()
            .expect("spawning a missing binary must error, not hang or panic");
        assert!(matches!(
            err.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::Other
        ));
    }

    #[test]
    #[serial_test::serial]
    fn disclaimed_stderr_piped_spawn_disable_env_forces_plain_path() {
        // SAFETY: single-threaded test setup around the env toggle.
        unsafe { std::env::set_var(DISABLE_ENV, "1") };
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.args(["-c", "echo out; echo err >&2; exit 2"]);
        let mut spawned = disclaimed_stderr_piped_spawn(cmd).unwrap();
        let mut stderr_buf = Vec::new();
        std::io::Read::read_to_end(&mut spawned.stderr, &mut stderr_buf).unwrap();
        let status = spawned.wait().unwrap();
        unsafe { std::env::remove_var(DISABLE_ENV) };
        assert_eq!(String::from_utf8_lossy(&stderr_buf), "err\n");
        assert_eq!(status.code(), Some(2));
    }

    // --- disclaimed_stdout_piped_spawn (issue #3267: probes::run_git_log) ---

    #[test]
    #[serial_test::serial]
    fn disclaimed_stdout_piped_spawn_captures_and_waits() {
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.args([
            "-c",
            "echo captured-out; echo should-be-discarded >&2; exit 5",
        ]);
        let spawned = disclaimed_stdout_piped_spawn(cmd).unwrap();
        assert!(spawned.id > 0, "pid must be available before waiting");
        let out = spawned.wait_with_output().unwrap();
        assert_eq!(out.status.code(), Some(5));
        assert_eq!(String::from_utf8_lossy(&out.stdout), "captured-out\n");
        assert!(out.stderr.is_empty());
    }

    #[test]
    #[serial_test::serial]
    fn disclaimed_stdout_piped_spawn_id_allows_watchdog_kill_before_wait() {
        // Reproduces the exact shape `run_git_log`'s 3-second SIGKILL
        // watchdog depends on: the pid must be usable to kill the child
        // BEFORE `wait_with_output` is called (which would otherwise block
        // forever on `sleep 30`'s stdout never reaching EOF).
        let mut cmd = std::process::Command::new("/bin/sleep");
        cmd.arg("30");
        let spawned = disclaimed_stdout_piped_spawn(cmd).unwrap();
        let pid = spawned.id;
        assert!(pid > 0);
        // SAFETY: `pid` is our own freshly spawned child.
        let killed = unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
        assert_eq!(killed, 0, "SIGKILL must reach the child by its exposed pid");
        let out = spawned.wait_with_output().unwrap();
        assert!(!out.status.success());
    }

    #[test]
    #[serial_test::serial]
    fn disclaimed_stdout_piped_spawn_reports_spawn_error_for_missing_binary() {
        let cmd =
            std::process::Command::new("/nonexistent/definitely-not-a-real-binary-3267-stdout");
        // `StdoutPipedSpawn` holds a `Box<dyn Read>` trait object, so it
        // isn't `Debug` and can't go through `expect_err` — same constraint
        // as `StderrPipedSpawn` above.
        let err = disclaimed_stdout_piped_spawn(cmd)
            .err()
            .expect("spawning a missing binary must error, not hang or panic");
        assert!(matches!(
            err.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::Other
        ));
    }

    #[test]
    #[serial_test::serial]
    fn disclaimed_stdout_piped_spawn_disable_env_forces_plain_path() {
        // SAFETY: single-threaded test setup around the env toggle.
        unsafe { std::env::set_var(DISABLE_ENV, "1") };
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.args(["-c", "echo out; exit 3"]);
        let spawned = disclaimed_stdout_piped_spawn(cmd).unwrap();
        let out = spawned.wait_with_output().unwrap();
        unsafe { std::env::remove_var(DISABLE_ENV) };
        assert_eq!(out.status.code(), Some(3));
        assert_eq!(String::from_utf8_lossy(&out.stdout), "out\n");
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
    #[serial_test::serial]
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

    /// Exercises the exact code path every non-macOS platform always takes
    /// for [`disclaimed_stderr_piped_spawn`] (and that macOS takes under
    /// [`DISABLE_ENV`]) — no `cfg(target_os = "macos")` gate, so this runs on
    /// every CI platform.
    #[test]
    #[serial_test::serial]
    fn disclaimed_stderr_piped_spawn_native_path_round_trips() {
        if cfg!(target_os = "windows") {
            return; // no portable `sh` equivalent; skip on Windows
        }
        // SAFETY: no other test in this binary reads/writes DISABLE_ENV
        // concurrently with this one.
        unsafe { std::env::set_var(DISABLE_ENV, "1") };
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.args(["-c", "echo out; echo native-stderr >&2; exit 4"]);
        let mut spawned = disclaimed_stderr_piped_spawn(cmd).unwrap();
        unsafe { std::env::remove_var(DISABLE_ENV) };

        let mut stderr_buf = Vec::new();
        std::io::Read::read_to_end(&mut spawned.stderr, &mut stderr_buf).unwrap();
        let status = spawned.wait().unwrap();
        assert_eq!(String::from_utf8_lossy(&stderr_buf), "native-stderr\n");
        assert_eq!(status.code(), Some(4));
    }

    /// Exercises the exact code path every non-macOS platform always takes
    /// for [`disclaimed_stdout_piped_spawn`] (and that macOS takes under
    /// [`DISABLE_ENV`]) — no `cfg(target_os = "macos")` gate, so this runs on
    /// every CI platform.
    #[test]
    #[serial_test::serial]
    fn disclaimed_stdout_piped_spawn_native_path_round_trips() {
        if cfg!(target_os = "windows") {
            return; // no portable `sh` equivalent; skip on Windows
        }
        // SAFETY: no other test in this binary reads/writes DISABLE_ENV
        // concurrently with this one.
        unsafe { std::env::set_var(DISABLE_ENV, "1") };
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.args(["-c", "echo native-stdout; exit 4"]);
        let spawned = disclaimed_stdout_piped_spawn(cmd).unwrap();
        unsafe { std::env::remove_var(DISABLE_ENV) };
        assert!(spawned.id > 0);

        let out = spawned.wait_with_output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "native-stdout\n");
        assert_eq!(out.status.code(), Some(4));
    }
}
