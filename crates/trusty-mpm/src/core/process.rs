//! OS-level process tracking for tmux-hosted `claude` sessions.
//!
//! Why: a tmux session can stay alive long after the `claude` process inside it
//! exits (the pane drops back to a shell). Tracking the real `claude` PID lets
//! the daemon detect a stopped session and mark it as such rather than reporting
//! a hollow tmux window as still active.
//! What: [`find_claude_pid_in_tmux`] resolves the `claude` PID under a tmux
//! pane's shell, and [`is_process_alive`] checks whether a recorded PID still
//! refers to a live process.
//! Test: `cargo test -p trusty-mpm-core process` covers liveness for the
//! current process, a guaranteed-dead PID, and a bogus tmux session name.

use std::process::Command;
use std::thread::sleep;
use std::time::Duration;

/// Find the PID of the `claude` process running as a child of a tmux pane.
///
/// Why: trusty-mpm launches `claude` with `tmux send-keys`, so it never gets a
/// PID directly; it must be discovered after the fact by walking the pane's
/// process tree.
/// What:
/// 1. Get the pane's shell PID: `tmux display-message -t <session> -p '#{pane_pid}'`.
/// 2. Find a child process named `claude`/`claude-code` via `pgrep -P <pane_pid>`.
/// 3. Retry up to `max_attempts` times with `delay` between attempts, since
///    `claude` takes 1-3 s to start after `send-keys`.
///
/// Returns `None` if tmux is unavailable, the pane has no shell PID, or no
/// `claude` child is found within the retry budget.
/// Test: `find_claude_pid_returns_none_for_nonexistent_session`.
pub fn find_claude_pid_in_tmux(
    session_name: &str,
    max_attempts: u8,
    delay: Duration,
) -> Option<u32> {
    for attempt in 0..max_attempts.max(1) {
        if attempt > 0 {
            sleep(delay);
        }
        let Some(pane_pid) = tmux_pane_pid(session_name) else {
            // No pane / tmux unavailable — retrying will not help.
            return None;
        };
        if let Some(pid) = claude_child_of(pane_pid) {
            return Some(pid);
        }
    }
    None
}

/// Read the shell PID of a tmux session's active pane.
///
/// Why: the `claude` process is a child of this shell; it is the root we walk
/// the process tree from.
/// What: runs `tmux display-message -t <session> -p '#{pane_pid}'` and parses
/// the single integer it prints. Returns `None` when tmux is absent or the
/// session does not exist.
/// Test: exercised via `find_claude_pid_returns_none_for_nonexistent_session`.
fn tmux_pane_pid(session_name: &str) -> Option<u32> {
    let output = Command::new("tmux")
        .args(["display-message", "-t", session_name, "-p", "#{pane_pid}"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    text.trim().parse::<u32>().ok()
}

/// Find the `claude` process under `shell_pid`, one hop through the #2997
/// disclaim wrapper if present.
///
/// Why: after `send-keys "claude"`, `claude` is normally a DIRECT child of the
/// pane's shell. But on macOS (issue #2997) the pane routes `claude` through
/// the `internal-spawn-disclaimed` shim so it can be `posix_spawn`ed with TCC
/// responsibility disclaimed — which makes `claude` a GRANDCHILD (`shell →
/// tm internal-spawn-disclaimed → claude`), not a direct child. A direct-child
/// `pgrep -P` would then deterministically miss it, breaking every downstream
/// consumer of the resolved PID (runtime-ready gate, `--task` injection,
/// graceful-stop SIGTERM, daemon PID capture). So after the direct-child scan
/// fails, walk exactly one hop through any child that is our wrapper (matched
/// by the unique [`crate::core::spawn_disclaim::PANE_DISCLAIM_SUBCOMMAND`]
/// token — deterministic, not a fragile name heuristic).
/// What: `pgrep -P <shell_pid>`; return the first direct child whose process
/// name contains `claude`; else, for each direct child that
/// [`is_disclaim_wrapper`] identifies, scan ITS children for `claude` and
/// return the first found. On non-macOS (and with the disclaim disabled) no
/// wrapper is ever present, so this reduces to the original direct-child scan.
/// Test: `find_claude_pid_returns_none_for_nonexistent_session`;
/// `claude_pid_resolves_through_disclaim_wrapper` (`#[ignore]`, real tmux).
fn claude_child_of(shell_pid: u32) -> Option<u32> {
    let children = child_pids_of(shell_pid);
    // Direct child (the pre-#2997 / non-macOS / disclaim-disabled shape).
    if let Some(pid) = children
        .iter()
        .copied()
        .find(|&pid| process_name_contains_claude(pid))
    {
        return Some(pid);
    }
    // #2997: walk one hop through the disclaim wrapper to reach the grandchild.
    children
        .into_iter()
        .filter(|&child| is_disclaim_wrapper(child))
        .find_map(|wrapper| {
            child_pids_of(wrapper)
                .into_iter()
                .find(|&pid| process_name_contains_claude(pid))
        })
}

/// Return the PIDs of the direct children of `pid` via `pgrep -P`.
///
/// Why: both the direct-child `claude` scan and the one-hop-through-the-wrapper
/// scan in [`claude_child_of`] need a process's child list; factoring it keeps
/// the two-level walk readable.
/// What: runs `pgrep -P <pid>` and parses one PID per line; an unavailable
/// `pgrep`, a non-zero exit (no children), or unparsable output all yield an
/// empty vector.
/// Test: exercised via `find_claude_pid_returns_none_for_nonexistent_session`.
fn child_pids_of(pid: u32) -> Vec<u32> {
    let Ok(output) = Command::new("pgrep")
        .args(["-P", &pid.to_string()])
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.trim().parse::<u32>().ok())
        .collect()
}

/// Whether `pid` is the #2997 disclaim-exec shim (`tm/trusty-mpm
/// internal-spawn-disclaimed …`).
///
/// Why: [`claude_child_of`] must know which of the pane shell's children is the
/// wrapper to walk one hop through it. Keying on the unique subcommand token
/// (rather than the process name `tm`, which could match an unrelated `tm`
/// child) makes the match deterministic and self-documenting.
/// What: reads the process's full argv ([`process_args`]) and tests for the
/// [`crate::core::spawn_disclaim::PANE_DISCLAIM_SUBCOMMAND`] token.
/// Test: `claude_pid_resolves_through_disclaim_wrapper` (`#[ignore]`).
fn is_disclaim_wrapper(pid: u32) -> bool {
    process_args(pid)
        .is_some_and(|args| args.contains(crate::core::spawn_disclaim::PANE_DISCLAIM_SUBCOMMAND))
}

/// Read process `pid`'s full command line (argv joined by spaces).
///
/// Why: [`is_disclaim_wrapper`] needs the argv, not just the short `comm`
/// name, to see the `internal-spawn-disclaimed` subcommand token.
/// What: reads `/proc/<pid>/cmdline` (NUL-separated → spaces) on Linux, else
/// `ps -p <pid> -o args=` (the macOS path). `None` on any lookup failure.
/// Test: exercised via `claude_pid_resolves_through_disclaim_wrapper`.
fn process_args(pid: u32) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        if let Ok(cmdline) = std::fs::read_to_string(format!("/proc/{pid}/cmdline")) {
            return Some(cmdline.replace('\0', " "));
        }
    }
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "args="])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Check whether process `pid`'s command name contains `claude`.
///
/// Why: a shell may have several children; only the `claude`/`claude-code`
/// process is the one trusty-mpm is tracking.
/// What: reads `/proc/<pid>/comm` on Linux, falling back to `ps -p <pid> -o
/// comm=` (the portable path used on macOS). A case-insensitive `claude`
/// substring match accepts both `claude` and `claude-code`.
/// Test: exercised via `find_claude_pid_returns_none_for_nonexistent_session`.
fn process_name_contains_claude(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        if let Ok(comm) = std::fs::read_to_string(format!("/proc/{pid}/comm")) {
            return comm.to_ascii_lowercase().contains("claude");
        }
    }
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .output();
    match output {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .to_ascii_lowercase()
            .contains("claude"),
        _ => false,
    }
}

/// Check whether the process at `pid` is (still) a `claude` process.
///
/// Why: the PID-file orphan-GC ([`crate::core::pid_registry`]) must verify, just
/// before SIGTERMing a recorded PID, that the process at that PID is still named
/// `claude` — the alpha-1 mitigation for the PID-reuse race (spec §13.5). A
/// recycled PID now owned by an unrelated process fails this check and is spared.
/// What: a thin public wrapper over the crate-internal `comm`/`ps` name check,
/// returning `true` only when the process's command name contains `claude`
/// (case-insensitive), matching both `claude` and `claude-code`. A dead PID or
/// any lookup failure returns `false`.
/// Test: `process_name_is_claude_for_dead_pid_is_false` (a non-existent PID is
/// never `claude`); the positive path is covered by `find_claude_pid_in_tmux`.
pub fn process_name_is_claude(pid: u32) -> bool {
    process_name_contains_claude(pid)
}

/// Check whether a process with the given PID is still alive.
///
/// Why: the daemon's reaper must distinguish a tmux session whose `claude`
/// process is still running from one that has dropped back to a bare shell.
/// What: uses `kill(pid, 0)` (POSIX) — a null signal that performs the
/// permission/existence check without delivering anything. Returns `true` when
/// the process exists (signal sent, or it exists but is owned by another user),
/// `false` only when no such process exists. A PID outside the positive `pid_t`
/// range is treated as dead — `kill` interprets `0` and negative values as
/// process groups, never a single process.
/// Test: `is_process_alive_current_process`, `is_process_alive_dead_pid`.
pub fn is_process_alive(pid: u32) -> bool {
    use nix::errno::Errno;
    use nix::sys::signal::kill;
    use nix::unistd::Pid;

    // A real process PID is a positive `pid_t` (i32). Reject anything that does
    // not fit, including `0` (current process group) and `u32::MAX` (which
    // would wrap to `-1`, meaning "every process").
    let Ok(raw) = i32::try_from(pid) else {
        return false;
    };
    if raw <= 0 {
        return false;
    }

    match kill(Pid::from_raw(raw), None) {
        Ok(()) => true,
        // EPERM: the process exists but is owned by another user.
        Err(Errno::EPERM) => true,
        // ESRCH (or anything else): no such process.
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_claude_pid_returns_none_for_nonexistent_session() {
        // A bogus tmux session name must yield `None` without panicking, even
        // when tmux itself is not installed in the test environment.
        let pid = find_claude_pid_in_tmux(
            "tmpm-definitely-not-a-real-session-xyz",
            2,
            Duration::from_millis(1),
        );
        assert_eq!(pid, None);
    }

    #[test]
    fn is_process_alive_current_process() {
        // The test process itself is, by definition, alive.
        assert!(is_process_alive(std::process::id()));
    }

    #[test]
    fn is_process_alive_dead_pid() {
        // u32::MAX is far above any real PID — no such process can exist.
        assert!(!is_process_alive(u32::MAX));
    }

    #[test]
    fn process_name_is_claude_for_dead_pid_is_false() {
        // A non-existent PID can never be a live `claude` process.
        assert!(!process_name_is_claude(u32::MAX));
    }

    /// Cheap, always-run sanity check for the process-tree helpers the #2997
    /// one-hop walk is built from ([`child_pids_of`], [`is_disclaim_wrapper`],
    /// [`process_args`]) against a REAL spawned child — no tmux needed.
    ///
    /// Why: the ignored live test below needs a real tmux + macOS; this guards
    /// the helpers in ordinary CI so a regression in the child-listing / argv
    /// read is caught without the heavier harness.
    #[test]
    fn tree_helpers_see_a_real_child_and_reject_a_non_wrapper() {
        use std::process::Stdio;
        let mut child = Command::new("sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sleep");
        let cpid = child.id();

        // child_pids_of lists the freshly spawned direct child.
        assert!(
            child_pids_of(std::process::id()).contains(&cpid),
            "child_pids_of must include the spawned sleep pid {cpid}"
        );
        // A plain `sleep` carries no disclaim token → not the wrapper.
        assert!(
            !is_disclaim_wrapper(cpid),
            "a bare sleep is not the wrapper"
        );
        // Its argv is readable and mentions the program.
        assert!(
            process_args(cpid).unwrap_or_default().contains("sleep"),
            "process_args must read the child's argv"
        );

        let _ = child.kill();
        let _ = child.wait();
    }

    /// Live end-to-end proof that PID discovery walks one hop through the
    /// #2997 disclaim wrapper (the class the pure string tests cannot catch).
    ///
    /// Reconstructs the exact wrapped pane topology in a real tmux session:
    /// `pane sh → <sh whose argv carries the internal-spawn-disclaimed token>
    /// → <a process `ps` reports as `claude`>`. The middle `sh` SPAWNS
    /// (backgrounds) the fake claude and `wait`s — mirroring the real shim's
    /// posix_spawn+reap, so the fake claude is a GRANDCHILD of the pane's
    /// process exactly as the real one is; `find_claude_pid_in_tmux` must
    /// resolve that grandchild through the wrapper.
    ///
    /// The session is created with the command inline (not `send-keys`, which
    /// races zsh's line-editor startup) on the DEFAULT tmux socket (so the
    /// production `find_claude_pid_in_tmux`, which shells out to a bare `tmux`,
    /// can see it); cleanup kills only this session, never the server. The fake
    /// `claude` is a SYMLINK to `/bin/sleep` — a copy would be SIGKILLed by
    /// macOS AMFI as an unsigned clone of a SIP binary, whereas the symlink
    /// execs the real signed `sleep` while `ps -o comm=` still reports the
    /// `claude` path.
    ///
    /// macOS-gated + `#[ignore]`: the disclaim wrapper only ever exists on
    /// macOS, and the symlink-`comm` trick is macOS-specific. Run with
    /// `cargo test -p trusty-mpm -- --include-ignored`.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn claude_pid_resolves_through_disclaim_wrapper() {
        if Command::new("tmux").arg("-V").output().is_err() {
            eprintln!("tmux not available; skipping");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        // A process `ps -o comm=` reports as `claude`: a symlink to /bin/sleep.
        std::os::unix::fs::symlink("/bin/sleep", dir.join("claude")).unwrap();
        // wrap.sh carries the token in ITS argv and SPAWNS claude (bg + wait,
        // not exec) → claude is a grandchild.
        let wrap = dir.join("wrap.sh");
        std::fs::write(
            &wrap,
            format!("#!/bin/sh\n\"{}/claude\" 60 &\nwait\n", dir.display()),
        )
        .unwrap();
        // drv.sh is the pane's process; it SPAWNS wrap.sh (bg + wait) so wrap
        // sits one level below the pane process, forcing the one-hop walk.
        let drv = dir.join("drv.sh");
        std::fs::write(
            &drv,
            format!(
                "#!/bin/sh\nsh \"{}\" {} &\nwait\n",
                wrap.display(),
                crate::core::spawn_disclaim::PANE_DISCLAIM_SUBCOMMAND
            ),
        )
        .unwrap();

        let session = format!("tmpm-disclaim-pid-{}", std::process::id());
        let tmux = |args: &[&str]| Command::new("tmux").args(args).status();
        let _ = tmux(&["kill-session", "-t", &session]);
        tmux(&[
            "new-session",
            "-d",
            "-s",
            &session,
            &format!("sh {}", drv.display()),
        ])
        .unwrap();

        let pid = find_claude_pid_in_tmux(&session, 25, Duration::from_millis(200));
        // Verify the resolved pid IS the claude grandchild WHILE the session is
        // still alive — kill-session below tears down the whole tree, so this
        // check must precede cleanup.
        let named_claude = pid.map(process_name_is_claude);
        let _ = tmux(&["kill-session", "-t", &session]);

        let pid = pid.expect("claude pid must resolve through the disclaim wrapper");
        assert_eq!(
            named_claude,
            Some(true),
            "resolved pid {pid} must be the fake claude grandchild reached via the wrapper hop"
        );
    }
}
