//! Tests for the trusty-events launchd plist (#10042).
//!
//! Why: the plist is the whole of the bus's supervision. A wrong `KeepAlive`
//! form either restarts a cleanly stopped bus forever or never restarts a
//! crashed one; a socket or port key would make launchd bind TCP for a daemon
//! ADR-0032 keeps on UDS.
//! Test: `cargo test -p trusty-installer events_plist`.

use super::*;
use crate::commands::plist_bootstrap::StubLaunchctl;

/// The golden render for `/Users/tester`.
const GOLDEN: &str = include_str!("testdata/com.trusty.events.plist.golden");

/// Why: the golden file is the reviewed plist; any change to the template shows
/// up as a diff against it rather than slipping through a key-by-key check.
/// What: a fixed home and binary path render byte-for-byte to the golden file.
/// Test: This is the test.
#[test]
fn events_plist_matches_the_golden_file() {
    let rendered = fill_events_template("/Users/tester", "/Users/tester/.cargo/bin/trusty-events");
    assert_eq!(rendered, GOLDEN);
}

/// Why (#10042): with `SuccessfulExit: false`, a stray SIGTERM (exit 0) left
/// the job loaded but not running, and the doctor row's remedy, `tctl start
/// trusty-events`, answers a loaded label with "already loaded (skipped
/// restart)" — so the bus stayed down. `tctl stop` is a `launchctl bootout`,
/// which unloads the job, so restarting on every exit never fights it.
/// What: `KeepAlive` is a bare `<true/>`, as the console's unit, never the
/// `SuccessfulExit` dict, and the restart throttle is 10 s.
/// Test: This is the test.
#[test]
fn events_plist_restarts_the_bus_after_any_exit() {
    let p = fill_events_template("/h", "/b/trusty-events");
    assert!(p.contains("<key>KeepAlive</key>\n  <true/>"), "{p}");
    assert!(!p.contains("SuccessfulExit"), "{p}");
    assert!(
        p.contains("<key>ThrottleInterval</key>\n  <integer>10</integer>"),
        "{p}"
    );
    assert!(p.contains("<string>serve</string>"), "{p}");
}

/// Why (ADR-0032): the bus binds its Unix socket itself. A launchd `Sockets`
/// key or a bind address in the job would hand it a listener nobody reviewed.
/// What: the rendered plist names no socket, port, host or `--http` flag.
/// Test: This is the test.
#[test]
fn events_plist_carries_no_tcp_endpoint() {
    let p = fill_events_template("/h", "/b/trusty-events");
    for forbidden in [
        "<key>Sockets</key>",
        "SockServiceName",
        "SockNodeName",
        "127.0.0.1",
        "localhost",
        "--http",
        "--port",
        "--addr",
    ] {
        assert!(!p.contains(forbidden), "plist names {forbidden}: {p}");
    }
}

/// Why: the plist's `Label` must be the label `tctl start|stop|status` resolve,
/// or every lifecycle verb targets a job that does not exist (#4868).
/// What: the label is the `com.trusty.<stem>` convention for the bus member,
/// and the rendered plist carries it.
/// Test: This is the test.
#[test]
fn events_label_is_the_conventional_label() {
    assert_eq!(events_label(), "com.trusty.events");
    assert_eq!(
        events_label(),
        trusty_common::launchd_labels::canonical_label("trusty-events")
    );
    assert!(fill_events_template("/h", "/b").contains("<string>com.trusty.events</string>"));
}

/// Why: a path with `&` or `<` written raw makes the plist invalid XML, and
/// launchd refuses to load it.
/// What: both embedded paths are escaped.
/// Test: This is the test.
#[test]
fn events_plist_escapes_the_paths_it_embeds() {
    let p = fill_events_template("/Users/a&b", "/opt/<x>/trusty-events");
    assert!(
        p.contains("/Users/a&amp;b/.trusty-events/logs/stderr.log"),
        "{p}"
    );
    assert!(p.contains("/opt/&lt;x&gt;/trusty-events"), "{p}");
    assert!(!p.contains("a&b"), "{p}");
}

/// Why: only the bus takes the installer-written branch; every other daemon
/// keeps its own `service install`.
/// What: `installer_owns_plist` is true for trusty-events alone.
/// Test: This is the test.
#[test]
fn installer_owns_only_the_events_plist() {
    assert!(installer_owns_plist("trusty-events"));
    for other in [
        "trusty-search",
        "trusty-memory",
        "trusty-console",
        "trusty-mpm",
    ] {
        assert!(!installer_owns_plist(other), "{other}");
    }
}

/// Why (#3551): the write path must be provable without touching the live
/// launchd domain or the real `~/Library/LaunchAgents`.
/// What: under a tempdir home with a stub `launchctl`, the plist is written,
/// the log dir exists, and exactly one bootout then one bootstrap are sent.
/// Test: This is the test.
#[test]
fn install_events_plist_for_writes_and_bootstraps_through_the_stub() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let stub = StubLaunchctl::new();
    let target = SupervisorTarget {
        home: tmp.path().to_owned(),
        domain: "gui/999999-isolated-test-domain".to_owned(),
        launchctl: &stub,
    };
    let exe = tmp.path().join("bin").join("trusty-events");
    let plist = install_events_plist_for(&target, &exe).expect("install");

    assert_eq!(
        plist,
        tmp.path()
            .join("Library")
            .join("LaunchAgents")
            .join("com.trusty.events.plist")
    );
    let written = std::fs::read_to_string(&plist).expect("plist written");
    assert!(written.contains(&exe.display().to_string()), "{written}");
    assert!(tmp.path().join(".trusty-events").join("logs").is_dir());
    let calls = stub.calls();
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert!(calls[0].starts_with("bootout gui/999999-isolated-test-domain "));
    assert!(calls[1].starts_with("bootstrap gui/999999-isolated-test-domain "));
}

/// Why: a bootstrap that failed must not read as an installed service.
/// What: the stub's bootstrap error comes back as `Err` naming launchctl.
/// Test: This is the test.
#[test]
fn install_events_plist_for_surfaces_a_bootstrap_failure() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let stub = StubLaunchctl::failing_bootstrap("Bootstrap failed: 5: Input/output error");
    let target = SupervisorTarget {
        home: tmp.path().to_owned(),
        domain: "gui/999999-isolated-test-domain".to_owned(),
        launchctl: &stub,
    };
    let err = install_events_plist_for(&target, &tmp.path().join("trusty-events"))
        .expect_err("a failed bootstrap is an error");
    assert!(
        err.to_string().contains("launchctl bootstrap failed"),
        "{err:#}"
    );
}
