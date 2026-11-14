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
        ..LiveSessions::default()
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

/// Error arm (Fail-Open Check): a live server the driver cannot reach
/// degrades only its own records to unknown (read as live); the default
/// server and the other workspace servers still reconcile. Red on 420b3717bf,
/// where the whole observation was `Err`.
#[test]
fn one_unlistable_server_degrades_only_its_own_records() {
    let tmux = ListingDriver {
        names: names(&["tm-a"]),
        servers: HashMap::from([("tm-ws-ok".to_owned(), names(&["tm-b"]))]),
        live: names(&["tm-ws-gone", "tm-ws-ok"]),
    };
    let live = live_sessions_of(&tmux).expect("the default server was observed");
    assert_eq!(live.unknown, HashSet::from(["tm-ws-gone".to_owned()]));
    // The unlistable server's record is unknown: live, never decommissioned.
    let stuck = record("tm-z", Some("/s/tm-ws-gone"));
    assert!(live.holds(&stuck));
    assert!(!live.server_lost(&stuck));
    // Everything else still reconciles.
    assert!(live.holds(&record("tm-b", Some("/s/tm-ws-ok"))));
    assert!(!live.holds(&record("tm-c", Some("/s/tm-ws-ok"))));
    assert!(!live.holds(&record("tm-c", None)));
}

/// Error arm (Fail-Open Check): an unreadable server directory makes every
/// named record unknown, and the default server still reconciles.
#[test]
fn an_unreadable_server_directory_leaves_named_records_unknown() {
    struct NoDir;
    impl ManagedTmuxDriver for NoDir {
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
            Ok(names(&["tm-a"]))
        }
        fn workspace_servers(&self) -> Result<Vec<String>, ManagedError> {
            Err(ManagedError::TmuxUnavailable("unreadable".into()))
        }
    }
    let live = live_sessions_of(&NoDir).expect("default observed");
    assert!(live.named_unlisted);
    assert!(live.holds(&record("tm-z", Some("/s/tm-ws-x"))));
    assert!(!live.server_lost(&record("tm-z", Some("/s/tm-ws-x"))));
    assert!(!live.holds(&record("tm-c", None)));
}

/// A driver whose every listing blocks far past the probe budget.
struct StuckDriver;

impl ManagedTmuxDriver for StuckDriver {
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
        std::thread::sleep(std::time::Duration::from_secs(30));
        Ok(Vec::new())
    }
}

/// Error arm (Fail-Open Check): a workspace server whose listing hangs is
/// abandoned at the probe budget and reads as unknown. Red on 420b3717bf,
/// whose untimed listing held the caller for the full hang.
#[test]
fn a_stuck_workspace_server_is_bounded_and_unknown() {
    struct Host;
    impl ManagedTmuxDriver for Host {
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
            Ok(names(&["tm-a"]))
        }
        fn on_server(&self, _server: &str) -> Option<Arc<dyn ManagedTmuxDriver>> {
            Some(Arc::new(StuckDriver))
        }
        fn workspace_servers(&self) -> Result<Vec<String>, ManagedError> {
            Ok(names(&["tm-ws-stuck"]))
        }
    }
    let started = std::time::Instant::now();
    let live = live_sessions_of(&Host).expect("default observed");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "the probe was not bounded: {:?}",
        started.elapsed()
    );
    assert_eq!(live.unknown, HashSet::from(["tm-ws-stuck".to_owned()]));
    assert!(live.holds(&record("tm-z", Some("/s/tm-ws-stuck"))));
}

/// Error arm (Fail-Open Check): a record whose socket is outside the
/// driver's socket directory (`TMUX_TMPDIR` changed since launch) is refused
/// by the router, and liveness reads it by its own socket, never by the
/// same-named server in the current directory. Red on 420b3717bf, which
/// routed `-L <name>` to the current directory's server.
#[test]
fn a_socket_outside_the_socket_dir_is_refused_and_reads_unknown() {
    use crate::session_manager::RealTmuxDriver;
    let dir = crate::test_support::hermetic_temp_dir();
    let current = dir.path().join("now");
    let old = dir.path().join("then");
    std::fs::create_dir_all(&current).expect("current");
    std::fs::create_dir_all(&old).expect("old");
    let driver = crate::daemon::tmux::TmuxDriver::with_tmux_path_for_test("/nonexistent/tmux");
    let tmux =
        RealTmuxDriver::from_driver_for_test(driver).with_socket_dir_for_test(current.clone());

    // A record launched under the old TMUX_TMPDIR, still running there.
    let socket = old.join("tm-ws-x");
    let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
    let rec = record("tm-a", socket.to_str());
    let err = route(&tmux, &rec).err().expect("refused");
    assert!(err.to_string().contains("outside"), "{err}");
    let here = record("tm-a", current.join("tm-ws-x").to_str());
    assert!(route(&tmux, &here).expect("in the dir").is_named());

    // The current directory's same-named server lists nothing for it.
    let live = LiveSessions {
        named: HashMap::from([("tm-ws-x".to_owned(), HashSet::new())]),
        socket_dir: Some(current),
        ..LiveSessions::default()
    };
    assert!(live.holds(&rec), "its own socket answers, so it is live");
    drop(listener);
    let _ = std::fs::remove_file(&socket);
    assert!(!live.holds(&rec), "its own server is gone");
}

/// A named `RealTmuxDriver` sends its rename and its harness probe to its own
/// server; the probes that read the default server are not used.
// #10748: the tmux spawn guard reads process-global `$HOME`, which a
// concurrent `#[serial]` test redirects; the rename then fails as refused.
#[serial_test::serial]
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

/// A server whose listing blocks until the test releases it, counting every
/// listing started.
struct HungDriver {
    started: Arc<std::sync::atomic::AtomicUsize>,
    release: Arc<std::sync::Mutex<std::sync::mpsc::Receiver<()>>>,
}

impl ManagedTmuxDriver for HungDriver {
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
        self.started
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let _ = self.release.lock().map(|rx| rx.recv());
        Ok(Vec::new())
    }
}

/// Error arm (Fail-Open Check): a workspace server whose listing hangs is
/// listed once across repeated observations. Each observation returns within
/// the probe budget with the server `unknown`, and no second listing starts
/// while the first is blocked. Red on 2df3760494, which started one listing
/// (one thread and one tmux client) per observation.
#[test]
fn a_hung_workspace_server_is_listed_once_across_observations() {
    struct Host {
        dir: PathBuf,
        hung: Arc<dyn ManagedTmuxDriver>,
    }
    impl ManagedTmuxDriver for Host {
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
            Ok(Vec::new())
        }
        fn on_server(&self, _server: &str) -> Option<Arc<dyn ManagedTmuxDriver>> {
            Some(Arc::clone(&self.hung))
        }
        fn workspace_servers(&self) -> Result<Vec<String>, ManagedError> {
            Ok(names(&["tm-ws-hung"]))
        }
        fn socket_dir(&self) -> Option<PathBuf> {
            Some(self.dir.clone())
        }
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let started = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (release, rx) = std::sync::mpsc::channel::<()>();
    let host = Host {
        dir: dir.path().to_path_buf(),
        hung: Arc::new(HungDriver {
            started: Arc::clone(&started),
            release: Arc::new(std::sync::Mutex::new(rx)),
        }),
    };
    let budget = crate::core::tmux_server::PROBE_BUDGET + std::time::Duration::from_secs(1);
    for round in 0..3 {
        let clock = std::time::Instant::now();
        let live = observe_workspace_servers(&host, HashSet::new());
        assert!(clock.elapsed() < budget, "round {round} was not bounded");
        assert_eq!(live.unknown, HashSet::from(["tm-ws-hung".to_owned()]));
    }
    assert_eq!(
        started.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "a listing started while one was still blocked"
    );
    drop(release);
}

/// Error arm (Fail-Open Check), the Architect's ruling on #10101, arm (a): a
/// workspace server that vanished with its socket (a clean exit, a kill of
/// its last session, `kill-server`) was stopped on purpose, so its record is
/// not lost and is not auto-resumed. Red on 26474c385d, which read every
/// vanished server as lost.
#[test]
fn a_vanished_server_with_no_socket_is_a_deliberate_stop() {
    let dir = tempfile::tempdir().expect("tempdir");
    let live = LiveSessions {
        socket_dir: Some(dir.path().to_path_buf()),
        ..LiveSessions::default()
    };
    let rec = record("tm-a", dir.path().join("tm-ws-gone").to_str());
    assert!(!live.holds(&rec));
    assert!(
        !live.server_lost(&rec),
        "a socket-less server reads as a kill"
    );
}

/// Arm (b): a workspace server whose socket is still on disk but refuses a
/// connection crashed, so its record is lost and auto-resumes. This
/// behaviour is unchanged from 26474c385d; its red proof reverts the
/// socket-present decision in the new code.
#[test]
fn a_vanished_server_whose_socket_refuses_is_lost() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("tm-ws-crashed");
    // A datagram socket refuses a stream connect, as a dead server's does.
    drop(std::os::unix::net::UnixDatagram::bind(&socket).expect("bind"));
    let live = LiveSessions {
        socket_dir: Some(dir.path().to_path_buf()),
        ..LiveSessions::default()
    };
    let rec = record("tm-a", socket.to_str());
    assert!(!live.holds(&rec));
    assert!(live.server_lost(&rec), "a crashed server's record is lost");
}

/// Arm (c): a record whose tmux server started before the last host boot was
/// lost to the reboot, even when its server is up again and lists another
/// PM's session (which otherwise reads as a targeted kill). A record started
/// after the boot keeps the targeted-kill reading. Red on 26474c385d, which
/// read the pre-boot record as a targeted kill.
#[test]
fn a_record_started_before_the_last_boot_is_lost() {
    let live = LiveSessions {
        named: HashMap::from([("tm-ws-x".to_owned(), HashSet::from(["tm-other".to_owned()]))]),
        boot: Some(2_000),
        ..LiveSessions::default()
    };
    let mut before = record("tm-a", Some("/s/tm-ws-x"));
    before.tmux_server = Some("4242:1000".to_owned());
    let mut after = before.clone();
    after.tmux_server = Some("4242:3000".to_owned());
    assert!(!live.holds(&before));
    assert!(live.server_lost(&before), "the reboot took it");
    assert!(
        !live.server_lost(&after),
        "a kill after the boot is deliberate"
    );
}

/// The critic's LOW on #10101: a listed server whose every session is
/// unmanaged is up, so a managed record missing from it was killed, not lost
/// with its server. Red on 26474c385d, which judged the filtered (managed)
/// set and read the server as empty.
#[test]
fn a_server_listing_only_unmanaged_sessions_is_not_lost() {
    let tmux = ListingDriver {
        names: names(&["tm-a"]),
        servers: HashMap::from([("tm-ws-x".to_owned(), names(&["scratch"]))]),
        live: names(&["tm-ws-x"]),
    };
    let mut live = live_sessions_of(&tmux).expect("observed");
    live.boot = None;
    let rec = record("tm-c", Some("/s/tm-ws-x"));
    assert!(!live.holds(&rec));
    assert!(!live.server_lost(&rec));
}
