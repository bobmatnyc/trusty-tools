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

/// Find a direct child of `shell_pid` whose command name contains `claude`.
///
/// Why: after `send-keys "claude"`, the `claude` process is a child of the
/// pane's shell; matching by command name avoids picking up unrelated children.
/// What: runs `pgrep -P <shell_pid>` and returns the first child whose process
/// name (`/proc/<pid>/comm` on Linux, `ps -p <pid> -o comm=` on macOS) contains
/// `claude`.
/// Test: exercised via `find_claude_pid_returns_none_for_nonexistent_session`.
fn claude_child_of(shell_pid: u32) -> Option<u32> {
    let output = Command::new("pgrep")
        .args(["-P", &shell_pid.to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .filter_map(|line| line.trim().parse::<u32>().ok())
        .find(|&pid| process_name_contains_claude(pid))
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
}
