//! Unit tests for [`super::ChatStore`] (WI-4, #2581; key-set bound #2602).
//!
//! Why: the conversation store's round-trip, bounding, per-key isolation, AND
//! the distinct-key-set LRU eviction are the invariants the multi-turn chat loop
//! (and, from phase 4 on, unauthenticated public channels) depend on; prove them
//! directly.
//! What: covers record→history round-trip, the per-key retention cap, key
//! isolation, and the #2602 key-set cap's least-recently-used eviction.
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
    let store = ChatStore::new(4, super::DEFAULT_MAX_KEYS);
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

/// Why: #2602 — the KEY SET must be bounded independent of the per-key turn
/// cap, or a caller minting distinct keys (unauthenticated public channels from
/// phase 4 on) grows daemon memory forever. A 2-key-capacity store must evict
/// the least-recently-used key once a third distinct key is recorded.
/// Test: itself.
#[test]
fn key_set_is_bounded_lru_eviction() {
    let store = ChatStore::new(super::DEFAULT_MAX_TURNS, 2);
    store.record_exchange("conv-1", "hi 1", "reply 1");
    store.record_exchange("conv-2", "hi 2", "reply 2");
    // Recording a third distinct key overflows the 2-key capacity; "conv-1" is
    // the least-recently-used (never read/re-written since insertion) and is
    // evicted, while "conv-2" and the new "conv-3" both survive.
    store.record_exchange("conv-3", "hi 3", "reply 3");

    assert!(
        store.history("conv-1").is_empty(),
        "least-recently-used key must be evicted"
    );
    assert_eq!(store.history("conv-2").len(), 2, "conv-2 survives eviction");
    assert_eq!(
        store.history("conv-3").len(),
        2,
        "newly-inserted key survives"
    );
}

/// Why: reading a key via [`super::ChatStore::history`] promotes it to
/// most-recently-used, so an actively-read (but not recently-written)
/// conversation must survive eviction in place of a truly idle one.
/// Test: itself.
#[test]
fn reading_a_key_promotes_it_and_protects_from_eviction() {
    let store = ChatStore::new(super::DEFAULT_MAX_TURNS, 2);
    store.record_exchange("conv-1", "hi 1", "reply 1");
    store.record_exchange("conv-2", "hi 2", "reply 2");
    // Touch "conv-1" so it becomes MRU; "conv-2" is now the LRU candidate.
    let _ = store.history("conv-1");
    store.record_exchange("conv-3", "hi 3", "reply 3");

    assert_eq!(
        store.history("conv-1").len(),
        2,
        "recently-read key survives"
    );
    assert!(
        store.history("conv-2").is_empty(),
        "un-touched key is evicted instead"
    );
    assert_eq!(store.history("conv-3").len(), 2);
}

/// Why: an evicted key must degrade gracefully, not error — the next chat turn
/// on that (now-stale) key simply starts a fresh empty history rather than
/// panicking or corrupting the store.
/// Test: itself.
#[test]
fn evicted_key_reads_as_empty_and_can_restart() {
    let store = ChatStore::new(super::DEFAULT_MAX_TURNS, 1);
    store.record_exchange("conv-1", "hi 1", "reply 1");
    store.record_exchange("conv-2", "hi 2", "reply 2");
    assert!(
        store.history("conv-1").is_empty(),
        "evicted by the 1-key cap"
    );

    // "conv-1" restarts cleanly as a brand-new conversation.
    let count = store.record_exchange("conv-1", "hi again", "reply again");
    assert_eq!(count, 2);
    assert_eq!(store.history("conv-1")[0].content, "hi again");
}
