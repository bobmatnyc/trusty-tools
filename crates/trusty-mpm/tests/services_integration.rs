//! Integration smoke tests for `tm services` discovery.
//!
//! Why: unit tests mock all probers to avoid I/O; these integration tests
//! verify the full pipeline (real pgrep + real HTTP) against an actually
//! running daemon. They are gated with `#[ignore]` so CI does not require
//! live daemons.
//! What: creates a minimal manifest pointing at `trusty-search` on :7878,
//! then calls `Discoverer::list()` and `Discoverer::health()` and asserts
//! the expected fields.
//! Test: run with:
//!   cargo test -p trusty-mpm --test services_integration -- --include-ignored --nocapture

use trusty_mpm::services::{Discoverer, HealthState, ServicesManifest};

/// Verify `tm services list` finds trusty-search when it is running on :7878.
///
/// Why: this test catches regressions where the manifest or discovery engine
/// breaks the end-to-end probe cycle against a real daemon.
/// What: parses the embedded default manifest (which includes trusty-search at
/// port 7878), calls `Discoverer::list()`, and asserts `trusty-search` appears
/// with `running=true` and `port=Some(7878)`.
/// Test: requires `trusty-search start` before running. Gated `#[ignore]`.
#[test]
#[ignore = "requires live trusty-search daemon on :7878"]
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
        "trusty-search should be running (start it before this test)"
    );
    assert_eq!(ts.port, Some(7878), "trusty-search should be on port 7878");
    assert!(ts.url.is_some(), "trusty-search url should be populated");
    println!("trusty-search status: {:?}", ts);
}

/// Verify health probe returns Ok for a running trusty-search.
///
/// Why: `health_bypasses_cache` is tested with a mock; this test verifies the
/// real HTTP prober reaches the actual `/health` endpoint.
/// What: calls `Discoverer::health("trusty-search")` and asserts `HealthState::Ok`.
/// Test: requires `trusty-search start` before running. Gated `#[ignore]`.
#[test]
#[ignore = "requires live trusty-search daemon on :7878"]
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
        "trusty-search /health should return 2xx when daemon is running"
    );
}
