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
use std::time::Duration;

/// Environment variable overriding the LRU open-handle cap
/// (`PalaceRegistry::with_max_open`).
///
/// Why: idle RSS on a busy host is dominated by how many palaces the daemon
/// keeps resident. Operators close to the fd ceiling (or trying to bound RAM)
/// need to shrink the cap without recompiling; hosts with a high fd limit may
/// raise it. An env var read once at registry construction is the lightest
/// knob.
/// What: parsed by [`max_open_palaces_from_env`]; consumed by
/// [`PalaceRegistry::from_env`].
/// Test: `registry_tests::from_env_respects_max_open_palaces_env`.
pub const MAX_OPEN_PALACES_ENV: &str = "TRUSTY_MEMORY_MAX_OPEN_PALACES";

/// Resolve the effective open-handle cap from the environment.
///
/// Why: centralises the env parse so `from_env` and diagnostics agree on the
/// value and the fallback.
/// What: reads [`MAX_OPEN_PALACES_ENV`]; returns its parsed `usize` when set to
/// a value `>= 1`, otherwise [`DEFAULT_MAX_OPEN_PALACES`]. An unset, empty,
/// non-numeric, or zero value falls back to the default (a zero cap would be
/// clamped to 1 anyway and is almost certainly a misconfiguration).
/// Test: `registry_tests::from_env_respects_max_open_palaces_env`.
pub fn max_open_palaces_from_env() -> usize {
    std::env::var(MAX_OPEN_PALACES_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&n| n >= 1)
        .unwrap_or(DEFAULT_MAX_OPEN_PALACES)
}

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
    /// Per-palace mutexes serialising `open_palace` so two concurrent misses
    /// for the same id never open the redb files twice at once.
    ///
    /// Why: idle-to-disk eviction makes cold-palace reopens common. redb takes
    /// an exclusive `flock(LOCK_EX)`; under `OpenIntent::Writer` a second
    /// same-process open of an already-open file returns `DatabaseAlreadyOpen`
    /// and, after the ~1.55 s handoff-retry window, fails loud (see
    /// `store::concurrent_open`). Without serialisation two racing recalls of a
    /// just-evicted palace would double-open and the loser would error. A tiny
    /// per-id sync mutex makes the second racer wait, re-check the cache, and
    /// share the winner's handle instead.
    /// What: `DashMap<PalaceId, Arc<parking_lot::Mutex<()>>>`. Held only across
    /// the (rare) open pipeline — never across an `.await` — so a
    /// `parking_lot::Mutex` is safe. Entries are lightweight and left in place
    /// (bounded by palace count) rather than reference-counted out.
    /// Test: `registry_tests::evict_idle_then_reopen_preserves_recall`.
    open_locks: Arc<DashMap<PalaceId, Arc<Mutex<()>>>>,
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
            open_locks: Arc::new(DashMap::new()),
        }
    }

    /// Create a registry whose open-handle cap comes from the environment.
    ///
    /// Why (idle-to-disk / issue #463): the daemon's resident-palace ceiling is
    /// the primary lever on idle RSS. `from_env` lets operators tune it via
    /// [`MAX_OPEN_PALACES_ENV`] without a rebuild while keeping the safe
    /// [`DEFAULT_MAX_OPEN_PALACES`] default. Callers that want the writer
    /// contract chain `.with_writer_intent()` as before.
    /// What: `with_max_open(max_open_palaces_from_env())`.
    /// Test: `registry_tests::from_env_respects_max_open_palaces_env`.
    pub fn from_env() -> Self {
        Self::with_max_open(max_open_palaces_from_env())
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
        // Serialise concurrent opens of the SAME palace so two racing misses
        // (common once idle-evict starts dropping cold palaces) never
        // double-open the redb files — under `Writer` intent the loser would
        // otherwise fail loud after the flock handoff-retry window. The winner
        // opens + registers; the loser re-checks the cache below and shares it.
        let open_lock = self
            .open_locks
            .entry(effective_id.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        let _open_guard = open_lock.lock();
        // Re-check under the open lock: another racer may have opened it while
        // we waited for the lock.
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

    /// Drop every idle, unreferenced palace handle — the "idle to disk" sweep.
    ///
    /// Why: even under the LRU cap the daemon keeps up to `max_open` palaces
    /// fully resident (drawer table + HNSW graph + KG adjacency, ~90 MB each)
    /// regardless of query activity, driving idle RSS to multiple GB. Dropping
    /// the whole `Arc<PalaceHandle>` for palaces no user has touched in the TTL
    /// window frees all of that heavy RAM at once; the durable redb store is
    /// the source of truth and the next access transparently re-opens from disk
    /// (`open_palace`, already covered by `lru_evicted_handle_reopens`).
    /// What: under the handles lock, selects ids whose handle (a) has been idle
    /// `>= threshold` (`PalaceHandle::idle_secs`) AND (b) has `Arc::strong_count
    /// == 1` — i.e. only the cache references it, so NO in-flight recall /
    /// remember / dream cycle holds it. That guard is the correctness anchor:
    /// dropping a strong_count==1 handle cannot race a live operation and its
    /// redb flocks release synchronously, so a concurrent reopen never
    /// double-opens the files. Victims are `pop`ed and dropped OUTSIDE the lock
    /// (fd close / RAM free must not run under it, mirroring `remove`). A zero
    /// `threshold` disables the sweep. Returns the number of handles evicted.
    /// Test: `registry_tests::evict_idle_drops_idle_unreferenced_handle`,
    /// `registry_tests::evict_idle_skips_referenced_handle`,
    /// `registry_tests::evict_idle_skips_recently_accessed_handle`.
    pub fn evict_idle(&self, threshold: Duration) -> usize {
        let threshold_secs = threshold.as_secs();
        if threshold_secs == 0 {
            return 0;
        }
        let evicted: Vec<Arc<PalaceHandle>> = {
            let mut cache = self.handles.lock();
            // Measure strong_count against the cache's OWN reference (never a
            // clone) so the == 1 guard is accurate; collect ids first because
            // `iter()` borrows the cache immutably and `pop()` needs it mutably.
            let victims: Vec<PalaceId> = cache
                .iter()
                .filter(|(_, h)| Arc::strong_count(h) == 1 && h.idle_secs() >= threshold_secs)
                .map(|(id, _)| id.clone())
                .collect();
            victims
                .into_iter()
                .filter_map(|id| cache.pop(&id))
                .collect()
        };
        let count = evicted.len();
        // `evicted` drops here, outside the lock: each handle's drawer table,
        // HNSW store, and KG close now, freeing RAM and releasing redb flocks.
        if count > 0 {
            tracing::info!(
                count,
                idle_threshold_secs = threshold_secs,
                "idle-evict: dropped {count} idle palace handle(s); redb remains the source of truth"
            );
        }
        count
    }
}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;
