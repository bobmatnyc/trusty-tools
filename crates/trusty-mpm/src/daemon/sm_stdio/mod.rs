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

#[cfg(feature = "sm-memory")]
use std::sync::Arc as StdArc;
#[cfg(feature = "sm-memory")]
use tokio::sync::Mutex;

#[cfg(feature = "sm-memory")]
use crate::core::sm::SmGoalStore;

/// The shared goal-store handle `sm.goals.*` operate over (feature-gated type).
///
/// Why: [`SmDispatcher::new`] takes ONE signature across both `sm-memory` and
/// the default build (a fragile dual-arity API was the prior shape). The goal
/// store only exists under `sm-memory`, so the constructor accepts this handle
/// as an `Option` that is simply always `None` in the no-memory build — gating
/// the TYPE, never the arity. Under `sm-memory` it is the live store behind a
/// mutex (the `&mut self` goal mutators serialise through it).
/// What: an `Arc<Mutex<SmGoalStore>>` under the feature; an uninhabited
/// `std::convert::Infallible` placeholder without it (never constructed —
/// callers pass `None`).
/// Test: `tests.rs::dispatcher_with` constructs `Some(..)`/`None` via this alias.
#[cfg(feature = "sm-memory")]
pub type SmGoalHandle = StdArc<Mutex<SmGoalStore>>;

/// Placeholder goal-store handle for the no-memory build (never constructed).
///
/// Why: lets [`SmDispatcher::new`] keep ONE signature without referencing the
/// `sm-memory`-only [`SmGoalStore`]. Callers in the default build always pass
/// `None`, so no value of this type is ever created.
/// What: a zero-sized uninhabitable-by-convention marker.
/// Test: compile-only; the no-memory `new` ignores its `Option<SmGoalHandle>`.
#[cfg(not(feature = "sm-memory"))]
pub type SmGoalHandle = std::convert::Infallible;

/// The transport-neutral SM dispatcher the stdio adapter drives (§1A.1).
///
/// Why: separating the dispatcher from the stdio loop is what makes the 14
/// methods HERMETICALLY testable — the round-trip tests construct an
/// `SmDispatcher` over mocks and call [`SmDispatcher::dispatch`] with constructed
/// JSON requests, with no real stdin/stdout, no network, and no tmux. The
/// dispatcher owns one handle per SM surface and does pure translation.
/// What: the SM [`SessionManagerAgent`] (chat + health), a config snapshot + the
/// `data_root` for opening the per-`conv_id` context engine (`sm.context.get`),
/// the [`SessionControl`] seam for `sm.sessions.*`, and — under `sm-memory` — the
/// shared [`SmGoalStore`] for `sm.goals.*`. Without the feature the goal store is
/// absent and `sm.goals.*`/`sm.context.get` return a graceful JSON-RPC error.
/// Test: `tests.rs` builds this over the mock resolver + mock session control.
pub struct SmDispatcher {
    /// The SM agent (SM-7 chat + SM-2/§5.3 health). Shared, cheap to clone.
    agent: Arc<SessionManagerAgent>,
    /// Config snapshot used to open the context engine for `sm.context.get`
    /// (inference + rounds) — read-only, never mutated here. Only consulted under
    /// `sm-memory` (the no-memory build returns the unavailable error before
    /// touching it).
    #[cfg_attr(not(feature = "sm-memory"), allow(dead_code))]
    config: SessionManagerConfig,
    /// Storage root under which per-`conv_id` context-engine state lives (SM-5).
    /// Only consulted under `sm-memory` (see `config`).
    #[cfg_attr(not(feature = "sm-memory"), allow(dead_code))]
    data_root: PathBuf,
    /// The managed-session control surface for `sm.sessions.*` (§2.6).
    sessions: Arc<dyn SessionControl>,
    /// The SM goal store for `sm.goals.*` (SM-6), behind a mutex for the
    /// `&mut self` mutators. Only present under `sm-memory`.
    #[cfg(feature = "sm-memory")]
    goals: StdArc<Mutex<SmGoalStore>>,
}

impl SmDispatcher {
    /// Build a dispatcher over the SM surfaces (ONE signature, both builds).
    ///
    /// Why: a single constructor across `sm-memory` and the default build keeps
    /// the public API stable for callers and tests — they wire the SAME five
    /// arguments regardless of feature. The goal store is an [`Option`]: `Some`
    /// under `sm-memory` (the live store), always `None` in the no-memory build
    /// (where `sm.goals.*`/`sm.context.get` return the graceful unavailable
    /// error). Only the goal-handle TYPE is feature-gated (via [`SmGoalHandle`]),
    /// never the arity.
    /// What: stores agent/config/data_root/sessions; under `sm-memory` also stores
    /// the unwrapped goal store (defaulting to an empty no-op store if `None`).
    /// No I/O.
    /// Test: `tests.rs::dispatcher_with` builds this with `Some`/`None` per build.
    pub fn new(
        agent: Arc<SessionManagerAgent>,
        config: SessionManagerConfig,
        data_root: impl Into<PathBuf>,
        sessions: Arc<dyn SessionControl>,
        goals: Option<SmGoalHandle>,
    ) -> Self {
        let data_root = data_root.into();
        #[cfg(feature = "sm-memory")]
        {
            let goals =
                goals.unwrap_or_else(|| StdArc::new(Mutex::new(empty_goal_store(&data_root))));
            Self {
                agent,
                config,
                data_root,
                sessions,
                goals,
            }
        }
        #[cfg(not(feature = "sm-memory"))]
        {
            // No-memory build: there is no goal store; the handle is never
            // constructed (callers always pass `None`).
            let _ = goals;
            Self {
                agent,
                config,
                data_root,
                sessions,
            }
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

    #[cfg(feature = "sm-memory")]
    {
        let goals = build_goal_store(&data_root, &config).await;
        Ok(SmDispatcher::new(
            agent,
            config,
            data_root,
            sessions,
            Some(goals),
        ))
    }
    #[cfg(not(feature = "sm-memory"))]
    {
        Ok(SmDispatcher::new(agent, config, data_root, sessions, None))
    }
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
) -> StdArc<Mutex<SmGoalStore>> {
    use crate::core::sm::memory::SmMemory;

    let store = match SmMemory::open(data_root.join("palace"), &config.memory) {
        Ok(mem) => {
            let mem: StdArc<dyn crate::core::sm::GoalMemory> = StdArc::new(mem);
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
    StdArc::new(Mutex::new(store))
}

/// Build an empty goal store over a no-op palace seam (degraded fallback).
///
/// Why: when the real palace is unavailable, `sm.goals.*` must still answer
/// (returning an empty list / accepting creates that simply do not durably
/// persist) rather than the whole stdio surface failing. A no-op [`GoalMemory`]
/// gives the store a seam that always succeeds with no entries.
/// What: constructs an [`SmGoalStore::new`] over a [`NoopGoalMemory`].
/// Test: runtime fallback; the store behaviour is covered by SM-6 tests.
#[cfg(feature = "sm-memory")]
fn empty_goal_store(data_root: &std::path::Path) -> SmGoalStore {
    SmGoalStore::new(StdArc::new(NoopGoalMemory), data_root.to_path_buf())
}

/// A [`GoalMemory`] that persists nothing and lists nothing (degraded seam).
///
/// Why: lets the goal store operate (in-memory only) when the real SM palace is
/// unavailable, so `sm.goals.*` degrade gracefully instead of erroring out.
/// What: `remember_goal` is a no-op success; `list_goals` returns an empty vec.
/// Test: runtime fallback only.
#[cfg(feature = "sm-memory")]
struct NoopGoalMemory;

#[cfg(feature = "sm-memory")]
#[async_trait::async_trait]
impl crate::core::sm::GoalMemory for NoopGoalMemory {
    async fn remember_goal(&self, _json: String, _tag: &str) -> Result<(), String> {
        Ok(())
    }
    async fn list_goals(&self, _tag: &str) -> Result<Vec<String>, String> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
