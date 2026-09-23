//! Tests for the `tmux_priority` doctor row (#8415).
//!
//! Every test drives the probe through a fake [`Runner`]; none starts, reads or
//! signals a real tmux server.

use super::*;

/// A canned answer from one fake subprocess.
type Answer = Result<CmdOut, String>;

fn ok(stdout: &str) -> Result<CmdOut, String> {
    Ok(CmdOut {
        success: true,
        stdout: stdout.to_owned(),
        stderr: String::new(),
    })
}

fn failed(stderr: &str) -> Result<CmdOut, String> {
    Ok(CmdOut {
        success: false,
        stdout: String::new(),
        stderr: stderr.to_owned(),
    })
}

/// Probe with canned answers for `tmux` and `ps`, then build the row.
fn row_for(
    tmux: Result<CmdOut, String>,
    ps: Result<CmdOut, String>,
) -> (TmuxPriority, DoctorCheck) {
    let run = move |program: &str, _args: &[&str]| -> Result<CmdOut, String> {
        if program == "ps" {
            ps.clone()
        } else {
            tmux.clone()
        }
    };
    let probe = probe_tmux_priority("tmux", &run);
    let row = build_tmux_priority_check(&probe);
    (probe, row)
}

/// REGRESSION (#8415): a server at the background band's priority 4 — and one
/// just under the threshold — fails, naming the PID, the priority and the
/// restart remedy.
#[test]
fn clamped_server_fails() {
    for priority in ["4", "19"] {
        let (_, row) = row_for(ok("5599\n"), ok(&format!("  {priority}\n")));
        assert_eq!(row.status, CheckStatus::Fail, "{}", row.message);
        assert!(row.message.contains("PID 5599"), "{}", row.message);
        assert!(
            row.message.contains(&format!("priority {priority}")),
            "{}",
            row.message
        );
        assert!(row.message.contains("tmux kill-server"), "{}", row.message);
        assert!(
            row.message.contains("launchd_process_type"),
            "{}",
            row.message
        );
    }
}

/// #8415: 20 to 30 is launchd `Standard` throttling — warn, pointing at the
/// plist row.
#[test]
fn standard_throttled_server_warns() {
    for priority in ["20", "30"] {
        let (_, row) = row_for(ok("20245\n"), ok(priority));
        assert_eq!(row.status, CheckStatus::Warn, "{}", row.message);
        assert!(row.message.contains("PID 20245"), "{}", row.message);
        assert!(
            row.message.contains("launchd_process_type"),
            "{}",
            row.message
        );
        assert!(row.message.contains("Standard"), "{}", row.message);
    }
}

/// An interactive 31, and anything above, passes.
#[test]
fn normal_server_passes() {
    for priority in ["31", "47"] {
        let (probe, row) = row_for(ok("20245\n"), ok(priority));
        assert_eq!(
            probe,
            TmuxPriority::Observed {
                pid: 20245,
                priority: priority.parse().expect("fixture")
            }
        );
        assert_eq!(row.status, CheckStatus::Ok, "{}", row.message);
    }
}

/// tmux's two no-server answers pass without calling `ps`.
#[test]
fn no_server_passes() {
    for stderr in [
        "no server running on /private/tmp/tmux-501/default\n",
        "error connecting to /private/tmp/tmux-501/default (No such file or directory)\n",
    ] {
        let (probe, row) = row_for(failed(stderr), Err("ps must not run".to_owned()));
        assert_eq!(probe, TmuxPriority::NoServer, "{stderr}");
        assert_eq!(row.status, CheckStatus::Ok, "{}", row.message);
    }
}

/// REGRESSION (#8415, fail-closed): every read that did not succeed is
/// `Unknown` with its reason — never `Ok`.
#[test]
fn probe_errors_are_unknown() {
    let cases: Vec<(&str, Answer, Answer)> = vec![
        (
            "tmux",
            Err("No such file or directory".to_owned()),
            ok("31"),
        ),
        (
            "display-message",
            failed("error connecting to /tmp/tmux-501/default (Permission denied)"),
            ok("31"),
        ),
        ("server PID", ok("not-a-pid\n"), ok("31")),
        ("server PID", ok("0\n"), ok("31")),
        ("ps", ok("5599\n"), Err("spawn failed".to_owned())),
        ("ps -o pri=", ok("5599\n"), failed("ps: no such process")),
        ("priority", ok("5599\n"), ok("\n")),
        ("priority", ok("5599\n"), ok("high\n")),
    ];
    for (reason, tmux, ps) in cases {
        let (probe, row) = row_for(tmux, ps);
        assert!(
            matches!(probe, TmuxPriority::Unreadable(_)),
            "{reason}: {probe:?}"
        );
        assert_eq!(
            row.status,
            CheckStatus::Unknown,
            "{reason}: {}",
            row.message
        );
        assert!(row.message.contains(reason), "{reason}: {}", row.message);
    }
}

/// #8415: off macOS the row never probes and never judges.
#[test]
fn other_platforms_are_not_applicable() {
    let row = tmux_priority_row(false, || panic!("the probe must not run off macOS"));
    assert_eq!(row.status, CheckStatus::Ok, "{}", row.message);
    assert!(
        row.message.contains("not applicable on this platform"),
        "{}",
        row.message
    );
    let row = tmux_priority_row(true, || TmuxPriority::Observed {
        pid: 7,
        priority: 4,
    });
    assert_eq!(row.status, CheckStatus::Fail, "{}", row.message);
}
