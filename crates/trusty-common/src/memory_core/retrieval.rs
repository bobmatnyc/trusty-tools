//! 4-layer progressive retrieval: L0 (identity) -> L1 (essential) ->
//! L2 (on-demand vector) -> L3 (deep search).
//!
//! Why: LLM context windows are precious. Always loading L0+L1 (~900 tokens)
//! gives the agent baseline grounding; L2/L3 are paid only when the query
//! demands them. This dramatically improves cost and latency vs. dumping the
//! whole memory store into context.
//! What: Layer types, async loaders, and the canonical `PalaceHandle` that
//! owns the per-palace storage handles plus pre-cached L0/L1.
//! Test: `cargo test -p trusty-memory-core retrieval::` exercises L0/L1 cache
//! and L2 vector retrieval end-to-end.

use crate::memory_core::analytics::{RecallEvent, RecallLog, query_hash};
const RECALL_LOG_FILENAME: &str = "recall.db";
use crate::memory_core::decay::DecayConfig;
use crate::memory_core::dream::extract_keywords;
use crate::memory_core::embed::{Embedder, FastEmbedder};
use crate::memory_core::filter::{FilterConfig, FilterReject, classify};
use crate::memory_core::palace::{Drawer, DrawerType, Palace, PalaceId, RoomType};
use crate::memory_core::store::kg::KnowledgeGraph;
use crate::memory_core::store::l1_cache::L1Cache;
use crate::memory_core::store::palace_store::PalaceStore;
use crate::memory_core::store::vector::{UsearchStore, VectorStore};
use anyhow::{Context, Result};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::OnceCell;
use uuid::Uuid;

/// Process-wide shared FastEmbedder.
///
/// Why: `FastEmbedder::new()` loads a ~90 MB ONNX session — creating one per
/// call (as the previous `recall_with_default_embedder` / `remember` /
/// dream `dedup_pass` did) blew memory to multiple GB and forked dozens of
/// model instances. Issue #57.
/// What: A `tokio::sync::OnceCell` initialised on first use and shared by every
/// caller that lacks a context-supplied embedder. Concurrent first-use races
/// collapse to a single load.
/// Test: `shared_embedder_is_singleton` confirms two calls return the same
/// `Arc` pointer.
static SHARED_EMBEDDER: OnceCell<Arc<FastEmbedder>> = OnceCell::const_new();

/// Resolve (or initialise) the process-wide shared `FastEmbedder`.
///
/// Why: Centralising fallback embedder construction guarantees at most one
/// ONNX session per process — critical for the daemon footprint (issue #57).
/// What: Returns a clone of the shared `Arc<FastEmbedder>`, initialising it
/// on first call. Errors propagate from the underlying ONNX load.
/// Test: `shared_embedder_is_singleton`.
pub async fn shared_embedder() -> Result<Arc<FastEmbedder>> {
    SHARED_EMBEDDER
        .get_or_try_init(|| async {
            let e = FastEmbedder::new()
                .await
                .context("init shared FastEmbedder")?;
            Ok::<Arc<FastEmbedder>, anyhow::Error>(Arc::new(e))
        })
        .await
        .cloned()
}

/// L0 — palace identity. Tiny (~100 tokens), always loaded, read from
/// `<data_dir>/identity.txt` on palace open.
pub struct L0Identity {
    pub content: String,
}

/// L1 — essential drawers (top-15 by importance, ~800 tokens), pre-computed
/// at write time and cached on the `PalaceHandle`.
pub struct L1Essential {
    pub drawers: Vec<Drawer>,
}

/// A single ranked memory result produced by any retrieval layer.
///
/// Why: All four layers need to produce a comparable, layer-tagged result so
/// callers can stitch them together and present consistent context to the LLM.
/// What: Bundles the matched drawer with an effective score (importance times
/// vector similarity for L2/L3, importance for L1, fixed 1.0 for L0) and the
/// originating layer index.
/// Test: See `l0_l1_always_present` and `l2_returns_relevant_drawer`.
#[derive(Debug, Clone)]
pub struct RecallResult {
    pub drawer: Drawer,
    pub score: f32,
    pub layer: u8,
}

/// Maximum number of drawers held in the L1 cache.
const L1_CAP: usize = 15;

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
    /// identity.txt, l1_cache.json, the usearch index, and the KG SQLite
    /// file all live). `None` for in-memory tests built via `new`.
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
    /// the vector store upsert, the KG SQLite row insert, or the in-memory
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

/// Options for `PalaceHandle::remember_with_options` (issue #61).
///
/// Why: The signal/noise gate, the curated-fact escape hatch (`memory_note`),
/// and the unconditional `force` override all share the same write pipeline.
/// Bundling them lets future knobs (e.g. per-call decay overrides) attach
/// without breaking the call surface again.
/// What: `filter` defaults to `FilterConfig::default()`; `force` skips the
/// gate entirely; `enforce_min_tokens` lets `memory_note` keep noise rejects
/// while accepting short content; `classify_as` pins the resulting
/// `DrawerType` (used by `memory_note` to force `UserFact`).
/// Test: See `remember_force_bypasses_filter` and friends in this file.
#[derive(Debug, Clone)]
pub struct RememberOptions {
    pub filter: FilterConfig,
    pub force: bool,
    pub enforce_min_tokens: bool,
    pub classify_as: Option<DrawerType>,
}

impl Default for RememberOptions {
    fn default() -> Self {
        Self {
            filter: FilterConfig::default(),
            force: false,
            enforce_min_tokens: true,
            classify_as: None,
        }
    }
}

impl RememberOptions {
    /// Preset for the `memory_note` curated-fact path.
    ///
    /// Why: `memory_note` stores short, high-signal facts ("User prefers
    /// snake_case") that would otherwise trip the token threshold. The
    /// noise-pattern rejects still apply so the tool can't be used to
    /// silently store auto-capture garbage.
    /// What: Disables `enforce_min_tokens` and pins `classify_as =
    /// UserFact`. Leaves `filter` at the default so noise patterns still
    /// reject.
    /// Test: `note_options_skip_token_check_but_keep_noise_filter`.
    pub fn note() -> Self {
        Self {
            filter: FilterConfig::default(),
            force: false,
            enforce_min_tokens: false,
            classify_as: Some(DrawerType::UserFact),
        }
    }

    /// Preset that bypasses every filter (the `force = true` MCP arg).
    pub fn forced() -> Self {
        Self {
            force: true,
            ..Self::default()
        }
    }
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
    /// opens the KG SQLite at `<data_dir>/kg.db`. The drawer table is
    /// initialized from the L1 snapshot (the L1 cache is the only
    /// authoritative drawer metadata until the full drawer table is
    /// persisted in a follow-up issue).
    /// Test: `registry_create_and_open` creates a palace, drops the registry,
    /// and re-opens it.
    pub fn open(palace: &Palace) -> Result<Arc<PalaceHandle>> {
        let data_dir = &palace.data_dir;
        std::fs::create_dir_all(data_dir)
            .with_context(|| format!("create palace data dir {}", data_dir.display()))?;

        let identity = PalaceStore::load_identity(data_dir)
            .with_context(|| format!("load identity for {}", palace.id))?
            .unwrap_or_default();

        let l1_drawers = L1Cache::load_l1_cache(data_dir)
            .with_context(|| format!("load L1 cache for {}", palace.id))?;

        let vector_path = data_dir.join("index.usearch");
        let vector_store = UsearchStore::new(vector_path, 384)
            .with_context(|| format!("open vector store for {}", palace.id))?;

        let kg_path = data_dir.join("kg.db");
        let kg =
            KnowledgeGraph::open(&kg_path).with_context(|| format!("open KG for {}", palace.id))?;

        // Load full drawer table from SQLite (the persistent source of truth).
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
    /// SQLite file; production palaces wire one in at construction time.
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
        let _write_guard = self.write_mutex.lock().await;

        // Issue #61: signal/noise gate. `force == true` bypasses entirely.
        // `enforce_min_tokens` lets `memory_note` keep the noise patterns
        // while permitting short curated facts ("User prefers snake_case").
        if !opts.force {
            opts.filter
                .apply(&content, opts.enforce_min_tokens)
                .map_err(|reject| match reject {
                    FilterReject::TooShort { .. }
                    | FilterReject::NoisePattern { .. }
                    | FilterReject::NonAlphabetic { .. } => anyhow::anyhow!("{reject}"),
                })?;
        }

        // Encode RoomType into the room_id deterministically by hashing the
        // debug repr. Until we wire a real Room table, this keeps the room
        // signal recoverable for `list_drawers` filtering.
        let room_id = room_to_uuid(&room);

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
        let embedder = shared_embedder()
            .await
            .context("acquire shared embedder for remember")?;
        let vecs = embedder
            .embed_batch(&[content])
            .await
            .context("embed drawer content")?;
        if let Some(v) = vecs.into_iter().next() {
            self.vector_store
                .upsert(id, v)
                .await
                .context("upsert drawer vector")?;
        }

        // Persist drawer metadata BEFORE the in-memory push so a crash mid-op
        // cannot leave an in-memory drawer with no SQLite row backing it.
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
        let _write_guard = self.write_mutex.lock().await;

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
        let target_room_id = room.as_ref().map(room_to_uuid);
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
}

/// Recall via the L0+L1+L2 path with the per-call `FastEmbedder`.
///
/// Why: CLI/MCP often want a one-shot "recall" without managing an embedder
/// handle; this convenience binds the embedder lifecycle to the call.
/// What: Initializes a `FastEmbedder` (which warms on first run), then
/// delegates to `recall`.
/// Test: `cli_remember_and_recall` integration test.
pub async fn recall_with_default_embedder(
    handle: &PalaceHandle,
    query: &str,
    top_k: usize,
) -> Result<Vec<RecallResult>> {
    let embedder = shared_embedder()
        .await
        .context("acquire shared embedder for recall")?;
    recall(handle, embedder.as_ref(), query, top_k).await
}

/// Deep recall with the shared `FastEmbedder` (issue #57).
pub async fn recall_deep_with_default_embedder(
    handle: &PalaceHandle,
    query: &str,
    top_k: usize,
) -> Result<Vec<RecallResult>> {
    let embedder = shared_embedder()
        .await
        .context("acquire shared embedder for recall_deep")?;
    recall_deep(handle, embedder.as_ref(), query, top_k).await
}

/// A cross-palace recall result, tagging each ranked drawer with its source
/// palace id so callers can attribute hits back to a namespace.
///
/// Why: When agents fan out a query across every palace on the machine, the
/// raw `RecallResult` loses the namespace signal — without the palace id the
/// caller cannot decide which palace a fact lives in. Wrapping rather than
/// extending `RecallResult` keeps single-palace call sites untouched.
/// What: Bundles the originating `palace_id` (kebab-case string) with the
/// underlying `RecallResult`.
/// Test: `recall_across_palaces_merges_results` asserts both palace ids appear
/// in the merged output.
#[derive(Debug, Clone)]
pub struct CrossPalaceResult {
    pub palace_id: String,
    pub result: RecallResult,
}

/// Fan out a recall across every palace handle and merge the results.
///
/// Why: Agents often want the most relevant memories regardless of which palace
/// they are stored in. This function fans out a single query across every open
/// palace handle, merges the results, deduplicates by drawer id, and re-ranks
/// by score descending.
/// What: For each palace handle in `handles`, runs `recall` (L0+L1+L2) or
/// `recall_deep` (L0+L1+L3) depending on `deep`, concurrently via
/// `futures::future::join_all`. Errors from individual palaces are logged via
/// `tracing::warn!` and skipped (not fatal). The merged list is deduplicated
/// by `result.drawer.id` (highest score wins on collision), sorted by
/// `result.score` descending, then truncated to `top_k`.
/// Test: `recall_across_palaces_merges_results` verifies results from two
/// palaces appear in the combined output.
pub async fn recall_across_palaces(
    handles: &[Arc<PalaceHandle>],
    embedder: &Arc<dyn Embedder + Send + Sync>,
    query: &str,
    top_k: usize,
    deep: bool,
) -> Result<Vec<CrossPalaceResult>> {
    if handles.is_empty() || top_k == 0 {
        return Ok(Vec::new());
    }

    // Fan out concurrently. Each future returns (palace_id, Result<Vec<...>>);
    // we keep the palace id alongside the result so failures can be logged
    // with the right context.
    let mut futures = Vec::with_capacity(handles.len());
    for handle in handles {
        let palace_id = handle.id.as_str().to_string();
        let handle = handle.clone();
        let embedder = embedder.clone();
        let query = query.to_string();
        futures.push(async move {
            let result = if deep {
                recall_deep(&handle, embedder.as_ref(), &query, top_k).await
            } else {
                recall(&handle, embedder.as_ref(), &query, top_k).await
            };
            (palace_id, result)
        });
    }

    let outcomes = futures::future::join_all(futures).await;

    // Deduplicate by drawer id — keep the highest-scoring occurrence. We index
    // into `merged` via a parallel `HashMap<Uuid, usize>` so we can mutate the
    // chosen entry in place when a higher-scoring duplicate arrives.
    let mut merged: Vec<CrossPalaceResult> = Vec::new();
    let mut by_drawer: HashMap<Uuid, usize> = HashMap::new();

    for (palace_id, outcome) in outcomes {
        match outcome {
            Ok(hits) => {
                for r in hits {
                    let drawer_id = r.drawer.id;
                    let candidate = CrossPalaceResult {
                        palace_id: palace_id.clone(),
                        result: r,
                    };
                    match by_drawer.get(&drawer_id).copied() {
                        Some(idx) if merged[idx].result.score >= candidate.result.score => {
                            // Existing entry wins; drop the candidate.
                        }
                        Some(idx) => {
                            merged[idx] = candidate;
                        }
                        None => {
                            by_drawer.insert(drawer_id, merged.len());
                            merged.push(candidate);
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!(palace = %palace_id, "recall_across_palaces: skipping palace: {e:#}");
            }
        }
    }

    merged.sort_by(|a, b| {
        b.result
            .score
            .partial_cmp(&a.result.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    merged.truncate(top_k);
    Ok(merged)
}

/// Convenience wrapper for `recall_across_palaces` using the process-wide
/// shared `FastEmbedder`.
///
/// Why: CLI / MCP / HTTP entry points should not have to thread an embedder
/// through every call; the shared singleton (issue #57) is the right default
/// for cross-palace fan-out too.
/// What: Resolves `shared_embedder()`, erases it to `Arc<dyn Embedder + Send +
/// Sync>`, and delegates to `recall_across_palaces`.
/// Test: Indirectly exercised via the MCP / HTTP / CLI integration paths;
/// `recall_across_palaces_merges_results` covers the core merge logic.
pub async fn recall_across_palaces_with_default_embedder(
    handles: &[Arc<PalaceHandle>],
    query: &str,
    top_k: usize,
    deep: bool,
) -> Result<Vec<CrossPalaceResult>> {
    let embedder = shared_embedder()
        .await
        .context("acquire shared embedder for recall_across_palaces")?;
    let erased: Arc<dyn Embedder + Send + Sync> = embedder;
    recall_across_palaces(handles, &erased, query, top_k, deep).await
}

/// Hash a `RoomType` to a deterministic `Uuid` so the room signal survives
/// through the in-memory drawer table without a real `Room` row.
///
/// Why: `Drawer.room_id` is a `Uuid`; until we wire a Room table, callers need
/// a stable mapping from `RoomType` to id so `list_drawers` can filter by room.
/// What: FNV-1a-like hash of the `Debug` repr, packed into 16 bytes.
/// Test: Indirectly via `cli_list_filters_by_room`.
fn room_to_uuid(room: &RoomType) -> Uuid {
    let label = format!("{room:?}");
    let mut bytes = [0u8; 16];
    // Fold each byte into the buffer with a simple xor-rot hash; collisions
    // here are fine — this only needs to be stable per-process.
    for (i, b) in label.bytes().enumerate() {
        bytes[i % 16] ^= b.wrapping_add(i as u8);
    }
    Uuid::from_bytes(bytes)
}

/// Compare two UUIDs by their first 8 bytes.
///
/// Why: The vector store keys vectors by the first 8 bytes of a UUID, so
/// search results carry a `Uuid` whose last 8 bytes are zero. Matching these
/// back to drawers must therefore compare prefixes only.
/// What: Returns true if `a` and `b` agree on bytes `0..8`.
/// Test: Implicitly exercised by `l2_returns_relevant_drawer`.
fn uuid_prefix_eq(a: Uuid, b: Uuid) -> bool {
    a.as_bytes()[..8] == b.as_bytes()[..8]
}

/// Build the always-on L0 + L1 portion of a recall.
///
/// Why: Every retrieval flow includes L0+L1; centralizing the construction
/// keeps `recall` and `recall_deep` short and makes L0/L1 layering testable
/// in isolation.
/// What: Emits one `RecallResult { layer: 0, score: 1.0 }` for the identity
/// (only when non-empty), followed by one result per cached L1 drawer with
/// `score = drawer.importance` and `layer: 1`. The L0 result reuses the
/// identity text inside a synthetic `Drawer` so callers can render every
/// layer uniformly.
/// Test: `l0_l1_always_present` asserts both layers appear.
pub fn retrieve_l0_l1(handle: &PalaceHandle) -> Vec<RecallResult> {
    let mut out: Vec<RecallResult> = Vec::with_capacity(1 + handle.l1_drawers.len());

    if !handle.identity.is_empty() {
        // Synthesize a Drawer for the identity so RecallResult stays uniform.
        let identity_drawer = Drawer {
            id: Uuid::nil(),
            room_id: Uuid::nil(),
            content: handle.identity.clone(),
            importance: 1.0,
            source_file: None,
            created_at: chrono::Utc::now(),
            tags: Vec::new(),
            last_accessed_at: None,
            access_count: 0,
            drawer_type: crate::memory_core::palace::DrawerType::UserFact,
            expires_at: None,
        };
        out.push(RecallResult {
            drawer: identity_drawer,
            score: 1.0,
            layer: 0,
        });
    }

    for d in &handle.l1_drawers {
        out.push(RecallResult {
            drawer: d.clone(),
            score: d.importance,
            layer: 1,
        });
    }
    out
}

/// L2 retrieval: metadata-filtered HNSW search.
///
/// Why: Most queries don't need a full deep search — a topic-scoped vector
/// search returns relevant drawers cheaply. Filtering by `RoomType` lets
/// callers narrow into a domain (e.g. only Backend rooms) when intent is
/// known.
/// What: Embeds the query, searches the vector store with `top_k * 3` to
/// leave room for filtering, maps each hit back to a drawer via UUID-prefix
/// match, applies the optional room filter (currently a TODO — see below),
/// scores as `drawer.importance * hit.score`, and returns the top `top_k`
/// drawers tagged with `layer: 2`.
/// Test: `l2_returns_relevant_drawer` upserts a Rust-themed drawer and
/// asserts a Rust-themed query retrieves it at rank 0.
pub async fn retrieve_l2(
    handle: &PalaceHandle,
    embedder: &dyn Embedder,
    query: &str,
    room_filter: Option<RoomType>,
    top_k: usize,
) -> Result<Vec<RecallResult>> {
    if top_k == 0 {
        return Ok(Vec::new());
    }
    let embeddings = embedder.embed_batch(&[query.to_string()]).await?;
    let Some(query_vec) = embeddings.into_iter().next() else {
        return Ok(Vec::new());
    };

    let overfetch = top_k.saturating_mul(3).max(top_k);
    let hits = handle.vector_store.search(&query_vec, overfetch).await?;

    let drawers = handle.drawers.read();
    let closets = handle.closets.read();
    let query_tokens: Vec<String> = extract_keywords(query);
    let mut results: Vec<RecallResult> = Vec::with_capacity(hits.len());

    for hit in hits {
        let Some(drawer) = drawers.iter().find(|d| uuid_prefix_eq(d.id, hit.drawer_id)) else {
            // Vector hit refers to a drawer we no longer have metadata for;
            // skip silently — this can happen during partial loads.
            continue;
        };

        // TODO(room-filter): RoomType lives on Room, not Drawer. Once a Room
        // table is wired into PalaceHandle (drawer.room_id -> RoomType), apply
        // the filter here. For now, accept all drawers regardless of filter.
        if room_filter.is_some() {
            // Filter is acknowledged but not yet enforceable — see TODO above.
        }

        let age_days = DecayConfig::age_days(drawer.created_at);
        let boost = drawer.accumulated_boost(&handle.decay_config);
        let eff_importance =
            handle
                .decay_config
                .effective_importance(drawer.importance, age_days, boost);
        let effective_score = eff_importance * hit.score;

        // Closet tag boost: if any query token matches a closet keyword that
        // contains this drawer, add a 0.15 bump (capped at 1.0) so topical
        // hits outrank generic semantic neighbors.
        let drawer_id = drawer.id;
        let in_closet = query_tokens
            .iter()
            .any(|tok| closets.get(tok).is_some_and(|ids| ids.contains(&drawer_id)));
        let tag_boost = if in_closet { 0.15_f32 } else { 0.0 };
        let final_score = (effective_score + tag_boost).min(1.0);

        results.push(RecallResult {
            drawer: drawer.clone(),
            score: final_score,
            layer: 2,
        });
    }
    drop(closets);
    drop(drawers);

    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results.truncate(top_k);
    Ok(results)
}

/// L3 retrieval: full HNSW deep search across the palace.
///
/// Why: For deep / exploratory queries the agent wants the broadest possible
/// recall; L3 skips the overfetch+filter dance and just returns the top-k
/// nearest neighbors with `layer: 3`.
/// What: Embeds the query, searches with exactly `top_k`, joins each hit to
/// its drawer via UUID-prefix match, scores as `importance * hit.score`,
/// sorts descending, and returns at most `top_k` `RecallResult`s.
/// Test: Symmetric with `l2_returns_relevant_drawer`; same join logic.
pub async fn retrieve_l3(
    handle: &PalaceHandle,
    embedder: &dyn Embedder,
    query: &str,
    top_k: usize,
) -> Result<Vec<RecallResult>> {
    if top_k == 0 {
        return Ok(Vec::new());
    }
    let embeddings = embedder.embed_batch(&[query.to_string()]).await?;
    let Some(query_vec) = embeddings.into_iter().next() else {
        return Ok(Vec::new());
    };

    let hits = handle.vector_store.search(&query_vec, top_k).await?;

    let drawers = handle.drawers.read();
    let closets = handle.closets.read();
    let query_tokens: Vec<String> = extract_keywords(query);
    let mut results: Vec<RecallResult> = Vec::with_capacity(hits.len());
    for hit in hits {
        let Some(drawer) = drawers.iter().find(|d| uuid_prefix_eq(d.id, hit.drawer_id)) else {
            continue;
        };
        let age_days = DecayConfig::age_days(drawer.created_at);
        let boost = drawer.accumulated_boost(&handle.decay_config);
        let eff_importance =
            handle
                .decay_config
                .effective_importance(drawer.importance, age_days, boost);
        let effective_score = eff_importance * hit.score;

        let drawer_id = drawer.id;
        let in_closet = query_tokens
            .iter()
            .any(|tok| closets.get(tok).is_some_and(|ids| ids.contains(&drawer_id)));
        let tag_boost = if in_closet { 0.15_f32 } else { 0.0 };
        let final_score = (effective_score + tag_boost).min(1.0);

        results.push(RecallResult {
            drawer: drawer.clone(),
            score: final_score,
            layer: 3,
        });
    }
    drop(closets);
    drop(drawers);

    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results.truncate(top_k);
    Ok(results)
}

/// Expand a user query with domain synonyms before embedding.
///
/// Why: There's a vocabulary gap between casual user queries ("how fast is X?")
/// and technical memory content ("HNSW provides O(log N) latency"). Appending
/// related terms steers the embedded query vector toward both the original
/// intent and the technical phrasing — boosting recall on speed/performance,
/// vector-search, memory-safety, and concurrency questions.
/// What: Lowercase-scans the query for trigger phrases and appends a list of
/// related domain terms. No-op when no triggers match.
/// Test: `expand_query_adds_synonyms`, `expand_query_noop_for_unmatched`.
pub fn expand_query(query: &str) -> String {
    let q = query.to_lowercase();
    let mut extra: Vec<&str> = Vec::new();

    if q.contains("fast")
        || q.contains("speed")
        || q.contains("latency")
        || q.contains("performance")
    {
        extra.push("latency performance speed throughput");
    }
    if q.contains("vector search")
        || q.contains("semantic search")
        || q.contains("nearest neighbor")
    {
        extra.push("HNSW ANN approximate nearest neighbor usearch vector index");
    }
    if q.contains("memory safe") || q.contains("borrow") || q.contains("ownership") {
        extra.push("borrow checker lifetime ownership Rust memory safety");
    }
    if q.contains("concurren") || q.contains("thread") || q.contains("parallel") {
        extra.push("concurrent async tokio DashMap RwLock mutex thread-safe");
    }

    if extra.is_empty() {
        query.to_string()
    } else {
        format!("{} {}", query, extra.join(" "))
    }
}

/// Standard recall = L0 + L1 + L2, deduplicated.
///
/// Why: This is the default path for "hey memory, what do you know about X?"
/// — always-on identity + essentials, plus the cheapest topic search.
/// What: Concatenates `retrieve_l0_l1` and `retrieve_l2` (no room filter),
/// then deduplicates by drawer id keeping the *earlier* (lower-numbered layer)
/// occurrence so L0/L1 always win over an L2 duplicate. Applies `expand_query`
/// before embedding to bridge the user-vocabulary / technical-vocabulary gap.
/// Test: Composition is exercised indirectly via the per-layer tests.
pub async fn recall(
    handle: &PalaceHandle,
    embedder: &dyn Embedder,
    query: &str,
    top_k: usize,
) -> Result<Vec<RecallResult>> {
    let expanded = expand_query(query);
    let mut combined = retrieve_l0_l1(handle);
    let l2 = retrieve_l2(handle, embedder, &expanded, None, top_k).await?;
    dedup_extend(&mut combined, l2);
    log_recall(handle, query, &combined);
    Ok(combined)
}

/// Deep recall = L0 + L1 + L3, deduplicated.
///
/// Why: When the user explicitly asks for deep search, fall through to L3
/// instead of the metadata-filtered L2.
/// What: Same as `recall` but uses `retrieve_l3` for the heavy layer.
/// Test: Symmetric with `recall`; covered indirectly.
pub async fn recall_deep(
    handle: &PalaceHandle,
    embedder: &dyn Embedder,
    query: &str,
    top_k: usize,
) -> Result<Vec<RecallResult>> {
    let expanded = expand_query(query);
    let mut combined = retrieve_l0_l1(handle);
    let l3 = retrieve_l3(handle, embedder, &expanded, top_k).await?;
    dedup_extend(&mut combined, l3);
    log_recall(handle, query, &combined);
    Ok(combined)
}

/// Fire-and-forget recall analytics.
///
/// Why: Hit/miss telemetry must never block the request path; spawning a task
/// keeps logging off the critical path while still capturing every event.
/// What: If `handle.recall_log` is set, spawns a task that records one event
/// per non-L0 result, or a single miss event when `results` only contains the
/// L0 identity (no real recall hits).
/// Test: `recall_logs_events_when_log_present` confirms the log row appears.
fn log_recall(handle: &PalaceHandle, query: &str, results: &[RecallResult]) {
    let Some(log) = handle.recall_log.clone() else {
        return;
    };
    let palace_id = handle.id.as_str().to_string();
    let q_hash = query_hash(query);
    // Only count L1+ entries — the synthetic L0 identity is always present
    // and would otherwise drown out genuine miss signals.
    let logged: Vec<RecallResult> = results.iter().filter(|r| r.layer > 0).cloned().collect();

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

/// Extend `base` with entries from `extra` whose drawer id isn't already in
/// `base`. L0/L1 priority is implied by call ordering: pass L0/L1 first.
fn dedup_extend(base: &mut Vec<RecallResult>, extra: Vec<RecallResult>) {
    for r in extra {
        if !base.iter().any(|b| b.drawer.id == r.drawer.id) {
            base.push(r);
        }
    }
}

// -- Legacy stubs (kept for backwards compatibility with existing callers) --

pub struct RetrievalLayers;

impl RetrievalLayers {
    /// Load L0 identity for a palace.
    ///
    /// Why: Provides a stable persona / project description that grounds every
    /// reply, without taking up real context budget.
    /// What: Reads `identity.txt` from the palace data dir; returns empty
    /// content if the file does not yet exist.
    /// Test: For a freshly created palace dir, returns `L0Identity { content: "" }`.
    pub async fn load_l0(_palace_data_dir: &Path) -> Result<L0Identity> {
        Ok(L0Identity {
            content: String::new(),
        })
    }

    /// Load L1 essential drawers.
    ///
    /// Why: Top-importance drawers are queried on virtually every request, so
    /// we want them already in memory and pre-ranked.
    /// What: Returns the top-15 drawers across the palace, sorted by importance.
    /// Test: For an empty palace, returns `L1Essential { drawers: [] }`.
    pub async fn load_l1(_palace_id: &PalaceId) -> Result<L1Essential> {
        Ok(L1Essential {
            drawers: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory_core::store::{kg::KnowledgeGraph, vector::UsearchStore};
    use tempfile::tempdir;

    fn make_handle(dir: &std::path::Path) -> PalaceHandle {
        let vs = UsearchStore::new(dir.join("idx.usearch"), 384).unwrap();
        let kg = KnowledgeGraph::open(&dir.join("kg.db")).unwrap();
        PalaceHandle::new(PalaceId::new("test"), "Test palace".to_string(), vs, kg)
    }

    #[test]
    fn l0_l1_always_present() {
        let dir = tempdir().unwrap();
        let mut handle = make_handle(dir.path());
        let room_id = uuid::Uuid::new_v4();
        let mut d = Drawer::new(room_id, "important fact");
        d.importance = 0.9;
        handle.add_drawer(d);
        handle.refresh_l1();

        let results = retrieve_l0_l1(&handle);
        assert!(results.iter().any(|r| r.layer == 0), "L0 identity missing");
        assert!(results.iter().any(|r| r.layer == 1), "L1 drawer missing");
    }

    #[tokio::test]
    async fn l2_returns_relevant_drawer() {
        let dir = tempdir().unwrap();
        let handle = make_handle(dir.path());
        let embedder = crate::memory_core::embed::FastEmbedder::new()
            .await
            .unwrap();

        let room_id = uuid::Uuid::new_v4();
        let drawer = Drawer::new(room_id, "Rust is a systems programming language");
        let drawer_id = drawer.id;

        let vecs = embedder
            .embed_batch(std::slice::from_ref(&drawer.content))
            .await
            .unwrap();
        handle
            .vector_store
            .upsert(drawer_id, vecs[0].clone())
            .await
            .unwrap();
        handle.add_drawer(drawer);

        let results = retrieve_l2(&handle, &embedder, "systems programming Rust", None, 5)
            .await
            .unwrap();
        assert!(!results.is_empty(), "L2 should return results");
        assert!(
            uuid_prefix_eq(results[0].drawer.id, drawer_id),
            "Top L2 result should match the upserted drawer (got {:?}, want {:?})",
            results[0].drawer.id,
            drawer_id
        );
        assert_eq!(results[0].layer, 2);
    }

    /// Why: End-to-end confirmation that `remember` + `recall` round-trip
    /// through the embedder and vector store correctly.
    /// What: Build a palace handle backed by a tempdir, remember three
    /// drawers in distinct rooms, recall on a keyword from one of them, and
    /// assert the matching drawer appears in the L2 results.
    /// Test: This test itself.
    #[tokio::test]
    async fn cli_remember_and_recall() {
        use crate::memory_core::palace::Palace;
        let dir = tempdir().unwrap();
        let palace = Palace {
            id: PalaceId::new("test"),
            name: "Test".into(),
            description: None,
            created_at: chrono::Utc::now(),
            data_dir: dir.path().join("test"),
        };
        std::fs::create_dir_all(&palace.data_dir).unwrap();
        let handle = PalaceHandle::open(&palace).unwrap();

        let _id = handle
            .remember(
                "Rust async runtime is tokio".into(),
                RoomType::Backend,
                vec!["rust".into()],
                0.7,
            )
            .await
            .unwrap();
        handle
            .remember(
                "React uses a virtual DOM".into(),
                RoomType::Frontend,
                vec![],
                0.5,
            )
            .await
            .unwrap();

        let results = recall_with_default_embedder(&handle, "tokio rust async", 5)
            .await
            .unwrap();
        assert!(
            results.iter().any(|r| r.drawer.content.contains("tokio")),
            "expected to recall the tokio drawer; got {results:?}"
        );
    }

    /// Why: Confirm `forget` removes a drawer from both the in-memory table
    /// and the vector store.
    /// What: Remember one drawer, forget it, then recall the same keyword and
    /// assert the drawer is no longer in the result list.
    /// Test: This test itself.
    #[tokio::test]
    async fn cli_forget_removes_drawer() {
        use crate::memory_core::palace::Palace;
        let dir = tempdir().unwrap();
        let palace = Palace {
            id: PalaceId::new("forget-test"),
            name: "Forget".into(),
            description: None,
            created_at: chrono::Utc::now(),
            data_dir: dir.path().join("forget-test"),
        };
        std::fs::create_dir_all(&palace.data_dir).unwrap();
        let handle = PalaceHandle::open(&palace).unwrap();

        let id = handle
            .remember(
                "ephemeral fact about Quokkas".into(),
                RoomType::General,
                vec![],
                0.5,
            )
            .await
            .unwrap();
        handle.forget(id).await.unwrap();

        let results = recall_with_default_embedder(&handle, "Quokkas ephemeral", 5)
            .await
            .unwrap();
        assert!(
            !results.iter().any(|r| r.drawer.id == id),
            "forgotten drawer should not appear in recall results"
        );
    }

    /// Regression test for issue #154: concurrent `remember_with_options`
    /// calls on the same palace must not race on the L1 snapshot write.
    ///
    /// Why: Pre-fix, 20 concurrent writers against the same palace produced
    /// 30–60% "No such file or directory" failures because multiple writers
    /// would write the same `l1_cache.json.tmp` file and the first
    /// `rename(tmp -> target)` removed the tmp before the second rename
    /// could see it. The per-palace write mutex + per-call tmp naming
    /// together eliminate the race.
    /// What: Spawns 32 concurrent `remember` tasks on the same handle,
    /// waits for all of them, and asserts every single one returned `Ok`.
    /// After the burst the drawer table contains all 32 entries.
    /// Test: this test.
    #[tokio::test]
    async fn remember_concurrent_does_not_lose_writes() {
        use crate::memory_core::palace::Palace;
        let dir = tempdir().unwrap();
        let palace = Palace {
            id: PalaceId::new("concurrent-test"),
            name: "Concurrent".into(),
            description: None,
            created_at: chrono::Utc::now(),
            data_dir: dir.path().join("concurrent-test"),
        };
        std::fs::create_dir_all(&palace.data_dir).unwrap();
        let handle = PalaceHandle::open(&palace).unwrap();

        // Spawn 32 concurrent writers. Each writes a distinct payload that
        // is long enough to pass the default token filter (>=8 tokens).
        let mut tasks = Vec::with_capacity(32);
        for i in 0..32u32 {
            let h = handle.clone();
            tasks.push(tokio::spawn(async move {
                h.remember(
                    format!(
                        "concurrent write test payload number {i} with enough \
                         tokens to satisfy the default token filter check"
                    ),
                    RoomType::General,
                    vec!["concurrent".into(), format!("idx-{i}")],
                    0.5,
                )
                .await
            }));
        }

        let mut ok = 0usize;
        let mut errs = Vec::new();
        for t in tasks {
            match t.await.expect("task panicked") {
                Ok(_id) => ok += 1,
                Err(e) => errs.push(format!("{e:#}")),
            }
        }
        assert_eq!(
            ok, 32,
            "expected all 32 concurrent remembers to succeed; failures: {errs:?}"
        );

        // Every write should be present in the in-memory drawer table.
        let drawer_count = handle.drawers.read().len();
        assert_eq!(
            drawer_count, 32,
            "expected 32 drawers after concurrent burst, got {drawer_count}"
        );

        // No leaked tmp files from racing renames.
        let leaked: Vec<_> = std::fs::read_dir(&palace.data_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("l1_cache.json") && n.contains(".tmp."))
            .collect();
        assert!(
            leaked.is_empty(),
            "expected no .tmp.* orphans after concurrent saves; found {leaked:?}"
        );
    }

    /// Why: Confirm the room filter in `list_drawers` actually narrows the
    /// returned set to drawers whose deterministic room id matches.
    /// What: Remember three drawers in three distinct rooms, list with the
    /// Backend filter, and assert exactly one drawer comes back.
    /// Test: This test itself.
    #[tokio::test]
    async fn cli_list_filters_by_room() {
        use crate::memory_core::palace::Palace;
        let dir = tempdir().unwrap();
        let palace = Palace {
            id: PalaceId::new("list-test"),
            name: "List".into(),
            description: None,
            created_at: chrono::Utc::now(),
            data_dir: dir.path().join("list-test"),
        };
        std::fs::create_dir_all(&palace.data_dir).unwrap();
        let handle = PalaceHandle::open(&palace).unwrap();

        handle
            .remember(
                "backend fact about the test fixture".into(),
                RoomType::Backend,
                vec![],
                0.5,
            )
            .await
            .unwrap();
        handle
            .remember(
                "frontend fact about the test fixture".into(),
                RoomType::Frontend,
                vec![],
                0.5,
            )
            .await
            .unwrap();
        handle
            .remember(
                "docs fact about the test fixture".into(),
                RoomType::Documentation,
                vec![],
                0.5,
            )
            .await
            .unwrap();

        let backend_only = handle.list_drawers(Some(RoomType::Backend), None, 10);
        assert_eq!(
            backend_only.len(),
            1,
            "expected exactly 1 backend drawer, got {backend_only:?}"
        );
        assert!(backend_only[0].content.contains("backend"));
    }

    /// Why: Confirm the recall_log wiring actually fires events end-to-end.
    /// What: Build a handle with a `RecallLog`, upsert one drawer, run
    /// `recall`, then poll `hit_count` on the spawned logger task until it
    /// reports >=1 (with a small bounded retry to allow the spawn to flush).
    /// Test: This test itself.
    #[tokio::test]
    async fn recall_logs_events_when_log_present() {
        let dir = tempdir().unwrap();
        let log = Arc::new(RecallLog::open(&dir.path().join("recall.db")).unwrap());
        let mut handle = make_handle(dir.path()).with_recall_log(log.clone());
        let embedder = crate::memory_core::embed::FastEmbedder::new()
            .await
            .unwrap();

        let room_id = uuid::Uuid::new_v4();
        let drawer = Drawer::new(room_id, "Rust is a systems programming language");
        let drawer_id = drawer.id;
        let vecs = embedder
            .embed_batch(std::slice::from_ref(&drawer.content))
            .await
            .unwrap();
        handle
            .vector_store
            .upsert(drawer_id, vecs[0].clone())
            .await
            .unwrap();
        handle.add_drawer(drawer);
        handle.refresh_l1();

        let _ = recall(&handle, &embedder, "systems programming Rust", 5)
            .await
            .unwrap();

        // The logger task is spawned; poll briefly for it to land at least
        // one event for our drawer.
        let mut hits = 0u64;
        for _ in 0..20 {
            hits = log.hit_count(drawer_id).await.unwrap();
            if hits >= 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        assert!(hits >= 1, "expected at least one logged hit, got {hits}");
    }

    /// Why: Issue #53 — `PalaceHandle::open` (the production palace-load path
    /// used by `PalaceRegistry::open_palace`) must auto-attach a recall log so
    /// the MCP daemon and CLI both get analytics for free without having to
    /// call `with_recall_log` manually.
    /// What: Open a palace from disk and assert `handle.recall_log` is `Some`,
    /// and that a recall fires a logged event end-to-end.
    /// Test: This test itself.
    #[tokio::test]
    async fn open_attaches_recall_log_automatically() {
        use crate::memory_core::palace::Palace;
        let dir = tempdir().unwrap();
        let palace = Palace {
            id: PalaceId::new("analytics-auto"),
            name: "AnalyticsAuto".into(),
            description: None,
            created_at: chrono::Utc::now(),
            data_dir: dir.path().join("analytics-auto"),
        };
        std::fs::create_dir_all(&palace.data_dir).unwrap();
        let handle = PalaceHandle::open(&palace).unwrap();

        assert!(
            handle.recall_log.is_some(),
            "PalaceHandle::open must auto-attach a RecallLog (issue #53)"
        );
        // Issue #57 migrated RecallLog from SQLite to redb. The legacy
        // `recall.db` path passed by retrieval.rs is silently rewritten to
        // `recall.redb`; assert the redb file lands on disk after open.
        assert!(
            palace.data_dir.join("recall.redb").exists(),
            "recall.redb must exist on disk after open"
        );

        // End-to-end: remember + recall should produce at least one logged hit.
        let drawer_id = handle
            .remember(
                "the platypus is a monotreme native to eastern Australia".into(),
                RoomType::Research,
                vec!["wildlife".into()],
                0.7,
            )
            .await
            .unwrap();

        let embedder = crate::memory_core::embed::FastEmbedder::new()
            .await
            .unwrap();
        let _ = recall(&handle, &embedder, "platypus monotreme", 5)
            .await
            .unwrap();

        let log = handle.recall_log.as_ref().unwrap().clone();
        let mut hits = 0u64;
        for _ in 0..20 {
            hits = log.hit_count(drawer_id).await.unwrap();
            if hits >= 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        assert!(
            hits >= 1,
            "auto-attached recall log must record events; got {hits}"
        );
    }

    /// Why: After `remember`, L2 tag-boosting depends on the closet index being
    /// up-to-date without waiting for a dream cycle.
    /// What: Remember a drawer with a distinctive keyword, then read the closet
    /// map and assert the keyword maps to the drawer's id.
    /// Test: This test itself.
    #[tokio::test]
    async fn closet_updated_after_remember() {
        use crate::memory_core::palace::Palace;
        let dir = tempdir().unwrap();
        let palace = Palace {
            id: PalaceId::new("closet-test"),
            name: "Closet".into(),
            description: None,
            created_at: chrono::Utc::now(),
            data_dir: dir.path().join("closet-test"),
        };
        std::fs::create_dir_all(&palace.data_dir).unwrap();
        let handle = PalaceHandle::open(&palace).unwrap();

        let id = handle
            .remember(
                "Quokkas are happy marsupials".into(),
                RoomType::General,
                vec![],
                0.5,
            )
            .await
            .unwrap();

        let closets = handle.closets.read();
        let entry = closets
            .get("quokkas")
            .expect("expected `quokkas` keyword in closet index");
        assert!(
            entry.contains(&id),
            "closet entry for `quokkas` should contain the new drawer id"
        );
    }

    /// Why: Query expansion must inject the right synonyms when speed/vector
    /// triggers fire so the embedder is steered toward technical phrasing.
    /// What: Call `expand_query` with the q5 benchmark question and assert the
    /// expanded string contains the expected synonym tokens.
    /// Test: This test itself.
    #[test]
    fn expand_query_adds_synonyms() {
        let out = expand_query("how fast is vector search?");
        assert!(out.contains("HNSW"), "expected HNSW synonym, got: {out}");
        assert!(
            out.contains("latency"),
            "expected latency synonym, got: {out}"
        );
    }

    /// Why: Borrow/ownership queries should still expand, but unmatched topics
    /// must remain unchanged so unrelated queries aren't polluted.
    /// What: Verify the borrow trigger fires (and adds Rust terms), and that a
    /// query with no triggers comes back identical.
    /// Test: This test itself.
    #[test]
    fn expand_query_noop_for_unmatched() {
        let out = expand_query("what is a borrow checker?");
        assert!(
            out.contains("borrow checker"),
            "expected original query preserved, got: {out}"
        );
        assert!(
            out.contains("ownership") || out.contains("lifetime"),
            "expected ownership/lifetime synonyms, got: {out}"
        );

        let untouched = expand_query("what colour is the sky on Tuesday");
        assert_eq!(
            untouched, "what colour is the sky on Tuesday",
            "queries with no triggers must pass through unchanged"
        );
    }

    /// Why: Regression test for issue #32 — after a cold restart, L2/L3 must
    /// still resolve vector hits to drawers beyond the top-15 L1 snapshot.
    /// What: Remember 20 drawers, drop the handle, reopen the palace from the
    /// same data_dir, and recall a keyword from a drawer that is NOT in the
    /// top-15 by importance. The drawer must still come back.
    /// Test: This test itself.
    #[tokio::test]
    async fn cold_restart_recalls_beyond_l1_snapshot() {
        use crate::memory_core::palace::Palace;
        let dir = tempdir().unwrap();
        let palace = Palace {
            id: PalaceId::new("cold-restart"),
            name: "Cold".into(),
            description: None,
            created_at: chrono::Utc::now(),
            data_dir: dir.path().join("cold-restart"),
        };
        std::fs::create_dir_all(&palace.data_dir).unwrap();

        // Use a separate scope so the first handle (and its Arc-wrapped
        // vector store) is fully dropped before we reopen.
        let needle_id = {
            let handle = PalaceHandle::open(&palace).unwrap();
            // 19 high-importance filler drawers (importance 0.9) — these will
            // dominate the top-15 L1 snapshot.
            for i in 0..19 {
                handle
                    .remember(
                        format!("filler drawer number {i} about generic topics"),
                        RoomType::General,
                        vec![],
                        0.9,
                    )
                    .await
                    .unwrap();
            }
            // The needle: low importance so it cannot be in the L1 top-15,
            // distinctive vocabulary so the query lands on it.
            handle
                .remember(
                    "the pangolin is a scaly nocturnal mammal".into(),
                    RoomType::Research,
                    vec![],
                    0.1,
                )
                .await
                .unwrap()
        };

        // Reopen the palace — simulating a cold restart.
        let handle2 = PalaceHandle::open(&palace).unwrap();

        // Drawer table should be fully hydrated, not just the 15-entry L1.
        let count = handle2.drawers.read().len();
        assert!(
            count >= 20,
            "expected >=20 drawers after cold reopen, got {count}"
        );

        let results = recall_with_default_embedder(&handle2, "pangolin scaly mammal", 10)
            .await
            .unwrap();
        assert!(
            results.iter().any(|r| r.drawer.id == needle_id),
            "low-importance drawer beyond L1 must still be recallable after cold restart; got {results:?}"
        );
    }

    /// Why: Issue #57 — at most one FastEmbedder must exist process-wide.
    /// `shared_embedder` must return the same `Arc` on every call so callers
    /// transitively share one ONNX session.
    /// What: Call `shared_embedder` twice and assert the `Arc` pointers are
    /// identical via `Arc::ptr_eq`.
    /// Test: This test itself.
    #[tokio::test]
    async fn shared_embedder_is_singleton() {
        let a = shared_embedder().await.unwrap();
        let b = shared_embedder().await.unwrap();
        assert!(
            Arc::ptr_eq(&a, &b),
            "shared_embedder must return the same Arc on every call"
        );
    }

    /// Why: Closet tag boost should raise a tagged drawer's rank above an
    /// untagged but otherwise-similar drawer.
    /// What: Insert two drawers — one whose content shares keywords with the
    /// query, one that doesn't — and assert the keyword-matched drawer ranks
    /// first in L2 results.
    /// Test: This test itself.
    #[tokio::test]
    async fn retrieve_l2_tag_boost_raises_rank() {
        use crate::memory_core::palace::Palace;
        let dir = tempdir().unwrap();
        let palace = Palace {
            id: PalaceId::new("boost-test"),
            name: "Boost".into(),
            description: None,
            created_at: chrono::Utc::now(),
            data_dir: dir.path().join("boost-test"),
        };
        std::fs::create_dir_all(&palace.data_dir).unwrap();
        let handle = PalaceHandle::open(&palace).unwrap();

        // Drawer A: contains keywords "vector" and "search" and "performance".
        let id_tagged = handle
            .remember(
                "Vector search performance benchmarks show low latency".into(),
                RoomType::Backend,
                vec!["vector-search".into()],
                0.5,
            )
            .await
            .unwrap();
        // Drawer B: unrelated topic, no shared keywords.
        let _id_other = handle
            .remember(
                "React components render through a virtual DOM".into(),
                RoomType::Frontend,
                vec![],
                0.5,
            )
            .await
            .unwrap();

        let embedder = crate::memory_core::embed::FastEmbedder::new()
            .await
            .unwrap();
        let results = retrieve_l2(&handle, &embedder, "vector search performance", None, 5)
            .await
            .unwrap();

        assert!(!results.is_empty(), "L2 should return results");
        assert!(
            uuid_prefix_eq(results[0].drawer.id, id_tagged),
            "tagged drawer should rank first; got {:?}",
            results[0].drawer.content
        );
    }

    /// Why: Cross-palace recall is the foundation of `memory_recall_all` —
    /// agents need to fan a query across every palace and merge the hits.
    /// Without this test a regression in the merge/dedup/rerank logic could
    /// silently return a single palace's results or drop palace_id tagging.
    /// What: Build two disk-backed palaces with distinct distinctive drawers,
    /// run `recall_across_palaces_with_default_embedder`, and assert at least
    /// one result from each palace appears in the merged output sorted by
    /// score descending.
    /// Test: This test itself.
    #[tokio::test]
    async fn recall_across_palaces_merges_results() {
        use crate::memory_core::palace::Palace;
        let dir = tempdir().unwrap();

        let palace_a = Palace {
            id: PalaceId::new("alpha"),
            name: "Alpha".into(),
            description: None,
            created_at: chrono::Utc::now(),
            data_dir: dir.path().join("alpha"),
        };
        std::fs::create_dir_all(&palace_a.data_dir).unwrap();
        let handle_a = PalaceHandle::open(&palace_a).unwrap();
        handle_a
            .remember(
                "the pangolin is a scaly nocturnal mammal".into(),
                RoomType::Research,
                vec![],
                0.6,
            )
            .await
            .unwrap();

        let palace_b = Palace {
            id: PalaceId::new("beta"),
            name: "Beta".into(),
            description: None,
            created_at: chrono::Utc::now(),
            data_dir: dir.path().join("beta"),
        };
        std::fs::create_dir_all(&palace_b.data_dir).unwrap();
        let handle_b = PalaceHandle::open(&palace_b).unwrap();
        handle_b
            .remember(
                "the platypus is a venomous monotreme".into(),
                RoomType::Research,
                vec![],
                0.6,
            )
            .await
            .unwrap();

        let handles = vec![handle_a, handle_b];
        let results = recall_across_palaces_with_default_embedder(
            &handles,
            "pangolin platypus mammal",
            10,
            false,
        )
        .await
        .unwrap();

        assert!(!results.is_empty(), "expected merged results, got none");
        assert!(
            results.iter().any(|r| r.palace_id == "alpha"),
            "expected at least one alpha result; got {:?}",
            results.iter().map(|r| &r.palace_id).collect::<Vec<_>>()
        );
        assert!(
            results.iter().any(|r| r.palace_id == "beta"),
            "expected at least one beta result; got {:?}",
            results.iter().map(|r| &r.palace_id).collect::<Vec<_>>()
        );

        // Sorted by score descending.
        for w in results.windows(2) {
            assert!(
                w[0].result.score >= w[1].result.score,
                "results not sorted: {} < {}",
                w[0].result.score,
                w[1].result.score
            );
        }
    }

    /// Issue #61: short content must be rejected with an actionable error.
    #[tokio::test]
    async fn remember_rejects_short_content() {
        let dir = tempdir().unwrap();
        let handle = make_handle(dir.path());
        let err = handle
            .remember("too short".to_string(), RoomType::General, vec![], 0.5)
            .await
            .expect_err("should reject");
        let msg = format!("{err:#}");
        assert!(
            msg.to_lowercase().contains("too short")
                || msg.contains("memory_note")
                || msg.contains("tokens"),
            "expected actionable error, got: {msg}"
        );
    }

    /// Issue #61: known noise patterns must be rejected even when long.
    #[tokio::test]
    async fn remember_rejects_known_noise_patterns() {
        let dir = tempdir().unwrap();
        let handle = make_handle(dir.path());
        let cases = [
            "Tool use: search_files with query parameter very_long_string_here",
            "feat(memory): add filter for noise patterns to reduce drawer clutter",
            "Running cargo test --workspace --all-features for the entire monorepo...",
        ];
        for c in cases {
            let err = handle
                .remember(c.to_string(), RoomType::General, vec![], 0.5)
                .await
                .expect_err("should reject");
            assert!(
                format!("{err:#}").to_lowercase().contains("noise")
                    || format!("{err:#}").to_lowercase().contains("low-signal"),
                "expected noise-pattern reject for: {c}",
            );
        }
    }

    /// Issue #61: `force = true` bypasses every filter.
    #[tokio::test]
    async fn remember_force_bypasses_filter() {
        let dir = tempdir().unwrap();
        let handle = make_handle(dir.path());
        let id = handle
            .remember_with_options(
                "x".to_string(),
                RoomType::General,
                vec![],
                0.5,
                RememberOptions::forced(),
            )
            .await
            .expect("force should bypass filter");
        assert_ne!(id, uuid::Uuid::nil());
    }

    /// Issue #61: `memory_note` preset accepts short curated facts but
    /// classifies them as `UserFact`.
    #[tokio::test]
    async fn note_options_skip_token_check_but_keep_noise_filter() {
        let dir = tempdir().unwrap();
        let handle = make_handle(dir.path());
        let id = handle
            .remember_with_options(
                "User prefers snake_case".to_string(),
                RoomType::General,
                vec![],
                1.0,
                RememberOptions::note(),
            )
            .await
            .expect("note should accept short curated fact");
        // Copy the field we need out under a tightly-scoped guard so no lock is
        // held across the subsequent `.await` (clippy::await_holding_lock).
        let stored_type = {
            let drawers = handle.drawers.read();
            let stored = drawers.iter().find(|d| d.id == id).expect("present");
            stored.drawer_type
        };
        assert_eq!(stored_type, DrawerType::UserFact);

        // Noise still rejected even in note mode.
        let err = handle
            .remember_with_options(
                "Tool use: x".to_string(),
                RoomType::General,
                vec![],
                1.0,
                RememberOptions::note(),
            )
            .await
            .expect_err("note must still reject noise patterns");
        assert!(format!("{err:#}").to_lowercase().contains("noise"));
    }

    /// Issue #61: commit-shaped content is classified as `Commit` when
    /// passed through with `force` (so the classifier still fires).
    #[tokio::test]
    async fn remember_classifies_commit_messages() {
        let dir = tempdir().unwrap();
        let handle = make_handle(dir.path());
        // Use force so the noise filter doesn't reject before classify runs.
        let id = handle
            .remember_with_options(
                "feat(scope): non-empty long enough message body here please".to_string(),
                RoomType::General,
                vec![],
                0.5,
                RememberOptions::forced(),
            )
            .await
            .expect("forced commit message");
        let drawers = handle.drawers.read();
        let stored = drawers.iter().find(|d| d.id == id).expect("present");
        assert_eq!(stored.drawer_type, DrawerType::Commit);
    }

    /// Issue #61: TTL sweep only drops drawers whose expires_at is in the
    /// past; future / `None` entries survive.
    #[tokio::test]
    async fn purge_expired_drops_only_past_ttl() {
        let dir = tempdir().unwrap();
        let handle = make_handle(dir.path());
        let room_id = uuid::Uuid::new_v4();

        // Expired drawer.
        let mut expired = Drawer::new(room_id, "expired");
        expired.expires_at = Some(chrono::Utc::now() - chrono::Duration::days(1));
        let expired_id = expired.id;

        // Future-TTL drawer.
        let mut future = Drawer::new(room_id, "future");
        future.expires_at = Some(chrono::Utc::now() + chrono::Duration::days(7));
        let future_id = future.id;

        // Never-expires drawer.
        let permanent = Drawer::new(room_id, "permanent");
        let permanent_id = permanent.id;

        handle.add_drawer(expired);
        handle.add_drawer(future);
        handle.add_drawer(permanent);

        let pruned = handle.purge_expired().await.expect("purge");
        assert_eq!(pruned, 1, "exactly one drawer should be pruned");

        let remaining: Vec<uuid::Uuid> = handle.drawers.read().iter().map(|d| d.id).collect();
        assert!(!remaining.contains(&expired_id));
        assert!(remaining.contains(&future_id));
        assert!(remaining.contains(&permanent_id));
    }
}
