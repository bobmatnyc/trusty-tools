//! Unit tests for the bounded-concurrency external-source resolution seam
//! (issue #2719). HTTP-free: they exercise dedupe, once-per-message invocation,
//! and the concurrency bound with a counting/gauging fake resolver.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use super::{resolve_unique, unique_unresolved_messages};
use crate::classify::pipeline_db::CommitRow;
use crate::classify::sources::ExternalSignal;
use crate::classify::tiers::ClassificationResult;

/// Build a minimal `CommitRow` for dedupe tests.
fn row(id: i64, message: &str) -> CommitRow {
    CommitRow {
        id,
        sha: format!("sha{id}"),
        message: message.to_string(),
        is_merge: false,
        repository: "acme/widgets".to_string(),
        existing_classification_id: None,
    }
}

/// Build a canned external signal.
fn signal(source: String) -> ExternalSignal {
    ExternalSignal {
        category: "bug_fix".to_string(),
        confidence: 0.92,
        source,
    }
}

/// Why: the concurrent path must resolve each *unique* message exactly once —
/// firing a duplicate fetch per commit is the regression this fix guards against.
/// What: feed three distinct messages through `resolve_unique` with a counting
/// fake resolver and assert the fake was invoked exactly three times and every
/// message landed in the result map.
/// Test: HTTP-free; counts invocations via an `AtomicUsize`.
#[tokio::test]
async fn resolve_unique_invokes_once_per_message() {
    let calls = AtomicUsize::new(0);
    let calls_ref = &calls;
    let messages = vec![
        "PROJ-1 fix null".to_string(),
        "PROJ-2 add widget".to_string(),
        "PROJ-3 tidy".to_string(),
    ];

    let map = resolve_unique(messages.clone(), 8, |m| async move {
        calls_ref.fetch_add(1, Ordering::SeqCst);
        Some(signal(m))
    })
    .await;

    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "each unique message must be resolved exactly once"
    );
    assert_eq!(
        map.len(),
        3,
        "every resolved message must appear in the map"
    );
    for m in &messages {
        assert!(map.contains_key(m), "missing signal for {m}");
    }
}

/// Why: the whole point of #2719 is to replace the serial network loop with a
/// bounded fan-out. A wall-clock assertion (`elapsed < serial / 2`) is the
/// exact load-sensitive flake class removed in #2714 — under CI scheduler
/// contention, sleeps can jitter enough to falsely fail. Replace it with a
/// deterministic gauge: an `AtomicUsize` incremented on entry and decremented
/// on exit of each fake-resolve call, tracking the maximum concurrently
/// in-flight count. This proves the *bound* holds without depending on
/// wall-clock timing at all.
/// What: resolve 16 messages through `resolve_unique` at `concurrency = 8`;
/// each fake resolve increments the gauge, yields once (so `buffer_unordered`
/// has a chance to launch further futures up to the bound before any
/// completes), then decrements. Asserts `1 < max_seen <= concurrency` —
/// strictly more than one in flight (proving concurrency actually happened,
/// not accidental seriality) and never more than the configured bound.
/// Test: HTTP-free, deterministic; no timing assumptions.
#[tokio::test]
async fn resolve_unique_bounds_concurrency() {
    let n = 16usize;
    let concurrency = 8usize;
    let messages: Vec<String> = (0..n).map(|i| format!("PROJ-{i} msg")).collect();

    let in_flight = AtomicUsize::new(0);
    let max_seen = AtomicUsize::new(0);
    let in_flight_ref = &in_flight;
    let max_seen_ref = &max_seen;

    let map = resolve_unique(messages, concurrency, |m| async move {
        let now = in_flight_ref.fetch_add(1, Ordering::SeqCst) + 1;
        max_seen_ref.fetch_max(now, Ordering::SeqCst);
        // Yield control (without depending on real elapsed time) so
        // `buffer_unordered` gets a chance to poll further futures up to the
        // concurrency bound before this one completes.
        tokio::time::sleep(Duration::from_millis(1)).await;
        in_flight_ref.fetch_sub(1, Ordering::SeqCst);
        Some(signal(m))
    })
    .await;

    let observed_max = max_seen.load(Ordering::SeqCst);
    eprintln!("resolve_unique: n={n} concurrency={concurrency} max_in_flight={observed_max}");

    assert_eq!(map.len(), n, "all messages resolved");
    assert!(
        observed_max > 1,
        "expected genuine concurrency (max_in_flight={observed_max}), \
         got effectively serial execution"
    );
    assert!(
        observed_max <= concurrency,
        "max_in_flight={observed_max} must never exceed the configured bound \
         ({concurrency})"
    );
}

/// Why: dedupe-before-spawn is the mechanism that keeps the concurrent path
/// fetching each ticket once; it must also honour Tier-0 overrides.
/// What: four commits — two sharing a message, one distinct, one carrying a
/// manual override — must reduce to the two distinct non-overridden messages
/// in first-occurrence order.
/// Test: pure function, no async, no HTTP.
#[test]
fn unique_messages_dedupes_and_excludes_overrides() {
    let commits = vec![
        row(1, "PROJ-1 fix"),
        row(2, "PROJ-1 fix"), // duplicate message → collapses
        row(3, "PROJ-2 feat"),
        row(4, "PROJ-3 manual"), // excluded: has a Tier-0 override
    ];
    let mut overrides: HashMap<i64, ClassificationResult> = HashMap::new();
    overrides.insert(4, ClassificationResult::unclassified());

    let unique = unique_unresolved_messages(&commits, &overrides);
    assert_eq!(
        unique,
        vec!["PROJ-1 fix".to_string(), "PROJ-2 feat".to_string()],
        "duplicates collapse and override commits are excluded"
    );
}
