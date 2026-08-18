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

/// Why (issue #4911): eager hydration catches a per-palace open error, logs a
/// WARN, and skips — so the palace is simply absent from the registry
/// afterwards, indistinguishable from one that never existed. Once a read-only
/// open of an incompatible-format store REFUSES instead of recreating it empty,
/// that skip became the new way to lose a palace: the bytes survive, but nothing
/// reports that the palace exists and cannot be read. Trading data destruction
/// for data invisibility is not a fix.
///
/// Failing the whole `open` is the wrong surfacing — a multi-palace root would
/// be bricked by one unrelated bad file, and the "one corrupt palace doesn't
/// take the registry down" contract is what `SmMemory` / `PortfolioMemory` /
/// trusty-agents construct against. So the palace is RETAINED as an observable
/// unopenable entry instead.
///
/// What: persists a healthy palace, then hand-writes a SECOND palace this
/// process never opens — a valid `palace.json` plus a DIRECTORY where the KG
/// store file (`kg.redb`) belongs, which fails to open at the OS layer on every
/// platform and redb version. Building it by hand matters: the store keeps a
/// process-wide cache of open databases keyed by path, so a palace this process
/// already opened would be served from cache and never read the corruption.
/// Asserts (a) `open` still succeeds, (b) the healthy palace hydrated, (c) the
/// broken one is NOT in the handle cache, and (d) it is nonetheless observable
/// via `unopenable()` / `unopenable_reason()` with a non-empty reason. (c) and
/// (d) together are the point: absent from the cache but not absent from the
/// registry's account of what exists.
/// Test: this is the test.
#[test]
fn open_keeps_an_unopenable_palace_observable() {
    use crate::memory_core::palace::Palace;
    use chrono::Utc;

    seed_shared_embedder_with_mock();
    let dir = tempdir().unwrap();
    let data_root = dir.path();

    {
        let registry = PalaceRegistry::open(data_root).unwrap();
        let palace = Palace {
            id: PalaceId::new("healthy"),
            name: "healthy".to_string(),
            description: None,
            created_at: Utc::now(),
            data_dir: data_root.join("healthy"),
        };
        registry.create_palace(data_root, palace).unwrap();
    }

    let broken_dir = data_root.join("broken");
    std::fs::create_dir_all(&broken_dir).unwrap();
    std::fs::write(
        broken_dir.join("palace.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "id": "broken",
            "name": "broken",
            "description": serde_json::Value::Null,
            "created_at": "2020-01-02T03:04:05Z",
            "data_dir": broken_dir,
            "schema_version": 1,
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::create_dir(broken_dir.join("kg.redb")).expect("directory where the KG store belongs");

    let registry = PalaceRegistry::open(data_root)
        .expect("one unopenable palace must not fail the whole registry open");

    assert!(
        registry.get(&PalaceId::new("healthy")).is_some(),
        "the healthy palace must still hydrate"
    );
    assert!(
        registry.get(&PalaceId::new("broken")).is_none(),
        "the unopenable palace cannot be in the handle cache — nothing opened it"
    );

    let reason = registry.unopenable_reason(&PalaceId::new("broken")).expect(
        "a palace that exists on disk and could not be opened must stay observable, \
             not silently vanish from the registry",
    );
    assert!(
        !reason.is_empty(),
        "the recorded reason must say why the palace could not be opened"
    );

    let ids: Vec<String> = registry
        .unopenable()
        .into_iter()
        .map(|(id, _)| id.as_str().to_string())
        .collect();
    assert_eq!(
        ids,
        vec!["broken".to_string()],
        "exactly the unopenable palace is recorded — the healthy one must not be"
    );
}

/// Why (issue #4911): the daemon hydrates through its own walk
/// (`AppState::load_palaces_from_disk`), not [`PalaceRegistry::open`], so it
/// needs `record_unopenable` to file a skip. A record that outlived the
/// condition would be worse than none — an operator would chase a palace that
/// has been healthy since the next lazy open — so the clearing half is the
/// property under test, not the insert.
/// What: records a skip, asserts it is readable, then registers a real handle
/// for that same id through `register_arc` (the sole success-path funnel) and
/// asserts the record is gone.
/// Test: this test itself.
#[test]
fn record_unopenable_is_cleared_by_a_later_success() {
    use crate::memory_core::palace::Palace;
    use chrono::Utc;

    seed_shared_embedder_with_mock();
    let dir = tempdir().unwrap();
    let data_root = dir.path();
    let id = PalaceId::new("flaky");

    let registry = PalaceRegistry::new();
    registry.record_unopenable(id.clone(), "EMFILE: too many open files".to_string());
    assert_eq!(
        registry.unopenable_reason(&id).as_deref(),
        Some("EMFILE: too many open files"),
        "a recorded skip must be readable back"
    );

    // The same palace opens fine on a later attempt — the record must not
    // survive that.
    registry
        .create_palace(
            data_root,
            Palace {
                id: id.clone(),
                name: "flaky".to_string(),
                description: None,
                created_at: Utc::now(),
                data_dir: data_root.join("flaky"),
            },
        )
        .expect("create_palace");

    assert!(
        registry.unopenable_reason(&id).is_none(),
        "a successful open must clear the skip record, not leave it to mislead"
    );
    assert!(
        registry.unopenable().is_empty(),
        "no unopenable palace remains"
    );
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
            .any(|d| d.content().contains("quokka") && d.tags.contains(&"wildlife".to_string())),
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
    let before_contents: Vec<String> = before
        .iter()
        .map(|r| r.drawer.content().to_string())
        .collect();
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
    let after_contents: Vec<String> = after
        .iter()
        .map(|r| r.drawer.content().to_string())
        .collect();
    assert_eq!(
        before_contents, after_contents,
        "recall after idle-evict + rehydrate must match pre-eviction results"
    );
}

/// Why (issue #3992): reproduces the reported hang under REAL multi-writer
/// contention, not a mocked one. `PalaceRegistry::open_palace` serialises
/// concurrent opens of the SAME palace id behind a per-palace
/// `open_lock: parking_lot::Mutex<()>` (see the `open_locks` field doc).
/// Each individual `Database::create` retry inside `try_open_or_snapshot`
/// IS already bounded (`WRITER_RETRY_ATTEMPTS` x `WRITER_RETRY_SLEEP_MS`,
/// ~1.55s) — but a failed Writer open is never cached, so the NEXT caller
/// repeats the same ~1.55s bounded dance from scratch. Under sustained
/// contention (the redb file never becomes available), every caller queued
/// behind `open_lock` pays its OWN ~1.55s bounded attempt AFTER waiting for
/// every earlier queued caller's ~1.55s attempt to finish first — so the
/// LAST caller's total wait scales with queue depth, not with any single
/// constant. That is "unbounded" from any individual caller's point of
/// view even though no single redb-open call exceeds ~1.55s. Live evidence
/// before this fix: 5 concurrent openers against a genuinely held lock made
/// the 5th caller wait 9.86s (and the pattern extrapolates linearly — this
/// is exactly how a single `memory_remember` was observed to hang 1800s
/// under sustained contention).
/// What: holds a genuine flock conflict on the real backing file
/// (`kg.redb` — `KnowledgeGraph::open_with_intent` translates the legacy
/// `kg.db` path callers pass in), exactly like `concurrent_open::tests::
/// writer_intent_fails_on_locked_file`, for the whole test. Uses
/// `with_open_queue_timeout` to inject a short (2s) deadline instead of
/// mutating the process-wide `TRUSTY_OPEN_QUEUE_TIMEOUT_SECS` env var (which
/// would race any other test running in parallel with this one). Fires
/// `WRITER_COUNT` concurrent `open_palace` calls at a `with_writer_intent`
/// registry for the SAME palace id and records each caller's wall-clock
/// wait; asserts every caller returns within one bounded window
/// (queue-timeout + one ~1.55-2.1s redb attempt) regardless of queue depth.
/// Test: this test.
#[test]
fn writer_open_queue_wait_is_bounded_under_sustained_contention() {
    use crate::memory_core::palace::Palace;
    use chrono::Utc;
    use redb::Database;

    const WRITER_COUNT: usize = 5;
    // Short enough that the test runs fast and deterministically; injected
    // per-registry (not via env var) so it cannot race other parallel tests.
    const OPEN_QUEUE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

    let dir = tempdir().unwrap();
    let data_root = dir.path();
    let palace_id = PalaceId::new("contended");
    let palace_dir = data_root.join(palace_id.as_str());
    std::fs::create_dir_all(&palace_dir).unwrap();

    // Persist palace.json so `open_palace` can load metadata.
    let palace = Palace {
        id: palace_id.clone(),
        name: "Contended".to_string(),
        description: None,
        created_at: Utc::now(),
        data_dir: palace_dir.clone(),
    };
    crate::memory_core::store::palace_store::PalaceStore::save_palace(&palace)
        .expect("save palace metadata");

    // Hold a REAL, permanent flock conflict on the actual redb file for the
    // whole test — mirrors `writer_intent_fails_on_locked_file` exactly,
    // just held longer. This process never releases it, so every Writer
    // open attempt genuinely exhausts its bounded retry window and fails.
    let kg_path = palace_dir.join("kg.redb");
    let _lock_holder = Database::create(&kg_path).expect("hold kg.redb lock for the test");

    let reg = std::sync::Arc::new(
        PalaceRegistry::new()
            .with_writer_intent()
            .with_open_queue_timeout(OPEN_QUEUE_TIMEOUT),
    );
    eprintln!("registry open_intent = {:?}", reg.open_intent());
    let start = std::time::Instant::now();
    let handles: Vec<_> = (0..WRITER_COUNT)
        .map(|i| {
            let reg = reg.clone();
            let data_root = data_root.to_path_buf();
            let palace_id = palace_id.clone();
            std::thread::spawn(move || {
                let t0 = std::time::Instant::now();
                let result = reg.open_palace(&data_root, &palace_id);
                let elapsed = t0.elapsed();
                match &result {
                    Ok(h) => eprintln!(
                        "writer {i}: elapsed={elapsed:?} ok=true is_read_only={} (since test start: {:?})",
                        h.is_read_only(),
                        start.elapsed()
                    ),
                    Err(e) => eprintln!(
                        "writer {i}: elapsed={elapsed:?} ok=false err={e:#} (since test start: {:?})",
                        start.elapsed()
                    ),
                }
                (result.is_ok(), elapsed)
            })
        })
        .collect();

    let results: Vec<(bool, std::time::Duration)> =
        handles.into_iter().map(|h| h.join().unwrap()).collect();
    let total = start.elapsed();
    let max_wait = results.iter().map(|(_, d)| *d).max().unwrap();
    eprintln!(
        "writer_open_queue_depth: {WRITER_COUNT} concurrent writers, total={total:?}, \
         max_single_caller_wait={max_wait:?}"
    );

    // Every attempt must fail (the lock is held for the whole test) — never
    // a silent snapshot degrade for Writer intent.
    assert!(
        results.iter().all(|(ok, _)| !ok),
        "every writer-intent open must fail while the lock is held, never succeed silently"
    );

    // The bounded-fix assertion: no single caller may wait longer than the
    // injected open-queue timeout plus one bounded writer-open attempt
    // (~1.55-2.1s in practice), REGARDLESS of how many other callers are
    // queued ahead of it for the same palace. Pre-fix, `open_lock.lock()`
    // was unbounded and callers queued behind every earlier caller's own
    // attempt in turn, so `max_wait` scaled with `WRITER_COUNT` (measured
    // live: 9.86s for 5 callers against a genuinely held lock — see the
    // test doc comment). Post-fix it stays pinned near
    // `OPEN_QUEUE_TIMEOUT` regardless of `WRITER_COUNT`.
    let slack = std::time::Duration::from_millis(2_500);
    assert!(
        max_wait < OPEN_QUEUE_TIMEOUT + slack,
        "a queued caller must not wait longer than the open-queue timeout plus \
         one bounded writer-open attempt, regardless of queue depth; got \
         {max_wait:?} (bound {OPEN_QUEUE_TIMEOUT:?} + {slack:?} slack) for \
         {WRITER_COUNT} concurrent callers (issue #3992)"
    );
}

/// Restore a path's mode on drop, including while unwinding from a failed
/// assertion, so a test can never leave `tempfile` an untraversable tree.
///
/// The mode is a field because the tests below lock two kinds of path: a
/// regular `palace.json` (0o600) and the directory holding it (0o700), which a
/// file's mode would leave untraversable.
#[cfg(unix)]
struct RestoreMode {
    path: std::path::PathBuf,
    mode: u32,
}

#[cfg(unix)]
impl Drop for RestoreMode {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(self.mode));
    }
}

/// Why (#5549, ADR-0045): `open_palace` returns `anyhow::Error`, which flattens
/// a genuine absence together with a denied read, a transient `EIO`/`ESTALE`,
/// undecodable metadata, an open-queue timeout, and a redb lock conflict. Every
/// caller that mapped `Err` to "not found" therefore told its client a palace
/// it could not read does not exist. `open_error_is_absent` is what lets a
/// caller keep the two apart, so if it answered `true` for a denied read the
/// callers would be exactly where they started.
/// What: creates a palace, evicts its cached handle so the next open reads
/// disk, strips `palace.json` to mode 000, and asserts the resulting open error
/// is NOT classified as absence — then asserts an id that was never created IS.
/// Panics rather than passing vacuously if the denial does not take hold.
/// Test: this test itself.
#[cfg(unix)]
#[test]
fn open_error_is_absent_only_for_a_genuine_absence() {
    use crate::memory_core::palace::Palace;
    use chrono::Utc;
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir().unwrap();
    let data_root = dir.path();
    let reg = PalaceRegistry::new();
    reg.create_palace(
        data_root,
        Palace {
            id: PalaceId::new("alpha"),
            name: "alpha".to_string(),
            description: None,
            created_at: Utc::now(),
            data_dir: data_root.join("alpha"),
        },
    )
    .expect("create palace");
    // Drop the cached handle so the next open goes back to disk.
    reg.remove(&PalaceId::new("alpha"));

    let target = data_root.join("alpha").join("palace.json");
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o000)).unwrap();
    // Declared after `dir` so it drops first: the mode is restored before
    // `TempDir` walks the tree, including while unwinding from a failure below.
    let _restore = RestoreMode {
        path: target.clone(),
        mode: 0o600,
    };

    // Root bypasses the mode bits outright and some filesystems ignore them,
    // so confirm the denial actually took hold. A vacuous pass here would
    // assert nothing at all.
    match std::fs::read(&target) {
        Ok(_) => panic!(
            "cannot exercise #5549: {} is still readable at mode 000. Run this suite as a \
             non-root user on a filesystem that honours POSIX permission bits.",
            target.display()
        ),
        Err(e) => assert_eq!(
            e.kind(),
            std::io::ErrorKind::PermissionDenied,
            "expected the locked palace.json to deny reads, got {e}"
        ),
    }

    // `PalaceHandle` is not `Debug`, so `expect_err` is unavailable here.
    let denied = match reg.open_palace(data_root, &PalaceId::new("alpha")) {
        Ok(_) => panic!("a palace.json that cannot be read must not open"),
        Err(e) => e,
    };
    assert!(
        !PalaceRegistry::open_error_is_absent(&denied),
        "a palace whose metadata could not be read was classified as absent — that is the \
         #5549 coercion, and the HTTP callers render it as 404 'palace not found': {denied:#}"
    );

    let missing = match reg.open_palace(data_root, &PalaceId::new("never-created")) {
        Ok(_) => panic!("an id with no palace on disk must not open"),
        Err(e) => e,
    };
    assert!(
        PalaceRegistry::open_error_is_absent(&missing),
        "a genuinely absent palace must still classify as absence, or every 404 the callers \
         owe becomes a 500: {missing:#}"
    );
}

/// Why (#5549, #5574): the sibling test above locks the FILE, so the stat
/// probe succeeds and the read under it fails. This one locks the DIRECTORY, so
/// the probe itself fails — the shape #5574 changed from `NotFound` to `Io` at
/// `load_palace`. Both halves live in this crate, so the claim that the
/// classifier carries #5574's new `Io` through as "not absence" belongs here
/// rather than only at the HTTP callers downstream.
/// What: creates a palace, evicts its cached handle, strips the palace's own
/// directory to mode 000, and asserts the resulting open error is NOT
/// classified as absence. Panics rather than passing vacuously if the denial
/// does not take hold.
/// Test: this test itself.
#[cfg(unix)]
#[test]
fn open_error_is_not_absent_for_an_unstattable_palace_json() {
    use crate::memory_core::palace::Palace;
    use chrono::Utc;
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir().unwrap();
    let data_root = dir.path();
    let reg = PalaceRegistry::new();
    reg.create_palace(
        data_root,
        Palace {
            id: PalaceId::new("alpha"),
            name: "alpha".to_string(),
            description: None,
            created_at: Utc::now(),
            data_dir: data_root.join("alpha"),
        },
    )
    .expect("create palace");
    reg.remove(&PalaceId::new("alpha"));

    let palace_dir = data_root.join("alpha");
    std::fs::set_permissions(&palace_dir, std::fs::Permissions::from_mode(0o000)).unwrap();
    // Declared after `dir` so it drops first: the mode is restored before
    // `TempDir` walks the tree, including while unwinding from a failure below.
    let _restore = RestoreMode {
        path: palace_dir.clone(),
        mode: 0o700,
    };

    let target = palace_dir.join("palace.json");
    match target.try_exists() {
        Ok(_) => panic!(
            "cannot exercise #5549: {} is still stattable with its directory at mode 000. Run \
             this suite as a non-root user on a filesystem that honours POSIX permission bits.",
            target.display()
        ),
        Err(e) => assert_eq!(
            e.kind(),
            std::io::ErrorKind::PermissionDenied,
            "expected the locked directory to deny statting palace.json, got {e}"
        ),
    }

    // `PalaceHandle` is not `Debug`, so `expect_err` is unavailable here.
    let denied = match reg.open_palace(data_root, &PalaceId::new("alpha")) {
        Ok(_) => panic!("a palace.json that cannot be statted must not open"),
        Err(e) => e,
    };
    assert!(
        !PalaceRegistry::open_error_is_absent(&denied),
        "a palace whose metadata could not be statted was classified as absent — #5574 made \
         that an Io error at load_palace, and this classifier read it back as absence: {denied:#}"
    );
}

/// Why (#5592): the two tests above reach `load_palace` under the id the caller
/// asked for, so `load_palace`'s own #5574 guard decides the answer. An ALIASED
/// open does not: `resolve_palace_alias` probes the target itself before any of
/// that, and while that probe used `Path::exists()` a target it was DENIED to
/// stat read as a target that is not there. The redirect was then dropped and
/// `load_palace` ran against the alias id's own genuinely-absent directory,
/// returning a truthful `NotFound` for the wrong palace — so a palace that
/// exists and merely could not be verified still reported absence, and the HTTP
/// callers still rendered 404. That is the same coercion #5549 exists to
/// remove, one call earlier in the same function.
/// What: creates palace `bare-repo`, aliases `owner-repo` to it, evicts the
/// cached handle so the next open reads disk, strips `bare-repo`'s own
/// directory to mode 000 (the probe itself is denied, not just the read), and
/// asserts the error from opening the ALIAS is NOT classified as absence.
/// Panics rather than passing vacuously if the denial does not take hold.
/// Test: this test itself.
#[cfg(unix)]
#[test]
fn open_error_is_not_absent_for_an_unstattable_alias_target() {
    use crate::memory_core::palace::Palace;
    use crate::palace_alias::PalaceAliasStore;
    use chrono::Utc;
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir().unwrap();
    let data_root = dir.path();
    let reg = PalaceRegistry::new();
    reg.create_palace(
        data_root,
        Palace {
            id: PalaceId::new("bare-repo"),
            name: "bare-repo".to_string(),
            description: None,
            created_at: Utc::now(),
            data_dir: data_root.join("bare-repo"),
        },
    )
    .expect("create palace");
    // `palace_aliases.json` sits in `data_root`, not inside the palace
    // directory, so locking that directory below leaves the alias map readable.
    PalaceAliasStore::register_alias(data_root, "owner-repo", "bare-repo").expect("register alias");
    reg.remove(&PalaceId::new("bare-repo"));

    let palace_dir = data_root.join("bare-repo");
    std::fs::set_permissions(&palace_dir, std::fs::Permissions::from_mode(0o000)).unwrap();
    // Declared after `dir` so it drops first: the mode is restored before
    // `TempDir` walks the tree, including while unwinding from a failure below.
    let _restore = RestoreMode {
        path: palace_dir.clone(),
        mode: 0o700,
    };

    let target = palace_dir.join("palace.json");
    match target.try_exists() {
        Ok(_) => panic!(
            "cannot exercise #5592: {} is still stattable with its directory at mode 000. Run \
             this suite as a non-root user on a filesystem that honours POSIX permission bits.",
            target.display()
        ),
        Err(e) => assert_eq!(
            e.kind(),
            std::io::ErrorKind::PermissionDenied,
            "expected the locked directory to deny statting palace.json, got {e}"
        ),
    }

    // `PalaceHandle` is not `Debug`, so `expect_err` is unavailable here.
    let denied = match reg.open_palace(data_root, &PalaceId::new("owner-repo")) {
        Ok(_) => panic!("an alias whose target cannot be statted must not open"),
        Err(e) => e,
    };
    assert!(
        !PalaceRegistry::open_error_is_absent(&denied),
        "an aliased palace whose target could not be statted was classified as absent — \
         resolve_palace_alias dropped the redirect and load_palace then answered for the alias \
         id's own empty directory, so the caller renders 404 for a palace that exists: {denied:#}"
    );
}
