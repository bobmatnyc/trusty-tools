//! Concurrent palace registry with LRU-bounded open-handle cache.
//!
//! Why: Issue #463 — each open palace holds ~3 redb file descriptors.
//! With many palaces the daemon can exhaust the OS fd limit (EMFILE).
//! An LRU-bounded cache lazily opens handles and evicts the least-recently-used
//! palace when the resident count reaches `max_open_palaces`, closing its fds.
//! The next access reopens from disk transparently.
//! What: Wraps a `parking_lot::Mutex<LruCache<PalaceId, Arc<PalaceHandle>>>` for
//! the open-handle set, with a `DashMap` for the knowledge-gap cache (unchanged).
//! The maximum number of concurrently-open handles is configurable via
//! `PalaceRegistry::with_max_open` and defaults to `DEFAULT_MAX_OPEN_PALACES`.
//! Test: `lru_evicts_least_recently_used`, `lru_evicted_handle_reopens`, and
//! `registry_remove_clears_cached_handle` in this module.

use crate::memory_core::community::KnowledgeGap;
use crate::memory_core::palace::{Palace, PalaceId};
use crate::memory_core::retrieval::PalaceHandle;
use crate::memory_core::store::concurrent_open::OpenIntent;
use crate::memory_core::store::palace_store::PalaceStore;
use crate::palace_alias::PalaceAliasStore;
use anyhow::{Context, Result};
use dashmap::DashMap;
use lru::LruCache;
use parking_lot::Mutex;
use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::Arc;

/// Default maximum number of palace handles to hold open simultaneously.
///
/// Why: Each open palace holds ~3 redb file descriptors (kg.db, index.usearch,
/// recall.db). With a typical macOS soft fd limit of 256 and daemon overhead,
/// 64 open palaces allows ~192 palace fds — leaving headroom for HTTP sockets,
/// log files, and other process fds. On Linux where the soft limit is commonly
/// 1 024 or higher, the default remains conservative; operators can raise it
/// via `PalaceRegistry::with_max_open`.
/// What: A compile-time constant; overridable per-instance.
/// Test: `lru_evicts_least_recently_used` forces eviction below this limit.
pub const DEFAULT_MAX_OPEN_PALACES: usize = 64;

/// Concurrent palace registry with LRU-bounded open-handle cache (issue #463).
///
/// Why: Unbounded `DashMap` growth meant one fd-exhaustion crash per large
/// workspace. The LRU strategy closes handles for idle palaces automatically
/// so the resident fd count stays bounded regardless of palace count.
/// What: `Mutex<LruCache<PalaceId, Arc<PalaceHandle>>>` for handles;
/// `DashMap<PalaceId, Vec<KnowledgeGap>>` for the gap cache (unchanged).
/// Cloning the registry is cheap — all heavyweight state lives behind `Arc`.
/// Test: `lru_evicts_least_recently_used`, `lru_evicted_handle_reopens`.
#[derive(Clone)]
pub struct PalaceRegistry {
    /// LRU cache of open palace handles bounded by `max_open_palaces`.
    ///
    /// Why: `Mutex` wraps the `LruCache` (which is not `Send + Sync` on its
    /// own) so it can be shared across async tasks via `Arc`. We use
    /// `parking_lot::Mutex` (not `std::sync::Mutex`) because it is
    /// `Send + Sync` and has lower overhead; we never hold it across an
    /// `.await` point.
    handles: Arc<Mutex<LruCache<PalaceId, Arc<PalaceHandle>>>>,
    /// Per-palace knowledge-gap cache populated by the dream cycle.
    ///
    /// Why: Issue #53 — community detection on the KG is too expensive to run
    /// on every `/kg/gaps` request (Louvain is O(|E|·passes) and the graph
    /// snapshot allocates). The dream cycle already walks the whole graph for
    /// dedup/decay, so it's the natural place to refresh the gap list once and
    /// stash the result for cheap read access from HTTP / MCP handlers.
    /// What: `DashMap<PalaceId, Vec<KnowledgeGap>>` so writers don't block
    /// readers across palaces. Missing entry == "dream cycle hasn't run yet";
    /// readers should treat that as an empty list, not an error.
    /// Test: `gaps_cache_round_trip` in this module.
    gaps_cache: Arc<DashMap<PalaceId, Vec<KnowledgeGap>>>,
    /// Redb open intent applied to every palace this registry opens.
    ///
    /// Why (issue #1487): the HTTP daemon is the sole writer and must open
    /// palace redb files with [`OpenIntent::Writer`] so a second daemon
    /// instance fails loud instead of silently degrading to a read-only
    /// snapshot. All other registries (CLI, stdio MCP, tests) default to
    /// [`OpenIntent::ReadOnlyClient`] to preserve the snapshot read-fallback
    /// (issue #59). The daemon opts in via [`PalaceRegistry::with_writer_intent`].
    /// What: `Copy` enum carried by value; threaded into `open_palace`,
    /// `create_palace`, and the eager `open` hydration path.
    /// Test: `with_writer_intent_sets_writer_open_intent`.
    open_intent: OpenIntent,
}

impl Default for PalaceRegistry {
    fn default() -> Self {
        Self::with_max_open(DEFAULT_MAX_OPEN_PALACES)
    }
}

impl PalaceRegistry {
    /// Create a registry with the default open-handle limit.
    ///
    /// Why: Most callers just want sane defaults; `new()` is the idiomatic
    /// constructor.
    /// What: Delegates to `with_max_open(DEFAULT_MAX_OPEN_PALACES)`.
    /// Test: All tests that call `PalaceRegistry::new()` implicitly exercise this.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a registry with a custom open-handle limit.
    ///
    /// Why: Issue #463 — operators on machines with a high fd limit may want
    /// more concurrent handles; operators running close to the fd ceiling can
    /// reduce the cap. The test suite uses small capacities to force eviction.
    /// What: Constructs a `LruCache` with capacity `max_open_palaces`; values
    /// below 1 are clamped to 1.
    /// Test: `lru_evicts_least_recently_used` uses capacity 2 to force eviction.
    pub fn with_max_open(max_open_palaces: usize) -> Self {
        let cap = NonZeroUsize::new(max_open_palaces.max(1)).expect("max(1) is always nonzero");
        Self {
            handles: Arc::new(Mutex::new(LruCache::new(cap))),
            gaps_cache: Arc::new(DashMap::new()),
            // Default to read-only-client intent so CLI / stdio / test
            // registries keep the issue-#59 snapshot read-fallback. The HTTP
            // daemon overrides this via `with_writer_intent` (issue #1487).
            open_intent: OpenIntent::ReadOnlyClient,
        }
    }

    /// Mark this registry as the sole writer: open every palace with
    /// [`OpenIntent::Writer`].
    ///
    /// Why (issue #1487): the HTTP daemon must fail loud when another live
    /// daemon already holds a palace's redb write lock, rather than silently
    /// opening a read-only snapshot and rejecting every write for its
    /// lifetime (the original bug — a rogue second listener served read-only
    /// and the legitimate `memory_remember` was lost). Calling this on the
    /// daemon's registry threads `Writer` intent down to every
    /// `PalaceHandle::open_with_intent`.
    /// What: Consuming builder that sets `open_intent = OpenIntent::Writer`
    /// and returns `self`. All `Arc`-shared state is preserved.
    /// Test: `with_writer_intent_sets_writer_open_intent`.
    #[must_use]
    pub fn with_writer_intent(mut self) -> Self {
        self.open_intent = OpenIntent::Writer;
        self
    }

    /// The redb open intent this registry applies to every palace it opens.
    ///
    /// Why: Lets the daemon assert (in tests / diagnostics) that it really is
    /// running as the writer, and lets callers branch if needed.
    /// What: Returns the `Copy` `OpenIntent` value.
    /// Test: `with_writer_intent_sets_writer_open_intent`.
    #[must_use]
    pub fn open_intent(&self) -> OpenIntent {
        self.open_intent
    }

    /// Insert a new palace handle, replacing any prior entry with the same id.
    ///
    /// Why: Registry is the single source of truth for live palaces; callers
    /// hand off ownership of a freshly built handle and the registry shares it
    /// behind an `Arc` to all concurrent readers. If the LRU capacity is
    /// reached, the least-recently-used handle is evicted (its fds close).
    /// What: Acquires the handle lock, calls `LruCache::put`, and drops the
    /// evicted entry (if any) outside the lock so Drop doesn't run under it.
    /// Test: `register_and_get_roundtrip` re-fetches by id and compares.
    pub fn register(&self, handle: PalaceHandle) {
        let id = handle.id.clone();
        let arc = Arc::new(handle);
        let _evicted = {
            let mut cache = self.handles.lock();
            cache.put(id, arc)
        };
        // `_evicted` drops here, outside the lock, closing fds.
    }

    /// Insert an already-shared handle.
    ///
    /// Why: Useful when the caller wants to keep its own `Arc` reference
    /// (e.g. to mutate L1 caches under a separate lock). Semantics match
    /// `register` — may evict the LRU handle if at capacity.
    /// What: Acquires the lock, calls `LruCache::put`.
    /// Test: Exercised by `open_palace` and `create_palace`.
    pub fn register_arc(&self, handle: Arc<PalaceHandle>) {
        let id = handle.id.clone();
        let _evicted = {
            let mut cache = self.handles.lock();
            cache.put(id, handle)
        };
    }

    /// Cheap clone of the `Arc` — promotes the entry to MRU position.
    ///
    /// Why: Returns the handle if present; the LRU get call also refreshes
    /// the access order so frequently-used palaces stay in cache.
    /// What: Acquires the lock, calls `LruCache::get`, clones the `Arc`.
    /// Test: `register_and_get_roundtrip`, `lru_evicts_least_recently_used`.
    pub fn get(&self, id: &PalaceId) -> Option<Arc<PalaceHandle>> {
        let mut cache = self.handles.lock();
        cache.get(id).cloned()
    }

    /// Peek at a handle without promoting it to MRU position.
    ///
    /// Why: Introspection paths (e.g. `len`, iteration) want to inspect without
    /// disturbing the eviction order that the request path relies on.
    /// What: Acquires the lock, calls `LruCache::peek`.
    /// Test: `lru_evicts_least_recently_used` uses `peek` to inspect LRU state.
    pub fn peek(&self, id: &PalaceId) -> Option<Arc<PalaceHandle>> {
        let cache = self.handles.lock();
        cache.peek(id).cloned()
    }

    /// List all currently open palace ids (order not guaranteed).
    ///
    /// Why: `palace list` and `status` need a registry-wide view of what is
    /// currently loaded. Note: this only returns currently-open handles; to list
    /// all persisted palaces use `PalaceRegistry::list_palaces`.
    /// What: Snapshots the LRU key set.
    /// Test: `list_contains_all_registered`.
    pub fn list(&self) -> Vec<PalaceId> {
        let cache = self.handles.lock();
        cache.iter().map(|(k, _)| k.clone()).collect()
    }

    /// Number of currently open handles.
    ///
    /// Why: Health and admin endpoints surface open-handle count for fd-exhaustion
    /// monitoring.
    /// What: Returns `LruCache::len()`.
    /// Test: `register_and_get_roundtrip`.
    pub fn len(&self) -> usize {
        self.handles.lock().len()
    }

    /// Whether the registry has no open handles.
    ///
    /// Why: Guard condition for startup code that expects an empty registry.
    /// What: Returns `true` when `len() == 0`.
    /// Test: `register_and_get_roundtrip`.
    pub fn is_empty(&self) -> bool {
        self.handles.lock().is_empty()
    }

    /// Store the latest knowledge-gap snapshot for `palace_id`.
    ///
    /// Why: The dream cycle computes gaps once per pass (issue #53); subsequent
    /// `/kg/gaps` and `kg_gaps` MCP calls read this cached vec instead of
    /// re-running Louvain on every request.
    /// What: Inserts (replacing any prior snapshot) into the per-registry
    /// `gaps_cache`. Cheap and lock-free at the per-palace granularity thanks
    /// to `DashMap`.
    /// Test: `gaps_cache_round_trip`.
    pub fn set_gaps(&self, palace_id: PalaceId, gaps: Vec<KnowledgeGap>) {
        self.gaps_cache.insert(palace_id, gaps);
    }

    /// Read the cached knowledge gaps for `palace_id`.
    ///
    /// Why: HTTP and MCP read paths must not pay the Louvain cost; they read
    /// whatever the dream cycle last wrote. A `None` return is meaningful —
    /// it means "no cycle has run yet" — and callers render an empty list
    /// rather than a 404.
    /// What: Clones the cached `Vec<KnowledgeGap>` so callers can serialize
    /// without holding the DashMap entry guard.
    /// Test: `gaps_cache_round_trip`.
    pub fn get_gaps(&self, palace_id: &PalaceId) -> Option<Vec<KnowledgeGap>> {
        self.gaps_cache.get(palace_id).map(|r| r.value().clone())
    }

    /// Drop the cached gaps for `palace_id` (e.g. on palace deletion).
    ///
    /// Why: Without explicit clearing the cache would retain entries for
    /// removed palaces and surface stale community shapes in the dashboard.
    /// What: Removes the entry; no-op when not present.
    /// Test: `gaps_cache_round_trip` covers the inverse (insert then read).
    pub fn clear_gaps(&self, palace_id: &PalaceId) {
        self.gaps_cache.remove(palace_id);
    }

    /// Drop the cached handle (and any cached gaps) for `palace_id`.
    ///
    /// Why: Palace deletion (issue #180) must invalidate the in-memory
    /// `Arc<PalaceHandle>` so future `open_palace` calls hit the disk and
    /// see the missing directory instead of silently serving the stale
    /// handle from cache. Without this, the daemon would keep returning
    /// the deleted palace's KG/drawer state until the next restart.
    /// What: Removes the LRU entry and the associated gap-cache entry.
    /// Both removes are no-ops when the entries are absent, so this method
    /// is safe to call on an already-cleared id.
    /// Test: `registry_remove_clears_cached_handle`.
    pub fn remove(&self, palace_id: &PalaceId) {
        let _evicted = {
            let mut cache = self.handles.lock();
            cache.pop(palace_id)
        };
        self.gaps_cache.remove(palace_id);
        // `_evicted` drops here, closing fds outside the lock.
    }

    /// Open a palace by id, hydrating from `<data_root>/<palace_id>/` on disk.
    ///
    /// Why: The CLI and MCP server look palaces up by id; this is the single
    /// entry point for reconstructing a `PalaceHandle` from disk and
    /// memoizing it in the LRU registry. If the cache is full the LRU entry
    /// is silently evicted (its fds close); the data is safe on disk.
    /// What: Returns the cached `Arc<PalaceHandle>` if present (promotes to MRU);
    /// otherwise loads metadata via `PalaceStore::load_palace`, calls
    /// `PalaceHandle::open`, and inserts the handle (may evict LRU).
    /// Test: `registry_create_and_open` round-trips create -> drop -> reopen;
    /// `lru_evicted_handle_reopens` verifies evicted handles are transparently
    /// reopened; `open_palace_follows_alias` covers the alias redirect.
    pub fn open_palace(&self, data_root: &Path, palace_id: &PalaceId) -> Result<Arc<PalaceHandle>> {
        // Fast path: an already-open handle under the requested id.
        if let Some(h) = self.get(palace_id) {
            return Ok(h);
        }
        // Issue #1939: when the requested palace has no on-disk metadata but a
        // persisted palace-level alias redirects it to an existing palace, open
        // (and cache under) the alias target so the alias and the canonical name
        // share ONE handle — never two writers over the same redb files.
        // `resolve_palace_alias` returns the original id unchanged when there is
        // no redirect; re-checking the cache under the (possibly redirected) id
        // shares an already-open canonical handle (a redundant miss otherwise).
        let effective_id = Self::resolve_palace_alias(data_root, palace_id);
        if let Some(h) = self.get(&effective_id) {
            return Ok(h);
        }
        let palace_dir = data_root.join(effective_id.as_str());
        let palace = PalaceStore::load_palace(&palace_dir)
            .with_context(|| format!("load palace metadata for {palace_id}"))?;
        // Issue #1487: honour the registry's open intent. On the HTTP daemon
        // (`Writer`) a second live instance holding the lock makes this fail
        // loud rather than returning a snapshot-mode (read-only) handle.
        let handle = PalaceHandle::open_with_intent(&palace, self.open_intent)?;
        self.register_arc(handle.clone());
        Ok(handle)
    }

    /// Resolve a palace-level alias, but ONLY when the requested palace is
    /// missing on disk (issue #1939).
    ///
    /// Why: an alias must never shadow a real palace of the same name — a lookup
    /// for an existing palace always resolves to itself. The redirect fires only
    /// in the split-brain case: the requested `owner-repo` palace was never
    /// created, yet an alias points it at an existing bare-repo palace. Requiring
    /// the target to also exist on disk stops a stale alias from redirecting to a
    /// deleted palace (which would just fail load anyway, but this keeps the
    /// error message about the ORIGINAL id).
    /// What: returns `palace_id` unchanged when `<data_root>/<palace_id>/palace.json`
    /// exists. Otherwise consults [`PalaceAliasStore::resolve_alias`]; if it maps
    /// to a `target` whose `<data_root>/<target>/palace.json` exists, returns that
    /// target id. In every other case returns `palace_id` unchanged so the caller
    /// surfaces the normal "metadata missing" error. Alias-map read errors are
    /// swallowed (best-effort redirect) — a broken alias file must not break
    /// resolution of palaces that DO exist.
    /// Test: `open_palace_follows_alias`, `open_palace_ignores_alias_when_target_missing`,
    /// `open_palace_prefers_real_palace_over_alias`.
    fn resolve_palace_alias(data_root: &Path, palace_id: &PalaceId) -> PalaceId {
        let exists = |id: &str| data_root.join(id).join("palace.json").exists();
        if exists(palace_id.as_str()) {
            return palace_id.clone();
        }
        match PalaceAliasStore::resolve_alias(data_root, palace_id.as_str()) {
            Ok(Some(target)) if exists(&target) => PalaceId::new(target),
            _ => palace_id.clone(),
        }
    }

    /// Create and persist a new palace, then open it.
    ///
    /// Why: `palace new` saves metadata and immediately wants a working handle
    /// for further operations; combining the steps avoids a TOCTOU between
    /// save and open.
    /// What: Computes `data_dir = data_root/<id>`, writes `palace.json`, and
    /// returns a freshly opened handle (registered in the LRU cache, possibly
    /// evicting the LRU entry if at capacity).
    /// Test: `registry_create_and_open`.
    pub fn create_palace(&self, data_root: &Path, mut palace: Palace) -> Result<Arc<PalaceHandle>> {
        // Always anchor data_dir under data_root/<id> so callers can pass a
        // bare Palace without worrying about path layout.
        let palace_dir = data_root.join(palace.id.as_str());
        palace.data_dir = palace_dir.clone();
        std::fs::create_dir_all(&palace_dir)
            .with_context(|| format!("create palace dir {}", palace_dir.display()))?;
        PalaceStore::save_palace(&palace)
            .with_context(|| format!("save palace metadata for {}", palace.id))?;
        // Issue #1487: honour the registry's open intent (Writer on the HTTP
        // daemon) so a freshly-created palace is opened under the same
        // fail-loud contract as a re-opened one.
        let handle = PalaceHandle::open_with_intent(&palace, self.open_intent)?;
        self.register_arc(handle.clone());
        Ok(handle)
    }

    /// List every palace persisted under `data_root`.
    ///
    /// Why: `palace list` and `status` need a registry-wide view that survives
    /// across daemon restarts.
    /// What: Delegates to `PalaceStore::list_palaces`.
    /// Test: `list_palaces_finds_saved_palaces` in the palace_store module
    /// covers the underlying walker.
    pub fn list_palaces(data_root: &Path) -> Result<Vec<Palace>> {
        PalaceStore::list_palaces(data_root)
            .with_context(|| format!("list palaces under {}", data_root.display()))
    }

    /// Open a registry rooted at `data_root` and pre-hydrate every persisted
    /// palace into the in-memory LRU cache.
    ///
    /// Why: Issue #52 — production hosts (open-mpm) want a single call that
    /// brings up the full registry on daemon startup so that recall paths
    /// don't pay a lazy-open latency on the first request after a restart.
    /// Existing call sites continue to use `new()` + `open_palace()`; this is
    /// the convenience for hosts that prefer an eager warmup.
    /// Note (issue #463): if the number of persisted palaces exceeds
    /// `max_open_palaces`, the oldest-opened ones are evicted during hydration
    /// — they will be lazily re-opened on first access. The LRU invariant is
    /// maintained throughout.
    /// What: Creates `data_root` if missing, calls `PalaceStore::list_palaces`,
    /// and for each persisted palace builds a `PalaceHandle` via
    /// `PalaceHandle::open` and registers it. Errors hydrating a single palace
    /// are logged and skipped so one corrupt palace doesn't take the whole
    /// registry down — matches the resiliency choice in `PalaceStore::list_palaces`.
    /// Test: `open_hydrates_persisted_palaces` exercises restart by writing,
    /// dropping, and reopening.
    pub fn open(data_root: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_root)
            .with_context(|| format!("create registry root {}", data_root.display()))?;
        let registry = Self::new();
        let palaces = PalaceStore::list_palaces(data_root)
            .with_context(|| format!("list palaces under {}", data_root.display()))?;
        for palace in palaces {
            // Use the registry's configured intent (issue #1487). The eager
            // hydration constructor builds a `ReadOnlyClient` registry via
            // `Self::new()`, so this preserves the historical snapshot-fallback
            // behaviour while staying correct if a future caller hydrates a
            // writer registry.
            match PalaceHandle::open_with_intent(&palace, registry.open_intent) {
                Ok(handle) => registry.register_arc(handle),
                Err(e) => {
                    tracing::warn!(palace = %palace.id, "skipping palace during registry open: {e:#}");
                }
            }
        }
        Ok(registry)
    }
}

#[cfg(test)]
mod tests {
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
        reg.create_palace(data_root, bare).expect("create bare palace");

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

        PalaceAliasStore::register_alias(data_root, "ghost", "does-not-exist")
            .expect("register alias");

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
}
