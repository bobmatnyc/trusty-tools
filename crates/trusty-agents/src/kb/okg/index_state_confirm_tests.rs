//! The `unconfirmed` count of the coverage read (#10746 S12 fix round).
//!
//! Why: with `synced` held at zero until an index identity exists
//! (SPEC-OKF-01 §4.9.2 rule 5), settled work must still be visible.
//! What: drives `KbStore::source_index_state` with and without a confirmed
//! identity.
//! Test: `cargo test -p trusty-agents kb::okg::index_state::confirm_tests`.

use super::tests::{confirm, fixture};
use super::*;

/// Why: settled paths read `unconfirmed` offline and `synced` only when a
/// Searchable pass confirmed the identity.
/// What: both entities confirmed; offline is (0, 2 unconfirmed), confirmed
/// is (2, 0). Test: self-contained.
#[test]
fn settled_paths_read_unconfirmed_until_the_identity_is_confirmed() {
    let (tmp, store, spec) = fixture();
    let state = IndexStateRef::new(tmp.path().join("state"), "idx");
    confirm(&store, &state, "notes/one.md");
    confirm(&store, &state, "notes/two.md");
    let view = store.index_view(Some(&state)).unwrap();
    let offline = store
        .source_index_state(&spec, view.as_ref(), false)
        .unwrap();
    assert_eq!(
        (
            offline.coverage.synced,
            offline.coverage.unconfirmed,
            offline.coverage.pending
        ),
        (0, 2, 0)
    );
    assert!(offline.blocking.is_empty());
    let confirmed = store
        .source_index_state(&spec, view.as_ref(), true)
        .unwrap();
    assert_eq!(
        (confirmed.coverage.synced, confirmed.coverage.unconfirmed),
        (2, 0)
    );
}
