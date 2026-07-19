//! `tm internal-spawn-disclaimed` — the pane's macOS TCC disclaim shim (#2997).
//!
//! Why: managed sessions launch `claude` by typing a command into a tmux pane;
//! the pane shell is forked by the shared tmux server, so a disclaim attribute
//! the daemon sets can never reach that `claude` and tccd blames the server.
//! [`crate::cli::Command::InternalSpawnDisclaimed`] (emitted into the pane
//! command by [`trusty_mpm::core::spawn_disclaim::disclaim_pane_command`]) is
//! the tm-owned process the pane routes `claude` through so it can be
//! `posix_spawn`ed WITH the disclaim attribute set.
//! What: [`run`] spawns the given program+args via the shared #3037
//! [`trusty_mpm::core::spawn_disclaim::disclaimed_status`] seam (inherited
//! stdio so `claude` stays interactive in the pane), waits, and exits with the
//! child's code. On macOS it first sets SIGINT/SIGQUIT to SIG_IGN so a pane
//! Ctrl-C reaches only `claude` (which the disclaimed spawn resets to SIG_DFL)
//! and the shim survives to reap and propagate the child's real exit status —
//! the standard wrapper convention. Non-macOS is a plain pass-through spawn (no
//! TCC there, and the shim is never emitted into a pane off macOS).
//! Test: `run_rejects_empty_argv`; the disclaim/spawn behaviour is covered by
//! `trusty_mpm::core::spawn_disclaim`'s `disclaimed_status_*` tests, and the
//! wrapped-pane PID discovery + signal topology by
//! `core::process::claude_pid_resolves_through_disclaim_wrapper` (`#[ignore]`)
//! plus the manual macOS pre-merge checks noted on the PR.

use anyhow::Context as _;

/// Spawn `argv[0]` with `argv[1..]` disclaimed and exit with its status code.
///
/// Why: this is the leaf of the #2997 fix — the one process that actually sets
/// the `posix_spawn` disclaim attribute on the pane's `claude`, so `claude`
/// becomes its own TCC responsible process instead of the shared tmux server.
/// What: builds a `std::process::Command` from `argv` (stdio left as the
/// default inherit, matching the pane's interactive tty), spawns it through
/// [`trusty_mpm::core::spawn_disclaim::disclaimed_status`] — which disclaims on
/// macOS and is a plain `Command::status()` elsewhere — then
/// `std::process::exit`s with the child's code (127 when it was killed by a
/// signal, mirroring a shell). Never returns `Ok`: it either exits the process
/// or returns the spawn error (an empty `argv`, or a spawn failure such as a
/// missing binary).
/// Test: `run_rejects_empty_argv`.
pub(crate) fn run(argv: Vec<String>) -> anyhow::Result<()> {
    let mut it = argv.into_iter();
    let program = it
        .next()
        .context("internal-spawn-disclaimed requires a program to launch")?;
    let rest: Vec<String> = it.collect();

    // #2997 review: ignore SIGINT/SIGQUIT in the shim so a pane Ctrl-C (or
    // Ctrl-\) reaches only `claude` — which the disclaimed spawn resets to
    // SIG_DFL so it installs its own handlers (see
    // `spawn_disclaim::macos::status`) — and the shim SURVIVES to reap the
    // child and propagate its real exit status. Without this the shim and
    // `claude` share the foreground process group, so the signal would kill
    // the shim (default disposition) out from under `claude`, orphaning it.
    // The standard wrapper convention (à la `nohup`/`system(3)`). macOS-only
    // to match where the child-side SIG_DFL reset actually happens (the shim
    // is only ever emitted into a pane on macOS).
    #[cfg(target_os = "macos")]
    // SAFETY: setting SIG_IGN for two async-signal-safe signals before any
    // child exists; no handler state is shared.
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_IGN);
        libc::signal(libc::SIGQUIT, libc::SIG_IGN);
    }

    let mut cmd = std::process::Command::new(&program);
    cmd.args(&rest);

    let status = trusty_mpm::core::spawn_disclaim::disclaimed_status(&mut cmd)
        .with_context(|| format!("failed to spawn `{program}` (disclaimed)"))?;

    // Mirror a shell: propagate the child's exit code; use 127 when it was
    // terminated by a signal (no exit code available).
    std::process::exit(status.code().unwrap_or(127));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_rejects_empty_argv() {
        // No program token → a clear error rather than a panic or a spawn of "".
        let err = run(Vec::new()).expect_err("empty argv must error");
        assert!(
            err.to_string().contains("requires a program"),
            "unexpected error: {err}"
        );
    }
}
