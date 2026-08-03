//! `ColdIndexStore` — in-memory registry of deferred ("cold") indexes (#993).
//!
//! Why: indexes not in the top-N by recency are parked here at startup instead
//! of being loaded into `IndexRegistry`. The double-checked-lock pattern (via
//! per-index `Mutex<()>`) prevents concurrent double-loads of the same index.
//! What: two `DashMap`s — one for persisted metadata, one for loading gates.
//! Test: `cold_store_*` tests in the parent module's `tests` block.

use std::sync::Arc;

use dashmap::DashMap;

use crate::core::registry::IndexId;
use crate::service::persistence::{warmboot_sort_key, PersistedIndex};

/// Split `entries` into `(eager, cold)` based on a recency cap.
///
/// Why: the warm-boot loop in `start.rs` calls `restore_indexes` for the eager
/// slice (driven by `TRUSTY_WARMBOOT_MAX_INDEXES`) and registers the cold
/// slice into `ColdIndexStore` without loading them. Issue #2161 reuses this
/// exact ranking for a second caller: the runtime residency sweep
/// (`lazy_loader::residency::ids_to_park`) calls this with the *currently
/// resident* entries and `TRUSTY_MAX_RESIDENT_INDEXES` to decide which
/// already-loaded indexes to cold-park. Sharing one comparator means the
/// boot-time and runtime "keep the hottest N" decisions can never drift apart.
/// What: when `max_n` is `None` (env var unset), all entries are eager and the
/// cold list is empty (back-compat). When `max_n == Some(0)`, all entries go
/// cold. Otherwise the top-N most-recently-used entries are eager (sort key:
/// `max(last_queried_unix, last_indexed_unix)` descending; ties break by id
/// ascending so the split is deterministic across restarts).
/// The sort is stable: entries with the same sort key keep their original order
/// within the sorted group, then id-alpha tie-break.
/// Test: `select_warmboot_entries_*` in the parent module's `tests` block;
///       `ids_to_park_*` in `residency::tests` for the runtime-sweep reuse.
pub fn select_warmboot_entries(
    entries: Vec<PersistedIndex>,
    max_n: Option<usize>,
) -> (Vec<PersistedIndex>, Vec<PersistedIndex>) {
    let Some(n) = max_n else {
        // Back-compat: no cap → all eager, nothing cold.
        return (entries, Vec::new());
    };

    if n == 0 {
        return (Vec::new(), entries);
    }

    if entries.len() <= n {
        // All fit within the cap — nothing goes cold.
        return (entries, Vec::new());
    }

    // Sort descending by recency sort key, then ascending by id for tie-break.
    let mut sorted = entries;
    sorted.sort_by(|a, b| {
        let ka = warmboot_sort_key(a);
        let kb = warmboot_sort_key(b);
        kb.cmp(&ka).then_with(|| a.id.cmp(&b.id))
    });

    let cold = sorted.split_off(n);
    (sorted, cold)
}

/// A cold entry paired with an opaque identity token (issue #3995 round 5).
///
/// Why: `ColdIndexStore::mark_loaded`'s unconditional `entries.remove(id)` was
/// found (PR #3995 round 4 review) to blindly reap whatever is CURRENTLY
/// parked under `id` — including an entry a completely different writer
/// legitimately installed a moment ago, orphaning that writer's index in
/// neither the hot registry nor the cold store. `token` gives every insertion
/// a unique, comparable identity (`Arc::ptr_eq`, mirroring the exact
/// discipline `IndexRegistry::restore` already applies on the registry side)
/// so a caller can snapshot "the entry I observed" via `entry_token` and later
/// remove-if-unchanged via `mark_loaded_if` — never someone else's entry.
/// What: a plain `(PersistedIndex, Arc<()>)` pair; the `Arc<()>` carries no
/// data, only identity.
#[derive(Clone)]
struct ColdEntry {
    persisted: PersistedIndex,
    token: Arc<()>,
}

/// In-memory registry of cold (not-yet-loaded) indexes.
///
/// Why (issue #993): indexes not in the top-N by recency are parked here at
/// startup. On first access via `get_or_load_index`, one background task loads
/// the index into the hot `IndexRegistry`. The per-index `Mutex<()>` prevents
/// concurrent double-loads.
///
/// Why (issue #1106): when `restore_fn` returns `false` (blocked volume,
/// missing root_path), the entry must be moved out of `entries` into
/// `failed_entries` so that (a) `indexes_lazy` only counts genuinely-restorable
/// pending indexes, (b) repeated queries for the same permanently-failed index
/// skip the expensive restore path and return a fast error, and (c) callers can
/// distinguish "not yet loaded" from "restore permanently failed".
///
/// What: three `DashMap`s — one for pending metadata (`entries`), one for
/// per-index loading gates, and one for permanently-failed index ids
/// (`failed_entries`). `len()` counts only `entries` (pending). `failed_len()`
/// counts `failed_entries`. Each `entries` value additionally carries an
/// identity token (issue #3995 round 5, see `ColdEntry`) so `mark_loaded_if`
/// can reap a specific, previously-observed entry without disturbing a
/// different one a concurrent writer installed under the same id since.
/// Test: `cold_store_*` tests in the parent module's `tests` block;
///       `cold_store_mark_failed_*` tests for the issue #1106 paths;
///       `cold_store_mark_loaded_if_*` for the round-5 identity guard.
#[derive(Clone, Default)]
pub struct ColdIndexStore {
    /// Persisted metadata (+ identity token) for each cold index, keyed by
    /// `IndexId`. Private: `ColdEntry` is store-internal, so cross-module
    /// readers (e.g. `loader.rs`) go through `get_persisted`/`entry_token`
    /// rather than reaching into this field directly.
    entries: Arc<DashMap<IndexId, ColdEntry>>,
    /// Per-index mutex preventing concurrent double-loads.
    loading_gates: Arc<DashMap<IndexId, Arc<tokio::sync::Mutex<()>>>>,
    /// Permanently-failed entries (issue #1106): indexes whose `restore_fn`
    /// returned `false`. These are evicted from `entries` so `len()` and
    /// `indexes_lazy` stay honest. Presence here signals "do not retry".
    ///
    /// Why: the value is `()` — we only need set semantics (O(1) membership
    /// test). A `DashMap<IndexId, ()>` gives that without an extra `HashSet`.
    /// What: populated by `mark_failed`; checked by `is_failed`.
    /// Test: `cold_store_mark_failed_*` unit tests.
    failed_entries: Arc<DashMap<IndexId, ()>>,
}

impl ColdIndexStore {
    /// Why: zero-arg constructor for default state construction.
    /// What: creates empty DashMaps; no disk I/O.
    /// Test: `ColdIndexStore::default()`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a batch of cold entries at daemon startup.
    ///
    /// Why: `restore_indexes` calls this once with the "cold" slice returned by
    /// `select_warmboot_entries` so the store is populated before any query arrives.
    /// What: inserts each entry under its `IndexId`, each stamped with a fresh
    /// identity token (issue #3995 round 5). Idempotent (re-insert replaces —
    /// and mints a NEW token, so any `entry_token` a caller captured before
    /// this call is intentionally invalidated by it). Returns the freshly
    /// minted token for each entry, in the same order as `entries` — most
    /// callers (the bulk boot-time path) ignore this; `cold_park_index_inner`
    /// uses it to track its own single insertion (see `mark_loaded_if`).
    /// Test: `cold_store_register_and_contains`.
    pub fn register_cold_entries(&self, entries: Vec<PersistedIndex>) -> Vec<Arc<()>> {
        entries
            .into_iter()
            .map(|entry| {
                let id = IndexId::new(entry.id.clone());
                let token = Arc::new(());
                self.entries.insert(
                    id,
                    ColdEntry {
                        persisted: entry,
                        token: token.clone(),
                    },
                );
                token
            })
            .collect()
    }

    /// Park an index whose eager warm-boot restore timed out (#4087).
    ///
    /// Why: a warm-boot restore timeout previously tallied a counter and did
    /// NOTHING else. The entry reached neither the registry nor this store, so
    /// for the remainder of that boot the index simply did not exist:
    /// `list_indexes` omitted it, `search` 404'd it, and `search_all` could not
    /// even count it as skipped. A live daemon lost 11 indexes this way,
    /// recoverable only by a restart. A timeout is the *most* transient failure
    /// the daemon has — the restore was slow, not wrong — so dropping it is the
    /// least appropriate response available. Parking makes it recoverable
    /// through the machinery that already exists for deferred indexes:
    /// `get_or_load_index` lazy-loads it on first query, and [`Self::len`]
    /// folds it into `search_all`'s `cold_indexes_skipped` so an incomplete
    /// fan-out stays visible.
    ///
    /// What: [`Self::register_cold_entries`] for one entry, plus a WARN naming
    /// the state change. Idempotent by replacement, so a second timeout for the
    /// same id simply re-parks it. Deliberately does NOT retry the restore
    /// inline — the abandoned blocking thread from the timeout may still hold
    /// the redb file, and the lazy-load path already handles that via the
    /// `DatabaseAlreadyOpen` retry and the #3659 open gate.
    /// Test: `timed_out_entry_is_parked_in_cold_store` in
    /// `service::server::tests_4087`.
    pub fn park_timed_out(&self, entry: PersistedIndex) {
        let id = entry.id.clone();
        self.register_cold_entries(vec![entry]);
        tracing::warn!(
            index_id = %id,
            "warm-boot: index '{id}' timed out during eager restore — PARKED in the cold \
             store for lazy load on first query instead of being dropped for the rest of \
             this boot (issue #4087). It stays absent from `list_indexes` until loaded, but \
             a query against it now restores it on demand rather than 404-ing until a \
             daemon restart."
        );
    }

    /// True when `id` is in the cold store (registered but not yet loaded).
    ///
    /// Why: `get_or_load_index` uses this to decide whether a 404 is a genuine
    /// unknown index or a not-yet-loaded cold index. Returns `false` for
    /// permanently-failed entries (issue #1106) so callers do not re-enter the
    /// expensive restore path.
    /// What: O(1) DashMap lookup on `entries` only (not `failed_entries`).
    /// Test: `cold_store_register_and_contains`.
    pub fn contains(&self, id: &IndexId) -> bool {
        self.entries.contains_key(id)
    }

    /// True when `id` has previously failed to restore (issue #1106).
    ///
    /// Why: distinguishes "not registered at all" from "registered but
    /// permanently unrestorable". Callers use this to return a fast 503
    /// (`index_restore_failed`) without re-entering the expensive restore path.
    /// What: O(1) DashMap lookup on `failed_entries`.
    /// Test: `cold_store_mark_failed_is_failed` unit test.
    pub fn is_failed(&self, id: &IndexId) -> bool {
        self.failed_entries.contains_key(id)
    }

    /// Total number of cold (not-yet-loaded) entries.
    ///
    /// Why: reported on `GET /health` as `indexes_lazy` so operators can see how
    /// many indexes are still pending their first load. Does NOT include
    /// permanently-failed entries (issue #1106) — those are counted separately
    /// by `failed_len()` so the metric stays honest.
    /// What: `DashMap::len()` on `entries` only.
    /// Test: `cold_store_len`.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when no entries remain PENDING their first load.
    ///
    /// Why: cheap O(1) check used by callers that want to know whether the cold
    /// store has been drained of pending entries.
    /// What: checks only `entries` (pending). Does NOT account for permanently-
    /// failed entries — those are absent from `entries` and already counted
    /// separately by `failed_len()`. In other words, `is_empty()` returning
    /// `true` does NOT imply `failed_len() == 0`; it only means every registered
    /// cold entry has either been successfully loaded (via `mark_loaded`) or
    /// permanently failed (via `mark_failed`).
    /// Test: `cold_store_register_and_contains` and `cold_store_mark_failed_*`.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Number of indexes that permanently failed to restore (issue #1106).
    ///
    /// Why: reported on `GET /health` as `indexes_failed` so operators can
    /// distinguish "pending lazy load" from "restore permanently failed" —
    /// e.g. blocked volume or deleted root_path. Before this fix both appeared
    /// as "lazy pending", making the metric misleading.
    /// What: `DashMap::len()` on `failed_entries`.
    /// Test: `cold_store_mark_failed_failed_len` unit test.
    pub fn failed_len(&self) -> usize {
        self.failed_entries.len()
    }

    /// Count how many cold entries are in the provided id set.
    ///
    /// Why: `global_search_handler` (PR #1103) needs to count cold indexes
    /// that a restricted fan-out caller requested but that were skipped because
    /// they are not yet hot. Providing a method here keeps the caller from
    /// iterating `entries` directly and coupling to the internal DashMap type.
    /// What: O(|ids|) DashMap lookups.
    /// Test: exercised by `test_global_search_surfaces_cold_indexes_skipped`.
    pub fn count_matching<I>(&self, ids: I) -> usize
    where
        I: IntoIterator,
        I::Item: AsRef<str>,
    {
        ids.into_iter()
            .filter(|s| self.entries.contains_key(&IndexId::new(s.as_ref())))
            .count()
    }

    /// Snapshot every currently-cold (pending) entry's persisted metadata.
    ///
    /// Why (issue #3993 second round — adversarial BLOCK): the write-side
    /// `root_path` collision guard (`find_root_path_collision`, shared by
    /// `create_index_handler`, `relocate_index_handler`, and the reindex
    /// `root_path` override) must treat a cold/unloaded entry's root_path as
    /// already claimed, exactly like a live handle's — otherwise a new
    /// registration can silently steal a pre-existing but not-yet-restored
    /// index's on-disk corpus with no race required at all (the cold entry
    /// just sits parked, untouched, before the write call arrives).
    /// What: clones every `entries` value out into a `Vec` (tens to low
    /// hundreds of entries in practice — the same order of magnitude
    /// `find_root_path_collision` already linear-scans for live handles, so
    /// this adds no new complexity class). Does NOT include `failed_entries`
    /// — a permanently-failed cold entry no longer owns a live claim on its
    /// root_path (its restore will never be retried, and its on-disk corpus,
    /// if any, is not held open by anything).
    /// Test: `create_index_rejects_root_path_owned_by_cold_entry`,
    /// `relocate_index_rejects_root_path_owned_by_cold_entry`,
    /// `reindex_root_override_rejects_collision_with_cold_entry` in
    /// `collision_3993_tests.rs`.
    pub fn snapshot(&self) -> Vec<PersistedIndex> {
        self.entries
            .iter()
            .map(|kv| kv.value().persisted.clone())
            .collect()
    }

    /// Fetch the persisted metadata for a single cold entry, if present.
    ///
    /// Why: `get_or_load_index` (`loader.rs`) needs the `PersistedIndex` to
    /// pass into `restore_fn`. `ColdEntry` (the `entries` value type) is
    /// private to this module — issue #3995 round 5 added the identity token
    /// alongside the persisted metadata — so cross-module readers go through
    /// this accessor instead of reaching into the field directly.
    /// What: O(1) DashMap lookup; clones the `PersistedIndex` out, dropping
    /// the identity token (irrelevant to this caller).
    /// Test: exercised by `get_or_load_index_loads_cold_index` (via
    /// `get_or_load_index`, the only caller).
    pub(crate) fn get_persisted(&self, id: &IndexId) -> Option<PersistedIndex> {
        self.entries.get(id).map(|kv| kv.value().persisted.clone())
    }

    /// Snapshot the identity token of `id`'s current cold entry, if any
    /// (issue #3995 round 5).
    ///
    /// Why: a caller about to perform a write that might race a concurrent
    /// cold-store mutation (e.g. the residency-sweep park, or a
    /// create/relocate/reindex handler about to reap what it believes is its
    /// own stale leftover) captures this token FIRST — before its own write —
    /// then passes it to `mark_loaded_if` afterward. This mirrors
    /// `IndexRegistry::get`'s role as the `expected` snapshot in the
    /// registry-side `Arc::ptr_eq` guard (`cold_park_index_inner`, round 4):
    /// the eventual reap only ever removes the SAME entry observed here —
    /// never a different one a concurrent writer installed in between.
    /// What: O(1) DashMap lookup; `None` means no cold entry was present at
    /// the moment of the call.
    /// Test: `cold_store_mark_loaded_if_*`.
    pub fn entry_token(&self, id: &IndexId) -> Option<Arc<()>> {
        self.entries.get(id).map(|kv| kv.value().token.clone())
    }

    /// Remove `id`'s cold entry only if it is still the exact entry
    /// identified by `expected_token` (issue #3995 round 5 CRITICAL).
    ///
    /// Why: `mark_loaded`'s unconditional removal is unsafe whenever the
    /// caller cannot prove the entry currently parked under `id` is the same
    /// one it intends to reap — see `mark_loaded`'s doc for the concrete
    /// orphan this closes. This is the identity-guarded counterpart,
    /// mirroring `IndexRegistry::restore`'s `Arc::ptr_eq` discipline on the
    /// registry side.
    /// What: `expected_token` is what `entry_token` (or the token returned by
    /// `register_cold_entries`) observed earlier. Removal proceeds only when
    /// the CURRENT token is `Arc::ptr_eq` to `expected_token` (both `Some`),
    /// or when both are `None` (nothing to remove either way — a harmless
    /// no-op). Any other combination means the entry changed since it was
    /// observed — e.g. a concurrent write installed a fresh entry, or a
    /// concurrent reap already removed the one that was there — so this call
    /// leaves the current state untouched rather than risk deleting an entry
    /// it does not recognize. The loading gate is always cleared regardless:
    /// it is a coordination mutex, not data, and a new one is lazily
    /// recreated on next access if still needed.
    /// Test: `cold_store_mark_loaded_if_removes_matching_token`,
    /// `cold_store_mark_loaded_if_leaves_mismatched_token`,
    /// `cold_store_mark_loaded_if_none_expected_and_none_present_is_noop`,
    /// `cold_store_mark_loaded_if_none_expected_but_entry_present_leaves_it`;
    /// end-to-end race reproduction in
    /// `cold_park_index_restores_concurrently_swapped_handle_instead_of_orphaning`
    /// and the create/relocate/reindex handler tests in
    /// `collision_3993_tests.rs`.
    pub fn mark_loaded_if(&self, id: &IndexId, expected_token: Option<Arc<()>>) {
        let current_token = self.entries.get(id).map(|kv| kv.value().token.clone());
        let should_remove = match (&current_token, &expected_token) {
            (Some(current), Some(expected)) => Arc::ptr_eq(current, expected),
            (None, None) => true,
            _ => false,
        };
        if should_remove {
            self.entries.remove(id);
        }
        self.loading_gates.remove(id);
    }

    /// Remove a cold entry after it has been successfully loaded into the hot registry.
    ///
    /// Why: once the index is in `IndexRegistry`, future `get_or_load_index` calls
    /// hit the hot-path branch and the cold entry is no longer needed.
    ///
    /// **CAUTION (issue #3995 round 4/5):** this removal is UNCONDITIONAL — it
    /// reaps whatever happens to be parked under `id` right now, with no way
    /// to tell "the stale leftover I mean to clean up" apart from "an entry a
    /// completely different concurrent writer legitimately installed a moment
    /// ago". Round 4 review proved this orphans an index (removed from the
    /// hot registry by a concurrent `cold_park_index`, then its freshly
    /// parked cold entry reaped right back out from under it by this call) in
    /// 2 of 10 possible interleavings between this method's callers
    /// (`create_index_handler` / `relocate_index_handler` / `reindex_handler`'s
    /// override) and the residency-sweep park. Only call this UNGUARDED when
    /// the caller can prove no concurrent writer can be racing this exact
    /// `id` (e.g. `get_or_load_index`'s call is protected by the per-index
    /// `loading_gate` mutex AND `cold_park_index_inner`'s own in-flight guard,
    /// which refuses to park any id still present in the cold store — see
    /// `cold_park_index`'s doc). Every OTHER caller must use
    /// [`Self::mark_loaded_if`] with a token captured via [`Self::entry_token`]
    /// (or returned by [`Self::register_cold_entries`]) BEFORE its own write,
    /// so the reap only ever removes the entry it actually observed.
    /// What: removes the entry and its loading gate.
    /// Test: exercised by `get_or_load_index_loads_cold_index`.
    pub fn mark_loaded(&self, id: &IndexId) {
        self.entries.remove(id);
        self.loading_gates.remove(id);
    }

    /// Record that a cold index permanently failed to restore (issue #1106).
    ///
    /// Why: when `restore_fn` returns `false` (blocked volume, deleted
    /// root_path, or panic→false), the entry must be evicted from `entries`
    /// so that (a) `len()` / `indexes_lazy` decrements and stays honest, (b)
    /// the search handler's `cold_store.contains()` returns `false` preventing
    /// it from re-entering the expensive restore path, and (c) callers can
    /// detect the failure via `is_failed()` and return a fast, accurate 503.
    ///
    /// Policy: failure is permanent for the daemon's lifetime. If the underlying
    /// cause is transient (e.g. a volume that was temporarily unmounted), the
    /// operator should restart the daemon or use `POST /indexes` to re-register
    /// the index. This is conservative but safe: it prevents unbounded restore
    /// retry storms on every query.
    ///
    /// What: moves the id from `entries` to `failed_entries`; also removes the
    /// loading gate so it can be reclaimed.
    /// Test: `cold_store_mark_failed_*` and
    ///       `get_or_load_index_restore_false_marks_failed` unit tests.
    pub fn mark_failed(&self, id: &IndexId) {
        self.entries.remove(id);
        self.loading_gates.remove(id);
        self.failed_entries.insert(id.clone(), ());
    }

    /// Acquire or create the per-index loading gate.
    ///
    /// Why: double-checked lock — two concurrent queries for the same cold index
    /// must not both try to restore it simultaneously. The first acquires the
    /// Mutex; the second blocks until the first finishes, then re-checks the hot
    /// registry and returns immediately.
    /// What: inserts a fresh `Arc<Mutex<()>>` if not already present; returns the
    /// existing one otherwise. Returns `None` when the id is not in the cold store.
    /// Test: exercised by concurrent-load tests.
    pub fn loading_gate(&self, id: &IndexId) -> Option<Arc<tokio::sync::Mutex<()>>> {
        if !self.entries.contains_key(id) {
            return None;
        }
        Some(
            self.loading_gates
                .entry(id.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone(),
        )
    }
}
