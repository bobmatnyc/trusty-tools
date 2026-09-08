//! Unit tests for the #7106 startup open budget.
//!
//! Why: the bound this module adds is invisible in every other signal — a
//! bounded startup run and an unbounded one open the same palaces and log the
//! same summary, and the difference only shows up as gigabytes on the host that
//! reported #7106. These tests assert the bound itself.
//! What: contention against the gate, the fail-open parse rule, and the two
//! refusals `release_after_sweep` owes the #7087 residency ruling.
//! Test: this file.

use super::*;
use std::sync::atomic::AtomicUsize;
use trusty_common::memory_core::palace::{Palace, PalaceId};

/// Why (#7106): the whole fix rests on "at most N palaces open at once". A gate
/// that admitted N+1 under contention would leave the daemon exactly as it was
/// while looking fixed.
/// What: 16 tasks each take a permit, record the live count they observe, hold
/// until every task has been admitted at least once, then release. Asserts the
/// observed peak equals the limit — never above it, and not below, which would
/// mean the gate serialised instead of bounding.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gate_never_exceeds_its_limit_under_contention() {
    let limit = 3;
    let gate = StartupOpenGate::with_limit(limit);
    let observed_peak = Arc::new(AtomicUsize::new(0));
    let live = Arc::new(AtomicUsize::new(0));

    let mut set = tokio::task::JoinSet::new();
    for _ in 0..16 {
        let gate = gate.clone();
        let observed_peak = Arc::clone(&observed_peak);
        let live = Arc::clone(&live);
        set.spawn(async move {
            let permit = gate.acquire().await;
            let now = live.fetch_add(1, Ordering::SeqCst) + 1;
            observed_peak.fetch_max(now, Ordering::SeqCst);
            // Hold long enough that the other tasks genuinely contend rather
            // than each finding the gate empty.
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            live.fetch_sub(1, Ordering::SeqCst);
            drop(permit);
        });
    }
    while set.join_next().await.is_some() {}

    let peak = observed_peak.load(Ordering::SeqCst);
    assert!(
        peak <= limit,
        "#7106: the gate admitted {peak} concurrent holders against a limit of {limit}"
    );
    assert_eq!(
        gate.peak_concurrent(),
        peak,
        "the gate's own high-water mark must agree with what the holders saw"
    );
    assert_eq!(
        peak, limit,
        "the gate must actually use its budget, not serialise"
    );
    assert_eq!(live.load(Ordering::SeqCst), 0, "every permit must release");
}

/// Why (#7106, Fail-Open Check): a rejected override must not silently restore
/// unbounded startup opens — that failure looks identical to a working daemon
/// until the host runs out of RAM.
/// What: garbage, a negative and an explicit `0` all yield the bounded default
/// plus a warning naming the variable and the offending value.
/// Test: this test.
#[test]
fn parse_open_limit_warns_and_keeps_the_default_on_garbage() {
    for bad in ["banana", "-2", "0", "4 palaces"] {
        let (limit, warning) = parse_open_limit(Some(bad));
        assert_eq!(
            limit, DEFAULT_STARTUP_OPEN_LIMIT,
            "{bad:?} must fall back to the bounded default"
        );
        let warning =
            warning.unwrap_or_else(|| panic!("{bad:?} must produce a warning, not a silence"));
        assert!(warning.contains(STARTUP_OPEN_LIMIT_ENV), "{warning}");
        assert!(warning.contains(bad), "{warning}");
    }
    assert_eq!(parse_open_limit(Some("2")), (2, None));
    assert_eq!(parse_open_limit(None), (DEFAULT_STARTUP_OPEN_LIMIT, None));
}

/// Build a palace on disk and open it into `registry`.
fn seed_palace(registry: &PalaceRegistry, root: &std::path::Path, id: &str) -> PalaceId {
    let palace = Palace {
        id: PalaceId::new(id.to_string()),
        name: id.to_string(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: root.join(id),
    };
    let handle = registry
        .create_palace(root, palace)
        .expect("create test palace");
    let palace_id = handle.id.clone();
    drop(handle);
    palace_id
}

/// Why (#7087): the owner's ruling is that a palace a client used recently
/// stays resident. A sweep that reclaimed it anyway would evict the working set
/// out from under an active session every time a background job ran.
/// What: stamps the palace's `last_used` file at "now", then asks
/// `release_after_sweep` to hand it back. Asserts it refuses and the handle is
/// still cached.
/// Test: this test.
#[test]
fn release_after_sweep_keeps_a_recently_used_palace() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let registry = PalaceRegistry::new();
    let id = seed_palace(&registry, tmp.path(), "recent");
    let data_dir = tmp.path().join(&id.0);
    crate::palace_last_used::write(&data_dir, crate::palace_last_used::now_unix())
        .expect("stamp last_used");

    let released = release_after_sweep(
        &registry,
        &id,
        &data_dir,
        false,
        std::time::Duration::from_secs(DEFAULT_KEEP_RECENT_SECS),
    );
    assert!(
        !released,
        "#7087: a palace used inside the keep-recent window must stay resident"
    );
    assert!(
        registry.peek(&id).is_some(),
        "the handle must still be cached"
    );
}

/// Why (#7106): the sweep may hand back only what it brought in. A palace that
/// was already resident was warmed by something else, and dropping it would
/// make a background job the reason a client's next call pays a cold open.
/// What: passes `was_resident_before = true` for a palace with no `last_used`
/// stamp at all — the case that would otherwise release — and asserts the
/// refusal. Then passes `false` for the same palace and asserts it does
/// release, so the refusal is the flag's doing and not a vacuous `false`.
/// Test: this test.
#[test]
fn release_after_sweep_keeps_an_already_resident_palace() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let registry = PalaceRegistry::new();
    let id = seed_palace(&registry, tmp.path(), "already-warm");
    let data_dir = tmp.path().join(&id.0);
    let keep = std::time::Duration::from_secs(DEFAULT_KEEP_RECENT_SECS);

    assert!(
        !release_after_sweep(&registry, &id, &data_dir, true, keep),
        "a palace resident before the sweep must not be released by it"
    );
    assert!(registry.peek(&id).is_some());

    assert!(
        release_after_sweep(&registry, &id, &data_dir, false, keep),
        "#7106: an unreferenced palace the sweep itself opened must be handed back"
    );
    assert!(
        registry.peek(&id).is_none(),
        "the handle must be gone from the cache; redb remains the source of truth"
    );
}

/// Why (#7106): `release_if_unreferenced` is the correctness anchor — a release
/// that raced an in-flight recall or dream cycle would drop a handle out from
/// under it.
/// What: holds a second `Arc` to the palace and asserts the release refuses,
/// then drops it and asserts the release succeeds.
/// Test: this test.
#[test]
fn release_after_sweep_refuses_while_a_reference_is_held() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let registry = PalaceRegistry::new();
    let id = seed_palace(&registry, tmp.path(), "in-flight");
    let data_dir = tmp.path().join(&id.0);
    let keep = std::time::Duration::from_secs(DEFAULT_KEEP_RECENT_SECS);

    let in_flight = registry.peek(&id).expect("palace is resident");
    assert!(
        !release_after_sweep(&registry, &id, &data_dir, false, keep),
        "a referenced handle must never be released"
    );
    drop(in_flight);
    assert!(release_after_sweep(&registry, &id, &data_dir, false, keep));
}
