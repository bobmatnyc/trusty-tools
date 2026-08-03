//! Process-wide file-descriptor bound for the chat-session store cache (#4639).
//!
//! Why this test lives alone in its own integration-test file: the bug it pins
//! is measured in *real process file descriptors*, and
//! `fd_metrics::count_open_fds` reports the whole process. Cargo runs the tests
//! inside one target as parallel threads of a single process, so as a unit test
//! alongside the crate's ~480 others this measurement is meaningless — the
//! surrounding tests' own fd churn swamped the signal (CI observed a +811 delta
//! against a 120-fd signal, while a filtered local run of only this module saw
//! the true +120). Cargo gives each `tests/*.rs` file its own process, so
//! keeping this the ONLY test in this target makes the delta attributable.
//! Do not add a second test to this file.
//!
//! What: opens far more distinct palaces than the cap through the cache and
//! asserts the process's open-fd growth stays bounded. Pre-fix (the unbounded
//! `DashMap`) this grows one fd per palace, forever.
//! Test: this file.

use tempfile::TempDir;
use trusty_memory::fd_metrics::count_open_fds;
use trusty_memory::session_store_cache::SessionStoreCache;

/// Why: entry count is only a proxy; the production symptom was 844 leaked
/// *file descriptors* against an 8 192 ceiling. This asserts the thing that
/// actually broke, end to end.
/// What: records the process fd count, opens 120 distinct palaces (dropping
/// each `Arc` immediately), and asserts growth stays within the cap plus a
/// small allowance for harness churn. With the cap removed this grows by 120.
/// Test: this test.
#[test]
fn session_store_fd_count_is_bounded_by_cap() {
    let dir = TempDir::new().unwrap();
    let cache = SessionStoreCache::with_max_open(8);

    let before = count_open_fds().expect("fd count available on this platform");
    for i in 0..120 {
        let id = format!("palace-{i}");
        let store = cache
            .get_or_open(&id, &dir.path().join(&id))
            .expect("open should succeed");
        drop(store);
    }
    let after = count_open_fds().expect("fd count available on this platform");
    let growth = after.saturating_sub(before);

    // 8 resident redb handles + slack for test-harness/allocator fd churn.
    assert!(
        growth <= 24,
        "fd growth after 120 palaces must stay bounded, grew by {growth} \
         (before={before}, after={after})"
    );
    assert_eq!(cache.len(), 8, "resident entries must be capped");
}
