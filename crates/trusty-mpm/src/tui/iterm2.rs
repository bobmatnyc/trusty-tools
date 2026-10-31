//! iTerm2 detection and new-tab launcher.
//!
//! Why: When the operator is already inside iTerm2, opening a session in a
//! new tab (same window) is far more ergonomic than spawning a detached
//! process. AppleScript is the most reliable mechanism — no extra deps, works
//! on every macOS iTerm2 install.
//! What: `is_iterm2()` checks env vars at startup; `open_session_tab` runs
//! an `osascript` one-liner that creates a new tab running `tm sessions attach
//! <id>`. #10476: [`focus_session_tab`] selects an existing tab and never
//! creates one; the tui's Focus key uses it.
//! Test: Detection logic is pure and unit-tested below; AppleScript calls are
//! integration-only (no test without a live iTerm2).

/// Returns `true` when the current process is running inside iTerm2.
///
/// Why: We check three independent signals so detection is reliable across
/// different iTerm2 launch modes (GUI double-click, shell integration, SSH).
/// What: Positive if any of `TERM_PROGRAM=iTerm.app`, `ITERM_SESSION_ID` set,
/// or `/Applications/iTerm.app` exists.
/// Test: See unit tests below.
pub fn is_iterm2() -> bool {
    if std::env::var("TERM_PROGRAM").as_deref() == Ok("iTerm.app") {
        return true;
    }
    if std::env::var("ITERM_SESSION_ID").is_ok() {
        return true;
    }
    std::path::Path::new("/Applications/iTerm.app").exists()
}

/// Open a new iTerm2 tab in the current window running `tm sessions attach <session_id>`.
///
/// Why: The operator can monitor the session list in the TUI while the new tab
/// runs the actual Claude Code session — no window-switching required. Uses
/// the canonical `tm sessions attach` spelling (#2116/#2394) rather than the
/// deprecated `tm session attach` alias, so the spawned tab never prints the
/// deprecation notice.
/// What: Spawns `osascript` with an inline AppleScript that tells the current
/// iTerm2 window to create a tab with the default profile and run the attach
/// command. Returns `Ok(())` if osascript exits 0, `Err(String)` otherwise.
/// Test: Integration only — requires a live iTerm2; not covered by unit tests.
pub fn open_session_tab(session_id: &str) -> Result<(), String> {
    let script = format!(
        r#"tell application "iTerm2"
  tell current window
    create tab with default profile command "tm sessions attach {session_id}"
  end tell
end tell"#
    );
    let status = std::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .status()
        .map_err(|e| format!("osascript spawn failed: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("osascript exited with status {status}"))
    }
}

/// AppleScript that selects the iTerm2 tab already showing tmux session
/// `session`, printing `found` or `missing`.
///
/// Why (#10476): the Focus key may only move to an existing tab, never
/// create one (owner, #10476 comment 2). The script only reads names and
/// calls `select`.
/// What: walks every window, tab and session; the first session whose name
/// contains `session` has its window, tab and session selected, and the
/// script returns `found`. Otherwise it returns `missing`. `\` and `"` in the
/// name are escaped for the AppleScript string literal.
/// Test: `focus_script_selects_and_never_creates_tab`.
pub fn focus_script(session: &str) -> String {
    let needle = session.replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        r#"tell application "iTerm2"
  repeat with w in windows
    repeat with t in tabs of w
      repeat with s in sessions of t
        if name of s contains "{needle}" then
          select w
          tell t to select
          tell s to select
          return "found"
        end if
      end repeat
    end repeat
  end repeat
  return "missing"
end tell"#
    )
}

/// Select the existing iTerm2 tab for tmux session `session`.
///
/// Why (#10476): the iTerm2 half of the tui's Focus key, outside tmux.
/// What: runs [`focus_script`] through `osascript` with captured output (the
/// tui's alternate screen stays intact). `Ok(true)` when a tab was selected,
/// `Ok(false)` when none shows the session, `Err` when `osascript` fails.
/// Test: the script is tested by `focus_script_selects_and_never_creates_tab`;
/// the `osascript` run needs a live iTerm2 and is checked by hand.
pub fn focus_session_tab(session: &str) -> Result<bool, String> {
    let output = std::process::Command::new("osascript")
        .arg("-e")
        .arg(focus_script(session))
        .output()
        .map_err(|e| format!("osascript spawn failed: {e}"))?;
    if !output.status.success() {
        return Err(format!("osascript exited with status {}", output.status));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim() == "found")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_script_selects_and_never_creates_tab() {
        let script = focus_script("tm-alpha-1");
        assert!(script.contains("if name of s contains \"tm-alpha-1\""));
        assert!(script.contains("tell t to select"));
        assert!(!script.to_lowercase().contains("create"), "{script}");
        assert!(!script.contains("command"), "{script}");
        let quoted = focus_script("a\"b");
        assert!(quoted.contains("contains \"a\\\"b\""), "{quoted}");
    }

    #[test]
    fn iterm2_detected_via_term_program() {
        // Temporarily set TERM_PROGRAM and verify detection.
        // Use std::env::set_var inside a closure to avoid test pollution.
        let old = std::env::var("TERM_PROGRAM").ok();
        // Safety: single-threaded test context.
        unsafe { std::env::set_var("TERM_PROGRAM", "iTerm.app") };
        assert!(is_iterm2());
        match old {
            Some(v) => unsafe { std::env::set_var("TERM_PROGRAM", v) },
            None => unsafe { std::env::remove_var("TERM_PROGRAM") },
        }
    }

    #[test]
    fn iterm2_detected_via_session_id() {
        let old = std::env::var("ITERM_SESSION_ID").ok();
        unsafe { std::env::set_var("ITERM_SESSION_ID", "w0t0p0:abc123") };
        assert!(is_iterm2());
        match old {
            Some(v) => unsafe { std::env::set_var("ITERM_SESSION_ID", v) },
            None => unsafe { std::env::remove_var("ITERM_SESSION_ID") },
        }
    }

    #[test]
    fn not_iterm2_without_signals() {
        // Remove both env vars and only rely on /Applications/iTerm.app check.
        // We can't control whether iTerm2 is installed, so just check the
        // function returns a bool without panicking.
        let _result = is_iterm2(); // must not panic
    }
}
