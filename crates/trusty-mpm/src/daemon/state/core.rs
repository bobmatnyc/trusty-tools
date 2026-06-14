//! [`DaemonState`] struct definition, constants, and constructors.
//!
//! Why: the struct and its three constructors are a natural single unit;
//! splitting them from the method groups (sessions, resources) keeps this
//! file under the SLOC cap while the struct definition stays readable.
//! What: defines [`DaemonState`], [`ReapResult`], and the three constructors
//! (`new`, `shared`, `with_root`, `with_paths`).
//! Test: see `super::tests`.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use dashmap::DashMap;
use parking_lot::{Mutex, RwLock};

use crate::core::agent::Delegation;
use crate::core::circuit::{CircuitBreaker, CircuitConfig};
use crate::core::hook::HookEventRecord;
use crate::core::memory::{MemoryConfig, MemoryUsage};
use crate::core::overseer::Overseer;
use crate::core::overseer_config::OverseerConfig;
use crate::core::paths::FrameworkPaths;
use crate::core::project::ProjectInfo;
use crate::core::session::{Session, SessionId};

use crate::daemon::audit::AuditLogger;
use crate::daemon::optimizer::OptimizerConfig;

use super::overseer::{build_overseer, load_optimizer_config, load_overseer, make_audit_logger};

/// Outcome of a reap sweep over the session registry.
///
/// Why: the reaper now does two distinct things — it *removes* tmux sessions
/// whose tmux window is gone, and it *marks Stopped* alive tmux sessions whose
/// tracked `claude` process has exited. Callers (and the dashboard) need to
/// tell those apart, so the sweep reports both counts.
/// What: `reaped` is the number of entries deleted from the registry;
/// `stopped` is the number transitioned to [`SessionStatus::Stopped`] in place.
/// Test: `reap_dead_sessions`, `reap_marks_stopped_when_pid_dead`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReapResult {
    /// Sessions removed from the registry (tmux session gone).
    pub reaped: usize,
    /// Sessions transitioned to `Stopped` (tmux alive but `claude` process dead).
    pub stopped: usize,
}

/// How many recent hook events the daemon retains for the dashboard feed.
///
/// Why: the live event feed needs scrollback, but an unbounded log would leak
/// memory in a long-lived daemon; a ring buffer caps it.
pub const HOOK_HISTORY_LIMIT: usize = 1024;

/// Capacity of the SSE event broadcast channel.
///
/// Why: `tokio::sync::broadcast` is a fixed-size ring buffer; late subscribers
/// that fall behind drop the oldest events. 1024 frames is generous for a UI
/// feed but still cheap memory-wise (each frame is a `serde_json::Value` Arc).
pub const EVENT_CHANNEL_CAPACITY: usize = 1024;

/// How long a one-time bot pairing code stays valid after it is issued.
///
/// Why: a pairing code is a low-entropy secret; a short five-minute window
/// limits the time an intercepted code is useful.
pub const PAIR_CODE_TTL: std::time::Duration = std::time::Duration::from_secs(300);

/// The daemon's shared, mutable view of the world.
///
/// Why: shared via `Arc<DaemonState>` into every axum handler and the MCP
/// backend — one source of truth, no global statics.
/// What: concurrent maps for sessions / delegations / breakers / memory, plus
/// a mutex-guarded ring buffer of hook events and the threshold configs.
/// Test: `register_and_list_sessions`, `hook_history_is_bounded`.
#[derive(Debug)]
pub struct DaemonState {
    /// Managed sessions, keyed by id.
    pub(super) sessions: DashMap<SessionId, Session>,
    /// Active delegations, keyed by delegation id.
    pub(super) delegations: DashMap<uuid::Uuid, Delegation>,
    /// Circuit breakers, keyed by agent name.
    pub(super) breakers: DashMap<String, CircuitBreaker>,
    /// Latest token-usage snapshot per session.
    pub(super) memory: DashMap<SessionId, MemoryUsage>,
    /// Bounded ring buffer of the most recent hook events.
    pub(super) hook_history: Mutex<std::collections::VecDeque<HookEventRecord>>,
    /// Memory-protection thresholds (warn / alert / compact).
    pub memory_config: MemoryConfig,
    /// Circuit-breaker tuning applied to newly-seen agents.
    pub circuit_config: CircuitConfig,
    /// Discovered trusty sidecar service addresses, set once at startup.
    pub(super) trusty_addrs: Mutex<Option<crate::daemon::discover::TrustyAddrs>>,
    /// Token-use optimizer config; read on every PostToolUse, updatable at
    /// runtime via the HTTP API, hence behind an `RwLock`.
    pub(super) optimizer: Arc<parking_lot::RwLock<OptimizerConfig>>,
    /// Registered projects, keyed by their absolute working-directory path.
    ///
    /// Why: sessions are grouped by project; the `project` subcommands and the
    /// dashboard read this registry. An `RwLock<HashMap>` suits a low-churn
    /// registry that is read far more often than written.
    pub(super) projects: Arc<RwLock<HashMap<PathBuf, ProjectInfo>>>,
    /// Session overseer — evaluates hook events for allow/block/respond/flag.
    ///
    /// Why: oversight is a pluggable strategy; the daemon holds it behind
    /// `dyn Overseer` so the deterministic and LLM implementations are
    /// interchangeable. Opt-in: a disabled overseer fast-paths every call.
    pub(super) overseer: Arc<dyn Overseer>,
    /// Name of the active overseer strategy, for the `GET /overseer` endpoint
    /// and the audit log (`"deterministic"` or `"composite-llm"`).
    pub(super) overseer_handler: String,
    /// Standalone LLM overseer for the interactive `POST /llm/chat` endpoint.
    ///
    /// Why: the overseer composed into `overseer` is hidden behind
    /// `dyn Overseer`, which has no `chat` method; the chat endpoint needs the
    /// concrete [`LlmOverseer`]. It is `Some` only when an OpenRouter API key
    /// resolved — i.e. exactly when LLM chat is available.
    /// Test: `llm_overseer_is_none_without_key`.
    pub(super) llm: Option<Arc<crate::daemon::llm_overseer::LlmOverseer>>,
    /// Append-only JSONL logger for every overseer decision.
    pub(super) audit: Arc<AuditLogger>,
    /// The Telegram chat id paired with this daemon, when one has confirmed a
    /// pairing code.
    ///
    /// Why: the Telegram bot pairs a single chat with the daemon so push alerts
    /// have an unambiguous destination; the chat id is stored here after a
    /// successful `/pair` handshake.
    /// What: `None` until a pairing completes, then the confirmed chat id.
    /// Test: `pairing_round_trip`.
    pub(super) paired_chat_id: Mutex<Option<i64>>,
    /// The outstanding one-time pairing code and the instant it was issued.
    ///
    /// Why: `tm pair` generates a short code valid for five minutes; the daemon
    /// must remember it (with its issue time, for TTL enforcement) until a
    /// `/pair` confirm consumes it or it expires.
    /// What: `None` when no code is outstanding, else `(code, issued_at)`.
    /// Test: `pairing_round_trip`, `expired_pair_code_is_rejected`.
    pub(super) pair_code: Mutex<Option<(String, std::time::Instant)>>,
    /// The `~/.trusty-mpm` directory the daemon persists state under.
    ///
    /// Why: the pairing record (`pairing.json`) must survive restarts; it is
    /// written under this root. Holding the resolved path means tests can point
    /// it at a temp directory while production uses the home-relative root.
    /// What: the framework root, the directory `pairing.json` lives in.
    /// Test: `pairing_persists_to_disk`.
    pub(super) framework_root: PathBuf,
    /// Broadcast channel for live hook events.
    ///
    /// Why: the GUI and other real-time consumers subscribe to a Server-Sent
    /// Events stream rather than polling `GET /events/poll`. A
    /// `tokio::sync::broadcast` channel fans one publish out to every active
    /// SSE subscriber. The payload is `serde_json::Value` so the broadcast is
    /// generic across event shapes and avoids tying every consumer to a
    /// specific Rust type.
    /// What: the sender side of the channel; SSE handlers call
    /// [`Self::event_subscribe`] to obtain a `Receiver`.
    /// Test: `ingest_hook_broadcasts_to_subscribers` exercises subscribe + publish.
    pub event_tx: tokio::sync::broadcast::Sender<serde_json::Value>,
}

impl Default for DaemonState {
    fn default() -> Self {
        Self::new()
    }
}

impl DaemonState {
    /// Construct empty state with default thresholds.
    ///
    /// Why: the optimizer and overseer policies are framework-managed on disk
    /// (`~/.trusty-mpm/framework/hooks/`); the daemon must reflect whatever the
    /// installed framework declares without an API round-trip.
    /// What: reads the optimizer config from
    /// [`FrameworkPaths::optimizer_config`] and the overseer policy from
    /// [`FrameworkPaths::overseer_config`], falling back to safe defaults when
    /// either file is missing (framework not yet installed) or unparseable
    /// (logged, not fatal); builds the audit logger under `~/.trusty-mpm/logs`.
    /// Test: `new_reads_default_when_optimizer_file_missing`,
    /// `new_overseer_is_disabled_when_file_missing`.
    pub fn new() -> Self {
        let optimizer = load_optimizer_config();
        let build = load_overseer();
        let framework_root = FrameworkPaths::default().root;
        // Restore a persisted Telegram pairing so push alerts survive restarts.
        let paired = crate::daemon::pairing_store::load(&framework_root).map(|r| r.chat_id);
        if let Some(chat_id) = paired {
            tracing::info!("restored persisted Telegram pairing (chat {chat_id})");
        }
        let (event_tx, _) = tokio::sync::broadcast::channel(EVENT_CHANNEL_CAPACITY);
        Self {
            sessions: DashMap::new(),
            delegations: DashMap::new(),
            breakers: DashMap::new(),
            memory: DashMap::new(),
            hook_history: Mutex::new(VecDeque::with_capacity(HOOK_HISTORY_LIMIT)),
            memory_config: MemoryConfig::default(),
            circuit_config: CircuitConfig::default(),
            trusty_addrs: Mutex::new(None),
            optimizer: Arc::new(parking_lot::RwLock::new(optimizer)),
            projects: Arc::new(RwLock::new(HashMap::new())),
            overseer: build.overseer,
            overseer_handler: build.handler,
            llm: build.llm,
            audit: make_audit_logger(&framework_root),
            paired_chat_id: Mutex::new(paired),
            pair_code: Mutex::new(None),
            framework_root,
            event_tx,
        }
    }

    /// Wrap the state in an `Arc` for sharing across tasks.
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::new())
    }

    /// Construct default state whose persisted pairing lives under `root`.
    ///
    /// Why: pairing now writes `pairing.json` to disk; tests that exercise
    /// confirm / clear must redirect that write to a temp directory so they
    /// never touch (or depend on) the operator's real `~/.trusty-mpm`.
    /// What: builds [`DaemonState::new`]'s defaults but overrides the framework
    /// root with `root`, re-reading any pairing record already under it.
    /// Test: `pairing_persists_to_disk`, `pairing_reset_clears_disk`.
    #[doc(hidden)]
    pub fn with_root(root: PathBuf) -> Self {
        let mut state = Self::new();
        let paired = crate::daemon::pairing_store::load(&root).map(|r| r.chat_id);
        *state.paired_chat_id.lock() = paired;
        state.framework_root = root;
        state
    }

    /// Construct state whose framework-managed config is read from `paths`.
    ///
    /// Why: [`DaemonState::new`] reads the optimizer / overseer policy and the
    /// audit log location from the real `~/.trusty-mpm` install. End-to-end
    /// tests must point those reads at a hermetic temp directory instead so a
    /// test never touches (or depends on) the operator's real framework. This
    /// constructor takes an explicit [`FrameworkPaths`] — typically built with
    /// [`FrameworkPaths::under`] against a `tempfile::TempDir`.
    /// What: loads `optimizer.toml` / `overseer.toml` from `paths.hooks` and
    /// builds the audit logger under `paths.root/logs`, falling back to safe
    /// defaults exactly as [`DaemonState::new`] does when a file is absent.
    /// Test: the `e2e` integration suite (`test_optimizer`, `test_overseer`).
    pub fn with_paths(paths: &FrameworkPaths) -> Self {
        let optimizer = match OptimizerConfig::load_from_file(&paths.optimizer_config()) {
            Ok(cfg) => cfg,
            Err(e) => {
                tracing::warn!("failed to load optimizer config: {e}; using defaults");
                OptimizerConfig::default()
            }
        };
        let overseer_cfg = OverseerConfig::load_from(&paths.overseer_config());
        let build = build_overseer(overseer_cfg);
        let framework_root = paths.root.clone();
        let paired = crate::daemon::pairing_store::load(&framework_root).map(|r| r.chat_id);
        let (event_tx, _) = tokio::sync::broadcast::channel(EVENT_CHANNEL_CAPACITY);
        Self {
            sessions: DashMap::new(),
            delegations: DashMap::new(),
            breakers: DashMap::new(),
            memory: DashMap::new(),
            hook_history: Mutex::new(VecDeque::with_capacity(HOOK_HISTORY_LIMIT)),
            memory_config: MemoryConfig::default(),
            circuit_config: CircuitConfig::default(),
            trusty_addrs: Mutex::new(None),
            optimizer: Arc::new(parking_lot::RwLock::new(optimizer)),
            projects: Arc::new(RwLock::new(HashMap::new())),
            overseer: build.overseer,
            overseer_handler: build.handler,
            llm: build.llm,
            audit: make_audit_logger(&framework_root),
            paired_chat_id: Mutex::new(paired),
            pair_code: Mutex::new(None),
            framework_root,
            event_tx,
        }
    }
}
