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
//! It is therefore also the ONE place that knows the session's project root at
//! sink-construction time, which makes it the only place that can enforce the
//! #4638 bound: the recorder may bring a NEW palace into being for a durable
//! project root and never for an ephemeral one.
//! What: [`SessionRegistry::memory_sink_for`].
//! Test: `registry_tests::memory_sink_for_reuses_the_same_sink_across_calls`,
//! `registry_tests::memory_sink_for_unknown_session_returns_none`,
//! `registry_tests::memory_sink_for_many_sessions_mint_at_most_one_palace`.

use std::path::Path;

use tracing::debug;

use crate::session::memory_sink::{PalaceCreation, TurnMemorySink, derive_palace_id_for_project};

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
    /// via `trusty_common::memory_rpc::resolve_memory_base_url_or_unreachable`
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
    ///
    /// (#4638) This is also where the sink's [`PalaceCreation`] entitlement is
    /// decided, and that decision is the fix for the leak the paragraph above
    /// was written to prevent. The projectless branch guarded only the `None`
    /// case, so a session BOUND to a `tempfile::TempDir` walked straight
    /// through it: [`derive_palace_id_for_project`] fell to its
    /// `parent_dir_slug` level and produced a per-RUN-unique id
    /// (`t-tmp<random>` on macOS, from `$TMPDIR/.tmpXXXXXX`), and
    /// [`TurnMemorySink`]'s drain task auto-CREATED that palace on the live
    /// daemon at the first turn. Every such run minted one palace nothing would
    /// ever read again: 5,667 orphans in three weeks, 97.8% of every palace on
    /// the machine, which turned trusty-memory's O(n) full-registry handlers
    /// (#4637) into a ~90-minute `GET /api/v1/status`. A palace is an expensive
    /// object (usearch index, KG redb, drawer table, recall log), not a cheap
    /// namespace, and the trusty-code suite alone drives dozens of temp-rooted
    /// sessions per `cargo test` run.
    ///
    /// The BOUND: [`PalaceCreation::Allowed`] only for a DURABLE project root,
    /// so the recorder can create at most one palace per real project, reused
    /// by every session on that project forever — the same palace
    /// `catchup::pm_catchup_context` reads its digest from. N sessions create
    /// at most one palace, never N. Sessions stay distinguishable INSIDE it via
    /// `chat_turn_append`'s `session_id` and `memory_remember`'s
    /// `session:<id>` tag (exactly how #2348's `recall_session` already scopes
    /// its query), so no session-level recall is lost.
    ///
    /// An ephemeral root still gets a full sink — only the CREATE is withheld.
    /// That distinction is deliberate: the sink is what registers #2348's
    /// `recall_session` tool and what `run_and_record` reuses for its
    /// `base_url`/`palace`, so returning `None` here would silently change the
    /// PM's tool surface for a temp-rooted run. With
    /// [`PalaceCreation::Forbidden`] the drain task still writes into a palace
    /// that ALREADY exists and merely declines to bring a new one into being —
    /// see `memory_sink::drain`.
    /// Test: `registry_tests::memory_sink_for_reuses_the_same_sink_across_calls`,
    /// `registry_tests::memory_sink_for_unknown_session_returns_none`,
    /// `registry_tests::memory_sink_for_projectless_session_returns_none`,
    /// `registry_tests::memory_sink_for_temp_rooted_session_forbids_palace_create`,
    /// `registry_tests::memory_sink_for_many_sessions_mint_at_most_one_palace`.
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
            let base_url = trusty_common::memory_rpc::resolve_memory_base_url_or_unreachable();
            // #4638: only a durable project root entitles the recorder to bring
            // a new palace into being — a temp root's id is unique per run.
            let creation = if trusty_common::bin_resolve::is_under_system_temp(project_dir) {
                debug!(
                    project_dir = %project_dir.display(),
                    palace = %palace,
                    "turn_recorder: project root is under a system temp root — \
                     palace auto-create withheld (#4638)"
                );
                PalaceCreation::Forbidden
            } else {
                PalaceCreation::Allowed
            };
            entry.memory_sink = Some(Arc::new(TurnMemorySink::new(base_url, palace, creation)));
        }
        entry.memory_sink.clone()
    }
}
