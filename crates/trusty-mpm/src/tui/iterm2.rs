//! iTerm2 detection and new-tab launcher.
//!
//! Why: When the operator is already inside iTerm2, opening a session in a
//! new tab (same window) is far more ergonomic than spawning a detached
//! process. AppleScript is the most reliable mechanism — no extra deps, works
//! on every macOS iTerm2 install.
//! What: `is_iterm2()` checks env vars at startup; `open_session_tab` runs
//! an `osascript` one-liner that creates a new tab running `tm sessions attach
//! <id>`. #10476: [`focus_session_tab`] selects the one existing tab whose
//! session is named exactly the tmux session, under a [`FOCUS_TIMEOUT`]
//! bound, and never creates one; the tui's Focus key uses it.
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

/// How long the Focus key waits on `osascript`, both calls together.
///
/// #10476: a hung iTerm2 or a pending Automation prompt must not hold the tui.
pub const FOCUS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// What the iTerm2 half of the Focus key found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItermMatch {
    /// Exactly one tab's session was named the tmux session; it is selected.
    Selected,
    /// No tab's session has that exact name; nothing was selected.
    Missing,
    /// This many tabs' sessions have that exact name; nothing was selected.
    Ambiguous(usize),
}

/// AppleScript that prints every iTerm2 session as `<id>\t<name>` lines.
///
/// Why (#10476): matching happens in Rust ([`pick_exact`]), so the exact-name
/// and ambiguity rules are testable without iTerm2. The script only reads.
pub fn list_script() -> &'static str {
    r#"tell application "iTerm2"
  set out to ""
  repeat with w in windows
    repeat with t in tabs of w
      repeat with s in sessions of t
        set out to out & (id of s) & tab & (name of s) & linefeed
      end repeat
    end repeat
  end repeat
  return out
end tell"#
}

/// AppleScript that selects the iTerm2 session with id `session_id`, printing
/// `found` or `missing`.
///
/// Why (#10476): the Focus key may only move to an existing tab, never
/// create one (owner, #10476 comment 2). The script compares ids and calls
/// `select`; it creates nothing.
/// What: walks every window, tab and session; the session whose `id` equals
/// `session_id` has its window, tab and session selected. `\` and `"` are
/// escaped for the AppleScript string literal.
/// Test: `focus_script_selects_and_never_creates_tab`.
pub fn focus_script(session_id: &str) -> String {
    let needle = session_id.replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        r#"tell application "iTerm2"
  repeat with w in windows
    repeat with t in tabs of w
      repeat with s in sessions of t
        if (id of s) is "{needle}" then
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

/// Parse [`list_script`] output into `(id, name)` pairs.
fn parse_sessions(out: &str) -> Vec<(String, String)> {
    out.lines()
        .filter_map(|line| line.split_once('\t'))
        .map(|(id, name)| (id.to_string(), name.to_string()))
        .collect()
}

/// The one session id whose name is exactly `name`, or why there is none.
///
/// Why (#10476 review): a substring match sent `tm-x-1` to the `tm-x-10`
/// tab, and an empty name matched every tab. Only an exact, unique name may
/// pick a tab; anything else is a notice, never a guess.
/// What: `Ok(id)` for exactly one exact match, case-sensitive and untrimmed;
/// `Err(Missing)` for none; `Err(Ambiguous(n))` for `n > 1`. An empty `name`
/// is `Err(Missing)`.
/// Test: `exact_name_does_not_select_a_longer_name`,
/// `two_exact_matches_are_ambiguous`.
pub fn pick_exact(sessions: &[(String, String)], name: &str) -> Result<String, ItermMatch> {
    if name.is_empty() {
        return Err(ItermMatch::Missing);
    }
    let hits: Vec<&String> = sessions
        .iter()
        .filter(|(_, n)| n == name)
        .map(|(id, _)| id)
        .collect();
    match hits.as_slice() {
        [] => Err(ItermMatch::Missing),
        [id] => Ok((*id).clone()),
        many => Err(ItermMatch::Ambiguous(many.len())),
    }
}

/// Run `program args`, waiting until `deadline`; kill and reap it after.
///
/// Why (#10476 review): `osascript` can hang (iTerm2 busy, an Automation
/// consent prompt). It runs as a tokio child, so the async worker is never
/// blocked, and the wait is bounded.
/// What: stdout on a zero exit; `Err` on a spawn failure, a non-zero exit, or
/// the deadline. On the deadline the child is killed and awaited, so no
/// process is left behind.
/// Test: `bounded_run_kills_the_child_at_the_deadline` (paused clock),
/// `bounded_run_returns_stdout_before_the_deadline`.
pub(crate) async fn run_bounded(
    program: &str,
    args: &[String],
    deadline: tokio::time::Instant,
) -> Result<String, String> {
    use tokio::io::AsyncReadExt as _;
    let mut child = tokio::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("{program} spawn failed: {e}"))?;
    let mut stdout = child.stdout.take();
    let run = async {
        let mut out = String::new();
        if let Some(pipe) = stdout.as_mut() {
            pipe.read_to_string(&mut out)
                .await
                .map_err(|e| format!("{program} output: {e}"))?;
        }
        let status = child
            .wait()
            .await
            .map_err(|e| format!("{program} wait: {e}"))?;
        if status.success() {
            Ok(out)
        } else {
            Err(format!("{program} exited with status {status}"))
        }
    };
    match tokio::time::timeout_at(deadline, run).await {
        Ok(result) => result,
        Err(_) => {
            // `kill` sends SIGKILL and waits, so the child is reaped here.
            let _ = child.kill().await;
            Err(format!(
                "{program} timed out after {}s; nothing selected",
                FOCUS_TIMEOUT.as_secs()
            ))
        }
    }
}

/// Select the existing iTerm2 tab whose session is named exactly `session`.
///
/// Why (#10476): the iTerm2 half of the tui's Focus key, outside tmux.
/// What: refuses an empty name before any `osascript` runs. Otherwise lists
/// the sessions ([`list_script`]), picks the exact unique name
/// ([`pick_exact`]) and selects it by id ([`focus_script`]). Both calls share
/// one [`FOCUS_TIMEOUT`] deadline through [`run_bounded`]; output is captured,
/// so the tui's alternate screen stays intact.
/// Test: `empty_name_is_refused_before_osascript`; the matching and the bound
/// have their own tests; the live `osascript` run is checked by hand.
pub async fn focus_session_tab(session: &str) -> Result<ItermMatch, String> {
    if session.trim().is_empty() {
        return Err("no tmux session name to focus".to_string());
    }
    let deadline = tokio::time::Instant::now() + FOCUS_TIMEOUT;
    let script = |s: String| vec!["-e".to_string(), s];
    let listed = run_bounded("osascript", &script(list_script().to_string()), deadline).await?;
    let id = match pick_exact(&parse_sessions(&listed), session) {
        Ok(id) => id,
        Err(outcome) => return Ok(outcome),
    };
    let picked = run_bounded("osascript", &script(focus_script(&id)), deadline).await?;
    Ok(if picked.trim() == "found" {
        ItermMatch::Selected
    } else {
        ItermMatch::Missing
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_script_selects_and_never_creates_tab() {
        let script = focus_script("w0t0p0:ABC");
        assert!(script.contains("if (id of s) is \"w0t0p0:ABC\""));
        assert!(script.contains("tell t to select"));
        for read_only in [script.as_str(), list_script()] {
            assert!(!read_only.to_lowercase().contains("create"), "{read_only}");
            assert!(!read_only.contains("command"), "{read_only}");
            assert!(!read_only.contains("contains"), "{read_only}");
        }
        let quoted = focus_script("a\"b");
        assert!(quoted.contains("is \"a\\\"b\""), "{quoted}");
    }

    fn sessions(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(id, name)| (id.to_string(), name.to_string()))
            .collect()
    }

    #[test]
    fn exact_name_does_not_select_a_longer_name() {
        let listed = parse_sessions("id-10\ttm-x-10\nid-t\ttm-trusty-tools\n");
        assert_eq!(pick_exact(&listed, "tm-x-1"), Err(ItermMatch::Missing));
        assert_eq!(pick_exact(&listed, "tm-trusty"), Err(ItermMatch::Missing));
        assert_eq!(pick_exact(&listed, "tm-x-10"), Ok("id-10".to_string()));
        let both = sessions(&[("a", "tm-x-10"), ("b", "tm-x-1")]);
        assert_eq!(pick_exact(&both, "tm-x-1"), Ok("b".to_string()));
        assert_eq!(pick_exact(&both, ""), Err(ItermMatch::Missing));
    }

    #[test]
    fn two_exact_matches_are_ambiguous() {
        let listed = sessions(&[("a", "tm-x-1"), ("b", "tm-x-1"), ("c", "tm-x-2")]);
        assert_eq!(pick_exact(&listed, "tm-x-1"), Err(ItermMatch::Ambiguous(2)));
    }

    #[tokio::test]
    async fn empty_name_is_refused_before_osascript() {
        for name in ["", "  "] {
            let refused = focus_session_tab(name).await;
            assert_eq!(refused, Err("no tmux session name to focus".to_string()));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn bounded_run_kills_the_child_at_the_deadline() {
        // A paused clock jumps to the deadline once the runtime is idle, so no
        // wall-clock time is waited or asserted on.
        let deadline = tokio::time::Instant::now() + FOCUS_TIMEOUT;
        let result = run_bounded("sleep", &["30".to_string()], deadline).await;
        let err = result.expect_err("a 30 s child must hit the 5 s deadline");
        assert!(err.contains("timed out after 5s"), "{err}");
    }

    #[tokio::test]
    async fn bounded_run_returns_stdout_before_the_deadline() {
        // A real clock here: a paused one would jump past the deadline while
        // the child's output is still in flight. The deadline is a bound only.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
        let out = run_bounded("echo", &["hi".to_string()], deadline).await;
        assert_eq!(out.as_deref().map(str::trim), Ok("hi"));
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
