//! Unit tests for the cleanup sweep's `gh`-auth backoff (#8058).

use super::*;

/// Why (#8058): the 2026-09-15 log's failure was `gh auth login`, and the gate
/// must recognise the wording `gh` actually prints rather than a shape invented
/// here.
#[test]
fn auth_failure_is_recognised_from_gh_wording() {
    for message in [
        "`gh pr view 42 --json state` failed: To get started with GitHub CLI, please run: gh auth login.",
        "gh: Authentication required",
        "HTTP 401: Bad credentials",
        "gh: To use GitHub CLI set the GH_TOKEN environment variable",
    ] {
        assert!(is_auth_failure(message), "not recognised: {message}");
    }
}

/// Why: a 404 or a parse error is a per-entry problem, not a host-wide one.
/// Suspending the whole sweep for one bad registry entry would turn a single
/// stale row into an outage.
#[test]
fn a_plain_failure_is_not_an_auth_failure() {
    for message in [
        "`gh pr view 42` failed: GraphQL: Could not resolve to a PullRequest",
        "cannot parse `gh pr view 42` JSON: expected value at line 1",
        "gh: connection reset by peer",
    ] {
        assert!(!is_auth_failure(message), "wrongly recognised: {message}");
    }
}

/// Why: the whole point is that the sweep stops calling `gh`. The window must
/// open at the strike count and lift on its own.
#[test]
fn strikes_suspend_the_sweep_and_the_window_expires() {
    let gate = AuthBackoff::new();
    let t0 = Instant::now();
    assert!(gate.may_poll(t0));
    for _ in 0..STRIKES {
        gate.record_auth_failure(t0, "gh auth login");
    }
    assert!(!gate.may_poll(t0), "the window must be open");
    assert!(
        !gate.may_poll(t0 + BACKOFF_BASE - Duration::from_secs(1)),
        "the window must still be open one second early"
    );
    assert!(
        gate.may_poll(t0 + BACKOFF_BASE + Duration::from_secs(1)),
        "the window must lift on its own"
    );
}

/// Why (#8058): the defect was one `warn!` per pending entry per tick. One line
/// per window is the contract.
#[test]
fn an_auth_failure_is_reported_once_per_window() {
    let gate = AuthBackoff::new();
    let t0 = Instant::now();
    let reports: Vec<bool> = (0..5)
        .map(|_| gate.record_auth_failure(t0, "gh auth login"))
        .collect();
    assert_eq!(
        reports.iter().filter(|r| **r).count(),
        usize::try_from(STRIKES).expect("STRIKES fits usize"),
        "reported on: {reports:?}"
    );
}

/// Why: an answer of any kind proves the credential worked, so holding strikes
/// across it would suspend a healthy sweep.
#[test]
fn any_answer_clears_the_strikes() {
    let gate = AuthBackoff::new();
    let t0 = Instant::now();
    for _ in 0..STRIKES {
        gate.record_auth_failure(t0, "gh auth login");
    }
    assert!(!gate.may_poll(t0));
    gate.record_answer();
    assert!(gate.may_poll(t0));
    assert!(gate.degraded_reason().is_none());
}

/// Why: `/health` must name the reason, not merely a boolean, and must say
/// nothing at all while the sweep is fine.
#[test]
fn degraded_reason_is_published_only_while_suspended() {
    let gate = AuthBackoff::new();
    let t0 = Instant::now();
    assert!(gate.degraded_reason().is_none());
    for _ in 0..STRIKES {
        gate.record_auth_failure(t0, "please run: gh auth login");
    }
    let reason = gate
        .degraded_reason()
        .expect("a suspended sweep has a reason");
    assert!(reason.contains("pr-cleanup sweep suspended"), "{reason}");
    assert!(reason.contains("gh auth login"), "{reason}");
}

/// Why: a window that only ever grows is a permanently disabled feature; one
/// that never grows keeps paying for a dependency that has failed six times.
#[test]
fn backoff_grows_and_then_saturates() {
    assert_eq!(backoff_for(STRIKES), BACKOFF_BASE);
    assert_eq!(backoff_for(STRIKES + 1), BACKOFF_BASE * 2);
    assert_eq!(backoff_for(STRIKES + 40), BACKOFF_MAX);
}

/// Why: the daemon's strike count must outlive a single tick, which it only
/// does if every tick sees the same gate.
#[test]
fn shared_is_one_instance() {
    assert!(std::ptr::eq(shared(), shared()));
}
