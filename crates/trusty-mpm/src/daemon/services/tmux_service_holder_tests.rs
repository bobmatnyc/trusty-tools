//! #10101: a by-name daemon call reaches the one server holding the session.
//! A scripted fake tmux answers per server; no real tmux runs.

use std::os::unix::net::UnixListener;

use super::holder_of;
use crate::daemon::tmux::TmuxDriver;

/// A scratch dir with live sockets for `servers`, and a fake tmux whose
/// default server lists `default` and whose named servers list `named`.
fn fixture(
    servers: &[&str],
    default: &[&str],
    named: &[(&str, &[&str])],
) -> (tempfile::TempDir, Vec<UnixListener>, TmuxDriver) {
    let dir = crate::test_support::hermetic_temp_dir();
    let listeners = servers
        .iter()
        .map(|s| UnixListener::bind(dir.path().join(s)).expect("bind"))
        .collect();
    let rows = |names: &[&str]| -> String {
        names
            .iter()
            .map(|n| format!("echo '{n}:1700000000:0';"))
            .collect()
    };
    let mut cases = String::new();
    for (server, names) in named {
        cases.push_str(&format!(
            "if [ \"$2\" = '{server}' ] && [ \"$3\" = list-sessions ]; then {} exit 0; fi\n",
            rows(names)
        ));
    }
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = -L ]; then\n{cases}exit 0\nfi\n\
         if [ \"$1\" = list-sessions ]; then {} exit 0; fi\nexit 0\n",
        rows(default)
    );
    let bin = dir.path().join("fake-tmux");
    trusty_common::exec_stub::write_executable(&bin, script).expect("fake tmux");
    let driver = TmuxDriver::with_tmux_path_for_test(bin.to_string_lossy().into_owned());
    (dir, listeners, driver)
}

/// Error arm (Fail-Open Check): a session only a workspace server holds is
/// reached there, never on the default server. Red on 420b3717bf, whose
/// capture/send/input-box always used the default server.
#[serial_test::serial]
#[test]
fn a_by_name_call_reaches_the_server_that_holds_the_session() {
    let (dir, _l, driver) = fixture(
        &["tm-ws-a", "tm-ws-b"],
        &["tm-other"],
        &[("tm-ws-a", &[]), ("tm-ws-b", &["tm-pm"])],
    );
    let holder = holder_of(&driver, dir.path(), "tm-pm").expect("one holder");
    assert_eq!(holder.server(), Some("tm-ws-b"));
    let default = holder_of(&driver, dir.path(), "tm-other").expect("default holds it");
    assert_eq!(default.server(), None);
    // No holder: the default server answers its own "no session".
    assert_eq!(
        holder_of(&driver, dir.path(), "tm-none")
            .expect("none")
            .server(),
        None
    );
}

/// Error arm (Fail-Open Check): a name two servers hold is refused.
#[serial_test::serial]
#[test]
fn a_by_name_call_refuses_a_name_on_two_servers() {
    let (dir, _l, driver) = fixture(&["tm-ws-a"], &["tm-pm"], &[("tm-ws-a", &["tm-pm"])]);
    let err = holder_of(&driver, dir.path(), "tm-pm").expect_err("two holders");
    assert!(err.contains("2 servers"), "{err}");
}

/// Error arm (Fail-Open Check): a workspace server whose `list-sessions`
/// hangs makes every by-name lookup refuse within the probe budget, and only
/// one listing of it is ever started while it hangs. Red on 2df3760494, whose
/// lookup listed the server with no bound and blocked on it.
#[serial_test::serial]
#[test]
fn a_hung_workspace_server_holds_one_probe_and_refuses_the_call() {
    let dir = crate::test_support::hermetic_temp_dir();
    let _live = UnixListener::bind(dir.path().join("tm-ws-hung")).expect("bind");
    let hang = crate::test_support::HangFifo::new(dir.path());
    let starts = dir.path().join("starts");
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = -L ] && [ \"$3\" = list-sessions ]; then\n\
         echo x >> '{}'; read _ < '{}'; exit 0; fi\n\
         if [ \"$1\" = list-sessions ]; then echo 'tm-other:1700000000:0'; fi\nexit 0\n",
        starts.display(),
        hang.path().display()
    );
    let bin = dir.path().join("fake-tmux");
    trusty_common::exec_stub::write_executable(&bin, script).expect("fake tmux");
    let driver = TmuxDriver::with_tmux_path_for_test(bin.to_string_lossy().into_owned());
    let budget = crate::core::tmux_server::PROBE_BUDGET + std::time::Duration::from_secs(1);

    for round in 0..3 {
        let (d, socket_dir) = (driver.clone(), dir.path().to_path_buf());
        let answer = crate::core::tmux_server::bounded_call(budget, move || {
            holder_of(&d, &socket_dir, "tm-pm").map(|h| h.server().map(str::to_owned))
        });
        let err = answer
            .unwrap_or_else(|| panic!("round {round} was not bounded"))
            .expect_err("unknown holder");
        assert!(err.contains("tm-ws-hung"), "{err}");
    }
    let started = std::fs::read_to_string(&starts).expect("starts");
    assert_eq!(started.lines().count(), 1, "listings started: {started:?}");
}
