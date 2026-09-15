//! Unit tests for the diff-base freshness gate (#7748).
//!
//! Why: the gate's whole value is in the arms that REFUSE, and each of them —
//! a ref that stays behind, a read that cannot be made — is unreachable against
//! a real remote in a unit test.
//! What: a scripted [`BaseRefs`] probe, one test per verdict.
//! Test: this file IS the test module.

use std::cell::RefCell;

use super::{BaseFreshness, BaseRefs, check};

/// A `BaseRefs` whose three answers are scripted.
struct FakeRefs {
    /// Local shas, popped in order: the pre-fetch read, then the post-fetch one.
    local: RefCell<Vec<Result<Option<String>, String>>>,
    /// What `ls-remote` reports.
    remote: Result<String, String>,
    /// What the fetch does.
    fetch: Result<(), String>,
    /// Whether the fetch was issued.
    fetched: RefCell<bool>,
}

impl FakeRefs {
    /// A probe whose local ref reads `first` and then `then`.
    fn new(first: &str, then: &str, remote: &str) -> Self {
        Self {
            local: RefCell::new(vec![
                Ok(Some(first.to_string())),
                Ok(Some(then.to_string())),
            ]),
            remote: Ok(remote.to_string()),
            fetch: Ok(()),
            fetched: RefCell::new(false),
        }
    }
}

impl BaseRefs for FakeRefs {
    fn local(&self, _base: &str) -> Result<Option<String>, String> {
        let mut queue = self.local.borrow_mut();
        if queue.is_empty() {
            return Ok(None);
        }
        queue.remove(0)
    }
    fn remote(&self, _base: &str) -> Result<String, String> {
        self.remote.clone()
    }
    fn fetch(&self, _base: &str) -> Result<(), String> {
        *self.fetched.borrow_mut() = true;
        self.fetch.clone()
    }
}

#[test]
fn fresh_when_the_local_ref_already_matches_the_remote() {
    let refs = FakeRefs::new("aaa111", "aaa111", "aaa111");
    let verdict = check(&refs, "main");
    assert_eq!(
        verdict,
        BaseFreshness::Fresh {
            sha: "aaa111".to_string()
        }
    );
    assert!(verdict.refusal().is_none());
    assert!(
        !*refs.fetched.borrow(),
        "an already-current base must cost no fetch"
    );
}

/// The reported case: the checkout is behind, and the fetch closes the gap.
#[test]
fn a_stale_ref_is_refreshed_by_the_fetch() {
    let refs = FakeRefs::new("old111", "new222", "new222");
    let verdict = check(&refs, "main");
    assert_eq!(
        verdict,
        BaseFreshness::Refreshed {
            was: "old111".to_string(),
            now: "new222".to_string()
        }
    );
    assert!(
        verdict.refusal().is_none(),
        "a base this call brought current is diffable"
    );
    assert!(*refs.fetched.borrow(), "the gap must be closed by a fetch");
}

/// #7748: a base that stays behind refuses, naming the stale sha and the remote.
#[test]
fn a_ref_that_stays_behind_refuses_and_names_both_shas() {
    let refs = FakeRefs::new("old111", "old111", "new222");
    let refusal = check(&refs, "main")
        .refusal()
        .expect("a base that is still behind must refuse");
    assert!(refusal.contains("old111"), "{refusal}");
    assert!(refusal.contains("new222"), "{refusal}");
}

/// #7748 fail-closed: an unanswerable comparison is a refusal, not a pass.
///
/// Why: this is the security half. The scan's base is the thing being verified,
/// so "could not verify" must never be reported to a caller as "verified".
#[test]
fn an_unanswerable_comparison_refuses_rather_than_passing() {
    let refs = FakeRefs {
        local: RefCell::new(vec![Ok(Some("old111".to_string()))]),
        remote: Err("`git ls-remote origin refs/heads/main` failed: no such remote".to_string()),
        fetch: Ok(()),
        fetched: RefCell::new(false),
    };
    let verdict = check(&refs, "main");
    assert!(
        matches!(verdict, BaseFreshness::Undetermined { .. }),
        "{verdict:?}"
    );
    let refusal = verdict.refusal().expect("an unverified base must refuse");
    assert!(refusal.contains("no such remote"), "{refusal}");
    assert!(
        !*refs.fetched.borrow(),
        "nothing to fetch toward when the remote never answered"
    );
}

/// An empty `ls-remote` answer is unanswerable too, not an empty agreement.
#[test]
fn an_empty_remote_answer_is_undetermined() {
    let refs = FakeRefs {
        local: RefCell::new(vec![Ok(None)]),
        remote: Ok(String::new()),
        fetch: Ok(()),
        fetched: RefCell::new(false),
    };
    assert!(check(&refs, "main").refusal().is_some());
}

/// A failed fetch leaves an established disagreement, so it reports `Stale`.
#[test]
fn a_failed_fetch_reports_stale_rather_than_undetermined() {
    let refs = FakeRefs {
        local: RefCell::new(vec![Ok(Some("old111".to_string()))]),
        remote: Ok("new222".to_string()),
        fetch: Err("could not read from remote repository".to_string()),
        fetched: RefCell::new(false),
    };
    let verdict = check(&refs, "main");
    let refusal = verdict.refusal().expect("a failed refresh must refuse");
    assert!(refusal.contains("old111"), "{refusal}");
    assert!(refusal.contains("could not read"), "{refusal}");
}

/// A base ref this clone has never fetched reads as `absent`, not as a blank.
#[test]
fn an_absent_local_ref_is_named_in_the_refusal() {
    let refs = FakeRefs {
        local: RefCell::new(vec![Ok(None), Ok(None)]),
        remote: Ok("new222".to_string()),
        fetch: Ok(()),
        fetched: RefCell::new(false),
    };
    let refusal = check(&refs, "main")
        .refusal()
        .expect("no local base ref is not a diffable base");
    assert!(refusal.contains("absent"), "{refusal}");
}
