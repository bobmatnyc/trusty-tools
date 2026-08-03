//! Tests for the LRU-bounded chat-session-store cache (issue #4639).
//!
//! Why: the bug these pin is a *quantitative* one — nothing was functionally
//! broken, the daemon simply never released a `chat_sessions.redb` fd. So the
//! assertions here are on counts (resident entries), not just on happy-path
//! behaviour, plus the two correctness invariants eviction must not violate.
//! The matching *real file descriptor* assertion lives in
//! `tests/session_store_fd_bound.rs`, which needs its own process to be
//! meaningful — see that file's header.
//! What: exercises the cap, transparent reopen after eviction, the in-use
//! guard, concurrent-open dedup, and explicit removal.
//! Test: this module.

use super::*;
use std::sync::Barrier;
use tempfile::TempDir;

/// Open `count` distinct palaces through `cache`, dropping each `Arc`
/// immediately so nothing pins an entry.
fn touch_palaces(cache: &SessionStoreCache, root: &Path, count: usize) {
    for i in 0..count {
        let id = format!("palace-{i}");
        let store = cache
            .get_or_open(&id, &root.join(&id))
            .expect("open should succeed");
        drop(store);
    }
}

/// Why: the leak was an unbounded map — the cap is the fix, so it must hold
/// against far more distinct palaces than the cap allows.
/// What: touches 8× the cap worth of distinct palaces and asserts the resident
/// entry count never exceeds the cap.
/// Test: this test.
#[test]
fn open_handles_are_bounded_by_cap() {
    let dir = TempDir::new().unwrap();
    let cache = SessionStoreCache::with_max_open(8);
    touch_palaces(&cache, dir.path(), 64);
    assert_eq!(
        cache.len(),
        8,
        "resident stores must be capped at 8, got {}",
        cache.len()
    );
}

/// Why: eviction is only safe if it is invisible — a cold palace must reopen
/// transparently with its data intact, not error or come back empty.
/// What: writes a session into palace-0, evicts it by touching many others,
/// then reopens palace-0 and reads the session back.
/// Test: this test.
#[test]
fn evicted_store_reopens_with_data_intact() {
    let dir = TempDir::new().unwrap();
    let cache = SessionStoreCache::with_max_open(4);
    let sid = {
        let store = cache
            .get_or_open("palace-0", &dir.path().join("palace-0"))
            .unwrap();
        store.create_session(Some("kept".to_string())).unwrap()
    };
    touch_palaces(&cache, dir.path(), 40);
    let reopened = cache
        .get_or_open("palace-0", &dir.path().join("palace-0"))
        .expect("evicted palace must reopen cleanly");
    let session = reopened
        .get_session(&sid)
        .expect("read after reopen")
        .expect("session must survive eviction");
    assert_eq!(session.title.as_deref(), Some("kept"));
}

/// Why: THE correctness constraint. redb takes an exclusive `flock` and
/// `ChatSessionStore::open` has no snapshot fallback, so evicting a store a
/// caller still holds and then reopening it fails with "Database already open.
/// Cannot acquire lock." — turning a slow leak into an immediate outage on the
/// chat streaming path, which holds an `Arc` for a whole SSE response.
/// What: holds an `Arc` for palace-0, floods the cache well past its cap, then
/// re-requests palace-0. Asserts the request succeeds AND returns the very same
/// instance (never a second, conflicting open).
/// Test: this test.
#[test]
fn in_use_store_is_never_evicted() {
    let dir = TempDir::new().unwrap();
    let cache = SessionStoreCache::with_max_open(4);
    let held = cache
        .get_or_open("palace-0", &dir.path().join("palace-0"))
        .unwrap();
    touch_palaces(&cache, dir.path(), 40);
    let again = cache
        .get_or_open("palace-0", &dir.path().join("palace-0"))
        .expect("an in-use store must never be evicted and double-opened");
    assert!(
        Arc::ptr_eq(&held, &again),
        "the held store must be returned, not a second open of the same file"
    );
}

/// Why: two concurrent callers that each opened their own `ChatSessionStore`
/// for one palace would hit the same `DatabaseAlreadyOpen` failure — this is
/// what `PalaceRegistry` uses per-id mutexes to prevent.
/// What: races 8 threads on one palace id behind a barrier; asserts every
/// caller succeeded, they all share one instance, and only one entry landed.
/// Test: this test.
#[test]
fn concurrent_callers_share_one_store() {
    let dir = TempDir::new().unwrap();
    let cache = Arc::new(SessionStoreCache::with_max_open(4));
    let barrier = Arc::new(Barrier::new(8));
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let cache = Arc::clone(&cache);
            let barrier = Arc::clone(&barrier);
            let path = dir.path().join("shared");
            std::thread::spawn(move || {
                barrier.wait();
                cache.get_or_open("shared", &path).expect("concurrent open")
            })
        })
        .collect();
    let stores: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    for store in &stores[1..] {
        assert!(
            Arc::ptr_eq(&stores[0], store),
            "all concurrent callers must share one store"
        );
    }
    assert_eq!(cache.len(), 1);
}

/// Why: `delete_palace` unlinks the palace directory; a retained handle would
/// pin the deleted inode forever — that is exactly the 844 deleted-but-open
/// handles measured on the live daemon.
/// What: opens a palace, removes it, asserts the cache is empty and that
/// removing an unknown id is a harmless no-op.
/// Test: this test.
#[test]
fn remove_drops_cached_handle() {
    let dir = TempDir::new().unwrap();
    let cache = SessionStoreCache::with_max_open(4);
    drop(cache.get_or_open("gone", &dir.path().join("gone")).unwrap());
    assert_eq!(cache.len(), 1);
    cache.remove("gone");
    assert!(cache.is_empty(), "remove must drop the cached handle");
    cache.remove("never-opened");
    assert!(cache.is_empty());
}

/// Why: operators near the fd ceiling need to shrink the cap without a rebuild,
/// mirroring `TRUSTY_MEMORY_MAX_OPEN_PALACES`.
/// What: asserts the unset default and that the constructor clamps a zero cap
/// to 1 rather than building a cache that can hold nothing.
/// Test: this test.
#[test]
fn env_override_is_honoured() {
    // The env var is process-global; only assert the unset default here so this
    // test cannot race others running in parallel.
    if std::env::var(MAX_OPEN_SESSION_STORES_ENV).is_err() {
        assert_eq!(
            max_open_session_stores_from_env(),
            DEFAULT_MAX_OPEN_SESSION_STORES
        );
    }
    assert_eq!(SessionStoreCache::with_max_open(0).capacity(), 1);
    assert_eq!(SessionStoreCache::with_max_open(17).capacity(), 17);
}
