//! Unit tests for the `gh_account` doctor check — every branch of
//! `build_gh_account_check`, driven by a constructed [`GhAuthProbe`] so no live
//! `gh` is needed.
//! Test: this module IS the test suite for `super`.

use super::*;

/// An answered probe carrying `active` and `logged_in` — the shape
/// [`build_gh_account_check`] folds (#5032 changed it from two loose arguments).
fn answered(active: Option<&str>, logged_in: &[&str]) -> GhAuthProbe {
    GhAuthProbe::Answered(GhAccountStatus {
        active: active.map(str::to_string),
        logged_in: logged_in.iter().map(|s| s.to_string()).collect(),
    })
}

#[test]
fn build_gh_account_check_single_ok() {
    // A single clear active account is healthy (Ok), naming the login.
    let check = build_gh_account_check(answered(Some("bobmatnyc"), &["bobmatnyc"]));
    assert_eq!(check.status, CheckStatus::Ok);
    assert_eq!(check.name, "gh_account");
    assert!(check.message.contains("bobmatnyc"));
}

#[test]
fn build_gh_account_check_multi_warn() {
    // Multiple logged-in accounts is the ambiguity that hid the admin-merge
    // bug: Warn, name both accounts, and point at `gh auth switch`.
    let check = build_gh_account_check(answered(Some("bob-duetto"), &["bob-duetto", "bobmatnyc"]));
    assert_eq!(check.status, CheckStatus::Warn);
    assert!(check.message.contains("bob-duetto"));
    assert!(check.message.contains("bobmatnyc"));
    assert!(check.message.contains("gh auth switch"));
}

#[test]
fn build_gh_account_check_unauthenticated_warn() {
    // No account at all: Warn (advisory), not Fail, pointing at `gh auth login`.
    let check = build_gh_account_check(answered(None, &[]));
    assert_eq!(check.status, CheckStatus::Warn);
    assert!(check.message.contains("not authenticated"));
    assert!(check.message.contains("gh auth login"));
}

#[test]
fn build_gh_account_check_inconclusive_is_not_unauthenticated() {
    // #5032: a probe that could not finish must read as UNKNOWN. It must not
    // claim the definite negative the unauthenticated arm claims.
    let check = build_gh_account_check(GhAuthProbe::Inconclusive(
        "`gh auth status` did not answer within 5s".to_string(),
    ));
    assert_eq!(check.status, CheckStatus::Warn);
    assert!(check.message.contains("UNKNOWN"));
    assert!(check.message.contains("did not answer"));
    assert!(!check.message.contains("gh is not authenticated"));
    assert!(!check.message.contains("gh auth login"));
    // The two states must be distinguishable in the rendered output.
    assert_ne!(
        check.message,
        build_gh_account_check(answered(None, &[])).message
    );
}

#[test]
fn gh_account_check_is_advisory_only() {
    // Every branch must be advisory — never a hard Fail.
    for check in [
        build_gh_account_check(answered(Some("a"), &["a"])),
        build_gh_account_check(answered(Some("a"), &["a", "b"])),
        build_gh_account_check(answered(None, &[])),
        build_gh_account_check(answered(None, &["a"])),
        build_gh_account_check(GhAuthProbe::Inconclusive("timed out".to_string())),
    ] {
        assert_ne!(
            check.status,
            CheckStatus::Fail,
            "gh_account must never Fail"
        );
    }
}
