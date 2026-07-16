//! `SessionRegistry`'s #2345 turn-recorder sink lazy-init/lookup. Split out
//! of `registry.rs` for the same 500-SLOC-cap reason `registry_events.rs`
//! was split out (#2344) — a child module of `registry` (declared via
//! `#[path = ...] mod memory_sink_ext;`), so it shares full access to
//! `SessionRegistry`'s private `lock` helper and `SessionEntry`'s private
//! `memory_sink` field exactly as if this method were still defined in
//! `registry.rs`.
//!
//! Why: the turn recorder's sink (and its background drain task) must be
//! constructed exactly ONCE per session — see the `memory_sink` module's own
//! docs — not once per `task.run`; this is the single lazy-init/lookup entry
//! point `task::executor::run_and_record` calls at each run's turn boundary.
//! What: [`SessionRegistry::memory_sink_for`].
//! Test: `registry_tests::memory_sink_for_reuses_the_same_sink_across_calls`,
//! `registry_tests::memory_sink_for_unknown_session_returns_none`.

use std::path::Path;

use crate::session::memory_sink::{TurnMemorySink, derive_palace_id_for_project};

use super::*;

impl SessionRegistry {
    /// Lazily construct (on first call) or return (on every later call) this
    /// session's durable turn-recorder sink.
    ///
    /// Why: `task::executor::run_and_record` needs the SAME
    /// `Arc<TurnMemorySink>` — and therefore the same background drain task
    /// and bounded queue — across every `task.run` on one session, not a
    /// fresh one per run (a fresh sink per run would spawn a new drain task
    /// each time, multiplying in-flight writers with no bound). Constructing
    /// it here, inside the session-map lock, means two calls can never build
    /// two different sinks for the same session even under a hypothetical
    /// race (in practice `Self::begin_execution`'s single-in-flight-run guard
    /// already prevents concurrent calls per session).
    /// What: `None` if `id` is unknown (best-effort — mirrors
    /// `Self::set_run_outcome`'s framing; the caller has already validated
    /// the session's existence earlier in the same run via
    /// `begin_execution`/`begin_pm_transcript`). On the FIRST call for a
    /// session, derives the palace id from `project_dir` via
    /// [`derive_palace_id_for_project`], resolves trusty-memory's base URL
    /// via `trusty_common::mcp::memory_rpc::resolve_memory_base_url_or_unreachable`
    /// (fail-open — the sink is built regardless of whether the daemon is
    /// currently reachable; every write attempt after that is independently
    /// fail-open, see `memory_sink::write_turn`), and stores the constructed
    /// `Arc`. On every SUBSEQUENT call, `project_dir` is ignored and the
    /// already-built sink is returned unchanged — a session's palace/base-url
    /// binding is fixed for its lifetime, mirroring
    /// `Self::begin_pm_transcript`'s analogous "the system prompt is fixed
    /// after the first call" rule.
    /// `project_dir` is `None` for a PROJECTLESS session, which returns `None`:
    /// a memory palace is project-scoped by construction, so with no project
    /// there is nothing to scope one to. This is a deliberate skip, not a
    /// degradation — the caller must NOT substitute the run's scratch root,
    /// since deriving a palace id from a throwaway temp path would mint a fresh,
    /// orphaned palace on every projectless run. The whole sink is already
    /// fail-open and fire-and-forget, so its absence costs a projectless run
    /// nothing but the durable turn record it has no home for anyway.
    /// Test: `registry_tests::memory_sink_for_reuses_the_same_sink_across_calls`,
    /// `registry_tests::memory_sink_for_unknown_session_returns_none`,
    /// `registry_tests::memory_sink_for_projectless_session_returns_none`.
    pub fn memory_sink_for(
        &self,
        id: &str,
        project_dir: Option<&Path>,
    ) -> Option<Arc<TurnMemorySink>> {
        let project_dir = project_dir?;
        let mut sessions = self.lock();
        let entry = sessions.get_mut(id)?;
        if entry.memory_sink.is_none() {
            let palace = derive_palace_id_for_project(project_dir);
            let base_url = trusty_common::mcp::memory_rpc::resolve_memory_base_url_or_unreachable();
            entry.memory_sink = Some(Arc::new(TurnMemorySink::new(base_url, palace)));
        }
        entry.memory_sink.clone()
    }
}
