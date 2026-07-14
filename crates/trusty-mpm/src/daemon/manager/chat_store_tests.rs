//! Unit tests for [`super::ChatStore`] (WI-4, #2581).
//!
//! Why: the conversation store's round-trip, bounding, and per-key isolation are
//! the invariants the multi-turn chat loop depends on; prove them directly.
//! What: covers record→history round-trip, the retention cap, and key isolation.
//! Test: this file IS the test module.

use super::{ChatStore, TurnRole};

/// Why: a recorded exchange must reappear in history oldest-first with roles.
/// Test: itself.
#[test]
fn record_and_history_round_trips() {
    let store = ChatStore::default();
    let count = store.record_exchange("conv-1", "what's up?", "all quiet");
    assert_eq!(count, 2);
    let history = store.history("conv-1");
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].role, TurnRole::User);
    assert_eq!(history[0].content, "what's up?");
    assert_eq!(history[1].role, TurnRole::Assistant);
    assert_eq!(history[1].content, "all quiet");
}

/// Why: a long-lived conversation must not grow without bound; the window keeps
/// the most recent turns.
/// Test: itself.
#[test]
fn history_is_bounded() {
    let store = ChatStore::new(4);
    for i in 0..5 {
        store.record_exchange("conv-1", &format!("q{i}"), &format!("a{i}"));
    }
    let history = store.history("conv-1");
    assert_eq!(history.len(), 4, "capped at max_turns");
    // Oldest retained is the second-to-last exchange's user turn.
    assert_eq!(history[0].content, "q3");
    assert_eq!(history[3].content, "a4");
}

/// Why: distinct conversation keys must never bleed context into each other.
/// Test: itself.
#[test]
fn distinct_keys_are_isolated() {
    let store = ChatStore::default();
    store.record_exchange("conv-a", "hi a", "reply a");
    store.record_exchange("conv-b", "hi b", "reply b");
    assert_eq!(store.history("conv-a").len(), 2);
    assert_eq!(store.history("conv-b")[0].content, "hi b");
    assert!(store.history("conv-missing").is_empty());
}
