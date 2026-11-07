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
#[test]
fn a_by_name_call_refuses_a_name_on_two_servers() {
    let (dir, _l, driver) = fixture(&["tm-ws-a"], &["tm-pm"], &[("tm-ws-a", &["tm-pm"])]);
    let err = holder_of(&driver, dir.path(), "tm-pm").expect_err("two holders");
    assert!(err.contains("2 servers"), "{err}");
}
