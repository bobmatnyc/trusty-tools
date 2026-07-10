//! Per-conversation turn serialization for the SM context engine (#1309).
//!
//! Why: the file-based [`SmContextEngine`](crate::core::sm::context::SmContextEngine)
//! is opened fresh per turn and persists by ATOMIC WHOLE-FILE replace with NO
//! merge. Two concurrent turns for the SAME `conv_id` each load the state into
//! their own in-memory conversation, append their round, and save — so the second
//! save clobbers the first and SILENTLY LOSES a round, even though that round's
//! reply was already returned to the caller (a data-integrity bug, #1309). This
//! became reachable once the SM was wired into the async `coordinator/chat`
//! endpoint, which can drive multiple in-flight turns for one `conv_id`.
//! Serializing turns per `conv_id` closes the read-modify-write race with the
//! lightest correct mechanism — a per-`conv_id` async lock held across
//! open → record → save — mirroring the per-palace `write_mutex` precedent in
//! `trusty_common::memory_core::retrieval::handle`.
//! What: [`ConvLocks`] is a small registry of per-`conv_id` [`tokio::sync::Mutex`]es.
//! [`ConvLocks::acquire`] hands back an owned guard the turn holds for its whole
//! duration; concurrent turns for the SAME id wait for it, turns for DIFFERENT ids
//! never block each other. The registry stores [`Weak`] handles and prunes dead
//! entries on every miss, so it stays bounded by the number of concurrently-active
//! conversations rather than growing once per `conv_id` seen for the daemon's life.
//! Test: `chat_tests.rs::concurrent_turns_same_conv_id_persist_both_rounds`
//! (two concurrent same-id turns both persist — the pre-fix loss is gone) and
//! `different_conv_ids_do_not_serialize` (distinct ids run without blocking).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

/// A registry of per-`conv_id` async turn locks (#1309).
///
/// Why: the SM agent is shared (an `Arc<SessionManagerAgent>` on the daemon
/// state), so a single registry living on the agent lets every concurrent chat
/// turn coordinate through the SAME per-`conv_id` lock. Cloning the agent shares
/// the registry (it is an `Arc` field), so serialization holds across clones too.
/// What: wraps a [`std::sync::Mutex`] over a map from `conv_id` to a [`Weak`]
/// handle on that conversation's [`AsyncMutex`]. The std mutex guards ONLY the
/// tiny map lookup/insert (never held across an await); the async mutex is the one
/// a turn actually holds across its `open → record → save` critical section.
/// Test: see the module-level tests.
pub(super) struct ConvLocks {
    /// Map of `conv_id` → a weak handle on its turn lock. Weak so a conversation
    /// with no in-flight turn drops its lock and the entry can be pruned.
    locks: Mutex<HashMap<String, Weak<AsyncMutex<()>>>>,
}

impl ConvLocks {
    /// Build an empty lock registry.
    ///
    /// Why: the agent constructs one per instance (shared across clones via the
    /// `Arc` field); no I/O and no allocation beyond an empty map.
    /// What: returns a [`ConvLocks`] wrapping an empty map.
    /// Test: exercised by every `ConvLocks` test via the agent constructors.
    pub(super) fn new() -> Self {
        Self {
            locks: Mutex::new(HashMap::new()),
        }
    }

    /// Acquire the exclusive turn lock for `conv_id`, awaiting any in-flight turn.
    ///
    /// Why: the returned guard MUST be held for the whole turn (open → record →
    /// save) so no other turn for the same `conv_id` can interleave its
    /// read-modify-write on the state file. Turns for different ids get distinct
    /// locks and never block one another.
    /// What: looks up (or mints) the per-`conv_id` [`AsyncMutex`] and awaits its
    /// owned lock, returning the [`OwnedMutexGuard`]. The caller drops the guard by
    /// letting it fall out of scope at the end of the turn.
    /// Test: `concurrent_turns_same_conv_id_persist_both_rounds`,
    /// `different_conv_ids_do_not_serialize`.
    pub(super) async fn acquire(&self, conv_id: &str) -> OwnedMutexGuard<()> {
        self.lock_for(conv_id).lock_owned().await
    }

    /// Return the [`AsyncMutex`] for `conv_id`, creating it on a miss and pruning
    /// any dead entries so the registry stays bounded.
    ///
    /// Why: keeping the map keyed by live conversations only (rather than every id
    /// ever seen) bounds memory on a long-running daemon. The std-mutex critical
    /// section is a pure map operation with NO await, so it stays negligibly short.
    /// What: upgrades the existing weak entry if the lock is still live; otherwise
    /// mints a fresh [`AsyncMutex`], stores a weak handle to it, and drops every
    /// now-dead weak entry. A poisoned std mutex (only possible after a panic while
    /// holding it — which never happens in this pure section) is recovered rather
    /// than propagated so the SM stays panic-free.
    /// Test: `different_conv_ids_do_not_serialize` (distinct locks per id);
    /// pruning is covered indirectly (a re-acquired id after release mints anew).
    fn lock_for(&self, conv_id: &str) -> Arc<AsyncMutex<()>> {
        let mut map = self.locks.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = map.get(conv_id).and_then(Weak::upgrade) {
            return existing;
        }
        let fresh = Arc::new(AsyncMutex::new(()));
        map.insert(conv_id.to_string(), Arc::downgrade(&fresh));
        // Drop entries whose lock has been released (no strong refs remain). The
        // freshly inserted entry is still held by `fresh`, so it survives.
        map.retain(|_, weak| weak.strong_count() > 0);
        fresh
    }
}
