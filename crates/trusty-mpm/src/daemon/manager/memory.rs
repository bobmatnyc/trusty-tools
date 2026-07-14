//! Portfolio `trusty-memory` palace provisioning for `tm manager` (WI-5, #2582).
//!
//! Why: DOC-36 §3.4 gives the Layer-3 portfolio manager ONE additional
//! `trusty-memory` palace — scoped to `tm manager` itself, distinct from every
//! per-project palace — to hold digest history, escalation dispositions, and
//! portfolio chat-session turns (manager-level observations *about* the
//! portfolio, never project-scoped facts). §7 Q3 (RESOLVED 2026-07-14) fixes the
//! provisioning convention: the palace is **auto-created at daemon startup**
//! (consistent with §3.1's "daemon as source of truth"), idempotently
//! (create-if-absent, never clobber) — not a separate `tm manager init` step.
//! Crucially, reaching the memory engine must NEVER be able to fail daemon
//! startup: if the palace cannot be opened (unwritable root, corrupt store) — or
//! the heavy `memory-core` engine is not even compiled into this build — the
//! manager surface stays fully operable and simply reports the palace as
//! unavailable (DOC-36 §4 degrade-graceful bar). This mirrors the SM palace's
//! own "log and disable recall, never crash" posture ([`crate::core::sm::memory`]).
//! What: [`PortfolioPalace`] is the ALWAYS-present handle threaded into
//! [`crate::daemon::manager::ManagerState`]; it carries the fixed palace id
//! ([`PORTFOLIO_PALACE_ID`]) plus an availability flag/reason so callers can read
//! "is the palace usable, and if not why" without any feature-cfg of their own.
//! Under the opt-in `manager-memory` feature it also owns a live
//! [`PortfolioMemory`] — a direct `memory-core` library binding (no MCP/network
//! hop) mirroring [`crate::core::sm::memory::SmMemory`] — that idempotently
//! ensures the single portfolio palace and offers the minimal remember/recall
//! surface later phases (digest history, chat turns) build on.
//! Test: `portfolio_palace_reports_unavailable_without_feature` (default build)
//! and, under `--features manager-memory`,
//! `portfolio_palace_provisions_and_is_idempotent` /
//! `portfolio_memory_remember_then_recall_round_trips` in this file's `tests`.

use std::path::Path;

/// The fixed palace id for the single portfolio-scoped manager palace.
///
/// Why: DOC-36 §3.4 mandates exactly ONE portfolio palace, distinct from the
/// SM palace (`"session-manager"`) and every per-project palace. Pinning the id
/// to a stable constant makes provisioning idempotent across restarts (the same
/// id always resolves the same on-disk palace) and makes the scope auditable —
/// there is no code path here that targets any other id.
/// What: the string id used for the portfolio palace directory + registry key.
/// Test: `portfolio_palace_exposes_stable_id`.
pub const PORTFOLIO_PALACE_ID: &str = "tm-manager-portfolio";

/// Subdirectory under the framework root that holds `tm manager` runtime state.
///
/// Why: keeps the portfolio palace under one well-known directory, mirroring how
/// SM state lives under `<root>/sm` and the managed store under
/// `<root>/session-manager`. Isolating it means the portfolio palace never
/// collides with a per-project or SM palace on disk.
/// What: the `manager` segment joined onto the framework root; the palace itself
/// lives at `<root>/manager/palace/<PORTFOLIO_PALACE_ID>/`.
/// Test: exercised via [`PortfolioPalace::provision`] in the feature-gated tests.
const MANAGER_DATA_SUBDIR: &str = "manager";

/// The daemon-owned handle to the portfolio manager palace.
///
/// Why: [`ManagerState`](crate::daemon::manager::ManagerState) needs a single,
/// always-constructible handle it can thread through regardless of whether the
/// heavy `memory-core` engine is compiled or reachable — the manager routes must
/// be able to answer "is the palace available?" on every build. Making the
/// availability flag + reason live OUTSIDE any feature-cfg means route handlers
/// (and the `GET /manager/version` capabilities stub) read one plain struct with
/// no `#[cfg]` of their own.
/// What: the stable palace `id`, an `available` flag, an optional
/// human-readable `reason` when unavailable, and — only under `manager-memory` —
/// the live [`PortfolioMemory`] binding. Built once at daemon startup via
/// [`Self::provision`]; a failure to open the palace degrades to
/// `available = false` with the error captured in `reason`, never a panic.
/// Test: `portfolio_palace_reports_unavailable_without_feature`,
/// `portfolio_palace_provisions_and_is_idempotent`.
pub struct PortfolioPalace {
    /// Stable palace id ([`PORTFOLIO_PALACE_ID`]).
    id: String,
    /// Whether the palace is provisioned and usable for reads/writes.
    available: bool,
    /// Why the palace is unavailable, when `available` is false (feature not
    /// compiled, or the open/create failed). `None` when available.
    reason: Option<String>,
    /// The live `memory-core` binding — present only when `manager-memory` is
    /// compiled AND the open/create succeeded.
    #[cfg(feature = "manager-memory")]
    memory: Option<PortfolioMemory>,
}

impl std::fmt::Debug for PortfolioPalace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Deliberately omit the `memory-core` registry handle (not `Debug`, and
        // uninteresting in logs); the id + availability are the diagnostic bits.
        f.debug_struct("PortfolioPalace")
            .field("id", &self.id)
            .field("available", &self.available)
            .field("reason", &self.reason)
            .finish()
    }
}

impl PortfolioPalace {
    /// Provision the portfolio palace under `framework_root`, degrading on any
    /// failure (WI-5, §7 Q3).
    ///
    /// Why: this is the "auto-created at daemon startup" seam. It MUST be
    /// infallible from the daemon's perspective — a missing feature, an
    /// unwritable root, or a corrupt store yields an unavailable-but-constructed
    /// handle, never an error the daemon startup could propagate. Idempotency is
    /// inherited from the underlying open-or-create: calling this across restarts
    /// against the same root converges on exactly one palace.
    /// What: under `manager-memory`, opens (or creates) the single portfolio
    /// palace at `<framework_root>/manager/palace`; on success stores the live
    /// binding and marks available, on failure logs a warning and marks
    /// unavailable with the error string. Without the feature, returns an
    /// unavailable handle whose reason names the missing feature.
    /// Test: `portfolio_palace_reports_unavailable_without_feature`,
    /// `portfolio_palace_provisions_and_is_idempotent`.
    pub fn provision(framework_root: &Path) -> Self {
        let id = PORTFOLIO_PALACE_ID.to_string();

        #[cfg(feature = "manager-memory")]
        {
            let data_root = framework_root.join(MANAGER_DATA_SUBDIR).join("palace");
            match PortfolioMemory::open(data_root) {
                Ok(memory) => Self {
                    id,
                    available: true,
                    reason: None,
                    memory: Some(memory),
                },
                Err(e) => {
                    tracing::warn!(
                        "portfolio manager palace unavailable: {e}; palace features disabled"
                    );
                    Self {
                        id,
                        available: false,
                        reason: Some(e.to_string()),
                        memory: None,
                    }
                }
            }
        }

        #[cfg(not(feature = "manager-memory"))]
        {
            let _ = (framework_root, MANAGER_DATA_SUBDIR);
            Self {
                id,
                available: false,
                reason: Some(
                    "portfolio memory engine not compiled (build with --features \
                     manager-memory to enable)"
                        .to_string(),
                ),
            }
        }
    }

    /// The stable portfolio palace id.
    ///
    /// Why: the `GET /manager/version` capabilities stub and every later-phase
    /// consumer needs to name the palace without reaching into private state.
    /// What: returns [`PORTFOLIO_PALACE_ID`].
    /// Test: `portfolio_palace_exposes_stable_id`.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Whether the palace is provisioned and usable.
    ///
    /// Why: manager routes branch on this to decide whether palace-backed
    /// features (digest history, chat-turn persistence) can run; a `false` here
    /// is a normal degraded state, not an error.
    /// What: the `available` flag set by [`Self::provision`].
    /// Test: `portfolio_palace_reports_unavailable_without_feature`,
    /// `portfolio_palace_provisions_and_is_idempotent`.
    pub fn is_available(&self) -> bool {
        self.available
    }

    /// Why the palace is unavailable, when it is.
    ///
    /// Why: surfacing the reason over the capabilities stub turns "palace down"
    /// into an actionable curl-observable signal (missing feature vs. failed
    /// open) rather than a silent gap.
    /// What: the captured reason string, or `None` when available.
    /// Test: `portfolio_palace_reports_unavailable_without_feature`.
    pub fn unavailable_reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }

    /// The live `memory-core` binding, when compiled and available.
    ///
    /// Why: later phases (WI-3 digest history, WI-4 chat turns) read/write the
    /// palace through this handle; exposing it lets those phases attach without
    /// re-plumbing provisioning.
    /// What: `Some` only under `manager-memory` with a successful open.
    /// Test: `portfolio_memory_remember_then_recall_round_trips`.
    #[cfg(feature = "manager-memory")]
    pub fn memory(&self) -> Option<&PortfolioMemory> {
        self.memory.as_ref()
    }
}

/// Structured errors for the portfolio palace subsystem (library → `thiserror`).
///
/// Why: this is library code in `trusty-mpm`; per the workspace convention
/// library errors are typed `thiserror` enums, never `unwrap()`/`panic!`. A
/// memory backend that is unavailable must degrade into a matchable error the
/// provisioning path can log and continue past — never crash the daemon.
/// What: wraps the palace-lifecycle and recall/remember failure surfaces,
/// preserving the underlying `anyhow` source chain.
/// Test: exercised via the feature-gated tests below.
#[cfg(feature = "manager-memory")]
#[derive(Debug, thiserror::Error)]
pub enum PortfolioMemoryError {
    /// Ensuring the portfolio palace exists (open-or-create) failed.
    #[error("portfolio palace '{palace}' unavailable: {source}")]
    Palace {
        /// The portfolio palace id the operation targeted.
        palace: String,
        /// The underlying registry/store error.
        source: anyhow::Error,
    },

    /// A recall / remember operation against the portfolio palace failed.
    #[error("portfolio memory operation '{op}' failed: {source}")]
    Operation {
        /// Short operation tag (`"recall"`, `"remember"`) for actionable logs.
        op: &'static str,
        /// The underlying engine error.
        source: anyhow::Error,
    },
}

/// The live portfolio palace binding (opt-in `manager-memory` feature).
///
/// Why: mirrors [`crate::core::sm::memory::SmMemory`] — a DIRECT `memory-core`
/// library call (not over MCP) scoped to a single [`PalaceId`] so the manager
/// can never touch another namespace. Binding one id for the struct's lifetime
/// makes "manager only ever reads/writes its own portfolio palace" a structural
/// invariant, not caller discipline (the DOC-36 §3.4 "never duplicates project
/// state" guarantee).
/// What: owns the open-handle [`PalaceRegistry`], the on-disk `data_root`, and
/// the bound `palace_id`. Built via [`Self::open`], which idempotently ensures
/// exactly one palace. Phase 1a exposes the minimal remember/recall surface
/// later phases extend; it deliberately does NOT reimplement the SM palace's
/// full API.
/// Test: `portfolio_palace_provisions_and_is_idempotent`,
/// `portfolio_memory_remember_then_recall_round_trips`.
#[cfg(feature = "manager-memory")]
#[derive(Clone)]
pub struct PortfolioMemory {
    /// Open-handle registry (LRU-bounded). Cheap to clone — state is behind `Arc`.
    registry: trusty_common::memory_core::registry::PalaceRegistry,
    /// Directory under which the portfolio palace's `<id>/` subtree lives.
    data_root: std::path::PathBuf,
    /// The single palace this instance is bound to. Never changes.
    palace_id: trusty_common::memory_core::palace::PalaceId,
}

#[cfg(feature = "manager-memory")]
mod live {
    use super::{PORTFOLIO_PALACE_ID, PortfolioMemory, PortfolioMemoryError};

    use std::path::PathBuf;
    use std::sync::Arc;

    use chrono::Utc;
    use trusty_common::memory_core::palace::{Palace, PalaceId, RoomType};
    use trusty_common::memory_core::registry::PalaceRegistry;
    use trusty_common::memory_core::retrieval::{
        PalaceHandle, RecallResult, RememberOptions, recall_with_default_embedder,
    };

    /// Result alias for portfolio memory operations.
    pub type PortfolioMemoryResult<T> = std::result::Result<T, PortfolioMemoryError>;

    /// Default importance assigned to portfolio-remembered drawers.
    ///
    /// Why: manager observations (digest deltas, escalation dispositions) are
    /// curated, high-signal facts — they should rank above incidental capture but
    /// need not be pinned at the ceiling. `0.7` matches the SM palace's curated
    /// importance so ranking behaves consistently across the two daemon palaces.
    /// What: a `0.0..=1.0` weight passed to the write path; the engine re-clamps.
    /// Test: exercised by `portfolio_memory_remember_then_recall_round_trips`.
    const PORTFOLIO_DEFAULT_IMPORTANCE: f32 = 0.7;

    /// How many recall hits the portfolio palace returns by default.
    ///
    /// Why: a small fixed top-k keeps phase-1a recall bounded and deterministic;
    /// later phases can make it configurable if a digest needs a wider window.
    /// What: the `top_k` passed to `recall_with_default_embedder`.
    /// Test: `portfolio_memory_remember_then_recall_round_trips`.
    const PORTFOLIO_RECALL_TOP_K: usize = 8;

    impl PortfolioMemory {
        /// Open the portfolio palace, idempotently ensuring it exists.
        ///
        /// Why: startup must guarantee the single portfolio palace is present
        /// without erroring when it already exists (the daemon restarts against a
        /// populated store). Calling this twice — in one process or across
        /// restarts — must leave EXACTLY ONE palace on disk, never duplicate or
        /// wipe it.
        /// What: opens the registry at `data_root`, binds [`PORTFOLIO_PALACE_ID`],
        /// then eagerly materialises the palace via the race-safe open-or-create
        /// [`Self::ensure_palace`]. Any failure is wrapped in
        /// [`PortfolioMemoryError::Palace`].
        /// Test: `portfolio_palace_provisions_and_is_idempotent`.
        pub fn open(data_root: impl Into<PathBuf>) -> PortfolioMemoryResult<Self> {
            let data_root = data_root.into();
            let palace_id = PalaceId::new(PORTFOLIO_PALACE_ID.to_string());
            let registry = PalaceRegistry::open(&data_root).map_err(|source| {
                PortfolioMemoryError::Palace {
                    palace: palace_id.to_string(),
                    source,
                }
            })?;
            let me = Self {
                registry,
                data_root,
                palace_id,
            };
            me.ensure_palace()?;
            Ok(me)
        }

        /// The palace id this instance is bound to.
        ///
        /// Why: lets tests and the capabilities stub confirm the bound namespace
        /// without reaching into private state, keeping the "manager-only" scope
        /// auditable.
        /// What: returns the bound [`PalaceId`].
        /// Test: `portfolio_palace_provisions_and_is_idempotent`.
        pub fn palace_id(&self) -> &PalaceId {
            &self.palace_id
        }

        /// Open-or-create the bound palace, returning a live handle (race-safe).
        ///
        /// Why: every operation needs a handle; centralising the idempotent
        /// open-or-create here means one well-tested path enforces "exactly one
        /// palace" for both construction and every later call, closing the
        /// TOCTOU window a `exists()`-then-branch form would open.
        /// What: returns the cached handle if registered; else tries `open_palace`
        /// (succeeds when `palace.json` is already on disk, including a concurrent
        /// creator that just finished) and falls back to the idempotent
        /// `create_palace`. Both branches are scoped entirely to `self.palace_id`.
        /// Test: `portfolio_palace_provisions_and_is_idempotent`.
        fn ensure_palace(&self) -> PortfolioMemoryResult<Arc<PalaceHandle>> {
            if let Some(handle) = self.registry.get(&self.palace_id) {
                return Ok(handle);
            }
            if let Ok(handle) = self.registry.open_palace(&self.data_root, &self.palace_id) {
                return Ok(handle);
            }
            let palace = Palace {
                id: self.palace_id.clone(),
                name: "tm manager (portfolio)".to_string(),
                description: Some(
                    "Portfolio-scoped tm manager palace (DOC-36 §3.4): digest \
                     history, escalation dispositions, and portfolio chat turns — \
                     manager-level observations about the portfolio, never \
                     project-scoped facts."
                        .to_string(),
                ),
                created_at: Utc::now(),
                data_dir: self.data_root.join(self.palace_id.as_str()),
            };
            self.registry
                .create_palace(&self.data_root, palace)
                .map_err(|source| PortfolioMemoryError::Palace {
                    palace: self.palace_id.to_string(),
                    source,
                })
        }

        /// Remember a curated, short-fact-tolerant observation into the palace.
        ///
        /// Why: the single write path later phases persist digest deltas,
        /// escalation dispositions, and chat turns through. Routing every write
        /// through the bound palace id guarantees the manager never writes
        /// elsewhere; the curated-note preset lets terse observations land past
        /// the engine's min-token gate.
        /// What: ensures the palace, then calls `remember_with_options` in the
        /// `General` room at [`PORTFOLIO_DEFAULT_IMPORTANCE`] with the supplied
        /// `tags` and [`RememberOptions::note`]. Returns the new drawer's UUID.
        /// Test: `portfolio_memory_remember_then_recall_round_trips`.
        pub async fn remember(
            &self,
            text: impl Into<String>,
            tags: Vec<String>,
        ) -> PortfolioMemoryResult<String> {
            let handle = self.ensure_palace()?;
            handle
                .remember_with_options(
                    text.into(),
                    RoomType::General,
                    tags,
                    PORTFOLIO_DEFAULT_IMPORTANCE,
                    RememberOptions::note(),
                )
                .await
                .map(|id| id.to_string())
                .map_err(|source| PortfolioMemoryError::Operation {
                    op: "remember",
                    source,
                })
        }

        /// Recall the top-k portfolio observations matching `query`.
        ///
        /// Why: feeds later-phase framing ("what changed since the last digest")
        /// from the manager's own durable observations. Scoped to the portfolio
        /// palace only, so recall can never surface another namespace's facts.
        /// What: ensures the palace, then runs `recall_with_default_embedder`
        /// against the bound handle with [`PORTFOLIO_RECALL_TOP_K`].
        /// Test: `portfolio_memory_remember_then_recall_round_trips`.
        pub async fn recall(&self, query: &str) -> PortfolioMemoryResult<Vec<RecallResult>> {
            let handle = self.ensure_palace()?;
            recall_with_default_embedder(&handle, query, PORTFOLIO_RECALL_TOP_K)
                .await
                .map_err(|source| PortfolioMemoryError::Operation {
                    op: "recall",
                    source,
                })
        }

        /// Number of palaces currently persisted under the portfolio data root.
        ///
        /// Why: the idempotency contract ("create twice → one palace") is most
        /// directly asserted by counting palaces on disk; exposing this keeps the
        /// test honest without reaching into registry internals.
        /// What: lists persisted palaces via [`PalaceRegistry::list_palaces`] and
        /// returns the count. Wrapped errors use [`PortfolioMemoryError::Palace`].
        /// Test: `portfolio_palace_provisions_and_is_idempotent`.
        pub fn persisted_palace_count(&self) -> PortfolioMemoryResult<usize> {
            PalaceRegistry::list_palaces(&self.data_root)
                .map(|palaces| palaces.len())
                .map_err(|source| PortfolioMemoryError::Palace {
                    palace: self.palace_id.to_string(),
                    source,
                })
        }
    }
}

#[cfg(test)]
#[path = "memory_tests.rs"]
mod tests;
