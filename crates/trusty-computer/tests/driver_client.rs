//! Driver client, manifest, launcher argv and session recovery, on the fake driver.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::time::{Duration, Instant};

use common::{FakeDaemon, FakeLauncher, short_tmp};
use serde_json::json;
use trusty_computer::driver::{DriverClient, DriverError, parse_session_ended};
use trusty_computer::launcher::{is_default_socket, open_argv};
use trusty_computer::manifest::{self, LeaseScope, Lifetimes};
use trusty_computer::supervisor::Supervisor;

const SCOPE: LeaseScope = LeaseScope {
    pid: 4242,
    window_id: 77,
};

fn started(dir: &tempfile::TempDir) -> Supervisor<FakeLauncher> {
    let mut sup = Supervisor::new(FakeLauncher::new("0.23.2", None), dir.path())
        .expect("supervisor")
        .with_ready_timeout(Duration::from_secs(2));
    sup.start(SCOPE).expect("start");
    sup
}

#[test]
fn is_error_result_is_never_success() {
    let dir = short_tmp();
    let sock = dir.path().join("d.sock");
    let _daemon = FakeDaemon::start(&sock, "0.23.2", None);
    let err = DriverClient::new(&sock)
        .call("list_windows", &json!({"pid": common::OUT_OF_MANIFEST_PID}))
        .expect_err("isError must be an error");
    match err {
        DriverError::ToolError { message, result } => {
            assert!(
                message.contains("outside the capability manifest"),
                "{message}"
            );
            assert_eq!(result["isError"], true);
        }
        other => panic!("expected ToolError, got {other:?}"),
    }
}

#[test]
fn refusal_and_bad_reply_are_errors() {
    let dir = short_tmp();
    let sock = dir.path().join("d.sock");
    let _daemon = FakeDaemon::start(&sock, "0.23.2", None);
    let err = DriverClient::new(&sock)
        .request("bogus", None, None)
        .expect_err("ok:false is an error");
    assert!(
        matches!(err, DriverError::Refused { exit_code: 64, .. }),
        "{err:?}"
    );
    let missing = DriverClient::new(&dir.path().join("none.sock"))
        .metadata()
        .expect_err("no daemon");
    assert!(matches!(missing, DriverError::Io { .. }), "{missing:?}");
}

#[test]
fn request_times_out_on_a_silent_daemon() {
    let dir = short_tmp();
    let sock = dir.path().join("silent.sock");
    let _listener = UnixListener::bind(&sock).expect("bind");
    let started = Instant::now();
    let err = DriverClient::with_timeout(&sock, Duration::from_millis(200))
        .metadata()
        .expect_err("must time out");
    assert!(matches!(err, DriverError::Io { .. }), "{err:?}");
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn session_ended_text_is_parsed_and_others_are_not() {
    let live = "session 's1' has ended; tool call 'list_windows' was rejected. Call start_session with this id to revive it before issuing further actions, or use a new session id.";
    assert_eq!(
        parse_session_ended(live),
        Some(("s1".to_owned(), "list_windows".to_owned()))
    );
    for other in [
        "Policy loading error: capability manifest idle timeout exceeded",
        "Unknown method: bogus",
        "session 's1' has ended",
    ] {
        assert_eq!(parse_session_ended(other), None, "{other}");
    }
}

#[test]
fn manifest_pins_exact_pid_and_window_only() {
    let text = manifest::render(SCOPE, &Lifetimes::default());
    assert!(text.contains("applications: [4242]"), "{text}");
    assert!(
        text.contains("- pid: 4242\n        window_id: 77"),
        "{text}"
    );
    assert!(
        !text.contains("bundle_id") && !text.contains("executable"),
        "{text}"
    );
    assert!(text.contains("display: false") && text.contains("idle_timeout: 15m"));
}

#[test]
fn manifest_file_is_private() {
    let dir = short_tmp();
    let path = dir.path().join("m.yaml");
    manifest::write(&path, SCOPE, &Lifetimes::default()).expect("write");
    let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[test]
fn open_argv_is_launchservices_bounded_and_private() {
    let argv = open_argv(Path::new("/s/d.sock"), Path::new("/s/m.yaml"));
    assert_eq!(&argv[..5], ["/usr/bin/open", "-n", "-g", "-a", "CuaDriver"]);
    let tail = argv.join(" ");
    assert!(tail.contains("--env CUA_DRIVER_RS_TELEMETRY_ENABLED=false"));
    assert!(tail.contains("--env CUA_DRIVER_RS_UPDATE_CHECK=false"));
    assert!(tail.ends_with(
        "--args serve --socket /s/d.sock --permission-mode bounded --capability-manifest /s/m.yaml \
         --approve-capability-manifest --no-permissions-gate --no-overlay"
    ));
    assert!(is_default_socket(Path::new(
        "/Users/x/Library/Caches/cua-driver/cua-driver.sock"
    )));
    assert!(!is_default_socket(Path::new("/s/d.sock")));
}

#[test]
fn rescope_restarts_only_on_a_scope_change() {
    let dir = short_tmp();
    let mut sup = started(&dir);
    assert_eq!(sup.rescope(SCOPE).expect("same"), None);
    let next = LeaseScope {
        window_id: 78,
        ..SCOPE
    };
    assert!(sup.rescope(next).expect("moved").is_some());
    let l = sup.launcher();
    assert_eq!((l.starts, l.stops), (2, 1));
    assert!(l.manifests[1].contains("window_id: 78"));
    assert_eq!(sup.scope(), Some(next));
    sup.stop().expect("stop");
}

#[test]
fn read_only_call_retries_once_after_session_ended() {
    let dir = short_tmp();
    let sup = started(&dir);
    sup.launcher()
        .daemon
        .as_ref()
        .expect("daemon")
        .end_session(sup.session());
    sup.call("list_windows", &json!({"pid": 4242}))
        .expect("retried after start_session");
    let names: Vec<String> = sup
        .launcher()
        .daemon
        .as_ref()
        .expect("daemon")
        .calls()
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    assert_eq!(
        names,
        [
            "start_session",
            "list_windows",
            "start_session",
            "list_windows"
        ]
    );
}

#[test]
fn mutating_call_is_not_rerun_after_session_ended() {
    let dir = short_tmp();
    let sup = started(&dir);
    let daemon = sup.launcher().daemon.as_ref().expect("daemon");
    daemon.end_session(sup.session());
    let err = sup
        .call("click", &json!({"element_token": "tok-button"}))
        .expect_err("must not re-run");
    assert!(
        matches!(err, DriverError::SessionRestarted { .. }),
        "{err:?}"
    );
    let names: Vec<String> = daemon.calls().into_iter().map(|(n, _)| n).collect();
    assert_eq!(names, ["start_session", "click", "start_session"]);
}

#[test]
fn idle_timeout_lockout_is_recognised_exactly() {
    use trusty_computer::driver::{MANIFEST_IDLE_TEXT, is_manifest_idle};
    let refused = |m: &str| DriverError::Refused {
        message: m.to_owned(),
        exit_code: 1,
    };
    // The live 0.23.2 line in tests/fixtures/evidence/session-ended-0.23.2.txt.
    assert!(is_manifest_idle(&refused(
        "Policy loading error: capability manifest idle timeout exceeded"
    )));
    assert!(is_manifest_idle(&refused(MANIFEST_IDLE_TEXT)));
    assert!(!is_manifest_idle(&refused("Unknown method: bogus")));
    assert!(!is_manifest_idle(&DriverError::Lifecycle(
        MANIFEST_IDLE_TEXT.into()
    )));
}
