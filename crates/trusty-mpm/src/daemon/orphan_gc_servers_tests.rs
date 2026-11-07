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
