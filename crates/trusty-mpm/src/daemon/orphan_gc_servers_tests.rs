//! #10101: the orphan GC reaps on workspace tmux servers (fake tmux).

use std::os::unix::net::UnixListener;

use super::*;
use crate::session_manager::RealTmuxDriver;

/// A probe that reports no live child for every pane.
struct NoChild;

impl ChildLivenessProbe for NoChild {
    fn has_live_child(&self, _pane_pid: Option<u32>) -> bool {
        false
    }
}

/// Error arm (Fail-Open Check): an untracked idle pane on a workspace server
/// is reaped on that server after the two-pass debounce, and a tracked one is
/// kept. Red on 420b3717bf, whose sweep listed only the default server.
#[serial_test::serial]
#[test]
fn an_orphan_on_a_workspace_server_is_reaped_there() {
    let dir = crate::test_support::hermetic_temp_dir();
    let _live = UnixListener::bind(dir.path().join("tm-ws-a")).expect("bind");
    let log = dir.path().join("calls.log");
    let bin = dir.path().join("fake-tmux");
    let script = format!(
        "#!/bin/sh\necho \"$*\" >> '{}'\n\
         if [ \"$1\" = -L ] && [ \"$2\" = tm-ws-a ] && [ \"$3\" = list-panes ]; then\n\
         printf 'tm-orphan\\tzsh\\t99991\\t%%1\\t/tmp\\ntm-kept\\tzsh\\t99992\\t%%2\\t/tmp\\n'; fi\nexit 0\n",
        log.display()
    );
    trusty_common::exec_stub::write_executable(&bin, script).expect("fake tmux");
    let driver = TmuxDriver::with_tmux_path_for_test(bin.to_string_lossy().into_owned());
    let mtmux = RealTmuxDriver::from_driver_for_test(driver.clone());
    let tracked = TrackedNames {
        managed: ["tm-kept".to_owned()].into_iter().collect(),
        ..TrackedNames::default()
    };
    let mut gc = WorkspaceServerGc::default();

    let first = gc.sweep_in(dir.path(), &tracked, &NoChild, &driver, &mtmux);
    let second = gc.sweep_in(dir.path(), &tracked, &NoChild, &driver, &mtmux);

    assert_eq!((first, second), (0, 1), "debounced, then reaped");
    let calls = std::fs::read_to_string(&log).expect("log");
    let kills: Vec<&str> = calls
        .lines()
        .filter(|l| l.contains("kill-session"))
        .collect();
    assert_eq!(kills.len(), 1, "{calls}");
    assert!(
        kills[0].starts_with("-L tm-ws-a ") && kills[0].contains("tm-orphan"),
        "{calls}"
    );
}

/// Error arm (Fail-Open Check): a workspace server whose `list-panes` hangs
/// is skipped within the probe budget on every sweep, and only one listing of
/// it is ever started while it hangs. Red on 2df3760494, whose sweep listed
/// the server with no bound and held the orphan-GC worker on it.
#[serial_test::serial]
#[test]
fn a_hung_workspace_server_is_listed_once_across_sweeps() {
    let dir = crate::test_support::hermetic_temp_dir();
    let _live = UnixListener::bind(dir.path().join("tm-ws-hung")).expect("bind");
    let hang = crate::test_support::HangFifo::new(dir.path());
    let starts = dir.path().join("starts");
    let bin = dir.path().join("fake-tmux");
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = -L ] && [ \"$3\" = list-panes ]; then\n\
         echo x >> '{}'; read _ < '{}'; fi\nexit 0\n",
        starts.display(),
        hang.path().display()
    );
    trusty_common::exec_stub::write_executable(&bin, script).expect("fake tmux");
    let driver = TmuxDriver::with_tmux_path_for_test(bin.to_string_lossy().into_owned());
    let budget = crate::core::tmux_server::PROBE_BUDGET + std::time::Duration::from_secs(1);

    let socket_dir = dir.path().to_path_buf();
    let swept = crate::core::tmux_server::bounded_call(budget * 3, move || {
        let mtmux = RealTmuxDriver::from_driver_for_test(driver.clone());
        let mut gc = WorkspaceServerGc::default();
        (0..3)
            .map(|_| {
                let clock = std::time::Instant::now();
                let reaped = gc.sweep_in(
                    &socket_dir,
                    &TrackedNames::default(),
                    &NoChild,
                    &driver,
                    &mtmux,
                );
                (reaped, clock.elapsed())
            })
            .collect::<Vec<_>>()
    })
    .expect("the sweeps were not bounded");

    for (round, (reaped, took)) in swept.iter().enumerate() {
        assert_eq!(*reaped, 0, "round {round} reaped on an unlisted server");
        assert!(*took < budget, "round {round} took {took:?}");
    }
    let started = std::fs::read_to_string(&starts).expect("starts");
    assert_eq!(started.lines().count(), 1, "listings started: {started:?}");
}
