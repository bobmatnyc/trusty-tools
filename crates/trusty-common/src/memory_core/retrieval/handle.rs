//! `PalaceHandle` — per-palace storage handle with write/read operations.
//!
//! Why: Extracted from retrieval/mod.rs to keep each file under the 500-SLOC
//! cap (#607). Owns the struct definition and all its `impl` methods.
//! What: `PalaceHandle` struct plus `new`, `open`, `flush`, `remember`,
//! `remember_with_options`, `forget`, `list_drawers`, `purge_expired`,
//! `rebuild_closets`, `refresh_l1`, `is_compacting`, `is_read_only`.
//! Test: `registry_create_and_open`, `cli_remember_and_recall`, and
//! the full retrieval test suite in retrieval::tests.

use super::embedder::shared_embedder;
use super::types::{L1_CAP, RememberOptions};
use crate::memory_core::analytics::{RecallEvent, RecallLog, query_hash};
use crate::memory_core::decay::DecayConfig;
use crate::memory_core::dream::extract_keywords;
use crate::memory_core::filter::{FilterReject, classify};
use crate::memory_core::palace::{Drawer, DrawerType, Palace, PalaceId, RoomType};
use crate::memory_core::store::concurrent_open::OpenIntent;
use crate::memory_core::store::kg::KnowledgeGraph;
use crate::memory_core::store::l1_cache::L1Cache;
use crate::memory_core::store::palace_store::PalaceStore;
use crate::memory_core::store::vector::{UsearchStore, VectorStore};
use crate::memory_core::timeouts;
use anyhow::{Context, Result};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use uuid::Uuid;

const RECALL_LOG_FILENAME: &str = "recall.db";

/// Per-palace handle. Cheap to clone (all heavyweight state lives behind `Arc`).
///
/// Why: The registry hands out `Arc<PalaceHandle>` to many concurrent tasks;
/// the handle owns the vector store, KG pool, the in-memory drawer table used
/// by retrieval to map vector hits back to metadata, and the pre-cached L0/L1
/// payloads.
/// What: Bundles `PalaceId`, identity text, an `l1_drawers` Vec (top-15 by
/// importance), `Arc<UsearchStore>`, `Arc<KnowledgeGraph>`, and an
/// `Arc<RwLock<Vec<Drawer>>>` for the in-memory drawer table.
/// Test: See `l0_l1_always_present` (constructor + cache) and
/// `l2_returns_relevant_drawer` (storage handles wired correctly).
pub struct PalaceHandle {
    pub id: PalaceId,
    pub identity: String,
    pub l1_drawers: Vec<Drawer>,
    pub vector_store: Arc<UsearchStore>,
    pub kg: Arc<KnowledgeGraph>,
    pub drawers: Arc<RwLock<Vec<Drawer>>>,
    /// On-disk data directory for this palace (where palace.json,
    /// identity.txt, l1_cache.json, the usearch index, and the KG redb
    /// store all live). `None` for in-memory tests built via `new`.
    pub data_dir: Option<std::path::PathBuf>,
    /// Temporal decay configuration applied during L2/L3 ranking.
    ///
    /// Why: Old drawers should fade unless refreshed by access; baking the
    /// config into the handle keeps retrieval calls free of extra parameters
    /// while still allowing per-palace overrides later.
    /// What: Defaults to `DecayConfig::default()` (90-day half-life, 0.05 floor).
    /// Test: `decay_applied_in_l2_score` confirms an aged drawer ranks below a
    /// fresh one of identical importance.
    pub decay_config: DecayConfig,
    /// Optional recall analytics log. When `Some`, each `recall` /
    /// `recall_deep` call fires a fire-and-forget event per result (or a
    /// single miss event when the query returned nothing).
    ///
    /// Why: Closes the feedback loop without blocking the request path.
    /// What: `None` by default so existing tests don't need a log directory.
    /// Test: `recall_logs_events_when_log_present` exercises the wiring.
    pub recall_log: Option<Arc<RecallLog>>,
    /// Closet pointer index: keyword -> drawer ids. Rebuilt during dream cycles.
    ///
    /// Why: Closets accelerate L2 by mapping topic keywords to candidate drawer
    /// ids without touching the vector store. The map is updated by
    /// `dream::Dreamer::dream_cycle` via NLP-only tokenization (no LLM calls).
    /// What: `Arc<RwLock<HashMap<String, Vec<Uuid>>>>` so reads can run
    /// concurrently with the (rare) dream-time rebuild.
    /// Test: `dream::tests::closet_refresh_builds_index`.
    pub closets: Arc<RwLock<HashMap<String, Vec<Uuid>>>>,
    /// Set to `true` for the duration of an in-flight `Dreamer::dream_cycle`.
    ///
    /// Why: The operator dashboard surfaces a per-palace "compacting / dreaming"
    /// spinner so writers can see when consolidation is active. A shared
    /// `AtomicBool` is the cheapest cross-task signal — readers (HTTP handlers)
    /// poll it with `Relaxed` ordering and writers (the dream loop) flip it on
    /// entry / exit via a guard so panics don't strand the flag.
    /// What: `Arc<AtomicBool>` initialised to `false`. Flipped by
    /// `CompactionGuard::new` (defined in `dream.rs`) at the start of every
    /// `dream_cycle` and cleared on drop.
    /// Test: `dream::tests::dream_cycle_toggles_is_compacting`.
    pub is_compacting: Arc<AtomicBool>,
    /// Serialises mutating ops (`remember_with_options`, `forget`) on this
    /// palace so concurrent writers don't race on the L1 snapshot rename,
    /// the vector store upsert, the KG redb record insert, or the in-memory
    /// drawer table.
    ///
    /// Why: Issue #154 — under 20 concurrent HTTP `memory_remember` calls,
    /// 30–60 % failed with "save L1 snapshot: io error … No such file or
    /// directory". The root cause was multiple writers racing on the same
    /// `l1_cache.json.tmp` file (fixed defensively in `L1Cache`), but the
    /// broader hazard is that the `remember_with_options` pipeline
    /// (embed → vector upsert → KG upsert → in-memory push → L1 snapshot)
    /// has no per-palace ordering guarantee. A per-palace mutex serialises
    /// those steps so the L1 snapshot always reflects a consistent
    /// drawer-table state, without blocking reads or cross-palace writes.
    /// What: `Arc<tokio::sync::Mutex<()>>`. Held only by the mutating
    /// methods; readers (`recall`, `recall_deep`, `list_drawers`) never
    /// touch it. Per-palace, not global, so distinct palaces still write
    /// in parallel. Held across `.await` points, so we use the tokio mutex
    /// rather than `parking_lot::Mutex` (which would deadlock the runtime).
    /// Test: `remember_concurrent_does_not_lose_writes` in this module.
    pub write_mutex: Arc<tokio::sync::Mutex<()>>,
}

impl PalaceHandle {
    /// Read the current compaction flag without acquiring a lock.
    ///
    /// Why: HTTP handlers that build `PalaceInfo` responses need the live
    /// compaction status without taking any lock that the dream cycle holds;
    /// a cheap `load(Relaxed)` keeps the path contention-free.
    /// What: Returns the current value of `is_compacting`.
    /// Test: `dream::tests::dream_cycle_toggles_is_compacting`.
    pub fn is_compacting(&self) -> bool {
        self.is_compacting.load(Ordering::Relaxed)
    }

    /// Whether this palace handle was opened against read-only snapshots of
    /// its underlying redb files.
    ///
    /// Why: Issue #59 — when the HTTP daemon already holds the exclusive
    /// `flock` on a palace's `kg.redb` and `index.usearch.redb`, a stdio
    /// MCP client falls back to per-process snapshot copies so it can
    /// still serve `recall`, `kg_query`, and `palace_info`. Write surfaces
    /// (`remember`, `forget`, `kg_assert`, dream compaction) consult this
    /// flag and return a clear "writes go through the HTTP daemon" error
    /// instead of mutating the throw-away snapshot.
    /// What: Returns `true` when either the KG store or the vector store
    /// reports it is operating against a snapshot.
    /// Test: `palace_handle_read_only_when_locked_by_another_process`.
    pub fn is_read_only(&self) -> bool {
        self.kg.is_read_only() || self.vector_store.is_read_only()
    }

    /// Return a clone of the write mutex for use in tests.
    ///
    /// Why: Tests that simulate a held write lock need access to the mutex so
    /// they can acquire it in a background task before calling `remember`.
    /// Exposing a test-only accessor rather than poking the field directly
    /// keeps the field visibility flexible and makes intent explicit.
    /// What: Returns `Arc::clone(&self.write_mutex)`.
    /// Test: `write_lock_timeout_returns_error_when_held` in `timeout_tests`.
    #[cfg(test)]
    pub fn write_mutex_for_test(&self) -> Arc<tokio::sync::Mutex<()>> {
        Arc::clone(&self.write_mutex)
    }

    /// Construct a new `PalaceHandle` with empty drawer table and L1 cache.
    ///
    /// Why: The registry creates handles eagerly when a palace is opened; the
    /// drawer table and L1 cache are populated incrementally as memories are
    /// loaded or written.
    /// What: Wraps the storage handles in `Arc`s and initializes the drawer
    /// table and L1 cache to empty.
    /// Test: `make_handle` in tests round-trips through this constructor.
    pub fn new(
        id: PalaceId,
        identity: String,
        vector_store: UsearchStore,
        kg: KnowledgeGraph,
    ) -> Self {
        Self {
            id,
            identity,
            l1_drawers: Vec::new(),
            vector_store: Arc::new(vector_store),
            kg: Arc::new(kg),
            drawers: Arc::new(RwLock::new(Vec::new())),
            data_dir: None,
            decay_config: DecayConfig::default(),
            recall_log: None,
            closets: Arc::new(RwLock::new(HashMap::new())),
            is_compacting: Arc::new(AtomicBool::new(false)),
            write_mutex: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// Open a palace from disk, hydrating identity.txt, the L1 snapshot, the
    /// vector index, and the KG.
    ///
    /// Why: A long-lived daemon must reconstruct a palace from its on-disk
    /// state every time the registry is asked for one that isn't yet loaded.
    /// What: Creates the data directory if missing, loads identity.txt
    /// (defaulting to empty), loads the L1 snapshot (defaulting to empty),
    /// opens the usearch index at `<data_dir>/index.usearch` (384-d), and
    /// opens the KG redb store at `<data_dir>/kg.redb`. The drawer table is
    /// initialized from the L1 snapshot (the L1 cache is the only
    /// authoritative drawer metadata until the full drawer table is
    /// persisted in a follow-up issue).
    /// Test: `registry_create_and_open` creates a palace, drops the registry,
    /// and re-opens it.
    pub fn open(palace: &Palace) -> Result<Arc<PalaceHandle>> {
        Self::open_with_intent(palace, OpenIntent::ReadOnlyClient)
    }

    /// Open a palace from disk with the caller's redb open intent.
    ///
    /// Why (issue #1487): the HTTP daemon is the sole writer and must open its
    /// palaces with [`OpenIntent::Writer`] so a second daemon instance fails
    /// loud (after the bounded handoff window) instead of silently opening a
    /// read-only snapshot and rejecting every write for its lifetime. CLI,
    /// stdio MCP, and test callers keep [`OpenIntent::ReadOnlyClient`] so they
    /// can still serve reads from a snapshot while the daemon holds the lock.
    /// What: Same hydration pipeline as [`PalaceHandle::open`], but passes
    /// `intent` down to `UsearchStore::new_with_intent` and
    /// `KnowledgeGraph::open_with_intent`. When `Writer` and a second live
    /// daemon already holds the lock, this returns `Err` (no handle), so the
    /// daemon process fails to start rather than serving in a broken state.
    /// Test: `registry_create_and_open` (default read-only path) and
    /// `writer_intent_open_fails_loud_on_locked_palace` in the store tests.
    pub fn open_with_intent(palace: &Palace, intent: OpenIntent) -> Result<Arc<PalaceHandle>> {
        let data_dir = &palace.data_dir;
        std::fs::create_dir_all(data_dir)
            .with_context(|| format!("create palace data dir {}", data_dir.display()))?;

        let identity = PalaceStore::load_identity(data_dir)
            .with_context(|| format!("load identity for {}", palace.id))?
            .unwrap_or_default();

        let l1_drawers = L1Cache::load_l1_cache(data_dir)
            .with_context(|| format!("load L1 cache for {}", palace.id))?;

        let vector_path = data_dir.join("index.usearch");
        let vector_store = UsearchStore::new_with_intent(vector_path, 384, intent)
            .with_context(|| format!("open vector store for {}", palace.id))?;

        let kg_path = data_dir.join("kg.db");
        let kg = KnowledgeGraph::open_with_intent(&kg_path, intent)
            .with_context(|| format!("open KG for {}", palace.id))?;

        // Load full drawer table from redb (the persistent source of truth).
        // Fall back to an empty list on error so a corrupt table doesn't make
        // the palace unopenable — the L1 snapshot still provides essentials.
        let persisted_drawers = match kg.load_drawers() {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(palace = %palace.id, "load_drawers failed, falling back to L1 only: {e:#}");
                Vec::new()
            }
        };

        // Merge: persisted is authoritative; L1 snapshot fills gaps for
        // palaces created before drawer persistence existed (issue #32 migration).
        let mut all_drawers = persisted_drawers;
        for l1 in &l1_drawers {
            if !all_drawers.iter().any(|d| d.id == l1.id) {
                all_drawers.push(l1.clone());
            }
        }

        // Issue #61: prune expired session events at open. We delete the
        // persistent row synchronously here (best-effort — failures are
        // logged, never fatal) and drop the entry from the in-memory list
        // so it never participates in recall. Vector tombstones are left
        // for `palace_compact` since dropping them needs an async call.
        let now = chrono::Utc::now();
        let mut pruned = 0usize;
        all_drawers.retain(|d| {
            let expired = d.expires_at.is_some_and(|t| t < now);
            if expired {
                if let Err(e) = kg.delete_drawer_sync(d.id) {
                    tracing::warn!(
                        palace = %palace.id, id = %d.id,
                        "purge_expired: delete_drawer failed: {e:#}"
                    );
                }
                pruned += 1;
            }
            !expired
        });
        if pruned > 0 {
            tracing::info!(palace = %palace.id, count = pruned, "purged expired drawers at open");
        }

        // Surface orphaned vectors so operators can re-ingest if needed.
        let index_count = vector_store.index_size();
        let drawer_count = all_drawers.len();
        if index_count > drawer_count + 5 {
            tracing::warn!(
                palace = %palace.id,
                index_vectors = index_count,
                drawer_records = drawer_count,
                "vector index has orphaned entries — consider re-ingesting"
            );
        }

        let drawers = Arc::new(RwLock::new(all_drawers));

        // Attach a per-palace RecallLog at <data_dir>/recall.db so every disk-
        // backed palace records hit/miss telemetry by default. A failure to
        // open the log is non-fatal — log a warning and proceed without
        // analytics so the palace remains usable.
        //
        // Why: Issue #53 — the MCP daemon (and CLI) previously opened palaces
        // without a recall log, leaving `analytics show` permanently reporting
        // "not configured". Wiring the log at open-time ensures every consumer
        // of `PalaceRegistry::open_palace` gets logging for free.
        let recall_log = match RecallLog::open(&data_dir.join(RECALL_LOG_FILENAME)) {
            Ok(log) => Some(Arc::new(log)),
            Err(e) => {
                tracing::warn!(palace = %palace.id, "open recall log failed, analytics disabled: {e:#}");
                None
            }
        };

        let handle = PalaceHandle {
            id: palace.id.clone(),
            identity,
            l1_drawers,
            vector_store: Arc::new(vector_store),
            kg: Arc::new(kg),
            drawers,
            data_dir: Some(data_dir.clone()),
            decay_config: DecayConfig::default(),
            recall_log,
            closets: Arc::new(RwLock::new(HashMap::new())),
            is_compacting: Arc::new(AtomicBool::new(false)),
            write_mutex: Arc::new(tokio::sync::Mutex::new(())),
        };
        Ok(Arc::new(handle))
    }

    /// Persist the L1 cache snapshot and identity.txt for this palace.
    ///
    /// Why: Mutating paths (drawer ingest, identity edits) must durably record
    /// state so the next cold start sees up-to-date essentials.
    /// What: Re-sorts the drawer table by importance descending, snapshots
    /// the top-15 to `l1_cache.json`, and re-writes `identity.txt`. No-op when
    /// `data_dir` is `None` (in-memory test handles).
    /// Test: `registry_create_and_open` confirms identity survives a flush+reopen.
    pub fn flush(&self) -> Result<()> {
        let Some(data_dir) = self.data_dir.as_ref() else {
            return Ok(());
        };
        let drawers = self.drawers.read().clone();
        L1Cache::save_l1_cache(&drawers, data_dir)
            .with_context(|| format!("save L1 cache for {}", self.id))?;
        PalaceStore::save_identity(&self.id, &self.identity, data_dir)
            .with_context(|| format!("save identity for {}", self.id))?;
        Ok(())
    }

    /// Attach a recall analytics log to this handle.
    ///
    /// Why: Recall logging is opt-in so simple tests don't need to manage a
    /// redb file; production palaces wire one in at construction time.
    /// What: Builder-style mutator returning `self`.
    /// Test: `recall_logs_events_when_log_present` uses this to enable logging.
    pub fn with_recall_log(mut self, log: Arc<RecallLog>) -> Self {
        self.recall_log = Some(log);
        self
    }

    /// Override the decay configuration for this palace.
    pub fn with_decay_config(mut self, config: DecayConfig) -> Self {
        self.decay_config = config;
        self
    }

    /// Append a drawer to the in-memory drawer table.
    ///
    /// Why: Retrieval needs to map vector hits back to drawer metadata; until
    /// we have a persistent drawer table the in-memory `Vec<Drawer>` is the
    /// source of truth.
    /// What: Acquires the write lock on `drawers` and pushes `drawer`. Caller
    /// is responsible for invoking `refresh_l1` if importance ranking might
    /// have changed.
    /// Test: `l0_l1_always_present` exercises this path.
    pub fn add_drawer(&self, drawer: Drawer) {
        let mut drawers = self.drawers.write();
        drawers.push(drawer);
    }

    /// Rebuild the L1 cache (top-15 drawers by importance, descending).
    ///
    /// Why: L1 is the always-on essential context; we keep it pre-sorted so
    /// reads are constant-time. The L1 cap is small enough that a full re-sort
    /// is cheaper than maintaining a heap.
    /// What: Reads the drawer table, sorts a clone by importance descending,
    /// and stores the first `L1_CAP` entries on `self.l1_drawers`.
    /// Test: `l0_l1_always_present` asserts a high-importance drawer makes it
    /// into L1 after `refresh_l1` is called.
    pub fn refresh_l1(&mut self) {
        let drawers = self.drawers.read();
        let mut sorted: Vec<Drawer> = drawers.clone();
        sorted.sort_by(|a, b| {
            b.importance
                .partial_cmp(&a.importance)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        self.l1_drawers = sorted.into_iter().take(L1_CAP).collect();
    }

    /// Store a new memory: embed, upsert to vector store, append to drawer
    /// table, and persist the L1 snapshot.
    ///
    /// Why: First-class write path for CLI/MCP — keeps the embedding,
    /// vector-store, drawer-table, and L1 snapshot in one transactional unit
    /// so callers don't have to thread the steps themselves.
    /// What: Builds a `Drawer` with a fresh UUID, embeds via `FastEmbedder`,
    /// inserts the vector keyed by the drawer id, pushes onto the in-memory
    /// drawer table, refreshes L1, and flushes the snapshot to disk.
    /// Test: `cli_remember_and_recall` round-trips through this method.
    pub async fn remember(
        &self,
        content: String,
        room: RoomType,
        tags: Vec<String>,
        importance: f32,
    ) -> Result<Uuid> {
        self.remember_with_options(content, room, tags, importance, RememberOptions::default())
            .await
    }

    /// Store a new memory with explicit filter / classification policy.
    ///
    /// Why: Issue #61 — `memory_remember` needs a `force` escape hatch and a
    /// way for `memory_note` to bypass only the token-length gate (keeping
    /// the noise patterns). Hoisting the policy into `RememberOptions` keeps
    /// the surface explicit without forking three near-identical methods.
    /// What: Applies the supplied `FilterConfig` (skipping it entirely when
    /// `force == true`), classifies the content, sets the appropriate TTL
    /// when the result is a `SessionEvent`, then runs the original
    /// embed/upsert/persist pipeline.
    /// Test: `remember_rejects_short_content`,
    /// `remember_force_bypasses_filter`, `remember_classifies_session_events`.
    pub async fn remember_with_options(
        &self,
        content: String,
        room: RoomType,
        tags: Vec<String>,
        importance: f32,
        opts: RememberOptions,
    ) -> Result<Uuid> {
        // Issue #59: short-circuit before doing any embedding work when the
        // palace is opened read-only. The store layer already rejects the
        // eventual write, but returning here saves the cost of an embed
        // and surfaces a single clear error rather than an inscrutable
        // upsert failure stack.
        if self.is_read_only() {
            return Err(anyhow::anyhow!(
                "palace '{}' is read-only: HTTP daemon holds the write lock — \
                 route writes through the daemon's HTTP API or stop the daemon \
                 before retrying via stdio",
                self.id
            ));
        }

        // Issue #154: serialise mutating ops on this palace so concurrent
        // writers don't race on the L1 snapshot rename, vector upsert, KG
        // row insert, or in-memory drawer push. Held across the full
        // pipeline below. Other palaces' writes proceed in parallel.
        // Reads (`recall`, `list_drawers`, etc.) never acquire this lock,
        // so the write mutex doesn't impact read throughput.
        // Issue #906: bound the lock acquisition so a stuck embedder on one
        // writer doesn't cascade an indefinite queue of waiters.
        let _write_guard = timeouts::lock_with_timeout(
            &self.write_mutex,
            timeouts::write_lock_timeout(),
            self.id.as_str(),
        )
        .await?;

        // Issue #61: signal/noise gate. `force == true` bypasses entirely.
        // `enforce_min_tokens` lets `memory_note` keep the noise patterns
        // while permitting short curated facts ("User prefers snake_case").
        if !opts.force {
            opts.filter
                .apply(&content, opts.enforce_min_tokens)
                .map_err(|reject| match reject {
                    // Issue #1481: `PotentialSecret` carries the offending token
                    // in its Display impl, so the same `{reject}` bubble names
                    // the trigger for the caller to remediate.
                    FilterReject::TooShort { .. }
                    | FilterReject::NoisePattern { .. }
                    | FilterReject::NonAlphabetic { .. }
                    | FilterReject::PotentialSecret { .. } => anyhow::anyhow!("{reject}"),
                })?;
        }

        // Encode RoomType into the room_id deterministically by hashing the
        // debug repr. Until we wire a real Room table, this keeps the room
        // signal recoverable for `list_drawers` filtering.
        let room_id = super::layers::room_to_uuid(&room);

        let mut drawer = Drawer::new(room_id, content.clone());
        drawer.tags = tags;
        drawer.importance = importance.clamp(0.0, 1.0);
        // Apply classification. The caller may pre-pin the type
        // (`memory_note` always pins `UserFact`); otherwise we run the
        // heuristic classifier with `Unknown` as the fallback so
        // unclassified prose stays unlabelled rather than getting tagged
        // as `SessionEvent` by accident.
        let final_type = match opts.classify_as {
            Some(t) => t,
            None => classify(&content, DrawerType::Unknown),
        };
        drawer = drawer.with_type(final_type);
        let id = drawer.id;

        // Embed and upsert. Use the process-wide shared embedder so we don't
        // spin up a fresh ONNX session per call (issue #57). The
        // OnceCell-backed `shared_embedder` guarantees at most one model load
        // for the lifetime of the process.
        //
        // Issue #1970: a caller whose daemon is still warming up (embedder
        // cold-init in progress) sets `opts.defer_embedding` so this write
        // returns as soon as the KG/redb portion below completes instead of
        // blocking behind a 30-120s ONNX/CoreML compile — the vector is
        // backfilled by a background task once the embedder resolves. Text
        // and KG indexing never depend on the embedder either way; this
        // branch only changes *when* the drawer becomes vector-searchable.
        if opts.defer_embedding {
            self.spawn_deferred_embed(id, content.clone());
        } else {
            // Issue #906: both `shared_embedder()` (cold init path) and
            // `embed_batch` carry their own bounded timeouts — if the embedder
            // hangs mid-batch the remember call returns an error instead of
            // blocking the write-lock indefinitely.
            let embedder = shared_embedder()
                .await
                .context("acquire shared embedder for remember")?;
            let embed_timeout = timeouts::embed_batch_timeout();
            let vecs = tokio::time::timeout(embed_timeout, embedder.embed_batch(&[content]))
                .await
                .map_err(|_| {
                    anyhow::anyhow!(
                        "embed_batch timed out after {:?} on remember path (issue #906); \
                         increase TRUSTY_EMBED_BATCH_TIMEOUT_SECS if batches legitimately \
                         take longer on this host",
                        embed_timeout
                    )
                })?
                .context("embed drawer content")?;
            if let Some(v) = vecs.into_iter().next() {
                self.vector_store
                    .upsert(id, v)
                    .await
                    .context("upsert drawer vector")?;
            }
        }

        // Persist drawer metadata BEFORE the in-memory push so a crash mid-op
        // cannot leave an in-memory drawer with no redb record backing it.
        self.kg
            .upsert_drawer(&drawer)
            .await
            .context("persist drawer metadata")?;

        {
            let mut drawers = self.drawers.write();
            drawers.push(drawer);
        }

        // L1 snapshot: re-sort the in-memory table and persist top-15.
        if let Some(data_dir) = self.data_dir.as_ref() {
            let snap = self.drawers.read().clone();
            L1Cache::save_l1_cache(&snap, data_dir).context("save L1 snapshot")?;
        }

        // Refresh the closet keyword index so L2 tag-boosting picks up the
        // new drawer without waiting for a dream cycle.
        self.rebuild_closets();

        Ok(id)
    }

    /// Fire-and-forget background embed + vector-store backfill for a
    /// drawer written while the shared embedder was cold (issue #1970).
    ///
    /// Why: `memory_remember` / `memory_note` / `task_add` must not block
    /// the caller behind a 30-120s CoreML/CUDA cold compile just to persist
    /// a memory — the KG/redb write already completed synchronously by the
    /// time this is called. This task's only job is to make the drawer's
    /// vector show up in `retrieve_l2` / `retrieve_l3` once the shared
    /// embedder resolves.
    /// What: clones the `Arc<UsearchStore>` handle and spawns a detached
    /// task that awaits `shared_embedder()` (retrying the cold init if
    /// necessary), embeds `content` under the same bounded timeout the
    /// synchronous path uses, and upserts the resulting vector under `id`.
    /// Every failure mode (embedder init failure, embed timeout, upsert
    /// error) is logged at `warn!` and dropped — the drawer is already
    /// durable via KG/redb regardless of whether the vector backfill
    /// succeeds.
    /// Known limitation: if the drawer is forgotten before this task
    /// completes, the backfilled vector becomes an orphaned entry in the
    /// vector store. Acceptable for a best-effort background lane (mirrors
    /// the existing best-effort tone of BM25 indexing and auto-KG
    /// extraction elsewhere in this pipeline); tighten with an
    /// existence check against `self.drawers` if this proves to matter in
    /// practice.
    /// Test: `deferred_embed_backfills_vector_once_embedder_ready` in
    /// `retrieval::tests`.
    fn spawn_deferred_embed(&self, id: Uuid, content: String) {
        let vector_store = self.vector_store.clone();
        let palace_id = self.id.clone();
        tokio::spawn(async move {
            let embedder = match shared_embedder().await {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!(
                        palace = %palace_id,
                        drawer = %id,
                        "deferred embed: shared embedder init failed (vector left unindexed): {e:#}"
                    );
                    return;
                }
            };
            let embed_timeout = timeouts::embed_batch_timeout();
            let vecs =
                match tokio::time::timeout(embed_timeout, embedder.embed_batch(&[content])).await {
                    Ok(Ok(v)) => v,
                    Ok(Err(e)) => {
                        tracing::warn!(
                            palace = %palace_id,
                            drawer = %id,
                            "deferred embed: embed_batch failed: {e:#}"
                        );
                        return;
                    }
                    Err(_) => {
                        tracing::warn!(
                            palace = %palace_id,
                            drawer = %id,
                            "deferred embed: embed_batch timed out after {embed_timeout:?}"
                        );
                        return;
                    }
                };
            if let Some(v) = vecs.into_iter().next() {
                match vector_store.upsert(id, v).await {
                    Ok(()) => tracing::info!(
                        palace = %palace_id,
                        drawer = %id,
                        "deferred embed: vector backfill complete"
                    ),
                    Err(e) => tracing::warn!(
                        palace = %palace_id,
                        drawer = %id,
                        "deferred embed: vector upsert failed: {e:#}"
                    ),
                }
            }
        });
    }

    /// Rebuild the closet keyword index from the current in-memory drawer table.
    ///
    /// Why: Keep the closet index current after every write so L2 tag-boosting
    /// works without waiting for a dream cycle.
    /// What: Rebuilds keyword -> Vec<drawer_id> map by tokenizing each drawer's
    /// content via `extract_keywords` (whitespace + stop-word filter).
    /// Test: `closet_updated_after_remember`.
    pub fn rebuild_closets(&self) {
        let snapshot: Vec<Drawer> = self.drawers.read().clone();
        let mut new_index: HashMap<String, Vec<Uuid>> = HashMap::new();
        for drawer in snapshot.iter() {
            for kw in extract_keywords(&drawer.content) {
                new_index.entry(kw).or_default().push(drawer.id);
            }
        }
        let mut closets = self.closets.write();
        *closets = new_index;
    }

    /// Remove a drawer by id.
    ///
    /// Why: Surface forget as a first-class op so CLI/MCP can drop stale data
    /// without leaking vectors in the HNSW index.
    /// What: Removes the vector from the vector store and drops the matching
    /// row from the in-memory drawer table. Persists the L1 snapshot afterward
    /// so the drop survives a restart.
    /// Test: `cli_forget_removes_drawer` asserts a recalled drawer disappears
    /// after forget.
    pub async fn forget(&self, id: Uuid) -> Result<()> {
        // Issue #59: short-circuit read-only handles so callers get a
        // clean error instead of two best-effort warnings followed by a
        // misleading "ok".
        if self.is_read_only() {
            return Err(anyhow::anyhow!(
                "palace '{}' is read-only: HTTP daemon holds the write lock — \
                 route forget through the daemon's HTTP API or stop the daemon \
                 before retrying via stdio",
                self.id
            ));
        }

        // Issue #154: serialise with concurrent `remember_with_options`
        // calls so the L1 snapshot rewritten below sees a consistent
        // drawer-table state and so the vector / KG / in-memory removals
        // can't interleave with an append. See `write_mutex` docs on
        // `PalaceHandle`.
        // Issue #906: bound the lock acquisition to avoid cascading hangs.
        let _write_guard = timeouts::lock_with_timeout(
            &self.write_mutex,
            timeouts::write_lock_timeout(),
            self.id.as_str(),
        )
        .await?;

        // Best-effort vector removal — usearch may legitimately not have the
        // key (e.g. if remember failed mid-flight); we propagate other errors.
        if let Err(e) = self.vector_store.remove(id).await {
            tracing::warn!(?id, "vector remove failed: {e:#}");
        }

        // Drop persistent metadata alongside the vector so cold restart
        // doesn't resurrect this drawer (issue #32).
        if let Err(e) = self.kg.delete_drawer(id).await {
            tracing::warn!(?id, "drawer metadata delete failed: {e:#}");
        }

        // Issue #278 (cascade-delete): remove all KG triples whose subject is
        // `drawer:<id>` — these are auto-extracted facts whose source drawer no
        // longer exists. Failure is best-effort (warn, don't abort the forget).
        if let Err(e) = self.kg.cascade_delete_by_drawer(id).await {
            tracing::warn!(?id, "kg cascade_delete_by_drawer failed: {e:#}");
        }

        {
            let mut drawers = self.drawers.write();
            drawers.retain(|d| d.id != id);
        }

        if let Some(data_dir) = self.data_dir.as_ref() {
            let snap = self.drawers.read().clone();
            L1Cache::save_l1_cache(&snap, data_dir).context("save L1 snapshot after forget")?;
        }

        Ok(())
    }

    /// List drawers with optional room/tag filters, sorted by importance desc.
    ///
    /// Why: CLI `list` and MCP introspection need a uniform read view over the
    /// in-memory drawer table without exposing the lock semantics.
    /// What: Snapshots the drawer table, applies filters, sorts by importance
    /// descending, and truncates to `limit`.
    /// Test: `cli_list_filters_by_room` writes drawers in distinct rooms and
    /// asserts the room filter narrows the list.
    pub fn list_drawers(
        &self,
        room: Option<RoomType>,
        tag: Option<String>,
        limit: usize,
    ) -> Vec<Drawer> {
        let drawers = self.drawers.read();
        let target_room_id = room.as_ref().map(super::layers::room_to_uuid);
        let mut filtered: Vec<Drawer> = drawers
            .iter()
            .filter(|d| match &target_room_id {
                Some(rid) => d.room_id == *rid,
                None => true,
            })
            .filter(|d| match &tag {
                Some(t) => d.tags.iter().any(|x| x == t),
                None => true,
            })
            .cloned()
            .collect();
        drop(drawers);
        filtered.sort_by(|a, b| {
            b.importance
                .partial_cmp(&a.importance)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        filtered.truncate(limit);
        filtered
    }

    /// Drop every drawer whose `expires_at` has fallen into the past.
    ///
    /// Why: Issue #61 — `SessionEvent` drawers carry a 7-day TTL so palaces
    /// don't permanently accumulate auto-capture noise. The sweep runs on
    /// palace open (and may be invoked by future dream cycles); failures
    /// are best-effort so a half-pruned palace still serves recalls.
    /// What: Snapshots the drawer table, collects ids whose `expires_at`
    /// is in the past, and routes each through `forget` so the vector
    /// index and persistent metadata stay in sync. Returns the number of
    /// drawers pruned. No-op on read-only handles.
    /// Test: `purge_expired_drops_only_past_ttl`.
    pub async fn purge_expired(&self) -> Result<usize> {
        if self.is_read_only() {
            return Ok(0);
        }
        let now = chrono::Utc::now();
        let expired_ids: Vec<Uuid> = self
            .drawers
            .read()
            .iter()
            .filter(|d| d.expires_at.is_some_and(|t| t < now))
            .map(|d| d.id)
            .collect();
        let count = expired_ids.len();
        for id in expired_ids {
            if let Err(e) = self.forget(id).await {
                tracing::warn!(?id, "purge_expired: forget failed: {e:#}");
            }
        }
        if count > 0 {
            tracing::info!(palace = %self.id, count, "purged expired drawers");
        }
        Ok(count)
    }

    /// Fire-and-forget recall analytics.
    ///
    /// Why: Hit/miss telemetry must never block the request path; spawning a task
    /// keeps logging off the critical path while still capturing every event.
    /// What: If `handle.recall_log` is set, spawns a task that records one event
    /// per non-L0 result, or a single miss event when `results` only contains the
    /// L0 identity (no real recall hits).
    /// Test: `recall_logs_events_when_log_present` confirms the log row appears.
    pub(super) fn log_recall(&self, query: &str, results: &[super::types::RecallResult]) {
        let Some(log) = self.recall_log.clone() else {
            return;
        };
        let palace_id = self.id.as_str().to_string();
        let q_hash = query_hash(query);
        // Only count L1+ entries — the synthetic L0 identity is always present
        // and would otherwise drown out genuine miss signals.
        let logged: Vec<super::types::RecallResult> =
            results.iter().filter(|r| r.layer > 0).cloned().collect();

        tokio::spawn(async move {
            let now = chrono::Utc::now();
            if logged.is_empty() {
                let _ = log
                    .record(RecallEvent {
                        palace_id,
                        query_hash: q_hash,
                        layer: 3,
                        drawer_id: None,
                        score: 0.0,
                        occurred_at: now,
                    })
                    .await;
            } else {
                for r in &logged {
                    let _ = log
                        .record(RecallEvent {
                            palace_id: palace_id.clone(),
                            query_hash: q_hash,
                            layer: r.layer,
                            drawer_id: Some(r.drawer.id),
                            score: r.score,
                            occurred_at: now,
                        })
                        .await;
                }
            }
        });
    }
}
