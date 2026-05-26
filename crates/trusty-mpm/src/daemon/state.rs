//! Shared daemon state.
//!
//! Why: the HTTP API, the MCP server, the hook relay, and the dashboard feed
//! all read and mutate the same picture of the world — managed sessions, their
//! delegation trees, per-agent circuit breakers, recent hook events, and
//! per-session memory usage. A single `Arc`-shared, lock-guarded state keeps
//! them consistent and is the daemon's composition root for dependency
//! injection into request handlers.
//! What: [`DaemonState`] holds `DashMap`s keyed by `SessionId`/agent name plus
//! a bounded ring buffer of recent [`HookEventRecord`]s; methods provide the
//! typed mutations the rest of the daemon needs.
//! Test: `cargo test -p trusty-mpm-daemon` exercises registration, the hook
//! ring-buffer bound, and memory-pressure classification.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use crate::core::agent::Delegation;
use crate::core::circuit::{CircuitBreaker, CircuitConfig};
use crate::core::deterministic_overseer::DeterministicOverseer;
use crate::core::hook::HookEventRecord;
use crate::core::memory::{MemoryConfig, MemoryPressure, MemoryUsage};
use crate::core::overseer::Overseer;
use crate::core::overseer_config::OverseerConfig;
use crate::core::paths::FrameworkPaths;
use crate::core::project::ProjectInfo;
use crate::core::session::{Session, SessionId};
use dashmap::DashMap;
use parking_lot::{Mutex, RwLock};

use super::audit::AuditLogger;
use super::optimizer::OptimizerConfig;
use super::tmux::TmuxDriver;

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
    sessions: DashMap<SessionId, Session>,
    /// Active delegations, keyed by delegation id.
    delegations: DashMap<uuid::Uuid, Delegation>,
    /// Circuit breakers, keyed by agent name.
    breakers: DashMap<String, CircuitBreaker>,
    /// Latest token-usage snapshot per session.
    memory: DashMap<SessionId, MemoryUsage>,
    /// Bounded ring buffer of the most recent hook events.
    hook_history: Mutex<VecDeque<HookEventRecord>>,
    /// Memory-protection thresholds (warn / alert / compact).
    pub memory_config: MemoryConfig,
    /// Circuit-breaker tuning applied to newly-seen agents.
    pub circuit_config: CircuitConfig,
    /// Discovered trusty sidecar service addresses, set once at startup.
    trusty_addrs: Mutex<Option<super::discover::TrustyAddrs>>,
    /// Token-use optimizer config; read on every PostToolUse, updatable at
    /// runtime via the HTTP API, hence behind an `RwLock`.
    optimizer: Arc<parking_lot::RwLock<OptimizerConfig>>,
    /// Registered projects, keyed by their absolute working-directory path.
    ///
    /// Why: sessions are grouped by project; the `project` subcommands and the
    /// dashboard read this registry. An `RwLock<HashMap>` suits a low-churn
    /// registry that is read far more often than written.
    projects: Arc<RwLock<HashMap<PathBuf, ProjectInfo>>>,
    /// Session overseer — evaluates hook events for allow/block/respond/flag.
    ///
    /// Why: oversight is a pluggable strategy; the daemon holds it behind
    /// `dyn Overseer` so the deterministic and LLM implementations are
    /// interchangeable. Opt-in: a disabled overseer fast-paths every call.
    overseer: Arc<dyn Overseer>,
    /// Name of the active overseer strategy, for the `GET /overseer` endpoint
    /// and the audit log (`"deterministic"` or `"composite-llm"`).
    overseer_handler: String,
    /// Standalone LLM overseer for the interactive `POST /llm/chat` endpoint.
    ///
    /// Why: the overseer composed into `overseer` is hidden behind
    /// `dyn Overseer`, which has no `chat` method; the chat endpoint needs the
    /// concrete [`LlmOverseer`]. It is `Some` only when an OpenRouter API key
    /// resolved — i.e. exactly when LLM chat is available.
    /// Test: `llm_overseer_is_none_without_key`.
    llm: Option<Arc<super::llm_overseer::LlmOverseer>>,
    /// Append-only JSONL logger for every overseer decision.
    audit: Arc<AuditLogger>,
    /// The Telegram chat id paired with this daemon, when one has confirmed a
    /// pairing code.
    ///
    /// Why: the Telegram bot pairs a single chat with the daemon so push alerts
    /// have an unambiguous destination; the chat id is stored here after a
    /// successful `/pair` handshake.
    /// What: `None` until a pairing completes, then the confirmed chat id.
    /// Test: `pairing_round_trip`.
    paired_chat_id: Mutex<Option<i64>>,
    /// The outstanding one-time pairing code and the instant it was issued.
    ///
    /// Why: `tm pair` generates a short code valid for five minutes; the daemon
    /// must remember it (with its issue time, for TTL enforcement) until a
    /// `/pair` confirm consumes it or it expires.
    /// What: `None` when no code is outstanding, else `(code, issued_at)`.
    /// Test: `pairing_round_trip`, `expired_pair_code_is_rejected`.
    pair_code: Mutex<Option<(String, std::time::Instant)>>,
    /// The `~/.trusty-mpm` directory the daemon persists state under.
    ///
    /// Why: the pairing record (`pairing.json`) must survive restarts; it is
    /// written under this root. Holding the resolved path means tests can point
    /// it at a temp directory while production uses the home-relative root.
    /// What: the framework root, the directory `pairing.json` lives in.
    /// Test: `pairing_persists_to_disk`.
    framework_root: PathBuf,
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

/// Read the optimizer policy from the installed framework, never failing.
///
/// Why: daemon startup must not abort because the framework is not installed
/// or its policy file is malformed; a sensible default keeps the daemon usable.
/// What: loads `~/.trusty-mpm/framework/hooks/optimizer.toml` via
/// [`OptimizerConfig::load_from_file`], logging and falling back to
/// `OptimizerConfig::default()` on any error.
/// Test: `new_reads_default_when_optimizer_file_missing`,
/// `reload_optimizer_config_picks_up_file_changes`.
fn load_optimizer_config() -> OptimizerConfig {
    let path = FrameworkPaths::default().optimizer_config();
    match OptimizerConfig::load_from_file(&path) {
        Ok(cfg) => cfg,
        Err(e) => {
            tracing::warn!(
                "failed to load optimizer config from {}: {e}; using defaults",
                path.display()
            );
            OptimizerConfig::default()
        }
    }
}

/// Build the session overseer from the installed framework policy.
///
/// Why: oversight is framework-managed and opt-in; daemon startup must reflect
/// `~/.trusty-mpm/framework/hooks/overseer.toml` (or a safe disabled default
/// when it is absent) without ever failing to construct.
/// What: loads [`OverseerConfig`] from [`FrameworkPaths::overseer_config`] and
/// builds the overseer via [`build_overseer`]; a missing/malformed file yields
/// the disabled default config (handled inside `OverseerConfig::load_from`).
/// Test: `new_overseer_is_disabled_when_file_missing`.
fn load_overseer() -> OverseerBuild {
    let path = FrameworkPaths::default().overseer_config();
    build_overseer(OverseerConfig::load_from(&path))
}

/// The overseer strategy plus the optional standalone LLM chat handler.
///
/// Why: [`build_overseer`] resolves both the `dyn Overseer` used for hook
/// oversight and — when an OpenRouter key is present — a concrete
/// [`LlmOverseer`] reused for the `POST /llm/chat` endpoint; returning them as
/// one named struct keeps both daemon constructors aligned.
/// What: the composed overseer, its handler name, and `Some(LlmOverseer)` when
/// LLM chat is available.
/// Test: `overseer_is_deterministic_without_llm`.
struct OverseerBuild {
    /// The composed overseer used on the hook path.
    overseer: Arc<dyn Overseer>,
    /// The active strategy name (`"deterministic"` or `"composite-llm"`).
    handler: String,
    /// Standalone LLM overseer for interactive chat, when a key resolved.
    llm: Option<Arc<crate::daemon::llm_overseer::LlmOverseer>>,
}

/// Assemble the overseer strategy from a loaded [`OverseerConfig`].
///
/// Why: the daemon may run rule-based oversight alone, or compose it with the
/// LLM overseer when `[llm] enabled = true` *and* an API key is present.
/// Deciding the strategy in one place keeps `new()` / `with_paths()` aligned.
/// What: always builds a [`DeterministicOverseer`]; when the LLM section is
/// enabled and the configured API key resolves, wraps both in a
/// [`CompositeOverseer`] (deterministic first, LLM for uncertain cases).
/// Returns the overseer, its handler name, and the standalone LLM chat handler.
/// Test: `overseer_is_deterministic_without_llm`,
/// `overseer_falls_back_when_llm_key_missing`.
fn build_overseer(config: OverseerConfig) -> OverseerBuild {
    let deterministic = DeterministicOverseer::new(config.clone());
    if config.llm.enabled {
        let llm = Arc::new(crate::daemon::llm_overseer::LlmOverseer::new(
            config.llm.model.clone(),
            &config.llm.api_key_env,
        ));
        if llm.is_enabled() {
            tracing::info!(
                "LLM overseer active (model {}); composing with deterministic rules",
                config.llm.model
            );
            // The composite needs an owned overseer; build a second
            // `LlmOverseer` for it so the `Arc` above stays free for chat.
            let composite_llm = crate::daemon::llm_overseer::LlmOverseer::new(
                config.llm.model.clone(),
                &config.llm.api_key_env,
            );
            let composite = crate::daemon::overseer_compose::CompositeOverseer::new(
                Box::new(deterministic),
                Box::new(composite_llm),
            );
            return OverseerBuild {
                overseer: Arc::new(composite),
                handler: "composite-llm".to_string(),
                llm: Some(llm),
            };
        }
        tracing::warn!(
            "[llm] enabled but no API key in ${}; falling back to deterministic overseer",
            config.llm.api_key_env
        );
    }
    OverseerBuild {
        overseer: Arc::new(deterministic),
        handler: "deterministic".to_string(),
        llm: None,
    }
}

/// Resolve the daemon's logs directory (`~/.trusty-mpm/logs`).
///
/// Why: the audit logger writes under a single well-known directory; resolving
/// it via the home directory keeps it consistent with the framework root.
/// What: returns `<home>/.trusty-mpm/logs`, falling back to `./.trusty-mpm/logs`
/// when the home directory cannot be determined.
/// Test: exercised indirectly by `new_builds_audit_logger`.
fn logs_dir() -> PathBuf {
    FrameworkPaths::default().root.join("logs")
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
            audit: Arc::new(AuditLogger::new(&logs_dir())),
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
            audit: Arc::new(AuditLogger::new(&paths.root.join("logs"))),
            paired_chat_id: Mutex::new(paired),
            pair_code: Mutex::new(None),
            framework_root,
            event_tx,
        }
    }

    // ---- bot pairing ----------------------------------------------------

    /// Generate and store a one-time pairing code.
    ///
    /// Why: `tm pair` asks the daemon for a short code the operator types into
    /// the Telegram bot; the daemon must remember it (and its issue time) so a
    /// later `/pair` confirm can validate it within the TTL window.
    /// What: derives a six-character uppercase alphanumeric code from a fresh
    /// UUID, stores it with the current instant, and returns the code.
    /// Test: `pairing_round_trip`.
    pub fn generate_pair_code(&self) -> String {
        let code: String = uuid::Uuid::new_v4()
            .simple()
            .to_string()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .take(6)
            .collect::<String>()
            .to_uppercase();
        *self.pair_code.lock() = Some((code.clone(), std::time::Instant::now()));
        code
    }

    /// Confirm a pairing code and register `chat_id` on success.
    ///
    /// Why: the bot's `/pair <code>` flow validates the operator's code and, on
    /// success, binds the chat so push alerts have a destination — and the
    /// binding must survive a daemon restart.
    /// What: returns `true` and stores `chat_id` (in memory *and* persisted to
    /// `~/.trusty-mpm/pairing.json`) when `code` matches the outstanding code
    /// and it is within [`PAIR_CODE_TTL`]; clears the code either way (a used or
    /// expired code never validates twice). A failed disk write is logged, not
    /// fatal — the in-memory pairing still takes effect.
    /// Test: `pairing_round_trip`, `pairing_persists_to_disk`.
    pub fn confirm_pair_code(&self, code: &str, chat_id: i64) -> bool {
        let mut guard = self.pair_code.lock();
        let valid = matches!(
            guard.as_ref(),
            Some((stored, issued))
                if stored == code && issued.elapsed() < PAIR_CODE_TTL
        );
        *guard = None;
        if valid {
            *self.paired_chat_id.lock() = Some(chat_id);
            let record = crate::daemon::pairing_store::PairingRecord::new(chat_id);
            if let Err(e) = crate::daemon::pairing_store::save(&self.framework_root, &record) {
                tracing::warn!("failed to persist Telegram pairing: {e}");
            }
        }
        valid
    }

    /// Clear the Telegram pairing, in memory and on disk.
    ///
    /// Why: `POST /pair/reset` (or any explicit unpair) must drop the binding so
    /// a restart does not resurrect it from `pairing.json`.
    /// What: sets `paired_chat_id` to `None` and deletes the persisted record;
    /// a failed delete is logged, not fatal.
    /// Test: `pairing_reset_clears_disk`.
    pub fn clear_pairing(&self) {
        *self.paired_chat_id.lock() = None;
        if let Err(e) = crate::daemon::pairing_store::clear(&self.framework_root) {
            tracing::warn!("failed to delete persisted Telegram pairing: {e}");
        }
    }

    /// The chat id currently paired with this daemon, if any.
    ///
    /// Why: `GET /pair/status` and the alert loop need the paired destination.
    /// What: returns the stored chat id, or `None` when unpaired.
    /// Test: `pairing_round_trip`.
    pub fn paired_chat_id(&self) -> Option<i64> {
        *self.paired_chat_id.lock()
    }

    // ---- sessions -------------------------------------------------------

    /// Register (or replace) a managed session.
    pub fn register_session(&self, session: Session) {
        self.sessions.insert(session.id, session);
    }

    /// Record the OS-level `claude` process PID on a registered session.
    ///
    /// Why: the CLI and the daemon discover the real `claude` PID inside a tmux
    /// pane *after* launch; reporting it back lets the reaper check process
    /// liveness rather than relying on the tmux session alone.
    /// What: sets `session.pid = Some(pid)` under a write guard; returns `true`
    /// when the session existed, `false` for an unknown id.
    /// Test: `set_session_pid_updates_field`.
    pub fn set_session_pid(&self, id: SessionId, pid: u32) -> bool {
        self.update_session(&id, |s| s.pid = Some(pid))
    }

    /// Remove a session and its associated memory snapshot.
    pub fn remove_session(&self, id: SessionId) -> Option<Session> {
        self.memory.remove(&id);
        self.sessions.remove(&id).map(|(_, s)| s)
    }

    /// Snapshot all managed sessions.
    pub fn list_sessions(&self) -> Vec<Session> {
        self.sessions.iter().map(|e| e.value().clone()).collect()
    }

    /// Look up one session by id.
    pub fn session(&self, id: SessionId) -> Option<Session> {
        self.sessions.get(&id).map(|e| e.value().clone())
    }

    /// Mutate an existing session in place under a write lock.
    ///
    /// Why: the pause/resume handlers must change a session's `status`,
    /// `paused_at`, and `pause_summary` atomically without the read-modify-write
    /// race of `session()` + `register_session()`.
    /// What: takes a write guard on the session entry and calls `f` if the
    /// session exists; returns `true` when it ran, `false` for an unknown id.
    /// Test: `update_session_mutates_existing`, `update_session_missing_is_false`.
    pub fn update_session<F>(&self, id: &SessionId, f: F) -> bool
    where
        F: FnOnce(&mut Session),
    {
        match self.sessions.get_mut(id) {
            Some(mut entry) => {
                f(entry.value_mut());
                true
            }
            None => false,
        }
    }

    /// Snapshot the sessions belonging to one project.
    ///
    /// Why: `GET /sessions?project=<path>` and `trusty-mpm session list`
    /// scope the listing to the caller's project.
    /// What: returns every session whose `project_path` equals `path`.
    /// Test: `list_sessions_for_project_filters`.
    pub fn list_sessions_for_project(&self, path: &std::path::Path) -> Vec<Session> {
        self.sessions
            .iter()
            .filter(|e| e.value().project_path.as_deref() == Some(path))
            .map(|e| e.value().clone())
            .collect()
    }

    /// Look up one session by id or by friendly tmux name.
    ///
    /// Why: the `session stop` / `session info` subcommands accept either a
    /// UUID or the friendly `tmpm-<adj>-<noun>` name the daemon prints on
    /// start; resolving both keeps the CLI ergonomic.
    /// What: tries to parse `key` as a UUID first; on failure scans the
    /// registry for a session whose `tmux_name` matches.
    /// Test: `find_session_by_id_or_name`.
    pub fn find_session(&self, key: &str) -> Option<Session> {
        if let Ok(uuid) = uuid::Uuid::parse_str(key) {
            return self.session(SessionId(uuid));
        }
        self.sessions
            .iter()
            .find(|e| e.value().tmux_name == key)
            .map(|e| e.value().clone())
    }

    /// Drop dead tmux sessions and mark Stopped ones whose process has exited.
    ///
    /// Why: sessions accumulate forever otherwise — a dead tmux session leaves a
    /// stale registry entry behind. Additionally a tmux session can outlive the
    /// `claude` process inside it (the pane drops to a bare shell); such a
    /// session should be visibly `Stopped`, not silently "active". The daemon's
    /// housekeeping loop calls this periodically, and `DELETE /sessions/dead`
    /// calls it on demand.
    /// What: discovers the live tmux session names via `driver.list_sessions()`,
    /// then delegates to [`reap_against`](Self::reap_against). A failed tmux
    /// listing reaps nothing (returns a zeroed [`ReapResult`]) rather than
    /// wrongly deleting every session.
    /// Test: `reap_dead_sessions`, `reap_marks_stopped_when_pid_dead`.
    pub fn reap_dead_sessions(&self, driver: &TmuxDriver) -> ReapResult {
        let live: std::collections::HashSet<String> = match driver.list_sessions() {
            Ok(sessions) => sessions.into_iter().map(|s| s.name).collect(),
            Err(e) => {
                tracing::warn!("reap skipped — tmux list-sessions failed: {e}");
                return ReapResult::default();
            }
        };
        self.reap_against(&live)
    }

    /// Remove dead tmux sessions and mark Stopped ones with a dead process.
    ///
    /// Why: separating the set-difference logic from the tmux call makes the
    /// reaping rule unit-testable without spawning a tmux process. Native
    /// (`SessionHost::Native`) sessions have no tmux session, so the tmux
    /// liveness check must skip them — otherwise every discovered Terminal.app
    /// process would be reaped the instant after it was discovered.
    /// What: for tmux-origin sessions —
    /// - if the `tmux_name` is absent from `live`, the entry is removed;
    /// - if the `tmux_name` is alive but the session has a tracked `pid` whose
    ///   `claude` process has exited, the session is marked
    ///   [`SessionStatus::Stopped`] in place (kept so the operator can see it).
    ///
    /// Returns the [`ReapResult`] with both counts. Native sessions are left
    /// untouched.
    /// Test: `reap_dead_sessions`, `reap_keeps_native_sessions`,
    /// `reap_marks_stopped_when_pid_dead`.
    fn reap_against(&self, live: &std::collections::HashSet<String>) -> ReapResult {
        use crate::core::session::{SessionHost, SessionStatus};

        let mut dead: Vec<SessionId> = Vec::new();
        let mut stopped_ids: Vec<SessionId> = Vec::new();
        for entry in self.sessions.iter() {
            let session = entry.value();
            if session.origin != SessionHost::Tmux {
                continue;
            }
            if !live.contains(&session.tmux_name) {
                dead.push(*entry.key());
            } else if session.status != SessionStatus::Stopped
                && let Some(pid) = session.pid
                && !crate::core::process::is_process_alive(pid)
            {
                stopped_ids.push(*entry.key());
            }
        }
        for id in &dead {
            self.remove_session(*id);
        }
        for id in &stopped_ids {
            self.update_session(id, |s| s.status = SessionStatus::Stopped);
        }
        ReapResult {
            reaped: dead.len(),
            stopped: stopped_ids.len(),
        }
    }

    // ---- projects -------------------------------------------------------

    /// Register a project by its working-directory path.
    ///
    /// Why: `trusty-mpm project init` and `POST /projects` need to record a
    /// directory as a managed project so sessions can be associated with it.
    /// What: builds a [`ProjectInfo`] from `path`, inserting (or replacing) it
    /// in the registry keyed by the path; returns the stored info.
    /// Test: `register_and_list_projects`.
    pub fn register_project(&self, path: PathBuf) -> ProjectInfo {
        let info = ProjectInfo::new(path.clone());
        self.projects.write().insert(path, info.clone());
        info
    }

    /// Snapshot every registered project.
    ///
    /// Why: `trusty-mpm project list` and `GET /projects` need the full set.
    /// What: clones each [`ProjectInfo`] out from under a short read lock.
    /// Test: `register_and_list_projects`.
    pub fn list_projects(&self) -> Vec<ProjectInfo> {
        self.projects.read().values().cloned().collect()
    }

    /// Look up one registered project by its path.
    ///
    /// Why: `GET /projects/current` resolves the project for the caller's cwd.
    /// What: returns a clone of the stored [`ProjectInfo`], or `None` if the
    /// path is not registered.
    /// Test: `project_lookup_by_path`.
    pub fn project(&self, path: &std::path::Path) -> Option<ProjectInfo> {
        self.projects.read().get(path).cloned()
    }

    // ---- delegations ----------------------------------------------------

    /// Record a new (or updated) delegation.
    pub fn upsert_delegation(&self, delegation: Delegation) {
        self.delegations.insert(delegation.id.0, delegation);
    }

    /// All delegations belonging to one session.
    pub fn delegations_for(&self, session: SessionId) -> Vec<Delegation> {
        self.delegations
            .iter()
            .filter(|e| e.value().session == session)
            .map(|e| e.value().clone())
            .collect()
    }

    // ---- circuit breakers ----------------------------------------------

    /// Get a snapshot of an agent's circuit breaker, creating a closed one if
    /// the agent has not been seen before.
    pub fn breaker(&self, agent: &str) -> CircuitBreaker {
        self.breakers
            .entry(agent.to_string())
            .or_insert_with(|| CircuitBreaker::new(self.circuit_config))
            .value()
            .clone()
    }

    /// Record a delegation outcome against an agent's breaker.
    ///
    /// Why: the daemon must update breaker state after every delegation so the
    /// next `agent_delegate` call is gated correctly.
    /// What: success/failure drives `record_success` / `record_failure`.
    /// Test: `breaker_tracks_outcomes`.
    pub fn record_outcome(&self, agent: &str, success: bool) {
        let mut entry = self
            .breakers
            .entry(agent.to_string())
            .or_insert_with(|| CircuitBreaker::new(self.circuit_config));
        if success {
            entry.record_success();
        } else {
            entry.record_failure();
        }
    }

    /// Snapshot every known agent's circuit breaker.
    pub fn all_breakers(&self) -> Vec<(String, CircuitBreaker)> {
        self.breakers
            .iter()
            .map(|e| (e.key().clone(), e.value().clone()))
            .collect()
    }

    // ---- memory ---------------------------------------------------------

    /// Record a token-usage snapshot and classify the resulting pressure.
    ///
    /// Why: the MCP `memory_protect` tool and `TokenUsageUpdate` hooks both
    /// feed usage in; the daemon stores it and returns the pressure level so
    /// the caller (and dashboard) know whether to warn/alert/compact.
    /// What: stores `usage` for the session, returns `usage.pressure(config)`.
    /// Test: `memory_pressure_is_classified`.
    pub fn record_memory(&self, session: SessionId, usage: MemoryUsage) -> MemoryPressure {
        self.memory.insert(session, usage);
        usage.pressure(&self.memory_config)
    }

    /// Latest memory usage for a session, if any has been recorded.
    pub fn memory_for(&self, session: SessionId) -> Option<MemoryUsage> {
        self.memory.get(&session).map(|e| *e.value())
    }

    // ---- trusty sidecar discovery --------------------------------------

    /// Record the trusty sidecar addresses discovered at daemon startup.
    ///
    /// Why: discovery runs once when the HTTP daemon boots; the resolved
    /// addresses must be visible to request handlers that proxy to the
    /// trusty-memory / trusty-search sidecars.
    /// What: stores the `TrustyAddrs` snapshot under the mutex.
    /// Test: `trusty_addrs_round_trip`.
    pub fn set_trusty_addrs(&self, addrs: crate::daemon::discover::TrustyAddrs) {
        *self.trusty_addrs.lock() = Some(addrs);
    }

    /// Read the discovered trusty sidecar addresses, if discovery has run.
    ///
    /// Why: handlers need the resolved addresses; `None` means discovery has
    /// not completed (e.g. in MCP mode, which skips it).
    /// What: returns a clone of the stored `TrustyAddrs`.
    /// Test: `trusty_addrs_round_trip`.
    #[allow(dead_code)] // Read by sidecar-proxy handlers landing in a follow-up.
    pub fn trusty_addrs(&self) -> Option<crate::daemon::discover::TrustyAddrs> {
        self.trusty_addrs.lock().clone()
    }

    // ---- token-use optimizer -------------------------------------------

    /// Snapshot the current optimizer configuration.
    ///
    /// Why: the PostToolUse hook path reads this on every event; cloning a
    /// small struct under a short read lock keeps the hot path lock-free
    /// during compression itself.
    /// What: returns a clone of the stored `OptimizerConfig`.
    /// Test: `get_optimizer_returns_default`.
    pub fn optimizer_config(&self) -> OptimizerConfig {
        self.optimizer.read().clone()
    }

    /// Re-read the optimizer policy from the installed framework on disk.
    ///
    /// Why: the policy file is framework-managed and edited directly (or reset
    /// via `trusty-mpm install --force`); the file watcher calls this when
    /// `optimizer.toml` changes so the running daemon picks up edits without a
    /// restart.
    /// What: reloads `~/.trusty-mpm/framework/hooks/optimizer.toml`, replacing
    /// the in-memory config under a write lock. A missing or malformed file
    /// falls back to `OptimizerConfig::default()` (logged, not fatal).
    /// Test: `reload_optimizer_config_picks_up_file_changes`.
    pub fn reload_optimizer_config(&self) {
        *self.optimizer.write() = load_optimizer_config();
    }

    /// Reload the optimizer policy from an explicit file path.
    ///
    /// Why: tests must exercise the reload path against a temp file without
    /// touching the real `~/.trusty-mpm` framework install.
    /// What: loads `path` via [`OptimizerConfig::load_from_file`] and stores the
    /// result; a missing file yields `OptimizerConfig::default()`.
    /// Test: `reload_optimizer_config_picks_up_file_changes`.
    pub fn reload_optimizer_config_from(&self, path: &std::path::Path) -> anyhow::Result<()> {
        let cfg = OptimizerConfig::load_from_file(path)?;
        *self.optimizer.write() = cfg;
        Ok(())
    }

    // ---- overseer -------------------------------------------------------

    /// The session overseer for evaluating hook events.
    ///
    /// Why: the hook relay consults the overseer on tool-use events; handing
    /// out the shared `Arc` keeps every call site using the one configured
    /// strategy.
    /// What: returns a clone of the `Arc<dyn Overseer>`.
    /// Test: `overseer_is_accessible`.
    pub fn overseer(&self) -> Arc<dyn Overseer> {
        Arc::clone(&self.overseer)
    }

    /// Name of the active overseer strategy.
    ///
    /// Why: `GET /overseer` and the audit log report which strategy is in
    /// force; the name is fixed at construction so callers need no config.
    /// What: returns `"deterministic"` or `"composite-llm"`.
    /// Test: `overseer_handler_reports_strategy`.
    pub fn overseer_handler(&self) -> &str {
        &self.overseer_handler
    }

    /// The overseer audit logger.
    ///
    /// Why: the hook relay logs every overseer decision; sharing the `Arc`
    /// keeps all decisions flowing into the one dated JSONL file.
    /// What: returns a clone of the `Arc<AuditLogger>`.
    /// Test: `audit_logger_is_accessible`.
    pub fn audit(&self) -> Arc<AuditLogger> {
        Arc::clone(&self.audit)
    }

    /// The standalone LLM overseer for interactive chat, if configured.
    ///
    /// Why: `POST /llm/chat` needs the concrete [`LlmOverseer`] (the hook-path
    /// overseer is hidden behind `dyn Overseer`); this is `Some` exactly when an
    /// OpenRouter API key resolved at startup.
    /// What: returns a clone of the `Arc<LlmOverseer>`, or `None` when LLM chat
    /// is not configured.
    /// Test: `llm_overseer_is_none_without_key`.
    pub fn llm_overseer(&self) -> Option<Arc<crate::daemon::llm_overseer::LlmOverseer>> {
        self.llm.clone()
    }

    // ---- hook events ----------------------------------------------------

    /// Append a hook event to the bounded history ring buffer and broadcast it.
    ///
    /// Why: the dashboard's live feed reads recent events; the buffer must not
    /// grow without bound in a long-running daemon. The same call also fans
    /// the event out to every active SSE subscriber so push consumers (the
    /// GUI) see the event in real time without polling.
    /// What: pushes to the back, evicting the oldest once `HOOK_HISTORY_LIMIT`
    /// is exceeded, then best-effort broadcasts a JSON copy of the record to
    /// `event_tx`. Send errors (no active subscribers) are intentionally
    /// ignored.
    /// Test: `hook_history_is_bounded`, `ingest_hook_broadcasts_to_subscribers`.
    pub fn push_hook_event(&self, record: HookEventRecord) {
        let value = serde_json::to_value(&record).unwrap_or_default();
        {
            let mut buf = self.hook_history.lock();
            if buf.len() >= HOOK_HISTORY_LIMIT {
                buf.pop_front();
            }
            buf.push_back(record);
        }
        // Best-effort broadcast: a send error means no active subscribers,
        // which is the common case and not a fault.
        let _ = self.event_tx.send(value);
    }

    /// Subscribe to the live hook-event broadcast stream.
    ///
    /// Why: SSE handlers need a fresh receiver per connection. Wrapping the
    /// raw `Sender::subscribe` call in a method keeps the channel field's
    /// access pattern uniform and documents the intended consumer.
    /// What: returns a new `broadcast::Receiver` that will see every event
    /// published after `subscribe` was called, up to `EVENT_CHANNEL_CAPACITY`
    /// of backlog before lagging.
    /// Test: `ingest_hook_broadcasts_to_subscribers`.
    pub fn event_subscribe(&self) -> tokio::sync::broadcast::Receiver<serde_json::Value> {
        self.event_tx.subscribe()
    }

    /// Snapshot recent hook events, newest last.
    pub fn recent_hook_events(&self) -> Vec<HookEventRecord> {
        self.hook_history.lock().iter().cloned().collect()
    }

    /// Recent hook events for one session only.
    pub fn hook_events_for(&self, session: SessionId) -> Vec<HookEventRecord> {
        self.hook_history
            .lock()
            .iter()
            .filter(|r| r.session == session)
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::hook::HookEvent;
    use crate::core::session::{ControlModel, SessionStatus};

    fn sample_session() -> Session {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut s = Session::new(SessionId::new(), "/tmp/p", ControlModel::Tmux, None);
        s.tmux_name = format!("tmpm-test-{n}");
        s.status = SessionStatus::Active;
        s
    }

    #[test]
    fn register_and_list_sessions() {
        let state = DaemonState::new();
        let s = sample_session();
        let id = s.id;
        state.register_session(s);
        assert_eq!(state.list_sessions().len(), 1);
        assert!(state.session(id).is_some());
        assert!(state.remove_session(id).is_some());
        assert!(state.list_sessions().is_empty());
    }

    #[test]
    fn update_session_mutates_existing() {
        let state = DaemonState::new();
        let s = sample_session();
        let id = s.id;
        state.register_session(s);
        let ran = state.update_session(&id, |session| {
            session.status = SessionStatus::Paused;
            session.pause_summary = Some("note".to_string());
        });
        assert!(ran);
        let updated = state.session(id).expect("session exists");
        assert_eq!(updated.status, SessionStatus::Paused);
        assert_eq!(updated.pause_summary.as_deref(), Some("note"));
    }

    #[test]
    fn update_session_missing_is_false() {
        let state = DaemonState::new();
        let ran = state.update_session(&SessionId::new(), |_| {});
        assert!(!ran);
    }

    #[test]
    fn register_and_list_projects() {
        let state = DaemonState::new();
        assert!(state.list_projects().is_empty());
        let info = state.register_project(PathBuf::from("/work/demo"));
        assert_eq!(info.name, "demo");
        assert_eq!(state.list_projects().len(), 1);
        // Re-registering the same path replaces rather than duplicates.
        state.register_project(PathBuf::from("/work/demo"));
        assert_eq!(state.list_projects().len(), 1);
        state.register_project(PathBuf::from("/work/other"));
        assert_eq!(state.list_projects().len(), 2);
    }

    #[test]
    fn project_lookup_by_path() {
        let state = DaemonState::new();
        state.register_project(PathBuf::from("/work/demo"));
        assert!(state.project(std::path::Path::new("/work/demo")).is_some());
        assert!(
            state
                .project(std::path::Path::new("/work/missing"))
                .is_none()
        );
    }

    #[test]
    fn list_sessions_for_project_filters() {
        let state = DaemonState::new();
        let mut in_proj = sample_session();
        in_proj.project_path = Some(PathBuf::from("/work/demo"));
        let mut other_proj = sample_session();
        other_proj.project_path = Some(PathBuf::from("/work/other"));
        let no_proj = sample_session();
        state.register_session(in_proj.clone());
        state.register_session(other_proj);
        state.register_session(no_proj);

        let listed = state.list_sessions_for_project(std::path::Path::new("/work/demo"));
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, in_proj.id);
    }

    #[test]
    fn find_session_by_id_or_name() {
        let state = DaemonState::new();
        let s = sample_session();
        let id = s.id;
        let name = s.tmux_name.clone();
        state.register_session(s);

        assert!(state.find_session(&id.0.to_string()).is_some());
        assert!(state.find_session(&name).is_some());
        assert!(state.find_session("tmpm-no-such-name").is_none());
        assert!(
            state
                .find_session(&SessionId::new().0.to_string())
                .is_none()
        );
    }

    #[test]
    fn breaker_tracks_outcomes() {
        let state = DaemonState::new();
        // Default threshold is 3 consecutive failures.
        for _ in 0..3 {
            state.record_outcome("research", false);
        }
        let cb = state.breaker("research");
        assert!(!cb.allows_delegation());
        // A success resets the counter (after an attempt_reset path it closes).
        state.record_outcome("research", true);
        assert_eq!(state.breaker("research").consecutive_failures, 0);
    }

    #[test]
    fn memory_pressure_is_classified() {
        let state = DaemonState::new();
        let id = SessionId::new();
        let pressure = state.record_memory(
            id,
            MemoryUsage {
                used_tokens: 900,
                window_tokens: 1000,
            },
        );
        assert_eq!(pressure, MemoryPressure::Compact);
        assert!(state.memory_for(id).is_some());
    }

    #[test]
    fn trusty_addrs_round_trip() {
        let state = DaemonState::new();
        assert!(state.trusty_addrs().is_none());
        let addrs = crate::daemon::discover::TrustyAddrs {
            memory: "127.0.0.1:3038".parse().unwrap(),
            search: "127.0.0.1:7878".parse().unwrap(),
        };
        state.set_trusty_addrs(addrs);
        let got = state.trusty_addrs().expect("addrs stored");
        assert_eq!(got.memory, "127.0.0.1:3038".parse().unwrap());
        assert_eq!(got.search, "127.0.0.1:7878".parse().unwrap());
    }

    #[test]
    fn reap_dead_sessions() {
        // Three registered sessions; tmux reports only two of them alive.
        // `reap_against` (the testable core of `reap_dead_sessions`) must drop
        // exactly the one whose tmux_name is absent from the live set.
        let state = DaemonState::new();
        let alive_a = sample_session();
        let alive_b = sample_session();
        let dead = sample_session();
        let (id_a, id_b, id_dead) = (alive_a.id, alive_b.id, dead.id);
        state.register_session(alive_a.clone());
        state.register_session(alive_b.clone());
        state.register_session(dead);
        assert_eq!(state.list_sessions().len(), 3);

        let live: std::collections::HashSet<String> =
            [alive_a.tmux_name.clone(), alive_b.tmux_name.clone()]
                .into_iter()
                .collect();
        let result = state.reap_against(&live);

        assert_eq!(result.reaped, 1);
        assert_eq!(result.stopped, 0);
        assert!(state.session(id_a).is_some());
        assert!(state.session(id_b).is_some());
        assert!(state.session(id_dead).is_none());

        // Reaping again is idempotent — nothing left to remove.
        assert_eq!(state.reap_against(&live), ReapResult::default());
    }

    #[test]
    fn reap_against_empty_live_removes_all_tmux_sessions() {
        // An empty live set (e.g. tmux server fully stopped) drops every
        // tmux-hosted entry.
        let state = DaemonState::new();
        state.register_session(sample_session());
        state.register_session(sample_session());
        let result = state.reap_against(&std::collections::HashSet::new());
        assert_eq!(result.reaped, 2);
        assert!(state.list_sessions().is_empty());
    }

    #[test]
    fn reap_keeps_native_sessions() {
        // Native (Terminal.app) sessions have no tmux session; the tmux-based
        // reaper must never delete them, even against an empty live set.
        let state = DaemonState::new();
        let mut native = sample_session();
        native.origin = crate::core::session::SessionHost::Native;
        native.pid = Some(9999);
        let native_id = native.id;
        let tmux = sample_session();
        let tmux_id = tmux.id;
        state.register_session(native);
        state.register_session(tmux);

        let result = state.reap_against(&std::collections::HashSet::new());

        // Only the tmux-hosted session is reaped.
        assert_eq!(result.reaped, 1);
        assert!(state.session(native_id).is_some());
        assert!(state.session(tmux_id).is_none());
    }

    #[test]
    fn set_session_pid_updates_field() {
        // Registering a session leaves `pid` unset; set_session_pid records it.
        let state = DaemonState::new();
        let s = sample_session();
        let id = s.id;
        state.register_session(s);
        assert_eq!(state.session(id).unwrap().pid, None);

        assert!(state.set_session_pid(id, 4242));
        assert_eq!(state.session(id).unwrap().pid, Some(4242));

        // An unknown id is reported as not updated.
        assert!(!state.set_session_pid(SessionId::new(), 1));
    }

    #[test]
    fn reap_marks_stopped_when_pid_dead() {
        // A tmux session that is still alive but whose tracked `claude` process
        // has exited (u32::MAX is a guaranteed-dead PID) must be marked Stopped
        // — not removed — so the operator can still see it.
        let state = DaemonState::new();
        let mut session = sample_session();
        session.pid = Some(u32::MAX);
        let id = session.id;
        let tmux_name = session.tmux_name.clone();
        state.register_session(session);

        let live: std::collections::HashSet<String> = [tmux_name].into_iter().collect();
        let result = state.reap_against(&live);

        assert_eq!(result.reaped, 0);
        assert_eq!(result.stopped, 1);
        let after = state.session(id).expect("session is kept, not removed");
        assert_eq!(after.status, SessionStatus::Stopped);
    }

    #[test]
    fn new_reads_default_when_optimizer_file_missing() {
        // With no framework installed (the optimizer.toml file absent), the
        // daemon must still construct, falling back to the default policy.
        let state = DaemonState::new();
        assert_eq!(
            state.optimizer_config().default_level,
            crate::core::compress::CompressionLevel::Trim
        );
    }

    #[test]
    fn reload_optimizer_config_picks_up_file_changes() {
        // Reloading from an explicit temp file must overwrite the in-memory
        // policy with whatever the file declares.
        use std::io::Write;
        let state = DaemonState::new();
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("optimizer.toml");
        let mut file = std::fs::File::create(&path).expect("create file");
        writeln!(file, "[default]\nlevel = \"caveman\"").expect("write file");

        state
            .reload_optimizer_config_from(&path)
            .expect("reload succeeds");
        assert_eq!(
            state.optimizer_config().default_level,
            crate::core::compress::CompressionLevel::Caveman
        );

        // A missing file reloads to the default policy rather than erroring.
        state
            .reload_optimizer_config_from(&dir.path().join("absent.toml"))
            .expect("missing file is not an error");
        assert_eq!(
            state.optimizer_config().default_level,
            crate::core::compress::CompressionLevel::Trim
        );
    }

    #[test]
    fn new_overseer_is_disabled_when_file_missing() {
        // With no framework installed (overseer.toml absent), the overseer
        // must be present but disabled — oversight is opt-in.
        let state = DaemonState::new();
        assert!(!state.overseer().is_enabled());
    }

    #[test]
    fn overseer_is_deterministic_without_llm() {
        // With the `[llm]` section absent/disabled, the overseer is the plain
        // deterministic strategy and (with no rules) reports disabled.
        let cfg = OverseerConfig::default();
        let build = build_overseer(cfg);
        assert!(!build.overseer.is_enabled());
        assert_eq!(build.handler, "deterministic");
        assert!(build.llm.is_none());
    }

    #[test]
    fn overseer_falls_back_when_llm_key_missing() {
        // `[llm] enabled = true` but no API key resolves: the daemon must not
        // panic — it falls back to the deterministic overseer.
        let mut cfg = OverseerConfig::default();
        cfg.llm.enabled = true;
        cfg.llm.api_key_env = "TRUSTY_MPM_DEFINITELY_NOT_SET".to_string(); // pragma: allowlist secret
        let build = build_overseer(cfg);
        // Deterministic with no rules and disabled top-level flag → disabled.
        assert!(!build.overseer.is_enabled());
        assert_eq!(build.handler, "deterministic");
        assert!(build.llm.is_none());
    }

    #[test]
    fn llm_overseer_is_none_without_key() {
        // A default daemon (no OpenRouter key) exposes no LLM chat handler.
        let state = DaemonState::new();
        assert!(state.llm_overseer().is_none());
    }

    #[test]
    fn overseer_handler_reports_strategy() {
        // The default daemon reports the deterministic handler.
        let state = DaemonState::new();
        assert_eq!(state.overseer_handler(), "deterministic");
    }

    #[test]
    fn overseer_is_accessible() {
        let state = DaemonState::new();
        // The shared overseer can be cloned out and queried.
        let overseer = state.overseer();
        assert!(!overseer.is_enabled());
    }

    #[test]
    fn audit_logger_is_accessible() {
        let state = DaemonState::new();
        // The audit logger resolves a dated JSONL path under `logs/overseer`.
        let audit = state.audit();
        assert_eq!(
            audit.path().extension().and_then(|e| e.to_str()),
            Some("jsonl")
        );
    }

    #[test]
    fn hook_history_is_bounded() {
        let state = DaemonState::new();
        let id = SessionId::new();
        for _ in 0..(HOOK_HISTORY_LIMIT + 50) {
            state.push_hook_event(HookEventRecord::now(
                id,
                HookEvent::PreToolUse,
                serde_json::Value::Null,
            ));
        }
        assert_eq!(state.recent_hook_events().len(), HOOK_HISTORY_LIMIT);
        assert_eq!(state.hook_events_for(id).len(), HOOK_HISTORY_LIMIT);
    }

    #[test]
    fn pairing_round_trip() {
        // A freshly-generated code confirms once, binds the chat id, and is
        // then consumed so the same code cannot validate twice. The state is
        // rooted at a temp dir so the persisted record never touches HOME.
        let dir = tempfile::tempdir().expect("temp dir");
        let state = DaemonState::with_root(dir.path().to_path_buf());
        assert_eq!(state.paired_chat_id(), None);
        let code = state.generate_pair_code();
        assert_eq!(code.len(), 6);
        assert!(code.chars().all(|c| c.is_ascii_alphanumeric()));
        assert!(state.confirm_pair_code(&code, 12345678));
        assert_eq!(state.paired_chat_id(), Some(12345678));
        // The code was consumed; confirming it again must fail.
        assert!(!state.confirm_pair_code(&code, 999));
    }

    #[test]
    fn wrong_pair_code_is_rejected() {
        let dir = tempfile::tempdir().expect("temp dir");
        let state = DaemonState::with_root(dir.path().to_path_buf());
        let _code = state.generate_pair_code();
        assert!(!state.confirm_pair_code("ZZZZZZ", 12345678));
        assert_eq!(state.paired_chat_id(), None);
    }

    #[test]
    fn pairing_persists_to_disk() {
        // Confirming a code writes pairing.json; a fresh state rooted at the
        // same directory restores the binding without a new handshake.
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path().to_path_buf();
        let state = DaemonState::with_root(root.clone());
        let code = state.generate_pair_code();
        assert!(state.confirm_pair_code(&code, 555));
        // The on-disk record exists.
        assert_eq!(
            crate::daemon::pairing_store::load(&root).map(|r| r.chat_id),
            Some(555)
        );
        // A new state restores the pairing from disk.
        let restored = DaemonState::with_root(root);
        assert_eq!(restored.paired_chat_id(), Some(555));
    }

    #[test]
    fn pairing_reset_clears_disk() {
        // clear_pairing drops the binding in memory and removes pairing.json.
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path().to_path_buf();
        let state = DaemonState::with_root(root.clone());
        let code = state.generate_pair_code();
        assert!(state.confirm_pair_code(&code, 777));
        state.clear_pairing();
        assert_eq!(state.paired_chat_id(), None);
        assert!(crate::daemon::pairing_store::load(&root).is_none());
    }
}
