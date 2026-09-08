//! Unit tests for the bounded recall-all fan-out (issue #7125).
//!
//! Why: the integration tests in `tests/recall_all_residency.rs` drive the real
//! `memory_recall_all` dispatch, which only ever hands `recall_streamed` a
//! search that succeeds. The failure path — a search that errors partway
//! through the estate — has no route in from outside the crate, and it is the
//! path where a leaked batch would be invisible.
//! What: drives `recall_streamed` directly with a search that fails on its
//! second batch, plus the batch-walk bookkeeping (how many batches ran, how
//! wide each was).
//! Test: this IS the test module.

use super::recall_stream::{recall_streamed, RECALL_PALACE_BATCH};
use crate::AppState;
use anyhow::anyhow;
use chrono::Utc;
use std::cell::RefCell;
use std::path::Path;
use trusty_common::memory_core::palace::{Palace, PalaceId};
use trusty_common::memory_core::PalaceRegistry;

/// Create `n` palaces on disk and close every handle again.
///
/// Why: residency assertions need a baseline the fixture did not pollute.
/// What: creates through a throwaway registry, then drops it.
fn seed(data_root: &Path, n: usize) -> Vec<Palace> {
    let registry = PalaceRegistry::new();
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let id = PalaceId::new(format!("unit-{i:02}"));
        let palace = Palace {
            id: id.clone(),
            name: id.as_str().to_string(),
            description: None,
            created_at: Utc::now(),
            data_dir: data_root.join(id.as_str()),
        };
        registry
            .create_palace(data_root, palace.clone())
            .unwrap_or_else(|e| panic!("create_palace({id}) failed: {e:#}"));
        out.push(palace);
    }
    drop(registry);
    out
}

/// An erroring search still hands back the batch it was given.
///
/// Why (#7125): release runs before the `?` precisely so a failing search
/// cannot leak. Move the release below the `?` and this test reads
/// `RECALL_PALACE_BATCH` resident handles instead of zero — a leak nothing else
/// in the suite would notice, because the caller sees an error either way.
/// What: seeds two batches' worth of palaces, fails the search on the second
/// batch, asserts the error propagates AND the registry is back to its
/// baseline.
/// Test: this test.
#[tokio::test]
async fn recall_all_releases_every_batch_when_the_search_fails() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let palaces = seed(tmp.path(), RECALL_PALACE_BATCH * 2);
    let state = AppState::new(tmp.path().to_path_buf());
    assert_eq!(state.registry.len(), 0, "baseline: nothing open");

    let calls = RefCell::new(0usize);
    let outcome = recall_streamed(&state, &palaces, "unit", 5, |handles| {
        let mut n = calls.borrow_mut();
        *n += 1;
        let fail = *n == 2;
        assert_eq!(
            handles.len(),
            RECALL_PALACE_BATCH,
            "each batch must be exactly one chunk wide"
        );
        async move {
            if fail {
                Err(anyhow!("search exploded"))
            } else {
                Ok(Vec::new())
            }
        }
    })
    .await;

    assert!(outcome.is_err(), "the search error must reach the caller");
    assert_eq!(*calls.borrow(), 2, "the walk stops at the failing batch");
    assert_eq!(
        state.registry.len(),
        0,
        "a failing search must not leak the batch it was handed; {} handle(s) \
         left resident",
        state.registry.len()
    );
}

/// The walk covers every palace, one bounded chunk at a time.
///
/// Why (#7125): the bound is the whole point, and "searched everything" is the
/// correctness constraint it must not break — answering from a subset would be
/// a wrong answer that looks like a right one (#4637).
/// What: seeds a non-multiple of the batch width, records the ids each batch
/// carried, and asserts every palace was visited exactly once and no batch
/// exceeded `RECALL_PALACE_BATCH`.
/// Test: this test.
#[tokio::test]
async fn recall_streamed_visits_every_palace_in_bounded_batches() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let count = RECALL_PALACE_BATCH * 2 + 3;
    let palaces = seed(tmp.path(), count);
    let state = AppState::new(tmp.path().to_path_buf());

    let seen: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let widths: RefCell<Vec<usize>> = RefCell::new(Vec::new());
    let outcome = recall_streamed(&state, &palaces, "unit", 5, |handles| {
        widths.borrow_mut().push(handles.len());
        seen.borrow_mut()
            .extend(handles.iter().map(|h| h.id.as_str().to_string()));
        async move { Ok(Vec::new()) }
    })
    .await;

    outcome.expect("a search that never fails must not error");
    let mut visited = seen.into_inner();
    visited.sort();
    assert_eq!(visited.len(), count, "every palace is visited exactly once");
    let mut expected: Vec<String> = palaces.iter().map(|p| p.id.as_str().to_string()).collect();
    expected.sort();
    assert_eq!(visited, expected, "and the set is the whole estate");
    assert!(
        widths.borrow().iter().all(|w| *w <= RECALL_PALACE_BATCH),
        "no batch may exceed the residency bound: {:?}",
        widths.borrow()
    );
    assert_eq!(state.registry.len(), 0, "nothing is left resident");
}
