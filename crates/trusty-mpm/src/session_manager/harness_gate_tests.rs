//! #10136: the real driver's pane probe, which `require_live_harness` reads,
//! against live tmux panes on a private server.

use std::process::Command;
use std::time::{Duration, Instant};

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;

use crate::core::pane_foreground_tests::KillOnDrop;
use crate::core::process::{PaneClaude, harness_pids_under_pane, harness_under_pane};
use crate::session_manager::{ManagedTmuxDriver, RealTmuxDriver};
use crate::test_support::tmux_session::{
    PrivateTmuxServer, ScratchTmuxSession, reserved_session_name,
};

/// Poll `ready` every 20 ms, failing after 10 s.
fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A live session and the `sleep` processes under its pane.
struct SleepPane {
    /// Declared first, so it drops first: it kills the pane's `sleep`s while
    /// the session still roots them (#10136 critic MEDIUM).
    _reaper: KillOnDrop,
    _session: ScratchTmuxSession,
    name: String,
    pane: String,
    pid: u32,
}

/// A session on `server` whose pane runs `cmd`, returned once a process
/// named `sleep` runs under the pane.
fn sleep_pane(
    server: &PrivateTmuxServer,
    driver: &RealTmuxDriver,
    tag: &str,
    cmd: &str,
) -> SleepPane {
    let name = reserved_session_name(tag);
    let session = ScratchTmuxSession::spawn_on_socket("tmux", Some(server.name()), &name, cmd);
    let pane = driver
        .get_pane_id(&name)
        .expect("the live session has a pane");
    // #8443: an exact target; a `%N` pane id renders unchanged.
    let target = trusty_common::tmux::exact_pane_target(&name, &pane);
    let pid = server
        .query(&["display-message", "-p", "-t", &target, "#{pane_pid}"])
        .and_then(|pid| pid.parse().ok())
        .expect("the pane pid");
    // Declared after `session`, so a timeout below drops it first.
    let reaper = KillOnDrop {
        children: Vec::new(),
        roots: vec![pid],
        harness: "sleep",
    };
    wait_until(&format!("`sleep` under pane {pane}"), || {
        harness_under_pane(pid, "sleep") == PaneClaude::Present
    });
    SleepPane {
        _reaper: reaper,
        _session: session,
        name,
        pane,
        pid,
    }
}

/// `ps -o stat=` for `pid`, empty when `ps` fails.
fn ps_stat(pid: u32) -> String {
    Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        .unwrap_or_default()
}

/// #10136: a harness holds its pane only while it is running in the tty's
/// foreground group. A background job and a stopped harness both read
/// `Unknown`, which the gate refuses as `harness_unconfirmed` — never
/// `Absent`, which relaunch and registration act on.
#[serial_test::serial]
#[test]
fn a_harness_in_a_pane_holds_it_only_in_the_foreground() {
    if !ScratchTmuxSession::tmux_available("tmux") {
        eprintln!("tmux not available; skipping");
        return;
    }
    let server = PrivateTmuxServer::new("tmux", "10136-foreground");
    let driver = RealTmuxDriver::from_driver_for_test(
        crate::daemon::tmux::TmuxDriver::with_tmux_path_for_test(server.shim_bin()),
    );

    // The pane process is the harness: the tty's foreground group.
    let fg = sleep_pane(&server, &driver, "10136-fg", "exec sleep 600");
    assert_eq!(
        driver.pane_harness(&fg.name, &fg.pane, "sleep"),
        PaneClaude::Present
    );

    // `set -m` puts the `&` job in its own group; `sh` keeps the tty.
    let bg = sleep_pane(
        &server,
        &driver,
        "10136-bg",
        "exec sh -c 'set -m; sleep 600 & wait'",
    );
    assert_eq!(
        driver.pane_harness(&bg.name, &bg.pane, "sleep"),
        PaneClaude::Unknown,
        "a background harness does not hold the pane"
    );

    // A stopped harness still in the foreground group (`T+`). tmux continues
    // a stopped pane process, so the harness is the shell's child; with no
    // job control the shell keeps waiting and never takes the tty back.
    let st = sleep_pane(&server, &driver, "10136-st", "exec sh -c 'sleep 600; :'");
    let (sleeps, _) = harness_pids_under_pane(st.pid, "sleep");
    let harness = *sleeps.first().expect("the shell's sleep");
    let pid = Pid::from_raw(i32::try_from(harness).expect("a pid"));
    kill(pid, Signal::SIGSTOP).expect("stop the harness");
    wait_until("the harness to stop", || ps_stat(harness).starts_with('T'));
    let stopped = driver.pane_harness(&st.name, &st.pane, "sleep");
    assert_eq!(
        stopped,
        PaneClaude::Unknown,
        "a stopped harness does not hold the pane"
    );
}
