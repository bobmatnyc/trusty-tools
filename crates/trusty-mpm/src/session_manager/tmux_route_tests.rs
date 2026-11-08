//! Tests for `tmux_route` (#10101): naming a record's server, routing a
//! driver to it, and the (server, session) live set.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use super::*;
use crate::session_manager::real_tmux::FakeNoopTmuxDriver;

/// A record named `name` on the server whose socket is `socket`.
fn record(name: &str, socket: Option<&str>) -> SessionRecord {
    let mut raw = serde_json::json!({
        "id": crate::session_manager::ManagedSessionId::new().to_string(),
        "task": "t", "tmux_name": name, "cwd": "/tmp",
        "state": "active", "created_at": "2026-11-07T00:00:00Z",
    });
    if let Some(socket) = socket {
        raw["tmux_socket"] = serde_json::json!(socket);
    }
    serde_json::from_value(raw).expect("record")
}

/// A driver whose one server lists `names`, and which reaches the named
/// servers in `servers` (each listing its own names).
struct ListingDriver {
    names: Vec<String>,
    servers: HashMap<String, Vec<String>>,
    /// The workspace servers this driver reports as live.
    live: Vec<String>,
}

impl ManagedTmuxDriver for ListingDriver {
    fn create_session(&self, _n: &str, _w: &str) -> Result<(), ManagedError> {
        Ok(())
    }
    fn kill_session(&self, _n: &str) -> Result<(), ManagedError> {
        Ok(())
    }
    fn send_line(&self, _n: &str, _t: &str) -> Result<(), ManagedError> {
        Ok(())
    }
    fn capture(&self, _n: &str, _l: usize) -> Result<String, ManagedError> {
        Ok(String::new())
    }
    fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
        Ok(self.names.clone())
    }
    fn on_server(&self, server: &str) -> Option<Arc<dyn ManagedTmuxDriver>> {
        let names = self.servers.get(server)?.clone();
        Some(Arc::new(ListingDriver {
            names,
            servers: HashMap::new(),
            live: Vec::new(),
        }))
    }
    fn workspace_servers(&self) -> Result<Vec<String>, ManagedError> {
        Ok(self.live.clone())
    }
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|n| (*n).to_owned()).collect()
}

#[test]
fn a_socket_path_names_its_server() {
    assert_eq!(record_server(&record("tm-a", None)).expect("default"), None);
    let named = record("tm-a", Some("/tmp/tmux-501/tm-ws-repo-0123"));
    assert_eq!(
        record_server(&named).expect("named").as_deref(),
        Some("tm-ws-repo-0123")
    );
}

#[test]
fn an_unnamed_socket_path_refuses() {
    let err = record_server(&record("tm-a", Some("/"))).expect_err("no file name");
    assert!(matches!(err, ManagedError::TmuxUnavailable(_)), "{err:?}");
}

/// Error arm (Fail-Open Check): a driver that cannot reach the record's
/// server refuses; the caller's default-server driver is not used instead.
#[test]
fn a_driver_that_cannot_reach_a_named_server_refuses() {
    let tmux = FakeNoopTmuxDriver;
    let rec = record("tm-a", Some("/tmp/tmux-501/tm-ws-x"));
    let err = route(&tmux, &rec).err().expect("no route to tm-ws-x");
    assert!(err.to_string().contains("tm-ws-x"), "{err}");
    assert!(
        !route(&tmux, &record("tm-a", None))
            .expect("default")
            .is_named()
    );
}

#[test]
fn live_sessions_hold_a_record_only_on_its_own_server() {
    let live = LiveSessions {
        default: HashSet::from(["tm-a".to_owned()]),
        named: HashMap::from([("tm-ws-x".to_owned(), HashSet::from(["tm-b".to_owned()]))]),
    };
    assert!(live.holds(&record("tm-a", None)));
    assert!(live.holds(&record("tm-b", Some("/s/tm-ws-x"))));
    // The same names on the other server do not count.
    assert!(!live.holds(&record("tm-b", None)));
    assert!(!live.holds(&record("tm-a", Some("/s/tm-ws-x"))));
    // A bare name set is the default server's.
    let bare = HashSet::from(["tm-a".to_owned()]);
    assert!(bare.holds(&record("tm-a", None)));
    assert!(!bare.holds(&record("tm-a", Some("/s/tm-ws-x"))));
}

/// An unlisted server whose socket answers is alive, so its session's state
/// is unknown and reads as live; once nothing listens it reads as gone.
#[test]
fn an_unlisted_server_whose_socket_answers_reads_as_live() {
    let socket = PathBuf::from(format!("/tmp/t10101-{}-sock", std::process::id()));
    let _ = std::fs::remove_file(&socket);
    let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
    let rec = record("tm-a", socket.to_str());
    assert!(LiveSessions::default().holds(&rec));
    drop(listener);
    let _ = std::fs::remove_file(&socket);
    assert!(!LiveSessions::default().holds(&rec));
}

#[test]
fn live_sessions_list_every_workspace_server() {
    let tmux = ListingDriver {
        names: names(&["tm-a", "not-managed"]),
        servers: HashMap::from([("tm-ws-x".to_owned(), names(&["tm-b"]))]),
        live: names(&["tm-ws-x"]),
    };
    let live = live_sessions_of(&tmux).expect("observed");
    assert_eq!(live.default, HashSet::from(["tm-a".to_owned()]));
    assert_eq!(
        live.named,
        HashMap::from([("tm-ws-x".to_owned(), HashSet::from(["tm-b".to_owned()]))])
    );
}

/// Error arm (Fail-Open Check): a live server the driver cannot reach makes
/// the whole observation unknown, never a set read as complete without it.
#[test]
fn an_unreachable_workspace_server_makes_liveness_unknown() {
    let tmux = ListingDriver {
        names: names(&["tm-a"]),
        servers: HashMap::new(),
        live: names(&["tm-ws-gone"]),
    };
    let err = live_sessions_of(&tmux).expect_err("unknown");
    assert!(err.to_string().contains("tm-ws-gone"), "{err}");
}

/// A named `RealTmuxDriver` sends its rename and its harness probe to its own
/// server; the probes that read the default server are not used.
#[test]
fn a_named_real_driver_sends_every_call_to_its_server() {
    use crate::session_manager::RealTmuxDriver;
    let dir = crate::test_support::hermetic_temp_dir();
    let log = dir.path().join("calls.log");
    let bin = dir.path().join("fake-tmux");
    let script = format!("#!/bin/sh\necho \"$*\" >> '{}'\nexit 0\n", log.display());
    trusty_common::exec_stub::write_executable(&bin, script).expect("fake tmux");
    let driver = crate::daemon::tmux::TmuxDriver::with_tmux_path_for_test(
        bin.to_string_lossy().into_owned(),
    );
    let named = RealTmuxDriver::from_driver_for_test(driver)
        .on_server("tm-ws-x")
        .expect("a real driver reaches any server");

    named.rename_session("tm-a", "tm-b").expect("rename");
    assert_eq!(
        named.pane_claude("tm-b"),
        crate::core::process::PaneClaude::Unknown
    );
    assert!(!named.runtime_ready("tm-b"));

    let calls = std::fs::read_to_string(&log).expect("log");
    assert!(!calls.is_empty());
    assert!(
        calls.lines().all(|l| l.starts_with("-L tm-ws-x ")),
        "a call left the named server: {calls}"
    );
    assert!(calls.contains("rename-session"), "{calls}");
}
