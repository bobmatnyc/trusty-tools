//! LRU-bounded cache of per-palace `ChatSessionStore` handles (issue #4639).
//!
//! Why: `AppState::session_stores` was a plain `DashMap<String,
//! Arc<ChatSessionStore>>` with no `remove`, no TTL, and no cap — every palace
//! the daemon ever touched leaked one `chat_sessions.redb` file descriptor for
//! the process lifetime. A live daemon was measured holding 844 such handles
//! (all 844 pointing at files already unlinked from disk) against an 8 192 fd
//! ceiling, growing ~250-300/day. `PalaceRegistry` already solved exactly this
//! failure class for kg/usearch/recall via an LRU (issue #463); this module
//! applies the same shape to the one file type that registry never tracked.
//!
//! What: [`SessionStoreCache`] — a `parking_lot::Mutex<LruCache<..>>` (opened
//! unbounded, trimmed manually to [`SessionStoreCache::capacity`]) that opens a
//! store on miss, promotes on hit, and evicts from the cold end once resident
//! entries exceed the cap. Dropping the last `Arc` closes the redb `Database`
//! and releases its fd; the next request reopens from disk transparently.
//!
//! Two invariants make eviction safe, both enforced under the single cache
//! mutex:
//!   1. **Never evict a store a caller still holds.** redb takes an exclusive
//!      `flock`, and `ChatSessionStore::open` has no snapshot fallback, so a
//!      second open of a file that is still open *in this same process* fails
//!      hard with `Database already open. Cannot acquire lock.` (reproduced
//!      directly while diagnosing #4639). The chat streaming handler holds an
//!      `Arc` across a whole SSE response (`chat::handler`), so unconditional
//!      LRU eviction would turn a leak into an outage. Eviction therefore skips
//!      any entry whose `Arc::strong_count() > 1` and overshoots the cap rather
//!      than closing a store out from under its user.
//!   2. **Exactly one store per palace.** The open runs while the cache mutex
//!      is held, so two concurrent callers for the same id cannot both open the
//!      file (the `DatabaseAlreadyOpen` path `PalaceRegistry` guards with
//!      per-id mutexes). The critical section is pure blocking I/O with no
//!      `.await`, so `parking_lot::Mutex` is safe here.
//!
//! Test: `tests::open_handles_are_bounded_by_cap`,
//! `session_store_fd_count_is_bounded_by_cap` (in
//! `tests/session_store_fd_bound.rs` — needs its own process to measure real
//! fds; see that file's header),
//! `tests::evicted_store_reopens_with_data_intact`,
//! `tests::in_use_store_is_never_evicted`,
//! `tests::concurrent_callers_share_one_store`,
//! `tests::remove_drops_cached_handle`.

use anyhow::Result;
use lru::LruCache;
use parking_lot::Mutex;
use std::path::Path;
use std::sync::Arc;
use trusty_common::memory_core::store::ChatSessionStore;

/// Environment variable overriding the resident chat-session-store cap.
///
/// Why: mirrors `TRUSTY_MEMORY_MAX_OPEN_PALACES` so an operator close to the fd
/// ceiling can shrink the chat cache — or a host with a high limit can raise
/// it — without a rebuild.
/// What: parsed by [`max_open_session_stores_from_env`].
/// Test: `tests::env_override_is_honoured`.
pub const MAX_OPEN_SESSION_STORES_ENV: &str = "TRUSTY_MEMORY_MAX_OPEN_SESSION_STORES";

/// Default maximum number of `chat_sessions.redb` handles held open at once.
///
/// Why: deliberately half of `DEFAULT_MAX_OPEN_PALACES` (64), for two reasons.
/// (a) Access pattern: chat is a far colder path than KG recall — the measured
/// production workload opens a palace's session store once, appends a short
/// burst of turns, and never returns to it (~250-300 *distinct* palaces/day),
/// so cache hit rate past a small working set is ~0 and a larger cap buys
/// nothing but resident fds. (b) fd budget: 64 palaces × 3 registry files + 32
/// chat files = 224, which still fits inside the 256-fd macOS soft limit that
/// motivated issues #462/#463 — the fix holds even when the daemon runs outside
/// launchd's 8 192 ceiling. 32 also leaves ample headroom over the only
/// entries eviction cannot reclaim: concurrently-streaming chat sessions.
/// What: a compile-time constant, overridable per-instance via
/// [`SessionStoreCache::with_max_open`] or by [`MAX_OPEN_SESSION_STORES_ENV`].
/// Test: `tests::open_handles_are_bounded_by_cap`.
pub const DEFAULT_MAX_OPEN_SESSION_STORES: usize = 32;

/// Resolve the effective cap from the environment.
///
/// Why: centralises the parse so construction and diagnostics agree on both the
/// value and the fallback.
/// What: reads [`MAX_OPEN_SESSION_STORES_ENV`]; returns its parsed `usize` when
/// set to a value `>= 1`, else [`DEFAULT_MAX_OPEN_SESSION_STORES`].
/// Test: `tests::env_override_is_honoured`.
pub fn max_open_session_stores_from_env() -> usize {
    std::env::var(MAX_OPEN_SESSION_STORES_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&n| n >= 1)
        .unwrap_or(DEFAULT_MAX_OPEN_SESSION_STORES)
}

/// LRU-bounded, thread-safe cache of open per-palace chat-session stores.
///
/// Why/What: see the module docs. Cloning is cheap — callers share one instance
/// behind an `Arc` on `AppState`.
/// Test: see the module-level test list.
pub struct SessionStoreCache {
    /// Opened *unbounded* and trimmed by [`SessionStoreCache::trim`] instead of
    /// relying on `LruCache`'s own capacity, because the built-in eviction
    /// drops the cold-end entry unconditionally — including one a caller is
    /// still using, which is precisely the `DatabaseAlreadyOpen` hazard
    /// invariant (1) exists to prevent.
    inner: Mutex<LruCache<String, Arc<ChatSessionStore>>>,
    capacity: usize,
}

impl SessionStoreCache {
    /// Build a cache with an explicit resident-handle cap.
    ///
    /// Why: tests force eviction with a tiny cap; operators tune production via
    /// [`SessionStoreCache::from_env`].
    /// What: clamps `max_open` to at least 1 and opens an unbounded `LruCache`
    /// that [`SessionStoreCache::trim`] holds down to that cap.
    /// Test: `tests::open_handles_are_bounded_by_cap`.
    pub fn with_max_open(max_open: usize) -> Self {
        Self {
            inner: Mutex::new(LruCache::unbounded()),
            capacity: max_open.max(1),
        }
    }

    /// Build a cache whose cap comes from the environment.
    ///
    /// Why: the daemon's construction path wants the operator-tunable value.
    /// What: `with_max_open(max_open_session_stores_from_env())`.
    /// Test: `tests::env_override_is_honoured`.
    pub fn from_env() -> Self {
        Self::with_max_open(max_open_session_stores_from_env())
    }

    /// The resident-handle cap this cache enforces.
    ///
    /// Test: `tests::env_override_is_honoured`.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Number of stores currently resident (open) in the cache.
    ///
    /// Test: `tests::open_handles_are_bounded_by_cap`.
    pub fn len(&self) -> usize {
        self.inner.lock().len()
    }

    /// Whether no store is currently resident.
    ///
    /// Test: `tests::remove_drops_cached_handle`.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Return the cached store for `palace_id`, opening it under `dir` on miss.
    ///
    /// Why: the single entry point callers use, so the mutex-held open (which
    /// enforces one-store-per-palace) cannot be bypassed.
    /// What: promotes an existing entry to most-recently-used and returns it;
    /// on a miss creates `dir`, opens `dir/chat_sessions.db` (rewritten by
    /// `ChatSessionStore::open` to `chat_sessions.redb`), inserts it, then
    /// trims the cold end back to the cap.
    /// Test: `tests::evicted_store_reopens_with_data_intact`,
    /// `tests::concurrent_callers_share_one_store`.
    pub fn get_or_open(&self, palace_id: &str, dir: &Path) -> Result<Arc<ChatSessionStore>> {
        let mut cache = self.inner.lock();
        if let Some(existing) = cache.get(palace_id) {
            return Ok(existing.clone());
        }
        std::fs::create_dir_all(dir)
            .map_err(|e| anyhow::anyhow!("create palace dir {}: {e}", dir.display()))?;
        let store = Arc::new(ChatSessionStore::open(&dir.join("chat_sessions.db"))?);
        cache.put(palace_id.to_string(), store.clone());
        Self::trim(&mut cache, self.capacity);
        Ok(store)
    }

    /// Drop the cached handle for `palace_id`, closing its fd if unused.
    ///
    /// Why: palace deletion must not leave a handle pinning the inode of a
    /// directory that has just been unlinked — the exact shape of the 844
    /// deleted-but-open handles measured in #4639.
    /// What: `LruCache::pop`; a no-op when the palace was never opened. If a
    /// caller still holds an `Arc`, that caller's handle stays valid until it
    /// drops (`Arc` semantics) — the cache simply stops handing it out.
    /// Test: `tests::remove_drops_cached_handle`.
    pub fn remove(&self, palace_id: &str) {
        self.inner.lock().pop(palace_id);
    }

    /// Evict cold, unused entries until at most `capacity` remain.
    ///
    /// Why: enforces the bound while honouring invariant (1) — an entry whose
    /// `Arc` escaped to a live caller is skipped, never closed underneath it.
    /// Checking `strong_count` under the cache mutex is sound because every
    /// clone is minted by `get_or_open`, which holds that same mutex: a count
    /// of 1 here means the cache is provably the only owner.
    /// What: repeatedly scans coldest-first for an entry with
    /// `Arc::strong_count == 1` and pops it. If every resident store is in use
    /// it stops, deliberately overshooting the cap rather than corrupting an
    /// in-flight caller (the overshoot is bounded by live concurrency and
    /// reclaimed on the next call once those callers drop).
    /// Test: `tests::in_use_store_is_never_evicted`.
    fn trim(cache: &mut LruCache<String, Arc<ChatSessionStore>>, capacity: usize) {
        while cache.len() > capacity {
            let victim = cache
                .iter()
                .rev()
                .find(|(_, store)| Arc::strong_count(store) == 1)
                .map(|(key, _)| key.clone());
            match victim {
                Some(key) => {
                    cache.pop(&key);
                }
                None => break,
            }
        }
    }
}

impl Default for SessionStoreCache {
    fn default() -> Self {
        Self::with_max_open(DEFAULT_MAX_OPEN_SESSION_STORES)
    }
}

impl std::fmt::Debug for SessionStoreCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionStoreCache")
            .field("capacity", &self.capacity)
            .field("resident", &self.len())
            .finish()
    }
}

#[cfg(test)]
mod tests;
