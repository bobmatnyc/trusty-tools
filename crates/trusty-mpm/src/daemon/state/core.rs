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

use crate::control::SessionRegistry;
use crate::daemon::audit::AuditLogger;
use crate::daemon::optimizer::OptimizerConfig;

use super::overseer::{build_overseer, load_optimizer_config, load_overseer, make_audit_logger};
use super::sm::build_session_manager_agent;

/// Build the control-plane [`SessionRegistry`] from the operator's config.
///
/// Why: WI-5 (§9) makes the concurrency cap, launch stagger, auth timeout, and
/// classifier gate operator-configurable via the `[control_plane]` section of
/// `~/.trusty-mpm/config.toml`. The daemon must read that section at startup and
/// inject it into the registry so the cost guardrails reflect the operator's
/// settings; an absent section falls back to spec defaults (cap 5, stagger 2 s).
/// What: loads [`MpmConfig`](crate::core::config::MpmConfig) from `root`
/// (silently defaulting on absence/malformed input, exactly as the loader
/// documents) and constructs a [`SessionRegistry`] with its `control_plane`
/// config.
/// Test: exercised by every `DaemonState` constructor; control-plane defaults
/// are pinned by `control::config::tests` and the cap behavior by
/// `control::registry::tests`.
fn build_session_registry(root: &std::path::Path) -> SessionRegistry {
    let cfg = crate::core::config::MpmConfig::load(root);
    SessionRegistry::with_config(cfg.control_plane)
}

/// Outcome of a reap sweep over the session registry.
///
/// Why: the reaper now does two distinct things — it *removes* tmux sessions
/// whose tmux window is gone, and it *marks Stopped* alive tmux sessions whose
/// tracked `claude` process has exited. Callers (and the dashboard) need to
/// tell those apart, so the sweep reports both counts.
/// What: `reaped` is the number of entries deleted from the registry;
/// `stopped` is the number transitioned to [`SessionStatus::Stopped`](crate::core::session::SessionStatus::Stopped) in place.
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
    /// The Session Manager agent (DOC-14 SM-7), built once at startup.
    ///
    /// Why: `POST /api/v1/sessions/chat` (and its `/session-manager/*`
    /// aliases) route a chat turn through this agent when
    /// `[session_manager].enabled = true` AND a provider is available, superseding
    /// the legacy [`LlmOverseer`] path. Built unconditionally (cheap — config +
    /// a credentials probe) so the endpoint can consult `is_enabled()` /
    /// `has_runtime()` per request; disabled by default so the legacy path is
    /// preserved untouched.
    /// What: the shared `Arc<SessionManagerAgent>` wired with the provider
    /// resolver, the context-engine storage root, and (under `sm-memory`) the SM
    /// palace.
    /// Test: `sm_agent_built_disabled_by_default`.
    pub(super) session_manager_agent: Arc<crate::core::sm::SessionManagerAgent>,
    /// Append-only JSONL logger for every overseer decision.
    pub(super) audit: Arc<AuditLogger>,
    /// The peer message bus (DOC-60 §5.3).
    ///
    /// Why: DOC-60 §4 hosts the assistant-to-assistant bus in this daemon, so
    /// the instance registry and the per-instance delivery channels must live
    /// on shared daemon state for the HTTP handlers to reach them.
    /// What: the [`PeerBus`](crate::daemon::bus::PeerBus), constructed against
    /// this daemon's `logs/` directory so its durable §9 stream lands beside
    /// the overseer one.
    /// Test: `crate::daemon::bus::tests`.
    pub(super) bus: Arc<crate::daemon::bus::PeerBus>,
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
    /// The base the captured-error stores are read from, when this daemon must
    /// not resolve them from process-global state (#6505).
    ///
    /// Why: `GET /api/v1/errors` and `mpm.errors.list` aggregate
    /// `<data_dir>/<app>/errors.jsonl` for four daemons, and that `data_dir`
    /// resolution reads `TRUSTY_DATA_DIR_OVERRIDE` on every call. Tests in this
    /// crate's binary set and clear that variable, so two calls a few
    /// milliseconds apart need not read the same directory — which is how
    /// `parity_errors_agrees_across_transports` saw five errors over HTTP and
    /// none over the socket. Pinning the base at construction makes both
    /// transports read one directory.
    /// What: `None` on [`Self::new`] / [`Self::with_root`], which keep the OS
    /// data-directory resolution unchanged; `Some(root)` under
    /// [`Self::with_paths`], whose whole purpose is a temp-rooted daemon.
    /// Test: `parity_errors_agrees_across_transports`.
    pub(super) error_store_base: Option<PathBuf>,
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
    /// Lazily-initialized managed session manager for the `/sessions/managed`
    /// API surface.
    ///
    /// Why: [`crate::session_manager::SessionManager::new`] is async (it loads
    /// the on-disk store) and needs a tmux driver, so it cannot be built inside
    /// the synchronous `DaemonState` constructors. A `tokio::sync::OnceCell`
    /// defers construction to the first managed-session request and caches the
    /// shared handle thereafter.
    /// What: holds `None` until [`Self::session_manager`] first runs, then the
    /// shared `Arc<SessionManager>`.
    /// Test: `managed_routes` handler tests exercise the accessor via the router.
    pub(super) managed_sessions:
        tokio::sync::OnceCell<std::sync::Arc<crate::session_manager::SessionManager>>,
    /// Lazily-initialized activity monitor for the `/sessions/managed/{id}/activity`
    /// route.
    ///
    /// Why: `ActivityMonitor` must be shared across requests so the per-session
    /// content-hash cache persists between calls; a `OnceLock` defers
    /// construction until the first activity request and amortizes the cost.
    /// What: holds the shared monitor; built on first access using the default
    /// [`crate::activity::OpenRouterClassifier`], which resolves an inference
    /// provider through the shared `trusty_common::inference` credential ladder
    /// (#4427).
    /// Test: `managed_routes` handler tests exercise this via the router.
    pub(super) activity_monitor: std::sync::OnceLock<
        std::sync::Arc<
            crate::activity::monitor::ActivityMonitor<crate::activity::OpenRouterClassifier>,
        >,
    >,

    /// Lazily-initialized project registry for the `project_*` MCP tools (#1519).
    ///
    /// Why: `ProjectRegistry::load` is async (it reads the on-disk store) and
    /// cannot be called inside the synchronous `DaemonState` constructors. A
    /// `OnceCell` defers construction to the first `project_*` MCP request and
    /// caches the shared handle thereafter, following the same pattern as
    /// `managed_sessions`.
    /// What: holds `None` until [`Self::project_registry`] first runs, then the
    /// shared `Arc<ProjectRegistry>`.
    /// Test: `mcp_project` tests exercise the registry via the dispatch path.
    pub(super) project_registry:
        tokio::sync::OnceCell<std::sync::Arc<crate::project::ProjectRegistry>>,

    /// Lazily-initialized Deliverable/Milestone manager (§10.7, #2378).
    ///
    /// Why: [`crate::deliverable::DeliverableManager::load`] is async (it reads
    /// the two on-disk stores) and cannot run inside the synchronous
    /// `DaemonState` constructors. A `OnceCell` defers construction to the first
    /// deliverables/milestones request and caches the shared handle — the same
    /// pattern as `project_registry` and `managed_sessions`.
    /// What: holds `None` until [`Self::deliverable_manager`] first runs, then the
    /// shared `Arc<DeliverableManager>` fronting the central `deliverables.json` /
    /// `milestones.json` stores.
    /// Test: the deliverable route tests exercise the accessor via the router.
    pub(super) deliverable_manager:
        tokio::sync::OnceCell<std::sync::Arc<crate::deliverable::DeliverableManager>>,

    /// SESSCTL control-plane session registry (WI-2, #1593).
    ///
    /// Why: every HTTP handler and CLI command for the SESSCTL surface needs a
    /// single, shared, thread-safe view of live control-plane sessions. Storing
    /// it in `DaemonState` follows the same injection pattern as every other
    /// shared subsystem.
    /// What: wraps `HashMap<ControlSessionId, SessionActorHandle>` behind a
    /// `tokio::sync::RwLock`; actors are registered here via `run_session`.
    /// Test: `control_routes` handler tests register sessions and assert list/get.
    pub session_registry: Arc<SessionRegistry>,
    /// Per-conversation focus state for the local session-manager PROXY surface
    /// (`/api/v1/sessions/proxy/*`, TELUI-6 #1440).
    ///
    /// Why: the proxy routes construct a fresh
    /// [`crate::client::SessionProxy`] PER REQUEST (mirroring every other
    /// stateless handler in this file), but which session a conversation has
    /// focused must persist ACROSS requests — exactly the same durability
    /// requirement `paired_chat_id`/`pair_code` have, so this follows their
    /// plain-`Mutex`-field pattern rather than introducing a new subsystem.
    /// What: an `Arc<Mutex<HashMap<conversation_key, FocusTarget>>>` shared into
    /// each request's `SessionProxy` via
    /// [`crate::client::SessionProxy::with_focus_store`]. Volatile — does not
    /// survive a daemon restart, matching every external channel's focus state.
    /// Test: `daemon::managed_routes::proxy::tests`, `tests/proxy_routes.rs`.
    pub(super) proxy_focus:
        Arc<std::sync::Mutex<HashMap<String, crate::client::proxy::FocusTarget>>>,
    /// Launchd-supervision signal for `GET /health` (issue #2486).
    ///
    /// Why: a green `/health` alone does not prove launchd owns this process —
    /// see [`crate::daemon::api::types::HealthResponse::supervised`] for the
    /// full restart-race rationale. This field is the mutable half of that
    /// signal: it defaults to the safe value (`true`) at construction because
    /// the real probe (`commands::launchd_probe::compute_supervised`) needs
    /// process startup context (env vars, PPID) that only `daemon_run` — the
    /// actual `tm daemon` entry point — has readily at hand; test-only
    /// constructors never override it.
    /// What: an `AtomicBool` so `daemon_run` can set it once, post-construction,
    /// without requiring `&mut DaemonState` through the shared `Arc`. Read via
    /// [`Self::supervised`], written via [`Self::set_supervised`].
    /// Test: `health_response_serializes_supervised_field` asserts the default;
    /// `commands::launchd_probe::tests` cover the pure decision logic that
    /// `daemon_run` uses to compute the value passed to `set_supervised`.
    pub(super) supervised: std::sync::atomic::AtomicBool,
    /// Whether this daemon was started with the `tm daemon --force` opt-in
    /// (issue #4230, review MEDIUM-1).
    ///
    /// Why: `supervised == false` alone cannot distinguish the two populations
    /// that reach it — an unwanted ORPHAN (the #4230 incident) and a run the
    /// operator deliberately asked for with the `--force` flag that #4397 added
    /// and that both #4230 refusal messages recommend. Without this flag the
    /// `daemon_orphan` doctor check would call a deliberate `--force` daemon an
    /// orphan and tell the operator to kill it, which trains them to ignore the
    /// check — the exact alert-fatigue failure the check exists to avoid.
    /// What: an `AtomicBool` defaulting to `false`, set once post-construction by
    /// `daemon_run` alongside [`Self::set_supervised`]. Read via
    /// [`Self::unsupervised_forced`].
    /// Test: `health_response_serializes_forced_field` asserts the default;
    /// `daemon_orphan`'s `warns_not_fails_when_the_operator_forced_it` covers the
    /// verdict that consumes it.
    pub(super) unsupervised_forced: std::sync::atomic::AtomicBool,
    /// The three-state launchd answer behind [`Self::supervised`] (#4469).
    ///
    /// Why: see [`crate::daemon::api::types::HealthResponse::launchd_supervision`]
    /// — a bool cannot express "launchd could not be asked", and reporting that
    /// as `false` makes `tm doctor` prescribe killing a healthy daemon.
    /// What: the discriminant string, written once at startup by `daemon_run`
    /// via [`Self::set_launchd_supervision`]. Empty until then.
    /// Test: `health_response_serializes_launchd_supervision_field`.
    pub(super) launchd_supervision: std::sync::RwLock<String>,
    /// Layer-3 portfolio manager state (`tm manager`, epic #2109, DOC-36 §3.1).
    ///
    /// Why: DOC-36 §3.1 makes `tm manager` a daemon-owned component whose
    /// `ManagerState` "sits alongside `DaemonState`" — so it is owned HERE and
    /// threaded into the `/api/v1/manager/*` handlers, exactly as
    /// [`proxy_focus`](Self::proxy_focus) is owned for the L2 proxy surface.
    /// Provisioned once at construction (§7 Q3 "auto-created at daemon startup");
    /// held behind an `Arc` so the accessor hands out cheap clones without
    /// requiring `&mut self` through the shared daemon `Arc`.
    /// What: the portfolio palace handle (WI-5) today; the inference adapter and
    /// proactive poll loop attach onto the same `ManagerState` in later phases.
    /// Test: `manager_state_is_provisioned` in `state/tests.rs`;
    /// `manager_version_route_reports_capabilities` in `tests/manager_routes.rs`.
    pub(super) manager: Arc<crate::daemon::manager::ManagerState>,
    /// Progress registry for asynchronous managed-session provisioning (#2605).
    ///
    /// Why: `POST /api/v1/sessions/managed` with `background: true` returns a
    /// job id immediately and runs the (potentially minutes-long, on a large
    /// repo) workspace provision on a detached task. That task records live
    /// phase/detail progress and its terminal outcome HERE so the poll route
    /// (`GET .../{id}/provision-status`) and the CLI can follow along without
    /// holding one long HTTP request open. Owned as a plain field (not a
    /// `OnceCell`) because it is a cheap, interior-mutable `DashMap` with a
    /// trivial `Default` — the same ownership shape as `session_registry`.
    /// What: a [`crate::daemon::provisioning::ProvisioningRegistry`] keyed by
    /// job id; shared across the handler, its stage-updater task, and the poll
    /// route via the daemon `Arc`.
    /// Test: `crate::daemon::managed_routes::provision_status` tests exercise
    /// the async state machine through the router.
    pub provisioning: crate::daemon::provisioning::ProvisioningRegistry,
    /// Per-session idle-park auto-nudge ledger (#2621).
    ///
    /// Why: the auto-nudge spam guards — a per-session cap and a cooldown — need
    /// history that survives across hook receipts but not across a daemon
    /// restart. Owning it here (behind a `parking_lot::Mutex`, like `optimizer`'s
    /// lock) lets the `ingest_hook` nudge path read a session's
    /// [`crate::core::idle_nudge::NudgeRecord`] and record deliveries without a
    /// new subsystem. In-memory only — a restart resetting the counters only
    /// re-arms the already-conservative nudge, never spams. Growth is bounded:
    /// `daemon::idle_nudge::run_nudge` prunes entries for any session no longer
    /// known to the session store on every nudge attempt (#2621 code-critic
    /// HIGH 2), so this never accumulates permanent per-session entries for the
    /// daemon's lifetime.
    /// What: a [`crate::core::idle_nudge::NudgeLedger`] keyed by the managed
    /// session's [`crate::session_manager::ManagedSessionId`] inner UUID — NOT
    /// this daemon's own [`SessionId`] (the two are distinct id spaces; the
    /// nudge always targets a tmux-managed session, correlated via
    /// `claude_session_id`).
    /// Test: `daemon::idle_nudge` unit tests exercise the decide→record→prune
    /// path; the ledger's own semantics are covered by `core::idle_nudge` tests.
    pub(super) nudge_ledger: parking_lot::Mutex<crate::core::idle_nudge::NudgeLedger>,
    /// Serializes the shared-working-tree dispatch claim (#5324).
    ///
    /// Why: `tm hook --pm-guard` used to ASK which agents were already writing
    /// in a directory and record the answer's consequence later, so two
    /// dispatches issued in one PM turn could both ask before either was
    /// recorded, both see an empty set, and both be admitted — the collision the
    /// guard exists to prevent. Closing that needs the question and the record
    /// to be one indivisible step, and `delegations` is a `DashMap`: it makes
    /// each entry atomic, never a scan-then-insert pair. This mutex is what
    /// makes that pair atomic.
    /// What: guards nothing itself — it is held across the scan-and-record in
    /// [`DaemonState::claim_shared_tree_dispatch`], which is its only taker.
    /// Held for an in-memory scan of one session's delegations and at most one
    /// insert; no I/O, no await, no other lock nested inside it.
    /// Test: `shared_tree_dispatch_route_denies_the_second_claim`,
    /// `pm_guard_denies_the_second_of_two_simultaneous_dispatches`.
    pub(super) shared_tree_claim: parking_lot::Mutex<()>,
    /// Serializes creation and isolation-correction of one dispatch's
    /// delegation record (#5769).
    ///
    /// Why: two independent writers describe the same dispatch. The daemon's
    /// own `matcher: "*"` `PreToolUse` hook records it from the ORIGINAL
    /// payload, and `tm hook --pm-guard` posts the isolation it granted that
    /// same dispatch. Both resolve the record by `tool_use_id` and then insert
    /// if they find none, and a `DashMap` makes neither half of that pair atomic
    /// with the other — so the two could both miss and both insert, leaving one
    /// dispatch with two records and the unisolated one deciding later denies.
    /// What: held across the find-then-insert in
    /// [`crate::daemon::services::delegation_tracker::observe`]'s dispatch arm
    /// and across the whole upsert in
    /// [`crate::daemon::services::delegation_tracker::record_granted_isolation`].
    /// It is ALWAYS taken inside [`Self::shared_tree_claim`], never outside it,
    /// so the two can never deadlock. In-memory work only: no I/O, no await.
    /// Test: `a_grant_and_the_tracker_converge_in_either_order`.
    pub(super) dispatch_record: parking_lot::Mutex<()>,
    /// `SubagentStop`s that arrived before the `agent_id` naming them (#4142).
    ///
    /// Why: `PostToolUse` is async and `SubagentStop` synchronous, so the stop
    /// can win the race and match nothing. See
    /// [`crate::daemon::state::pending_stops`] for the whole rationale.
    /// Test: `out_of_order_subagent_stop_resolves_when_its_post_tool_use_lands`.
    pub(super) pending_stops: super::pending_stops::PendingStops,
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
    /// What: delegates to [`Self::with_root`] using the default
    /// [`FrameworkPaths`] root (`~/.trusty-mpm`), so startup cleanup and
    /// pairing restore are handled exactly once in the shared inner constructor.
    /// Test: `new_reads_default_when_optimizer_file_missing`,
    /// `new_overseer_is_disabled_when_file_missing`.
    pub fn new() -> Self {
        let framework_root = FrameworkPaths::default().root;
        Self::with_root(framework_root)
    }

    /// Wrap the state in an `Arc` for sharing across tasks.
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::new())
    }

    /// Construct default state whose persisted pairing lives under `root`.
    ///
    /// Why: pairing now writes `pairing.json` to disk; tests that exercise
    /// confirm / clear must redirect that write to a temp directory so they
    /// never touch (or depend on) the operator's real `~/.trusty-mpm`. This
    /// constructor is also the shared innermost path that both `new` and
    /// `with_root` delegate to, so startup cleanup runs **exactly once** and
    /// always against the correct `root` (not the default path).
    /// What: removes orphan `.claim.*` files left by a crashed process (exactly
    /// once, against `root`), then reads the optimizer config and overseer policy
    /// from the real framework hooks path, restores any persisted Telegram
    /// pairing under `root`, and builds the full [`DaemonState`].
    /// Test: `pairing_persists_to_disk`, `pairing_reset_clears_disk`.
    pub fn with_root(root: PathBuf) -> Self {
        let optimizer = load_optimizer_config();
        let build = load_overseer();
        // Remove any orphan `.claim.*` files left by a previous crashed process
        // before checking for a pairing record; this prevents a stale claim from
        // blocking the very first confirm after a crash.
        // Cleanup runs exactly once here — `new` delegates to this constructor
        // so there is no duplicate call against the default path.
        crate::daemon::pairing_store::cleanup_stale_claims(&root);
        // Restore a persisted Telegram pairing so push alerts survive restarts.
        let paired = crate::daemon::pairing_store::load(&root).map(|r| r.chat_id);
        if let Some(chat_id) = paired {
            tracing::info!("restored persisted Telegram pairing (chat {chat_id})");
        }
        let (event_tx, _) = tokio::sync::broadcast::channel(EVENT_CHANNEL_CAPACITY);
        // §9 (WI-5): build the control-plane registry with the operator's
        // [control_plane] auth+cost config (cap, stagger, classifier gate).
        let session_registry = Arc::new(build_session_registry(&root));
        // §7 Q3: auto-provision the portfolio manager palace at startup; a palace
        // failure degrades inside `provision` and never fails daemon startup.
        let manager = Arc::new(crate::daemon::manager::ManagerState::provision(&root));
        Self {
            sessions: DashMap::new(),
            delegations: DashMap::new(),
            breakers: DashMap::new(),
            memory: DashMap::new(),
            hook_history: Mutex::new(VecDeque::with_capacity(HOOK_HISTORY_LIMIT)),
            memory_config: MemoryConfig::default(),
            circuit_config: CircuitConfig::default(),
            optimizer: Arc::new(parking_lot::RwLock::new(optimizer)),
            projects: Arc::new(RwLock::new(HashMap::new())),
            overseer: build.overseer,
            overseer_handler: build.handler,
            llm: build.llm,
            session_manager_agent: build_session_manager_agent(&root),
            audit: make_audit_logger(&root),
            bus: Arc::new(crate::daemon::bus::PeerBus::new(&root.join("logs"))),
            paired_chat_id: Mutex::new(paired),
            pair_code: Mutex::new(None),
            framework_root: root,
            // Production keeps the OS data-directory resolution; see the field doc.
            error_store_base: None,
            event_tx,
            managed_sessions: tokio::sync::OnceCell::new(),
            activity_monitor: std::sync::OnceLock::new(),
            project_registry: tokio::sync::OnceCell::new(),
            deliverable_manager: tokio::sync::OnceCell::new(),
            session_registry,
            proxy_focus: Arc::new(std::sync::Mutex::new(HashMap::new())),
            supervised: std::sync::atomic::AtomicBool::new(true),
            unsupervised_forced: std::sync::atomic::AtomicBool::new(false),
            launchd_supervision: std::sync::RwLock::new(String::new()),
            manager,
            provisioning: crate::daemon::provisioning::ProvisioningRegistry::default(),
            nudge_ledger: parking_lot::Mutex::new(crate::core::idle_nudge::NudgeLedger::new()),
            shared_tree_claim: parking_lot::Mutex::new(()),
            dispatch_record: parking_lot::Mutex::new(()),
            pending_stops: super::pending_stops::PendingStops::default(),
        }
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
        crate::daemon::pairing_store::cleanup_stale_claims(&framework_root);
        let paired = crate::daemon::pairing_store::load(&framework_root).map(|r| r.chat_id);
        let (event_tx, _) = tokio::sync::broadcast::channel(EVENT_CHANNEL_CAPACITY);
        // §9 (WI-5): build the control-plane registry from [control_plane] under
        // this (possibly temp-dir) framework root, so e2e tests can inject a
        // hermetic auth+cost config and the real daemon reads the operator's.
        let session_registry = Arc::new(build_session_registry(&framework_root));
        // §7 Q3: auto-provision the portfolio manager palace at startup; a palace
        // failure degrades inside `provision` and never fails daemon startup.
        let manager = Arc::new(crate::daemon::manager::ManagerState::provision(
            &framework_root,
        ));
        Self {
            sessions: DashMap::new(),
            delegations: DashMap::new(),
            breakers: DashMap::new(),
            memory: DashMap::new(),
            hook_history: Mutex::new(VecDeque::with_capacity(HOOK_HISTORY_LIMIT)),
            memory_config: MemoryConfig::default(),
            circuit_config: CircuitConfig::default(),
            optimizer: Arc::new(parking_lot::RwLock::new(optimizer)),
            projects: Arc::new(RwLock::new(HashMap::new())),
            overseer: build.overseer,
            overseer_handler: build.handler,
            llm: build.llm,
            session_manager_agent: build_session_manager_agent(&framework_root),
            audit: make_audit_logger(&framework_root),
            bus: Arc::new(crate::daemon::bus::PeerBus::new(
                &framework_root.join("logs"),
            )),
            paired_chat_id: Mutex::new(paired),
            pair_code: Mutex::new(None),
            // #6505: a temp-rooted daemon reads its error stores under that same
            // root, so neither transport re-resolves the process data directory.
            error_store_base: Some(framework_root.clone()),
            framework_root,
            event_tx,
            managed_sessions: tokio::sync::OnceCell::new(),
            activity_monitor: std::sync::OnceLock::new(),
            project_registry: tokio::sync::OnceCell::new(),
            deliverable_manager: tokio::sync::OnceCell::new(),
            session_registry,
            proxy_focus: Arc::new(std::sync::Mutex::new(HashMap::new())),
            supervised: std::sync::atomic::AtomicBool::new(true),
            unsupervised_forced: std::sync::atomic::AtomicBool::new(false),
            launchd_supervision: std::sync::RwLock::new(String::new()),
            manager,
            provisioning: crate::daemon::provisioning::ProvisioningRegistry::default(),
            nudge_ledger: parking_lot::Mutex::new(crate::core::idle_nudge::NudgeLedger::new()),
            shared_tree_claim: parking_lot::Mutex::new(()),
            dispatch_record: parking_lot::Mutex::new(()),
            pending_stops: super::pending_stops::PendingStops::default(),
        }
    }

    /// The framework root directory this daemon was configured with.
    ///
    /// Why: the `GET /health` catalog-staleness check (HR-3) resolves the harness
    /// manifest and the deployed-content manifests under this root; exposing it
    /// lets the handler build a [`crate::core::paths::FrameworkPaths`] anchored to
    /// the SAME root the daemon uses (a temp dir in tests, `~/.trusty-mpm` in
    /// production) instead of recomputing the default.
    /// What: returns the resolved framework root path.
    /// Test: `health_reports_catalog_unknown_without_catalog` builds state
    /// `with_root` a tempdir and asserts the handler reads it.
    pub fn framework_root(&self) -> &std::path::Path {
        &self.framework_root
    }

    /// The base this daemon reads captured-error stores from, when one is
    /// pinned (#6505).
    ///
    /// Why: see [`Self::error_store_base`] — `core_ops::list_errors` must resolve
    /// the four store paths the same way on both transports, and re-reading the
    /// process data directory per call does not guarantee that.
    /// What: `Some` only for a daemon built with [`Self::with_paths`]; `None`
    /// means "resolve the OS data directory", the production behaviour.
    /// Test: `parity_errors_agrees_across_transports`.
    #[must_use]
    pub fn error_store_base(&self) -> Option<&std::path::Path> {
        self.error_store_base.as_deref()
    }

    /// The idle-park auto-nudge ledger (#2621).
    ///
    /// Why: the `ingest_hook` nudge path (`daemon::idle_nudge`) lives outside the
    /// `state` module, so it needs a public handle to the `pub(super)` ledger to
    /// read a session's [`crate::core::idle_nudge::NudgeRecord`] and record a
    /// delivery under the same lock.
    /// What: returns a reference to the `parking_lot::Mutex`-guarded
    /// [`crate::core::idle_nudge::NudgeLedger`].
    /// Test: `daemon::idle_nudge` tests lock and mutate it via this accessor.
    pub fn nudge_ledger(&self) -> &parking_lot::Mutex<crate::core::idle_nudge::NudgeLedger> {
        &self.nudge_ledger
    }

    /// Whether this daemon process is currently considered safely supervised
    /// (issue #2486). See [`Self::supervised`] field doc for the full rationale.
    /// What: relaxed-ordering read — this is a startup-once flag, not a
    /// synchronization primitive; any ordering suffices.
    /// Test: `health_response_serializes_supervised_field`.
    pub fn supervised(&self) -> bool {
        self.supervised.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Set the launchd-supervision signal (issue #2486). Called exactly once
    /// by `daemon_run::run_daemon` right after startup, using the value
    /// computed by `commands::launchd_probe::compute_supervised`.
    /// What: relaxed-ordering store — see [`Self::supervised`].
    /// Test: `health_response_serializes_supervised_field` exercises the
    /// default; the daemon e2e suite exercises the post-`run_daemon` value.
    pub fn set_supervised(&self, value: bool) {
        self.supervised
            .store(value, std::sync::atomic::Ordering::Relaxed);
    }

    /// Record the THREE-STATE launchd answer for `/health` (issue #4469).
    ///
    /// Why: the `supervised` bool cannot distinguish "launchd says no" from
    /// "launchd could not be asked", and the second must never drive the
    /// destructive orphan remediation.
    /// What: stores the discriminant string; a poisoned lock is recovered from
    /// rather than panicking a running daemon over a diagnostic field.
    /// Test: `health_response_serializes_launchd_supervision_field`.
    pub fn set_launchd_supervision(&self, value: impl Into<String>) {
        let mut slot = self
            .launchd_supervision
            .write()
            .unwrap_or_else(|e| e.into_inner());
        *slot = value.into();
    }

    /// Read the three-state launchd answer recorded at startup (issue #4469).
    ///
    /// Why: `/health` publishes it so clients can tell UNKNOWN from a negative.
    /// What: the stored discriminant, or `""` before startup recorded one.
    /// Test: `health_response_serializes_launchd_supervision_field`.
    pub fn launchd_supervision(&self) -> String {
        self.launchd_supervision
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Whether this daemon was started with the `tm daemon --force` opt-in
    /// (issue #4230). See [`Self::unsupervised_forced`] field doc for why an
    /// unsupervised run must be distinguishable from an unwanted orphan.
    /// What: relaxed-ordering read — a startup-once flag, not a synchronization
    /// primitive.
    /// Test: `health_response_serializes_forced_field`.
    pub fn unsupervised_forced(&self) -> bool {
        self.unsupervised_forced
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Record the `--force` opt-in (issue #4230). Called exactly once by
    /// `daemon_run::run_daemon` alongside [`Self::set_supervised`].
    /// What: relaxed-ordering store — see [`Self::unsupervised_forced`].
    /// Test: `health_response_serializes_forced_field` exercises the default; the
    /// daemon e2e suite exercises the post-`run_daemon` value.
    pub fn set_unsupervised_forced(&self, value: bool) {
        self.unsupervised_forced
            .store(value, std::sync::atomic::Ordering::Relaxed);
    }

    /// The shared focus-state store for the local session-manager PROXY routes.
    ///
    /// Why: `daemon::managed_routes::proxy` constructs a fresh
    /// [`crate::client::SessionProxy`] per request (mirroring every other
    /// stateless handler in this file) via
    /// [`crate::client::SessionProxy::with_focus_store`], which needs a shared,
    /// cross-request handle to this daemon's focus map.
    /// What: clones the `Arc` (cheap) around the `Mutex<HashMap<..>>`; every
    /// clone guards the SAME underlying map.
    /// Test: `daemon::managed_routes::proxy::tests`, `tests/proxy_routes.rs`.
    pub fn proxy_focus_store(
        &self,
    ) -> Arc<std::sync::Mutex<HashMap<String, crate::client::proxy::FocusTarget>>> {
        Arc::clone(&self.proxy_focus)
    }

    /// The Layer-3 portfolio manager state (`tm manager`, epic #2109).
    ///
    /// Why: the `/api/v1/manager/*` handlers reach the portfolio palace (and, in
    /// later phases, the inference adapter + poll loop) through this shared
    /// handle — the same cheap-`Arc`-clone accessor shape as
    /// [`Self::proxy_focus_store`], provisioned once at daemon startup.
    /// What: clones the `Arc<ManagerState>` (cheap); every clone shares the SAME
    /// provisioned manager state.
    /// Test: `manager_state_is_provisioned` in `state/tests.rs`;
    /// `manager_version_route_reports_capabilities` in `tests/manager_routes.rs`.
    pub fn manager_state(&self) -> Arc<crate::daemon::manager::ManagerState> {
        Arc::clone(&self.manager)
    }

    /// The PID-file registry rooted under this daemon's framework root.
    ///
    /// Why: the §10.3 orphan-GC needs a single, consistent answer for "where do
    /// daemon-owned `claude` child PIDs get recorded?". Deriving it from
    /// [`framework_root`](Self::framework_root) keeps the `pids/` directory under
    /// the SAME root the daemon uses — a temp dir in tests, `~/.trusty-mpm` in
    /// production — so the spawn path, the session-drop path, and the GC sweep all
    /// agree without any global state.
    /// What: returns a fresh [`crate::core::pid_registry::PidRegistry`] over
    /// `<framework_root>/pids`. The handle is cheap (just a path), so a new one
    /// per call is fine and avoids holding a long-lived field.
    /// Test: `pid_registry_is_under_framework_root` in `state/tests.rs`.
    pub fn pid_registry(&self) -> crate::core::pid_registry::PidRegistry {
        crate::core::pid_registry::PidRegistry::under(&self.framework_root)
    }

    /// Return the shared activity monitor, constructing it on first access.
    ///
    /// Why: the `/sessions/managed/{id}/activity` handler needs a single,
    /// shared `ActivityMonitor` whose per-session content-hash cache persists
    /// across requests; a `OnceCell` amortises the construction cost and
    /// guarantees the same cache is reused for every request.
    /// What: on first call builds `ActivityMonitor<OpenRouterClassifier>` and
    /// labels it with the slug the classifier itself resolved (#4427: taken from
    /// `classifier.model()` rather than re-reading `TRUSTY_LLM_MODEL` here, so
    /// the model recorded in the cost metrics can never drift from the model
    /// actually requested); returns the shared `Arc` on every subsequent call.
    /// Test: `handler_activity_cache_hit` in `tests/session_manager_mvp.rs`.
    pub fn activity_monitor(
        &self,
    ) -> std::sync::Arc<
        crate::activity::monitor::ActivityMonitor<crate::activity::OpenRouterClassifier>,
    > {
        self.activity_monitor
            .get_or_init(|| {
                let classifier = crate::activity::OpenRouterClassifier::new();
                let model = classifier.model().to_owned();
                Arc::new(crate::activity::monitor::ActivityMonitor::new(
                    classifier, model,
                ))
            })
            .clone()
    }

    /// Return the lazily-initialized managed [`SessionManager`](crate::session_manager::SessionManager).
    ///
    /// Why: the `/sessions/managed` handlers need a single shared session
    /// manager backed by an on-disk store and a real tmux driver. Because the
    /// manager's constructor is async, it is built on first access and cached
    /// in a `OnceCell` so every subsequent request reuses the same handle.
    /// What: on first call, loads the store under `<framework_root>/session-manager`
    /// with a [`crate::daemon::tmux::TmuxDriver`] and caches the `Arc`; returns
    /// the shared handle. Falls back to an in-memory temp dir if store load fails
    /// so a transient I/O error never poisons the OnceCell permanently.
    ///
    /// Scratch-data-root gate (#6348): [`crate::daemon::tmux::TmuxDriver::discover`]
    /// asks only whether `$HOME` was reassigned, so a daemon isolated by its
    /// framework root alone resolved a real driver and let `reconcile_on_boot`
    /// adopt the operator's live panes into a throwaway store. This asks
    /// [`crate::core::host_state_gate::host_state_access_for_root`] with the
    /// root this daemon actually runs against, and installs the same no-op
    /// driver the missing-tmux path installs when it refuses — that driver
    /// reports tmux UNOBSERVABLE rather than empty, so `reconcile_on_boot`
    /// leaves every record alone instead of reading a scratch store as proof.
    /// Test: `managed_routes` handler tests drive this via the router;
    /// `session_manager_refuses_tmux_on_a_scratch_framework_root`.
    pub async fn session_manager(&self) -> std::sync::Arc<crate::session_manager::SessionManager> {
        self.managed_sessions
            .get_or_init(|| async {
                let data_dir = self.framework_root.join("session-manager");
                // Use the real tmux-backed driver when available; fall back to a
                // no-op driver when `tmux` is not installed so the API still
                // responds (operations that need tmux will surface a typed error).
                // #6348: a scratch framework root refuses here too, not just a
                // scratch $HOME.
                let refusal =
                    crate::core::host_state_gate::host_state_access_for_root(&self.framework_root)
                        .skip_reason();
                let tmux: std::sync::Arc<dyn crate::session_manager::ManagedTmuxDriver> =
                    match refusal {
                        Some(reason) => {
                            tracing::warn!("#6348: managed-session tmux refused — {reason}");
                            std::sync::Arc::new(crate::session_manager::real_tmux::NoopTmuxDriver)
                        }
                        None => match crate::session_manager::RealTmuxDriver::discover() {
                            Ok(d) => std::sync::Arc::new(d),
                            Err(e) => {
                                tracing::warn!("tmux unavailable for managed sessions: {e}");
                                std::sync::Arc::new(
                                    crate::session_manager::real_tmux::NoopTmuxDriver,
                                )
                            }
                        },
                    };
                let mgr = match crate::session_manager::SessionManager::new(&data_dir, tmux.clone())
                    .await
                {
                    Ok(mgr) => mgr,
                    Err(e) => {
                        tracing::error!(
                            "failed to load managed session store at {}: {e}; using temp dir",
                            data_dir.display()
                        );
                        let tmp = std::env::temp_dir().join("trusty-mpm-session-manager");
                        let _ = std::fs::create_dir_all(&tmp);
                        crate::session_manager::SessionManager::new(&tmp, tmux)
                            .await
                            .expect("temp-dir session store must load")
                    }
                };
                // Reconcile persisted session records against live tmux state:
                // sessions whose tmux is gone are flipped to Stopped (resumable);
                // live sessions are re-adopted as Active.
                let auto_resume = std::env::var("TRUSTY_MPM_AUTO_RESUME")
                    .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                    .unwrap_or(false);
                match mgr.reconcile_on_boot(auto_resume).await {
                    Ok(report) => {
                        let n_adopted = report.adopted.len();
                        let n_stopped = report.stopped.len();
                        let n_external = report.external_adopted.len();
                        let n_stale_decisions = report.stale_decisions_cleared.len();
                        // #6118: a declined pane leaves no record, so this
                        // summary is the only place the boot accounts for it.
                        // It must gate the log too — a boot whose ONLY finding
                        // was declined panes would otherwise print nothing.
                        let n_declined = report.adoption_declined.len();
                        // #6116: same reasoning as `n_declined` — a refused
                        // test session leaves no record, and a swept one
                        // leaves only a tombstone, so a boot whose only
                        // finding was either would otherwise print nothing.
                        let n_test_refused = report.reserved_test_refused.len();
                        let n_test_swept = report.reserved_test_swept.len();
                        if n_adopted > 0
                            || n_stopped > 0
                            || n_external > 0
                            || n_stale_decisions > 0
                            || n_declined > 0
                            || n_test_refused > 0
                            || n_test_swept > 0
                        {
                            tracing::info!(
                                adopted = n_adopted,
                                stopped = n_stopped,
                                external = n_external,
                                stale_decisions_cleared = n_stale_decisions,
                                adoption_declined = n_declined,
                                reserved_test_refused = n_test_refused,
                                reserved_test_swept = n_test_swept,
                                "session-manager reconcile complete"
                            );
                        }
                    }
                    Err(e) => {
                        tracing::warn!("session-manager reconcile failed: {e}");
                    }
                }
                std::sync::Arc::new(mgr)
            })
            .await
            .clone()
    }

    /// Inject a pre-built session manager into the OnceCell for testing.
    ///
    /// Why: handler-level tests need a `SessionManager` backed by a fake tmux
    /// driver; this constructor lets tests seed the cell before the first request
    /// fires so `session_manager()` returns the pre-built instance.
    /// What: sets `managed_sessions` to `mgr`; calling `session_manager()` after
    /// this returns the injected value without touching the real tmux binary or
    /// disk store paths.
    /// Test: used by `handler_spawn_wires_provision_and_spawn` and
    /// `handler_activity_cache_hit` in `tests/session_manager_mvp.rs`.
    #[cfg(test)]
    pub fn with_session_manager(
        mgr: std::sync::Arc<crate::session_manager::SessionManager>,
    ) -> Self {
        let state = Self::new();
        let _ = state.managed_sessions.set(mgr);
        state
    }

    /// Construct test state whose managed sessions use a fake no-op tmux driver
    /// (#1734, #1790).
    ///
    /// Why: tests that exercise the managed-session API surface must NEVER touch
    /// the production `~/.trusty-mpm` store nor spawn real managed (`tm-*`/`tmpm-*`) tmux sessions.
    ///
    /// The lazy `session_manager()` initialiser calls `RealTmuxDriver::discover()`
    /// followed by `reconcile_on_boot`, which (a) adopts any managed (`tm-*`/`tmpm-*`) sessions from
    /// the host into the test-owned store and (b) creates real tmux sessions through
    /// `create_with_id`. Those real sessions escape into the host and get adopted by
    /// the production daemon's next `reconcile_on_boot`, polluting
    /// `~/.trusty-mpm/session-manager/sessions.json` with phantom sessions (#1790).
    ///
    /// Pre-seeding the `managed_sessions` OnceCell here with a
    /// `FakeNoopTmuxDriver`-backed manager prevents both failure modes:
    /// - The cell is already full when the first request fires, so the real tmux
    ///   binary is never invoked and the operator's `sessions.json` is never read.
    /// - `FakeNoopTmuxDriver::create_session` returns `Ok(())` without running
    ///   `tmux`, so tests that seed sessions via `create_with_id` produce record-only
    ///   state with no real pane on the host.
    /// - `FakeNoopTmuxDriver::list_sessions` returns an empty list, so
    ///   `reconcile_on_boot` (not called here) would never adopt real host sessions.
    ///
    /// What: asserts `root` is NOT the production `~/.trusty-mpm` (fail-fast guard),
    /// calls `with_root(root.clone())` for an isolated framework root (pairing,
    /// audit log, optimizer — all under the temp dir), then builds a `SessionManager`
    /// over `root/session-manager` with `FakeNoopTmuxDriver` and pre-seeds the
    /// `managed_sessions` OnceCell before any request fires.
    /// Test: `execute_health_against_test_daemon`,
    /// `execute_managed_list_against_test_daemon` in `client::executor::tests`;
    /// all session-manager handler tests in `tests/session_manager_mvp.rs` (#1790).
    ///
    /// Note: not gated on `#[cfg(test)]` so that integration tests in `tests/`
    /// (which compile the library without the `test` cfg) can call it. Never
    /// called in production binaries.
    #[doc(hidden)]
    pub async fn with_root_isolated_managed(root: std::path::PathBuf) -> Self {
        use crate::session_manager::real_tmux::FakeNoopTmuxDriver;

        let fake_tmux: std::sync::Arc<dyn crate::session_manager::ManagedTmuxDriver> =
            std::sync::Arc::new(FakeNoopTmuxDriver);
        Self::with_root_isolated_managed_and_driver(root, fake_tmux).await
    }

    /// Same as [`Self::with_root_isolated_managed`], but with an INJECTABLE tmux
    /// driver instead of the hardcoded [`crate::session_manager::real_tmux::FakeNoopTmuxDriver`] (#2022).
    ///
    /// Why: `FakeNoopTmuxDriver` is intentionally stateless — `create_session`
    /// always reports success without recording anything, and
    /// `list_sessions`/`session_exists` always report NOTHING is live. Many
    /// existing tests rely on exactly that ("no real tmux session escapes the
    /// test") and must not change. But the #2022 fix made the delete/prune
    /// running-guard a REAL tmux liveness probe — a test that specifically wants
    /// to exercise "a genuinely running session is still guarded without
    /// `--force`" needs a driver that actually TRACKS created/killed sessions so
    /// the probe has something real to observe. This constructor is the
    /// injection point for that, without touching `FakeNoopTmuxDriver`'s
    /// behavior (and therefore without touching the many unrelated tests that
    /// depend on it).
    /// What: identical to `with_root_isolated_managed` (same production-store
    /// guard, same isolated framework root, same `managed_sessions` pre-seed)
    /// except the caller supplies `driver` instead of a fixed `FakeNoopTmuxDriver`.
    /// Test: `session_delete_refuses_running_then_force_bypasses` (`mcp_session`
    /// tests); `delete_route_refuses_running_without_force` in
    /// `tests/session_manager_mvp.rs`.
    #[doc(hidden)]
    pub async fn with_root_isolated_managed_and_driver(
        root: std::path::PathBuf,
        driver: std::sync::Arc<dyn crate::session_manager::ManagedTmuxDriver>,
    ) -> Self {
        use crate::session_manager::SessionManager;

        // Hard guard: a test must NEVER bind to the production managed store.
        // Fail loudly here so the culprit test is easy to identify (#1790).
        if let Some(home) = dirs::home_dir() {
            let prod_root = home.join(".trusty-mpm");
            assert_ne!(
                root, prod_root,
                "with_root_isolated_managed must NOT point at the production \
                 store (~/.trusty-mpm). Pass a tempfile::TempDir path instead."
            );
        }

        let data_dir = root.join("session-manager");
        let _ = tokio::fs::create_dir_all(&data_dir).await;
        let mgr = SessionManager::new(&data_dir, driver)
            .await
            .expect("temp-dir fake session store must load");
        let state = Self::with_root(root);
        let _ = state.managed_sessions.set(std::sync::Arc::new(mgr));
        state
    }

    /// Inject a pre-built Session Manager agent for testing the SM endpoint path.
    ///
    /// Why: the `coordinator/chat` SM-path tests (SM-7) need a `DaemonState`
    /// carrying an ENABLED agent wired to a mock resolver, so the endpoint routes
    /// through the SM with no network. The default `new()` builds a
    /// disabled-by-default agent from the real config; this swaps it for the
    /// test agent. We also clear the legacy `llm` overseer so a test running on a
    /// developer machine with `OPENROUTER_API_KEY` set cannot accidentally route
    /// through the real LLM on the legacy path — all tests using this constructor
    /// are self-contained SM tests that never exercise the overseer fallback, so
    /// removing it makes the hermetic claim in every caller actually true.
    /// What: builds default state, replaces `session_manager_agent` with `agent`,
    /// and clears `llm` so the legacy overseer fallback path returns 503.
    /// Test: used by `api_tests` SM-path tests (`sm_chat_*`, alias tests,
    /// `disabled_sm_falls_back_to_legacy_503`).
    #[cfg(test)]
    pub fn with_session_manager_agent(agent: Arc<crate::core::sm::SessionManagerAgent>) -> Self {
        let mut state = Self::new();
        state.session_manager_agent = agent;
        // Clear the legacy LlmOverseer so tests are truly hermetic regardless of
        // whether OPENROUTER_API_KEY is set in the environment. Tests that need
        // the SM path never reach the legacy overseer, and the one test that
        // checks the legacy 503 (`disabled_sm_falls_back_to_legacy_503`) relies
        // on this being absent.
        state.llm = None;
        state
    }

    /// Return the shared project registry, constructing it on first access (#1519).
    ///
    /// Why: `ProjectRegistry::load` is async and needs a data directory; deferring
    /// construction to first use (like `session_manager()`) keeps the synchronous
    /// `new()` constructors unblocked. At startup the daemon seeds the registry
    /// from `config.yaml`'s `projects:` list and from session history.
    ///
    /// #3822 hardening (code-critic review): the session-history seed used to
    /// read `self.managed_sessions.get()` — a bare peek that returns `None`
    /// unless SOME other call site had already raced ahead and warmed the
    /// session-manager `OnceCell` first. That made the boot-time seed's
    /// completeness an unenforced ordering invariant ("whichever of
    /// `project_registry()`'s 15+ call sites fires first must not be the
    /// first thing that touches the daemon" — easy to silently violate on
    /// any future startup-sequence reorder, resurrecting #3822 for
    /// PRE-EXISTING sessions even after `register_from_session` closed the
    /// gap for newly-spawned ones). Calling `self.session_manager().await`
    /// instead `get_or_init`s it right here if nothing has touched it yet —
    /// the seed is now correct regardless of call order, structurally, not
    /// by accident of which caller happened to run first.
    /// What: on first call loads the registry from `<framework_root>/project-registry/`,
    /// seeds it from config and session history (via `self.session_manager()`,
    /// which is safe to call from inside this `OnceCell::get_or_init` closure —
    /// a DIFFERENT `OnceCell` than `self.project_registry`, so no self-deadlock),
    /// and caches the `Arc`. Subsequent calls return the cached handle.
    /// Test: `mcp_project` dispatch tests exercise this via the mock backend
    /// path; `project_registry_seeds_session_history_without_prewarmed_managed_sessions`
    /// pins that pre-existing sessions are registered at first `project_registry()`
    /// touch even when nothing warmed `managed_sessions` first (the #3822
    /// ordering-invariant regression this hardening closes).
    pub async fn project_registry(&self) -> std::sync::Arc<crate::project::ProjectRegistry> {
        // Warm (or reuse) the session manager BEFORE entering
        // `project_registry`'s own `get_or_init` — see the doc comment above
        // for why this replaces the old `self.managed_sessions.get()` peek.
        let sm = self.session_manager().await;
        self.project_registry
            .get_or_init(|| async {
                // #4300: one shared helper names this directory so the
                // out-of-process `tm` CLI reader can never point at a
                // different file than the daemon writes.
                let data_dir = crate::project::registry_data_dir_under(&self.framework_root);
                let registry = match crate::project::ProjectRegistry::load(&data_dir).await {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::error!(
                            "failed to load project registry at {}: {e}; using temp dir",
                            data_dir.display()
                        );
                        let tmp = std::env::temp_dir().join("trusty-mpm-project-registry");
                        let _ = std::fs::create_dir_all(&tmp);
                        crate::project::ProjectRegistry::load(&tmp)
                            .await
                            .expect("temp-dir project registry must load")
                    }
                };
                // Seed from config.yaml `projects:` list.
                let config = crate::core::trusty_tools_config::TrustyToolsConfig::load();
                if !config.projects.is_empty() {
                    registry.seed_from_config(&config.projects).await;
                }
                // Auto-register projects from existing session history.
                let records = sm.list().await;
                registry.auto_register_from_sessions(&records).await;
                std::sync::Arc::new(registry)
            })
            .await
            .clone()
    }

    /// Return the shared Deliverable/Milestone manager, building it on first
    /// access (§10.7, #2378).
    ///
    /// Why: the Deliverable/Milestone CRUD handlers need one shared manager
    /// backed by the central `deliverables.json` / `milestones.json` stores.
    /// Because [`DeliverableManager::load`](crate::deliverable::DeliverableManager::load)
    /// is async, it is built on first access and cached in a `OnceCell` so every
    /// subsequent request reuses the same handle — identical to
    /// [`Self::session_manager`] and [`Self::project_registry`].
    /// What: on first call loads the two central stores directly under
    /// `framework_root` (siblings to the registry, §10.7 / §13 Q5); falls back to
    /// a temp dir if load fails so a transient I/O error never poisons the
    /// `OnceCell` permanently.
    /// Test: the deliverable route tests drive this via the router.
    pub async fn deliverable_manager(
        &self,
    ) -> std::sync::Arc<crate::deliverable::DeliverableManager> {
        self.deliverable_manager
            .get_or_init(|| async {
                let data_dir = self.framework_root.clone();
                match crate::deliverable::DeliverableManager::load(&data_dir).await {
                    Ok(mgr) => std::sync::Arc::new(mgr),
                    Err(e) => {
                        tracing::error!(
                            "failed to load deliverable stores at {}: {e}; using temp dir",
                            data_dir.display()
                        );
                        let tmp = std::env::temp_dir().join("trusty-mpm-deliverables");
                        let _ = std::fs::create_dir_all(&tmp);
                        std::sync::Arc::new(
                            crate::deliverable::DeliverableManager::load(&tmp)
                                .await
                                .expect("temp-dir deliverable stores must load"),
                        )
                    }
                }
            })
            .await
            .clone()
    }
}
