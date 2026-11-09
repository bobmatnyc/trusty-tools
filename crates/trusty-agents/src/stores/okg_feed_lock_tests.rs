//! The feed lock and the `unconfirmed` report field (#10746 S12 fix round).
//!
//! Why: one feed pass runs per state journal, and the lock must be released
//! on every exit path; a pass's settled work reads `unconfirmed` until an
//! index identity exists (SPEC-OKF-01 §4.9.2 rule 5).
//! What: drives `FeedLock` directly and `feed_source` against the fake
//! daemon of `okg_feed_tests.rs`.
//! Test: `cargo test -p trusty-agents stores::okg_feed::lock_tests`.

use super::tests::{Fake, fixture};
use super::*;

/// Why: a panic inside a pass must not leave the lock held, or every later
/// feed would report "already running" forever.
/// What: a guard taken inside a closure that panics is released by the
/// unwind; a second guard is refused while one is held and granted after.
/// Test: self-contained.
#[test]
fn feed_lock_is_released_on_panic() {
    let (tmp, store, _) = fixture();
    let st = IndexStateRef::new(tmp.path().join("state"), "idx");
    let root = store.root.clone();
    let st2 = st.clone();
    let unwound = std::panic::catch_unwind(move || {
        let _guard = FeedLock::try_acquire(&root, &st2).unwrap().unwrap();
        panic!("synthetic failure inside a feed pass");
    });
    assert!(unwound.is_err());
    let held = FeedLock::try_acquire(&store.root, &st).unwrap();
    assert!(held.is_some(), "the panic released the lock");
    assert!(
        FeedLock::try_acquire(&store.root, &st).unwrap().is_none(),
        "a second caller is refused while it is held"
    );
    drop(held);
    assert!(FeedLock::try_acquire(&store.root, &st).unwrap().is_some());
}

/// Why: with `synced` at zero before D1, the report must still say the
/// pass settled this source's entities.
/// What: a clean pass reports both entities `unconfirmed`, none `synced`.
/// Test: self-contained.
#[tokio::test]
async fn pass_reports_unconfirmed_not_synced() {
    let (tmp, store, spec) = fixture();
    let st = IndexStateRef::new(tmp.path().join("state"), "idx");
    let fake = Fake::new(&store.root);
    let r = feed_source(&store, &spec, &st, &fake).await;
    assert_eq!((r.synced, r.unconfirmed, r.pending), (0, 2, 0), "{r:?}");
    assert!(r.acknowledged("idx"));
}
