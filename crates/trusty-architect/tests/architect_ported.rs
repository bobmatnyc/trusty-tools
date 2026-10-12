//! #9171 A3b-2: the prototype's window-rename and `architect_busy` tests,
//! ported by name.
//!
//! Sources at Architect commit 988152c: `scripts/test_fleet_poll_windows.py`
//! (plan cases 15-17) and `scripts/test_fleet_poll_watchdog.py:203,211`
//! (cases 18-19, Architect ruling Q3). The prototype passes a fake `runner`
//! or `capturer`; the ports script tmux (`ScriptedTmux`), so every argv is
//! recorded.

use trusty_architect::poller::{Pane, architect_busy, plan_window_renames, rename_windows};
use trusty_architect::{PaneId, ScriptedTmux};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn owned(rows: &[(&str, &str, &str)]) -> Vec<(String, String, String)> {
    rows.iter()
        .map(|(a, b, c)| ((*a).to_owned(), (*b).to_owned(), (*c).to_owned()))
        .collect()
}

// Python: test_mismatch_is_planned_and_match_is_left_alone
#[test]
fn test_mismatch_is_planned_and_match_is_left_alone() {
    let listing = "tm-a\t0\t2.1.287\ntm-b\t0\ttm-b\ntm-a\t1\ttm-a\n";
    assert_eq!(
        plan_window_renames(listing),
        owned(&[("tm-a", "0", "2.1.287")])
    );
}

// Python: test_non_tm_sessions_are_ignored
#[test]
fn test_non_tm_sessions_are_ignored() {
    assert!(plan_window_renames("work\t0\tzsh\nmain\t1\t2.1.287\n").is_empty());
}

// Python: test_runner_renames_with_exact_target_and_no_send_keys
#[test]
fn test_runner_renames_with_exact_target_and_no_send_keys() {
    let tmux = ScriptedTmux::new()
        .script("list-windows", "tm-architect\t0\t2.1.287\nother\t0\tzsh\n")
        .fallback("rename-window", "");
    let (done, _log) = rename_windows(&tmux, false);
    assert_eq!(done, 1);
    let calls = tmux.calls();
    assert_eq!(
        calls[1],
        ["rename-window", "-t", "=tm-architect:0", "tm-architect"]
    );
    assert!(
        calls
            .iter()
            .all(|c| c[0] == "list-windows" || c[0] == "rename-window"),
        "{calls:?}"
    );
}

/// A `list-windows` failure is logged and renames nothing; a failed rename
/// is logged and does not stop the next one. Neither raises.
#[test]
fn windows_list_failure_is_logged_not_raised() {
    let tmux = ScriptedTmux::new().fail("list-windows", "no server running");
    let (done, log) = rename_windows(&tmux, false);
    assert_eq!(done, 0);
    assert!(
        log.iter()
            .any(|l| l.starts_with("window-rename list ERROR")),
        "{log:?}"
    );

    let tmux = ScriptedTmux::new()
        .script("list-windows", "tm-a\t0\tx\ntm-b\t2\ty\n")
        .fail("rename-window", "can't find window")
        .fallback("rename-window", "");
    let (done, log) = rename_windows(&tmux, false);
    assert_eq!(done, 1, "{log:?}");
    assert!(log[0].starts_with("window-rename tm-a:0 ERROR"), "{log:?}");
    assert_eq!(log[1], "window-rename tm-b:2 'y' -> 'tm-b'");
}

/// A dry run lists what it would rename and calls nothing but `list-windows`.
#[test]
fn windows_dry_run_renames_nothing() {
    let tmux = ScriptedTmux::new().script("list-windows", "tm-a\t0\tx\n");
    let (done, log) = rename_windows(&tmux, true);
    assert_eq!(done, 0);
    assert_eq!(log, ["  would rename window tm-a:0 'x' -> 'tm-a'"]);
    assert_eq!(tmux.calls().len(), 1);
}

const SPIN: &str = "✻ Running PreToolUse hook… (2m 20s)";

fn arch(cmd: &str) -> Result<Vec<Pane>, trusty_architect::TmuxError> {
    Ok(vec![Pane {
        session: "tm-architect".to_owned(),
        pane: PaneId::parse("%1")?,
        cmd: cmd.to_owned(),
        active: String::new(),
        path: String::new(),
        dead: String::new(),
    }])
}

fn top() -> String {
    "─".repeat(40)
}

// Python: test_architect_busy_reads_spinner
#[test]
fn test_architect_busy_reads_spinner() -> TestResult {
    let top = top();
    let pane = format!("{SPIN}\n\n{top}\n❯ \n{top}\n  footer\n");
    let tmux = ScriptedTmux::new().fallback("capture-pane", &pane);
    assert_eq!(
        architect_busy(&tmux, &arch("claude")?)?,
        (true, SPIN.to_owned())
    );
    let calls = tmux.calls().len();
    assert_eq!(
        architect_busy(&tmux, &arch("zsh")?)?,
        (false, String::new())
    );
    assert_eq!(tmux.calls().len(), calls, "a shell pane is not captured");
    Ok(())
}

// Python: test_architect_busy_completed_turn_is_idle
#[test]
fn test_architect_busy_completed_turn_is_idle() -> TestResult {
    let top = top();
    let done = "✻ Brewed for 9s · done 7:14 AM · 1 monitor still running";
    let monitor = "◯ inbox-watch  tail events  1m 5s · ↓ 2 tokens";
    let pane = format!("{done}\n\n{top}\n❯ \n{top}\n  footer\n{monitor}\n");
    let tmux = ScriptedTmux::new().fallback("capture-pane", &pane);
    assert_eq!(
        architect_busy(&tmux, &arch("claude")?)?,
        (false, String::new())
    );
    let live = "✻ Brewing… (12s · ↓ 300 tokens)";
    let pane = format!("{live}\n\n{top}\n❯ \n{top}\n  footer\n");
    let tmux = ScriptedTmux::new().fallback("capture-pane", &pane);
    assert_eq!(
        architect_busy(&tmux, &arch("claude")?)?,
        (true, live.to_owned())
    );
    Ok(())
}

/// A capture failure is an error for the caller, never a reading of "idle".
#[test]
fn architect_busy_capture_failure_is_an_error() -> TestResult {
    let tmux = ScriptedTmux::new().fail("capture-pane", "can't find pane");
    assert!(architect_busy(&tmux, &arch("claude")?).is_err());
    Ok(())
}
