//! `tm sm serve --stdio` — the SM JSON-RPC 2.0 over STDIO adapter (DOC-14 §1A.1).
//!
//! Why: the SM's PRIMARY, API-first interface (epic #1283, SM-STDIO #1291). A
//! parent `claude-mpm`/PM drives the SM headlessly over newline-delimited JSON-RPC
//! to exercise EVERY capability — chat, goals, session launch/observe/verify,
//! context, health — with no UI (the §1A.2 test topology
//! `claude-mpm ⟷ SM ⟷ t-mpm`). The adapter is a THIN mapping: each method maps
//! onto an existing surface — `SessionManagerAgent::chat` (SM-7), the goal store
//! (SM-6), the rolling-context engine (SM-5), the provider/health surface
//! (SM-2/§5.3), and the managed-session control surface (§2.6). No business logic
//! lives in the transport.
//!
//! STDOUT DISCIPLINE (CRITICAL): stdout is reserved EXCLUSIVELY for JSON-RPC
//! framing (one response object per line). EVERY diagnostic goes to stderr via
//! `tracing`. There is ZERO `println!`/`print!` in this module tree or the SM
//! code path it touches — `tests::no_stdout_writes_in_sm_paths` greps the source
//! and asserts that mechanically.
//!
//! What: [`SmDispatcher`] owns the SM surfaces and exposes the transport-neutral
//! [`SmDispatcher::dispatch`] (request → response). [`run_sm_stdio`] builds the
//! dispatcher from the daemon state and drives the shared
//! [`trusty_common::mcp::run_stdio_loop`] (line framing + stderr-only logging),
//! so the wire framing is the SAME proven loop trusty-memory/trusty-search use.
//! Test: `tests.rs` — each of the 14 methods round-trips with correct JSON-RPC
//! framing, plus parse-error / method-not-found / stdout-cleanliness / scripted
//! sequence coverage.

use std::path::PathBuf;
use std::sync::Arc;

use trusty_common::mcp::{Request, Response, run_stdio_loop};

use crate::core::sm::{SessionManagerAgent, SessionManagerConfig};

mod control;
mod dispatch;
mod methods;

pub use control::{DaemonSessionControl, LaunchParams, SessionControl, SessionControlError};

use tokio::sync::Mutex;

use crate::core::sm::{NoopGoalMemory, SmGoalStore};

/// The shared goal-store handle `sm.goals.*` and `sm.delegate` operate over (#1477).
///
/// Why: the goal store (SM-6) is feature-INDEPENDENT — it works over the
/// [`GoalMemory`](crate::core::sm::GoalMemory) seam — so `sm.goals.*` and the
/// delegation loop can operate in EVERY build. Under `sm-memory` the store is
/// backed by the durable SM palace; on the default build it is backed by a no-op
/// seam ([`NoopGoalMemory`]) so goals function in-memory (non-durable) rather than
/// returning "unavailable" (#1477). Keeping ONE handle type across both builds
/// keeps [`SmDispatcher::new`] a single signature.
/// What: an `Arc<Mutex<SmGoalStore>>` (the `&mut self` goal mutators serialise
/// through the mutex).
/// Test: `tests.rs::dispatcher_with` constructs it in both builds.
pub type SmGoalHandle = Arc<Mutex<SmGoalStore>>;

/// The transport-neutral SM dispatcher the stdio adapter drives (§1A.1).
///
/// Why: separating the dispatcher from the stdio loop is what makes the 14
/// methods HERMETICALLY testable — the round-trip tests construct an
/// `SmDispatcher` over mocks and call [`SmDispatcher::dispatch`] with constructed
/// JSON requests, with no real stdin/stdout, no network, and no tmux. The
/// dispatcher owns one handle per SM surface and does pure translation.
/// What: the SM [`SessionManagerAgent`] (chat + health), a config snapshot + the
/// `data_root` for opening the per-`conv_id` context engine (`sm.context.get`),
/// the [`SessionControl`] seam for `sm.sessions.*`, and the shared [`SmGoalStore`]
/// for `sm.goals.*` + `sm.delegate`. All four surfaces are present in EVERY build
/// (#1477): the goal store is palace-backed under `sm-memory` and no-op/in-memory
/// on the default build, so no `sm.*` method returns "unavailable" purely for lack
/// of the feature (only a degraded provider makes `sm.chat`/`sm.delegate` degrade).
/// Test: `tests.rs` builds this over the mock resolver + mock session control.
pub struct SmDispatcher {
    /// The SM agent (SM-7 chat + SM-2/§5.3 health). Shared, cheap to clone.
    agent: Arc<SessionManagerAgent>,
    /// Config snapshot used to open the context engine for `sm.context.get`
    /// (inference + rounds) — read-only, never mutated here. The context engine is
    /// compiled in every build (#1477), so this is consulted in every build.
    config: SessionManagerConfig,
    /// Storage root under which per-`conv_id` context-engine state lives (SM-5).
    data_root: PathBuf,
    /// The managed-session control surface for `sm.sessions.*` (§2.6).
    sessions: Arc<dyn SessionControl>,
    /// The SM goal store for `sm.goals.*` (SM-6) + the delegation loop, behind a
    /// mutex for the `&mut self` mutators. Present in EVERY build (#1477): palace-
    /// backed under `sm-memory`, no-op/in-memory on the default build.
    goals: Arc<Mutex<SmGoalStore>>,
}

impl SmDispatcher {
    /// Build a dispatcher over the SM surfaces (ONE signature, both builds).
    ///
    /// Why: a single constructor across `sm-memory` and the default build keeps
    /// the public API stable for callers and tests — they wire the SAME five
    /// arguments regardless of feature. The goal store is an [`Option`]: `Some`
    /// supplies a caller-built store (palace-backed under `sm-memory`, no-op on the
    /// default build); `None` defaults to an empty no-op store so the dispatcher
    /// always has a working goal surface (#1477).
    /// What: stores agent/config/data_root/sessions and the goal store (unwrapping
    /// the `Option`, defaulting to an empty [`NoopGoalMemory`]-backed store). No I/O.
    /// Test: `tests.rs::dispatcher_with` builds this with `Some`/`None`.
    pub fn new(
        agent: Arc<SessionManagerAgent>,
        config: SessionManagerConfig,
        data_root: impl Into<PathBuf>,
        sessions: Arc<dyn SessionControl>,
        goals: Option<SmGoalHandle>,
    ) -> Self {
        let data_root = data_root.into();
        let goals = goals.unwrap_or_else(|| Arc::new(Mutex::new(empty_goal_store(&data_root))));
        Self {
            agent,
            config,
            data_root,
            sessions,
            goals,
        }
    }

    /// Dispatch one JSON-RPC request to its mapped SM surface (transport-neutral).
    ///
    /// Why: the single seam the stdio loop and every round-trip test drive. It
    /// routes the method name, parses params, calls the mapped surface, and shapes
    /// the result/error into a JSON-RPC 2.0 [`Response`] (id echoed). Unknown
    /// methods → method-not-found; bad params → invalid-params; surface failures →
    /// a structured error — never a panic.
    /// What: delegates to [`dispatch::dispatch`].
    /// Test: `tests.rs` — all 14 methods + error paths.
    pub async fn dispatch(&self, req: Request) -> Response {
        dispatch::dispatch(self, req).await
    }
}

/// Run the SM JSON-RPC stdio adapter to EOF (the `tm sm serve --stdio` entry).
///
/// Why: the process entry point. It builds the [`SmDispatcher`] from the daemon
/// state (reusing the SM-7 agent construction and the managed-session surface),
/// then drives the shared newline-delimited JSON-RPC loop on stdin/stdout. Logs
/// go to stderr only (the loop never writes anything but framed responses to
/// stdout), so a parent driver gets a clean channel.
/// What: constructs the agent + control + (feature-gated) goal store rooted under
/// the daemon's SM data root, then runs [`run_stdio_loop`] forwarding each request
/// to [`SmDispatcher::dispatch`]. Returns `Ok(())` on stdin EOF.
/// Test: the dispatch logic is covered by `tests.rs`; this thin wiring is
/// exercised at runtime via `tm sm serve --stdio`.
pub async fn run_sm_stdio(state: Arc<crate::daemon::state::DaemonState>) -> anyhow::Result<()> {
    let dispatcher = Arc::new(build_dispatcher(state).await?);
    run_stdio_loop(move |req| {
        let dispatcher = dispatcher.clone();
        async move { dispatcher.dispatch(req).await }
    })
    .await
}

/// Build the production [`SmDispatcher`] from the daemon state.
///
/// Why: isolates the wiring (which differs by the `sm-memory` feature) from the
/// loop so [`run_sm_stdio`] stays readable. The agent and control reuse the
/// daemon's already-constructed handles so the stdio surface and the HTTP/chat
/// surfaces share one core.
/// What: clones the daemon's SM agent, reads the SM config + data root, builds a
/// [`DaemonSessionControl`], and — under `sm-memory` — loads the goal store from
/// the dedicated SM palace (falling back gracefully on a palace-open failure).
/// Test: covered indirectly; the dispatch behaviour is unit-tested in `tests.rs`.
async fn build_dispatcher(
    state: Arc<crate::daemon::state::DaemonState>,
) -> anyhow::Result<SmDispatcher> {
    let agent = state.session_manager_agent();
    let config = agent.config().clone();
    let data_root = state.sm_data_root();
    let sessions: Arc<dyn SessionControl> = Arc::new(DaemonSessionControl::new(state.clone()));

    let goals = build_goal_store(&data_root, &config).await;
    Ok(SmDispatcher::new(
        agent,
        config,
        data_root,
        sessions,
        Some(goals),
    ))
}

/// Load the SM goal store from the dedicated SM palace (`sm-memory` build).
///
/// Why: `sm.goals.*` are backed by the SM-6 dual-persistence store over the SM
/// palace. Loading rebuilds the goal map from the palace (truth) with a cache
/// fallback, so the stdio surface sees the same goals the chat surface does.
/// What: opens the SM palace under `<data_root>/palace`, wraps it as the goal
/// store's [`GoalMemory`], and `SmGoalStore::load`s it. A palace-open or load
/// failure degrades to an empty in-memory store (logged to stderr) rather than
/// failing the whole stdio surface.
/// Test: covered by the SM-6 store tests; this wiring is runtime-only.
#[cfg(feature = "sm-memory")]
async fn build_goal_store(
    data_root: &std::path::Path,
    config: &SessionManagerConfig,
) -> Arc<Mutex<SmGoalStore>> {
    use crate::core::sm::memory::SmMemory;

    let store = match SmMemory::open(data_root.join("palace"), &config.memory) {
        Ok(mem) => {
            let mem: Arc<dyn crate::core::sm::GoalMemory> = Arc::new(mem);
            match SmGoalStore::load(mem, data_root.to_path_buf()).await {
                Ok(store) => store,
                Err(e) => {
                    tracing::warn!(
                        "sm stdio: goal store load failed ({e}); starting empty in-memory store"
                    );
                    empty_goal_store(data_root)
                }
            }
        }
        Err(e) => {
            tracing::warn!("sm stdio: SM palace unavailable ({e}); goals start empty in-memory");
            empty_goal_store(data_root)
        }
    };
    Arc::new(Mutex::new(store))
}

/// Build the no-op/in-memory goal store for the default build (#1477).
///
/// Why: without `sm-memory` there is no palace, but the goal store is
/// feature-independent, so `sm.goals.*` and the delegation loop DEGRADE GRACEFULLY
/// to non-durable in-memory operation (backed by [`NoopGoalMemory`]) rather than
/// returning "unavailable". Durable persistence requires `--features sm-memory`.
/// What: wraps an empty [`empty_goal_store`] in the shared mutex handle.
/// Test: `tests.rs` no-feature `sm.goals.*` / `sm.delegate` branches.
#[cfg(not(feature = "sm-memory"))]
async fn build_goal_store(
    data_root: &std::path::Path,
    _config: &SessionManagerConfig,
) -> Arc<Mutex<SmGoalStore>> {
    Arc::new(Mutex::new(empty_goal_store(data_root)))
}

/// Build an empty goal store over the no-op palace seam ([`NoopGoalMemory`]).
///
/// Why: gives the store a seam that always succeeds with no entries — used both as
/// the default-build backing (#1477) and as the `sm-memory` palace-unavailable
/// fallback, so `sm.goals.*` answer (empty list / non-durable creates) rather than
/// failing.
/// What: constructs an [`SmGoalStore::new`] over a [`NoopGoalMemory`].
/// Test: covered by SM-6 store tests + the no-feature dispatch tests.
fn empty_goal_store(data_root: &std::path::Path) -> SmGoalStore {
    SmGoalStore::new(Arc::new(NoopGoalMemory), data_root.to_path_buf())
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
