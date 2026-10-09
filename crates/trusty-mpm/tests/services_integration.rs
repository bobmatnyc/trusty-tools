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
//! no TCP port and no URL.
//! Test: start a socket-only daemon (`trusty-search start --no-http`), then run:
//!   cargo test -p trusty-mpm --test integration services_integration:: -- --include-ignored --nocapture

use trusty_mpm::services::{Discoverer, HealthState, ServicesManifest};

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
