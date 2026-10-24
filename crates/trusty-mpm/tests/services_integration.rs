//! Integration smoke tests for `tm services` discovery.
//!
//! Why: unit tests mock all probers to avoid I/O; these integration tests
//! verify the full pipeline (real pgrep + the real socket prober) against an
//! actually running daemon. They are gated with `#[ignore]` so CI does not
//! require live daemons.
//! What (#9543): uses the embedded default manifest, which probes trusty-search
//! by calling `search.health` on its Unix socket (`TRUSTY_SEARCH_SOCKET`, else
//! the daemon's data directory), then calls `Discoverer::list()` and
//! `Discoverer::health()` and asserts the socket-only shape: running, healthy,
//! no TCP port and no URL. With no daemon socket present each test prints a
//! `SKIP:` line and returns (#10230).
//! Test: start a socket-only daemon (`trusty-search start`), then run:
//!   cargo test -p trusty-mpm --test integration services_integration:: -- --include-ignored --nocapture

use std::path::Path;

use trusty_mpm::services::discoverer::{RealSearchSocketProber, SearchSocketProber};
use trusty_mpm::services::{Discoverer, HealthState, ServicesManifest};

/// The skip line for a live trusty-search test when no daemon socket exists.
///
/// Why (#10230): a host with no trusty-search daemon (EVO, by design) failed
/// these smoke tests with "unreachable" instead of skipping them.
/// What: `Some("SKIP: …")` only when nothing exists at `socket`. A present
/// socket returns `None`, so a stale socket or a bad health answer still fails.
/// Test: `socket_absent_skip_reason_skips_only_a_missing_socket`.
fn socket_absent_skip_reason(socket: &Path) -> Option<String> {
    (!socket.exists()).then(|| {
        format!(
            "SKIP: no trusty-search daemon socket at {}",
            socket.display()
        )
    })
}

/// The skip reason for the socket the real `Discoverer` probes, if absent.
///
/// Resolves through [`RealSearchSocketProber`] so the path matches the probe's;
/// a resolver error panics rather than skips (#10230).
fn live_search_skip_reason() -> Option<String> {
    let socket = RealSearchSocketProber
        .socket()
        .expect("resolve the trusty-search socket path");
    socket_absent_skip_reason(&socket)
}

/// The skip predicate fires only for a missing socket (#10230).
#[test]
fn socket_absent_skip_reason_skips_only_a_missing_socket() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("trusty-search.sock");
    let reason = socket_absent_skip_reason(&missing).expect("a missing socket skips");
    assert!(reason.starts_with("SKIP: "), "{reason}");
    std::fs::write(&missing, b"").expect("create a stand-in socket file");
    assert_eq!(
        socket_absent_skip_reason(&missing),
        None,
        "a present (even stale) socket must not skip"
    );
}

/// Verify `tm services list` finds a socket-only trusty-search.
///
/// Why: this test catches regressions where the manifest or discovery engine
/// breaks the end-to-end probe cycle against a real daemon.
/// What: parses the embedded default manifest, calls `Discoverer::list()`, and
/// asserts `trusty-search` appears running and healthy with `port` and `url`
/// both `None` — it binds no TCP port (#9543).
/// Test: requires a live trusty-search daemon on its socket. Gated `#[ignore]`.
#[test]
#[ignore = "requires a live trusty-search daemon on its Unix socket"]
fn smoke_test_services_list_against_live_trusty_search() {
    // #10230: no daemon socket is a skip; a present socket must answer.
    if let Some(reason) = live_search_skip_reason() {
        eprintln!("{reason}");
        return;
    }
    let manifest = ServicesManifest::default_manifest();
    let mut d = Discoverer::new(manifest);
    let list = d.list();

    let ts = list
        .iter()
        .find(|s| s.name == "trusty-search")
        .expect("trusty-search must be in the default manifest");

    assert!(ts.declared, "trusty-search should be declared");
    assert!(
        ts.running,
        "trusty-search should be running (start it before this test): {:?}",
        ts.health
    );
    assert_eq!(ts.health, HealthState::Ok, "search.health over the socket");
    assert_eq!(ts.port, None, "trusty-search binds no TCP port (#9543)");
    assert_eq!(ts.url, None, "trusty-search has no HTTP URL (#9543)");
    println!("trusty-search status: {:?}", ts);
}

/// Verify the health probe returns Ok for a running trusty-search.
///
/// Why: `health_bypasses_cache` is tested with a mock; this test verifies the
/// real socket prober reaches the daemon's `search.health` method (#9543).
/// What: calls `Discoverer::health("trusty-search")` and asserts `HealthState::Ok`.
/// Test: requires a live trusty-search daemon on its socket. Gated `#[ignore]`.
#[test]
#[ignore = "requires a live trusty-search daemon on its Unix socket"]
fn smoke_test_services_health_against_live_trusty_search() {
    // #10230: no daemon socket is a skip; a present socket must answer.
    if let Some(reason) = live_search_skip_reason() {
        eprintln!("{reason}");
        return;
    }
    let manifest = ServicesManifest::default_manifest();
    let mut d = Discoverer::new(manifest);
    let result = d
        .health("trusty-search")
        .expect("trusty-search must be in the manifest");

    println!(
        "health result: name={}, state={:?}, message={}",
        result.name, result.state, result.message
    );
    assert_eq!(
        result.state,
        HealthState::Ok,
        "trusty-search should answer search.health on its socket when running"
    );
}
