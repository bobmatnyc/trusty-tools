//! Allowlist tests for the Architect dashboard link (#9474).
//!
//! The show/hide rule end to end is pinned by the `architect_link_*` route
//! tests in `server/tests.rs`; these pin which recorded addresses may ever be
//! linked or probed, without touching the network.

use super::*;

/// #9474: only a loopback `http` address survives, normalised.
/// Test: this test itself.
#[test]
fn dashboard_target_accepts_only_loopback_http() {
    for (recorded, url, probes) in [
        ("127.0.0.1:7890", "http://127.0.0.1:7890/", 1),
        ("http://127.0.0.1:7890", "http://127.0.0.1:7890/", 1),
        ("  127.0.0.1:7890\n", "http://127.0.0.1:7890/", 1),
        ("[::1]:7890", "http://[::1]:7890/", 1),
        ("http://LOCALHOST:7890/", "http://localhost:7890/", 2),
    ] {
        let (got, addrs) = dashboard_target(recorded).expect(recorded);
        assert_eq!(got.as_str(), url, "{recorded}");
        assert_eq!(addrs.len(), probes, "{recorded}");
        assert!(addrs.iter().all(|a| a.ip().is_loopback()), "{recorded}");
    }
    for refused in [
        "",
        "dashboard.invalid:7890",
        "192.0.2.1:7890",
        "0.0.0.0:7890",
        "127.0.0.2:7890",
        "http://127.0.0.1@dashboard.invalid:7890/",
        "http://user@127.0.0.1:7890/",
        "https://127.0.0.1:7890/",
        "javascript:alert(1)",
        "file:///etc/passwd",
    ] {
        assert!(
            dashboard_target(refused).is_none(),
            "{refused} must be refused"
        );
    }
}

/// #9474: a refused address is never probed, and a missing one is no link.
/// Test: this test itself.
#[tokio::test]
async fn live_dashboard_url_needs_a_recorded_loopback_address() {
    assert_eq!(live_dashboard_url(None).await, None);
    assert_eq!(live_dashboard_url(Some("192.0.2.1:7890")).await, None);
}
