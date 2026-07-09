use super::*;
use crate::memory_core::retrieval::seed_shared_embedder_with_mock;
use crate::memory_core::store::{kg::KnowledgeGraph, vector::UsearchStore};
use tempfile::tempdir;

fn make_handle(id: &str, dir: &std::path::Path) -> PalaceHandle {
    let vs = UsearchStore::new(dir.join(format!("{id}.usearch")), 384).unwrap();
    let kg = KnowledgeGraph::open(&dir.join(format!("{id}.db"))).unwrap();
    PalaceHandle::new(PalaceId::new(id), format!("Identity for {id}"), vs, kg)
}

#[test]
fn register_and_get_roundtrip() {
    let dir = tempdir().unwrap();
    let reg = PalaceRegistry::new();
    reg.register(make_handle("alpha", dir.path()));
    let h = reg.get(&PalaceId::new("alpha")).expect("registered");
    assert_eq!(h.id.as_str(), "alpha");
}

/// Why (issue #1487): a default registry must open palaces as a read-only
/// client (preserving the snapshot fallback for CLI / stdio / tests),
/// while a registry built with `with_writer_intent` must open as a writer
/// (fail-loud on a held lock) — this is what the HTTP daemon relies on.
/// What: Asserts the default `open_intent()` is `ReadOnlyClient` and that
/// `with_writer_intent()` flips it to `Writer`.
/// Test: this test.
#[test]
fn with_writer_intent_sets_writer_open_intent() {
    let default_reg = PalaceRegistry::new();
    assert_eq!(
        default_reg.open_intent(),
        OpenIntent::ReadOnlyClient,
        "default registry must open palaces read-only (snapshot fallback)"
    );

    let writer_reg = PalaceRegistry::new().with_writer_intent();
    assert_eq!(
        writer_reg.open_intent(),
        OpenIntent::Writer,
        "with_writer_intent() must mark the registry as a writer"
    );
}

/// Why: Issue #180 — palace deletion must invalidate the in-memory
/// `PalaceRegistry` cache so a subsequent `open_palace` doesn't return
/// the stale handle for an on-disk-deleted palace.
/// What: Register a handle, set a gap entry, call `remove`, and assert
/// both the handle and the gap cache entry are gone.
/// Test: This test itself.
#[test]
fn registry_remove_clears_cached_handle() {
    let dir = tempdir().unwrap();
    let reg = PalaceRegistry::new();
    let id = PalaceId::new("doomed");
    reg.register(make_handle("doomed", dir.path()));
    reg.set_gaps(id.clone(), Vec::new());
    assert!(reg.get(&id).is_some());
    assert!(reg.get_gaps(&id).is_some());
    reg.remove(&id);
    assert!(reg.get(&id).is_none());
    assert!(reg.get_gaps(&id).is_none());
    // Calling remove again is a no-op.
    reg.remove(&id);
}

#[test]
fn registry_create_and_open() {
    use crate::memory_core::palace::Palace;
    use chrono::Utc;

    let dir = tempdir().unwrap();
    let data_root = dir.path();

    let palace = Palace {
        id: PalaceId::new("alpha"),
        name: "Alpha".to_string(),
        description: Some("test".to_string()),
        created_at: Utc::now(),
        data_dir: data_root.join("alpha"),
    };

    // Create through the registry.
    {
        let reg = PalaceRegistry::new();
        let handle = reg
            .create_palace(data_root, palace.clone())
            .expect("create_palace");
        assert_eq!(handle.id, PalaceId::new("alpha"));
        // Persist a tiny identity directly (PalaceHandle.identity is set
        // at open time so we mutate via PalaceStore for the test).
        crate::memory_core::store::palace_store::PalaceStore::save_identity(
            &handle.id,
            "I am Alpha",
            handle.data_dir.as_ref().expect("data_dir set"),
        )
        .expect("save identity");
    }

    // Drop the registry, reopen from disk.
    let reg2 = PalaceRegistry::new();
    let handle2 = reg2
        .open_palace(data_root, &PalaceId::new("alpha"))
        .expect("open_palace");
    assert_eq!(handle2.id, PalaceId::new("alpha"));
    assert_eq!(handle2.identity, "I am Alpha");

    // list_palaces sees it too.
    let palaces = PalaceRegistry::list_palaces(data_root).unwrap();
    assert_eq!(palaces.len(), 1);
    assert_eq!(palaces[0].name, "Alpha");
}

/// Why: Issue #52 — payloads (drawer content) must survive a process
/// restart. Open a registry, write a drawer with a known content string,
/// drop everything, reopen via `PalaceRegistry::open(path)`, and assert the
/// drawer content is still recoverable from the registered handle.
/// What: Uses `PalaceHandle::remember` (the canonical write path) so the
/// full persistence chain (kg drawer row + usearch vector + L1 snapshot)
/// is exercised, not just metadata.
/// Test: This test itself.
#[tokio::test]
async fn palace_payloads_survive_registry_restart() {
    // Pre-seed mock embedder so no HuggingFace download is attempted. Issue #850.
    seed_shared_embedder_with_mock();
    use crate::memory_core::palace::{Palace, RoomType};
    use chrono::Utc;

    let dir = tempdir().unwrap();
    let data_root = dir.path();

    // Phase 1: create palace + write a payload, then drop everything.
    {
        let registry = PalaceRegistry::open(data_root).unwrap();
        let palace = Palace {
            id: PalaceId::new("restart-test"),
            name: "Restart".to_string(),
            description: None,
            created_at: Utc::now(),
            data_dir: data_root.join("restart-test"),
        };
        let handle = registry.create_palace(data_root, palace).unwrap();
        handle
            .remember(
                "the quokka is a small marsupial native to Western Australia".to_string(),
                RoomType::Research,
                vec!["wildlife".to_string()],
                0.7,
            )
            .await
            .expect("remember persists the drawer");
    }

    // Phase 2: reopen from disk, assert the payload is still there.
    let registry = PalaceRegistry::open(data_root).unwrap();
    assert_eq!(
        registry.len(),
        1,
        "registry should have hydrated the persisted palace"
    );
    let handle = registry
        .get(&PalaceId::new("restart-test"))
        .expect("palace should be registered after open()");
    let drawers = handle.drawers.read().clone();
    assert!(
        drawers
            .iter()
            .any(|d| d.content.contains("quokka") && d.tags.contains(&"wildlife".to_string())),
        "persisted drawer content must survive restart; got {drawers:?}"
    );
}

#[test]
fn gaps_cache_round_trip() {
    use crate::memory_core::community::KnowledgeGap;

    let reg = PalaceRegistry::new();
    let pid = PalaceId::new("gap-cache");

    // Missing key returns None (not an error).
    assert!(reg.get_gaps(&pid).is_none());

    let gaps = vec![KnowledgeGap {
        entities: vec!["alpha".to_string(), "beta".to_string()],
        internal_density: 0.1,
        external_bridges: 1,
        suggested_exploration: "Explore connections between alpha and beta".to_string(),
    }];
    reg.set_gaps(pid.clone(), gaps.clone());

    let read = reg.get_gaps(&pid).expect("cached value");
    assert_eq!(read.len(), 1);
    assert_eq!(read[0].entities, gaps[0].entities);
    assert!((read[0].internal_density - 0.1).abs() < 1e-6);

    reg.clear_gaps(&pid);
    assert!(reg.get_gaps(&pid).is_none());
}

#[test]
fn list_contains_all_registered() {
    let dir = tempdir().unwrap();
    let reg = PalaceRegistry::new();
    reg.register(make_handle("a", dir.path()));
    reg.register(make_handle("b", dir.path()));
    let ids: Vec<_> = reg.list().into_iter().map(|p| p.0).collect();
    assert_eq!(ids.len(), 2);
    assert!(ids.contains(&"a".to_string()));
    assert!(ids.contains(&"b".to_string()));
}

/// Issue #463 — the LRU registry evicts the least-recently-used handle
/// when the capacity ceiling is reached, bounding resident fd usage.
///
/// Why: With many palaces the daemon can exhaust file descriptors
/// (EMFILE). This test proves that the eviction policy fires correctly:
/// inserting a third handle into a capacity-2 registry evicts the LRU,
/// and the two remaining entries are the ones accessed most recently.
/// What: Creates a capacity-2 registry, registers "a" (LRU) then "b"
/// (MRU), then registers "c" — expecting "a" to be evicted. Asserts
/// "b" and "c" are present and "a" is gone.
/// Test: This test itself (issue #463 regression guard).
#[test]
fn lru_evicts_least_recently_used() {
    let dir = tempdir().unwrap();
    let reg = PalaceRegistry::with_max_open(2);

    // Insert "a" first (will become LRU) then "b".
    reg.register(make_handle("a", dir.path()));
    reg.register(make_handle("b", dir.path()));
    assert_eq!(reg.len(), 2, "two handles registered");

    // "a" was inserted before "b"; inserting "c" must evict "a" (LRU).
    reg.register(make_handle("c", dir.path()));
    assert_eq!(reg.len(), 2, "capacity-2 registry must stay at 2");
    assert!(
        reg.peek(&PalaceId::new("a")).is_none(),
        "LRU handle 'a' must have been evicted"
    );
    assert!(
        reg.peek(&PalaceId::new("b")).is_some(),
        "MRU handle 'b' must survive"
    );
    assert!(
        reg.peek(&PalaceId::new("c")).is_some(),
        "newly inserted 'c' must be present"
    );
}

/// Issue #1939 — a palace requested by an aliased name resolves to the
/// alias target's on-disk store.
///
/// Why: trusty-mpm pins a session to the derived `owner-repo` palace, but the
/// real data lives under the bare repo name. Registering the alias must make
/// an `open_palace` for the (non-existent) `owner-repo` name transparently
/// return the handle for the existing bare palace — the split-brain fix.
/// What: creates palace `trusty-tools`, registers alias
/// `bobmatnyc-trusty-tools -> trusty-tools`, then opens the alias name and
/// asserts the returned handle's canonical id is `trusty-tools` (not the
/// alias), proving both names share the one store.
/// Test: this test itself (issue #1939 regression guard).
#[test]
fn open_palace_follows_alias() {
    use crate::memory_core::palace::Palace;
    use crate::palace_alias::PalaceAliasStore;
    use chrono::Utc;

    let dir = tempdir().unwrap();
    let data_root = dir.path();
    let reg = PalaceRegistry::new();

    // The real (bare-repo) palace on disk.
    let bare = Palace {
        id: PalaceId::new("trusty-tools"),
        name: "trusty-tools".to_string(),
        description: None,
        created_at: Utc::now(),
        data_dir: data_root.join("trusty-tools"),
    };
    reg.create_palace(data_root, bare)
        .expect("create bare palace");

    // Register the palace-level alias (owner-repo -> bare).
    PalaceAliasStore::register_alias(data_root, "bobmatnyc-trusty-tools", "trusty-tools")
        .expect("register alias");

    // Opening the aliased (non-existent) name must resolve to the bare store.
    let handle = reg
        .open_palace(data_root, &PalaceId::new("bobmatnyc-trusty-tools"))
        .expect("alias must resolve to the existing palace");
    assert_eq!(
        handle.id,
        PalaceId::new("trusty-tools"),
        "aliased open must return the canonical target handle, not the alias id"
    );
}

/// Issue #1939 — a stale alias whose target does not exist must NOT redirect;
/// the original "metadata missing" error stands.
///
/// Why: if an alias points at a deleted palace we must fail on the ORIGINAL
/// requested id rather than masking it, so operators see the name they asked
/// for. Requiring the target to exist on disk enforces this.
/// What: registers an alias to a non-existent target, opens the alias name,
/// and asserts the open errors (no redirect happened).
/// Test: this test itself.
#[test]
fn open_palace_ignores_alias_when_target_missing() {
    use crate::palace_alias::PalaceAliasStore;

    let dir = tempdir().unwrap();
    let data_root = dir.path();
    let reg = PalaceRegistry::new();

    PalaceAliasStore::register_alias(data_root, "ghost", "does-not-exist").expect("register alias");

    assert!(
        reg.open_palace(data_root, &PalaceId::new("ghost")).is_err(),
        "alias to a missing target must not redirect"
    );
}

/// Issue #1939 — an alias must never shadow a real palace of the same name.
///
/// Why: if both an alias entry AND a real palace exist under the same name,
/// the real palace wins (aliases only fill the "missing palace" gap).
/// What: creates palace `dup`, registers a (mischievous) alias
/// `dup -> other`, and asserts `open_palace("dup")` returns `dup` itself.
/// Test: this test itself.
#[test]
fn open_palace_prefers_real_palace_over_alias() {
    use crate::memory_core::palace::Palace;
    use crate::palace_alias::PalaceAliasStore;
    use chrono::Utc;

    let dir = tempdir().unwrap();
    let data_root = dir.path();
    let reg = PalaceRegistry::new();

    for id in ["dup", "other"] {
        let p = Palace {
            id: PalaceId::new(id),
            name: id.to_string(),
            description: None,
            created_at: Utc::now(),
            data_dir: data_root.join(id),
        };
        reg.create_palace(data_root, p).expect("create palace");
    }
    PalaceAliasStore::register_alias(data_root, "dup", "other").expect("register alias");

    let handle = reg
        .open_palace(data_root, &PalaceId::new("dup"))
        .expect("real palace opens");
    assert_eq!(
        handle.id,
        PalaceId::new("dup"),
        "a real palace must win over an alias of the same name"
    );
}

/// Issue #463 — a `get` call promotes the accessed handle to MRU,
/// protecting it from immediate eviction.
///
/// Why: LRU eviction must respect actual access order, not insertion
/// order. A handle that was inserted first but subsequently accessed
/// should survive longer than one that was inserted more recently but
/// never accessed.
/// What: Creates a capacity-2 registry, inserts "a" then "b", accesses
/// "a" (promoting it to MRU), inserts "c" — expects "b" to be evicted
/// instead of "a".
/// Test: This test itself (issue #463 regression guard).
#[test]
fn lru_get_promotes_to_mru() {
    let dir = tempdir().unwrap();
    let reg = PalaceRegistry::with_max_open(2);

    reg.register(make_handle("a", dir.path()));
    reg.register(make_handle("b", dir.path()));

    // Access "a" — promotes it to MRU; "b" is now LRU.
    let _ = reg.get(&PalaceId::new("a"));

    // Inserting "c" must evict "b" (the new LRU), not "a".
    reg.register(make_handle("c", dir.path()));
    assert_eq!(reg.len(), 2);
    assert!(
        reg.peek(&PalaceId::new("b")).is_none(),
        "'b' must be evicted — it was LRU after 'a' was promoted"
    );
    assert!(
        reg.peek(&PalaceId::new("a")).is_some(),
        "'a' must survive — it was promoted to MRU by get()"
    );
    assert!(
        reg.peek(&PalaceId::new("c")).is_some(),
        "'c' must be present"
    );
}

/// Issue #463 — an evicted palace handle is transparently reopened on
/// the next `open_palace` call.
///
/// Why: Eviction closes fds but must not lose data. The handle is only
/// in-memory state; the authoritative store is always on disk. This test
/// proves that after an eviction the palace can be reopened from disk
/// without error and its metadata is intact.
/// What: Creates a capacity-1 registry, creates palace "a", then
/// registers "b" (evicting "a"), then calls `open_palace` for "a"
/// (must reopen from disk successfully) and asserts the id matches.
/// Test: This test itself (issue #463 regression guard).
#[test]
fn lru_evicted_handle_reopens() {
    use crate::memory_core::palace::Palace;
    use chrono::Utc;

    let dir = tempdir().unwrap();
    let data_root = dir.path();
    let reg = PalaceRegistry::with_max_open(1);

    // Create and persist "alpha" on disk.
    let palace_a = Palace {
        id: PalaceId::new("alpha"),
        name: "Alpha".to_string(),
        description: None,
        created_at: Utc::now(),
        data_dir: data_root.join("alpha"),
    };
    reg.create_palace(data_root, palace_a)
        .expect("create alpha");
    assert_eq!(reg.len(), 1, "'alpha' registered");

    // Register "beta" directly — evicts "alpha" from the capacity-1 cache.
    reg.register(make_handle("beta", data_root));
    assert_eq!(reg.len(), 1, "capacity-1: only 'beta' remains");
    assert!(
        reg.peek(&PalaceId::new("alpha")).is_none(),
        "'alpha' must have been evicted"
    );

    // Reopening "alpha" from disk must succeed.
    let reopened = reg
        .open_palace(data_root, &PalaceId::new("alpha"))
        .expect("open_palace after eviction must succeed");
    assert_eq!(reopened.id, PalaceId::new("alpha"), "reopened id matches");
    assert!(
        reg.peek(&PalaceId::new("alpha")).is_some(),
        "'alpha' must be back in the cache after reopen"
    );
}

// ---------------------------------------------------------------------------
// Idle-to-disk eviction + configurable max-open (idle-evict feature)
// ---------------------------------------------------------------------------

/// Idle-to-disk config knob (Part 3): the LRU open-handle cap honours
/// `TRUSTY_MEMORY_MAX_OPEN_PALACES`.
///
/// Why: operators tune resident-palace RAM via the env var; `from_env` must
/// actually apply it.
/// What: sets the env to 2, builds `from_env()`, registers three handles, and
/// asserts the cap held the registry at 2 (LRU evicted the oldest). Restores
/// the env immediately after construction. `#[serial]` guards the env mutation
/// from parallel tests.
/// Test: this test.
#[test]
#[serial_test::serial]
fn from_env_respects_max_open_palaces_env() {
    let dir = tempdir().unwrap();
    // SAFETY: #[serial] ensures no other thread reads/writes this env var
    // concurrently; removed immediately after `from_env` reads it.
    unsafe {
        std::env::set_var(MAX_OPEN_PALACES_ENV, "2");
    }
    let reg = PalaceRegistry::from_env();
    unsafe {
        std::env::remove_var(MAX_OPEN_PALACES_ENV);
    }

    reg.register(make_handle("a", dir.path()));
    reg.register(make_handle("b", dir.path()));
    reg.register(make_handle("c", dir.path()));
    assert_eq!(
        reg.len(),
        2,
        "from_env cap=2 must bound the registry to 2 handles"
    );
}

/// Unset / invalid env falls back to the default cap.
///
/// Why: a misconfigured or absent env var must not silently drop the cap to an
/// unusable value.
/// What: with the env removed, `max_open_palaces_from_env` returns the default.
/// Test: this test.
#[test]
#[serial_test::serial]
fn max_open_palaces_from_env_defaults_when_unset() {
    unsafe {
        std::env::remove_var(MAX_OPEN_PALACES_ENV);
    }
    assert_eq!(max_open_palaces_from_env(), DEFAULT_MAX_OPEN_PALACES);
}

/// Idle-to-disk (Part 2): `evict_idle` drops a handle idle past the threshold
/// AND referenced only by the cache.
///
/// Why: the core "idle to disk" behaviour — reclaim a cold palace's heavy RAM.
/// What: registers a handle with `last_accessed` forced into the past, evicts
/// with a 1 s threshold, and asserts it left the cache.
/// Test: this test.
#[test]
fn evict_idle_drops_idle_unreferenced_handle() {
    let dir = tempdir().unwrap();
    let reg = PalaceRegistry::new();
    let h = make_handle("idle", dir.path());
    h.last_accessed
        .store(0, std::sync::atomic::Ordering::Relaxed);
    reg.register(h);
    assert_eq!(reg.len(), 1);

    let evicted = reg.evict_idle(std::time::Duration::from_secs(1));
    assert_eq!(evicted, 1, "an idle, unreferenced handle must be evicted");
    assert!(
        reg.get(&PalaceId::new("idle")).is_none(),
        "evicted handle must be gone from the cache"
    );
}

/// `evict_idle` must NOT drop a handle a live operation still references
/// (`strong_count > 1`), even when it is idle.
///
/// Why: this is the correctness anchor — dropping an in-use handle could race
/// the in-flight op and (under Writer intent) break the redb flock. The
/// strong_count==1 guard prevents it.
/// What: registers an idle handle, holds a second `Arc` clone (simulating an
/// in-flight recall), and asserts `evict_idle` skips it.
/// Test: this test.
#[test]
fn evict_idle_skips_referenced_handle() {
    let dir = tempdir().unwrap();
    let reg = PalaceRegistry::new();
    let h = make_handle("busy", dir.path());
    h.last_accessed
        .store(0, std::sync::atomic::Ordering::Relaxed);
    reg.register(h);
    // Simulate an in-flight operation holding a clone of the handle.
    let _in_flight = reg.get(&PalaceId::new("busy")).expect("registered");
    let evicted = reg.evict_idle(std::time::Duration::from_secs(1));
    assert_eq!(evicted, 0, "a referenced handle must be skipped");
    assert_eq!(reg.len(), 1);
}

/// `evict_idle` must skip a handle accessed within the TTL window and treat a
/// zero threshold as disabled.
///
/// Why: only genuinely-idle palaces should be reclaimed; a zero TTL is the
/// documented "disabled" value.
/// What: a fresh handle (last_accessed == now) survives a 1 h threshold, and a
/// zero threshold is a no-op even for an ancient handle.
/// Test: this test.
#[test]
fn evict_idle_skips_recent_and_respects_zero_threshold() {
    let dir = tempdir().unwrap();
    let reg = PalaceRegistry::new();
    reg.register(make_handle("fresh", dir.path()));
    assert_eq!(
        reg.evict_idle(std::time::Duration::from_secs(3600)),
        0,
        "a recently-accessed handle must not be evicted"
    );

    let h = make_handle("ancient", dir.path());
    h.last_accessed
        .store(0, std::sync::atomic::Ordering::Relaxed);
    reg.register(h);
    assert_eq!(
        reg.evict_idle(std::time::Duration::from_secs(0)),
        0,
        "zero threshold disables eviction"
    );
    assert_eq!(reg.len(), 2, "no handle should have been evicted");
}

/// `PalaceHandle::touch` must be a no-op while a dream cycle holds
/// `is_compacting`.
///
/// Why: internal consolidation calls the same remember/forget methods users
/// do; stamping `last_accessed` there would keep an idle-but-dreaming palace
/// resident forever, defeating idle-evict.
/// What: forces the idle clock to 0, sets `is_compacting`, calls `touch`, and
/// asserts the clock did not advance; then clears the flag and confirms `touch`
/// advances it.
/// Test: this test.
#[test]
fn touch_suppressed_during_compaction() {
    use std::sync::atomic::Ordering::Relaxed;
    let dir = tempdir().unwrap();
    let h = make_handle("dreaming", dir.path());
    h.last_accessed.store(0, Relaxed);
    h.is_compacting.store(true, Relaxed);
    h.touch();
    assert_eq!(
        h.last_accessed.load(Relaxed),
        0,
        "touch during compaction must not advance the idle clock"
    );
    h.is_compacting.store(false, Relaxed);
    h.touch();
    assert!(
        h.last_accessed.load(Relaxed) > 0,
        "touch after compaction must advance the idle clock"
    );
}

/// Idle-to-disk rehydrate correctness (Part 2): after an idle-evict drops a
/// palace, the next access transparently re-opens it from redb and recall
/// returns the same drawer content — no data loss.
///
/// Why: eviction reclaims RAM only if reopening is lossless; this is the
/// end-to-end proof that "idle to disk" round-trips through the durable store.
/// What: creates a palace, remembers a drawer, snapshots the recall result,
/// forces the palace idle and drops the handle so `strong_count == 1`, evicts
/// it, then reopens via `open_palace` and asserts recall returns byte-for-byte
/// the same drawer contents as before eviction.
/// Test: this test.
#[tokio::test]
async fn evict_idle_then_reopen_preserves_recall() {
    seed_shared_embedder_with_mock();
    use crate::memory_core::palace::{Palace, RoomType};
    use crate::memory_core::retrieval::recall_with_default_embedder;
    use chrono::Utc;

    let dir = tempdir().unwrap();
    let data_root = dir.path();
    let reg = PalaceRegistry::new();
    let palace = Palace {
        id: PalaceId::new("evict-reopen"),
        name: "Evict".to_string(),
        description: None,
        created_at: Utc::now(),
        data_dir: data_root.join("evict-reopen"),
    };
    let handle = reg.create_palace(data_root, palace).unwrap();
    handle
        .remember(
            "the quokka is a small marsupial native to Western Australia".to_string(),
            RoomType::Research,
            vec!["wildlife".to_string()],
            0.7,
        )
        .await
        .expect("remember persists the drawer");

    // Snapshot recall BEFORE eviction.
    let before = recall_with_default_embedder(&handle, "quokka", 5)
        .await
        .expect("pre-evict recall");
    let before_contents: Vec<String> = before.iter().map(|r| r.drawer.content.clone()).collect();
    assert!(
        before_contents.iter().any(|c| c.contains("quokka")),
        "sanity: pre-evict recall must find the drawer"
    );

    // Make the palace idle and release our reference so strong_count == 1.
    handle
        .last_accessed
        .store(0, std::sync::atomic::Ordering::Relaxed);
    let id = handle.id.clone();
    drop(handle);

    // Idle-evict drops the whole handle — heavy fields freed.
    let evicted = reg.evict_idle(std::time::Duration::from_secs(1));
    assert_eq!(evicted, 1, "idle palace must be evicted");
    assert!(
        reg.get(&id).is_none(),
        "palace must be cold (absent from the registry) after eviction"
    );

    // Next access lazily rehydrates from the durable redb store.
    let reopened = reg
        .open_palace(data_root, &id)
        .expect("reopen from disk after idle eviction");
    let after = recall_with_default_embedder(&reopened, "quokka", 5)
        .await
        .expect("post-reopen recall");
    let after_contents: Vec<String> = after.iter().map(|r| r.drawer.content.clone()).collect();
    assert_eq!(
        before_contents, after_contents,
        "recall after idle-evict + rehydrate must match pre-eviction results"
    );
}
