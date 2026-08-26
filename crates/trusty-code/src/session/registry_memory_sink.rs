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
use std::sync::Weak;

use tracing::{debug, warn};

use crate::session::memory_sink::{
    MemoryDurabilityObserver, MemoryTurnOutcome, PalaceCreation, TurnMemorySink,
    derive_palace_id_for_project,
};

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
    /// via `trusty_common::memory_rpc::resolve_memory_socket_or_unreachable`
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
    /// (#5811) A project root whose palace CANNOT BE RESOLVED gets no sink at
    /// all. [`derive_palace_id_for_project`] used to answer the shared literal
    /// `"unknown-project"` on any failure, and a durable root carries
    /// [`PalaceCreation::Allowed`], so two projects with unresolvable identity
    /// auto-created and then shared one palace holding both of their real
    /// prompts and responses. Declining the sink is the structural fix: there is
    /// no palace to name, so there is nothing to write into. The resolve runs
    /// BEFORE the temp-root check because an unresolvable identity is
    /// disqualifying whether or not the root is durable.
    ///
    /// An ephemeral root still gets a full sink — only the CREATE is withheld.
    /// That distinction is deliberate: the sink is what registers #2348's
    /// `recall_session` tool and what `run_and_record` reuses for its
    /// `socket`/`palace`, so returning `None` here would silently change the
    /// PM's tool surface for a temp-rooted run. With
    /// [`PalaceCreation::Forbidden`] the drain task still writes into a palace
    /// that ALREADY exists and merely declines to bring a new one into being —
    /// see `memory_sink::drain`.
    /// Test: `registry_tests::memory_sink_for_reuses_the_same_sink_across_calls`,
    /// `registry_tests::memory_sink_for_unknown_session_returns_none`,
    /// `registry_tests::memory_sink_for_projectless_session_returns_none`,
    /// `registry_tests::memory_sink_for_temp_rooted_session_forbids_palace_create`,
    /// `registry_tests::memory_sink_for_many_sessions_mint_at_most_one_palace`.
    ///
    /// (#2425) The receiver is `&Arc<Self>` rather than `&self` because the
    /// sink is handed a durability observer holding a `Weak<SessionRegistry>`,
    /// and a weak reference can only be minted from an `Arc`. `Weak` and not
    /// `Arc`: the registry owns the sink, which owns the observer, so a strong
    /// back-reference would be a cycle that never drops. This is a BREAKING
    /// change to `trusty-code`'s public API — every in-workspace caller already
    /// holds an `Arc<SessionRegistry>`, so no call site changed.
    pub fn memory_sink_for(
        self: &Arc<Self>,
        id: &str,
        project_dir: Option<&Path>,
    ) -> Option<Arc<TurnMemorySink>> {
        let project_dir = project_dir?;
        // #5811: never let a failed resolution become a shared, auto-creatable
        // palace id — decline the sink instead.
        let palace = match derive_palace_id_for_project(project_dir) {
            Ok(palace) => palace,
            Err(e) => {
                warn!(
                    project_dir = %project_dir.display(),
                    error = %e,
                    "turn_recorder: no palace could be resolved for this project — \
                     turn recording is DISABLED for this session (#5811)"
                );
                return None;
            }
        };
        let mut sessions = self.lock();
        let entry = sessions.get_mut(id)?;
        if entry.memory_sink.is_none() {
            let socket = trusty_common::memory_rpc::resolve_memory_socket_or_unreachable();
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
            // #2425: the sink reports every turn's durability verdict back to
            // this session's retained status.
            let observer = Arc::new(RegistryMemoryDurabilityObserver {
                registry: Arc::downgrade(self),
                session_id: id.to_string(),
            });
            entry.memory_sink = Some(Arc::new(TurnMemorySink::new_observed(
                socket, palace, creation, observer,
            )));
        }
        entry.memory_sink.clone()
    }

    /// Fold one turn's durability verdict into the session's retained status,
    /// emitting a session log event at each streak threshold it crosses
    /// (#2425).
    ///
    /// Why: a fail-open sink makes a session whose durable history is thinning
    /// look identical to a healthy one. Warning at the FIRST and THIRD
    /// consecutive failure — rather than every failure — is what keeps a
    /// multi-hour outage from burying the event log while still surfacing the
    /// problem on the turn it starts.
    /// What: `Err(session_not_found)` if the session is gone. `Err(internal)`
    /// when the reconciler refuses the outcome because its pending-run bound is
    /// exhausted; before returning that, `unrecorded_outcomes` is incremented,
    /// so `session.get_transcript` still SAYS its counters under-report rather
    /// than reading as if nothing was lost. Warning messages carry only the
    /// closed [`crate::session::MemoryFailureCategory`] vocabulary and the two
    /// counters — never a prompt, response, or daemon error string.
    /// Test: `registry_tests::memory_durability_retains_counts_resets_streak_and_warns_at_one_and_three`,
    /// `registry_tests::outcome_beyond_the_reorder_bound_is_counted_as_unrecorded`,
    /// `registry_tests::memory_degradation_event_is_redacted`.
    pub(crate) fn record_memory_durability(
        &self,
        id: &str,
        outcome: MemoryTurnOutcome,
    ) -> Result<(), RpcError> {
        let warnings = {
            let mut sessions = self.lock();
            let entry = sessions
                .get_mut(id)
                .ok_or_else(|| RpcError::session_not_found(id))?;
            match entry
                .memory_outcomes
                .observe(&mut entry.memory_durability, outcome)
            {
                Ok(warnings) => warnings,
                Err(()) => {
                    entry.memory_durability.unrecorded_outcomes = entry
                        .memory_durability
                        .unrecorded_outcomes
                        .saturating_add(1);
                    return Err(RpcError::internal("memory outcome reorder bound exceeded"));
                }
            }
        };
        for warning in warnings {
            let total = self
                .get_transcript(id)?
                .memory_durability
                .total_failed_turns;
            self.record_log(
                id,
                "warn",
                &format!(
                    "durable memory degraded: category={:?} total_failed_turns={total} \
                     consecutive_failed_turns={}",
                    warning.category, warning.consecutive
                ),
            )?;
        }
        Ok(())
    }
}

/// The [`MemoryDurabilityObserver`] a registry-owned sink is built with.
///
/// Why: `Weak` breaks the registry -> sink -> observer -> registry cycle, so a
/// dropped registry stays dropped.
/// What: forwards each outcome to [`SessionRegistry::record_memory_durability`]
/// on the thread that produced it.
/// Test: `observer_tests::weak_observer_does_not_keep_registry_alive`,
/// `observer_tests::unrecordable_outcomes_are_reported_in_subprocess`.
struct RegistryMemoryDurabilityObserver {
    registry: Weak<SessionRegistry>,
    session_id: String,
}

impl MemoryDurabilityObserver for RegistryMemoryDurabilityObserver {
    /// Report one outcome, failing OPEN on the turn and never SILENT on the
    /// signal (#2425).
    ///
    /// Why: this runs on the turn path (a synchronous queue-full report from
    /// `enqueue`) as well as in the detached drain, so it must not propagate,
    /// block, or panic — a failed memory write must not kill the turn. That is
    /// the execution contract, and it is unchanged. What it does NOT license is
    /// discarding the error: `record_memory_durability` fails exactly when the
    /// degradation signal could not be retained, so swallowing it made the
    /// signal loss itself unobservable — which is the failure #2425 exists to
    /// prevent. The registry-side counter this pairs with (`unrecorded_outcomes`)
    /// survives only while the session does; the `warn!` here survives the
    /// session's removal, which is the `session_not_found` case.
    /// What: a dead registry means the session and its status died with it, so
    /// there is nothing left to degrade and nothing to report. Any other error
    /// is warned about with the RPC code and no session content.
    /// Test: `observer_tests::weak_observer_does_not_keep_registry_alive`,
    /// `observer_tests::unrecordable_outcomes_are_reported_in_subprocess`.
    fn observe(&self, outcome: MemoryTurnOutcome) {
        let Some(registry) = self.registry.upgrade() else {
            return;
        };
        if let Err(error) = registry.record_memory_durability(&self.session_id, outcome) {
            warn!(
                failure_category = "memory_durability_unrecorded",
                rpc_code = error.code,
                "turn_recorder: a durable-memory outcome could not be recorded on its \
                 session — retained degradation counters under-report (#2425)"
            );
        }
    }
}

#[cfg(test)]
mod observer_tests {
    use super::*;
    use crate::session::memory_sink::MemoryFailureCategory;
    use chrono::Utc;
    use std::process::Command;

    const UNRECORDABLE_CHILD_ENV: &str = "TCODE_MEMORY_UNRECORDABLE_OUTCOME_CHILD";
    const CHILD_TEST_PATH: &str =
        "session::registry::memory_sink_ext::observer_tests::unrecordable_outcome_child";
    /// A prompt the child must never leak into a warning on stderr.
    const PROMPT_SENTINEL: &str = "sk-live-session-prompt-do-not-leak";

    /// The observer must not keep the registry it reports to alive — a strong
    /// back-reference would be a cycle (registry -> sink -> observer).
    #[test]
    fn weak_observer_does_not_keep_registry_alive() {
        let registry = Arc::new(SessionRegistry::new());
        let weak = Arc::downgrade(&registry);
        let observer = RegistryMemoryDurabilityObserver {
            registry: weak.clone(),
            session_id: "gone".into(),
        };
        drop(registry);
        assert!(weak.upgrade().is_none());
        observer.observe(MemoryTurnOutcome::Degraded {
            sequence: 1,
            category: MemoryFailureCategory::DrainClosed,
            at: Utc::now(),
        });
    }

    /// Drive both of `record_memory_durability`'s error arms through the
    /// observer, with the warnings this test's parent reads off stderr.
    ///
    /// Runs only under the parent below — a bare `cargo test` sees it return
    /// immediately.
    #[test]
    fn unrecordable_outcome_child() {
        if std::env::var_os(UNRECORDABLE_CHILD_ENV).is_none() {
            return;
        }
        crate::logging::init_tracing_for_test();

        let registry = Arc::new(SessionRegistry::new());
        let session = registry.create(
            PROMPT_SENTINEL.to_string(),
            None,
            crate::binding::ProjectBinding::None,
        );

        // Arm 1: the session is gone. Nothing can retain the signal, so the
        // warning on stderr is the only surviving record of it.
        let orphaned = RegistryMemoryDurabilityObserver {
            registry: Arc::downgrade(&registry),
            session_id: "no-such-session".into(),
        };
        orphaned.observe(MemoryTurnOutcome::Degraded {
            sequence: 1,
            category: MemoryFailureCategory::QueueFull,
            at: Utc::now(),
        });

        // Arm 2: sequence 1 never arrives, so every later outcome parks as its
        // own non-adjacent pending run until the bound refuses one.
        let observer = RegistryMemoryDurabilityObserver {
            registry: Arc::downgrade(&registry),
            session_id: session.id.clone(),
        };
        for sequence in (2..).step_by(2).take(reorder_bound() + 1) {
            observer.observe(MemoryTurnOutcome::Degraded {
                sequence,
                category: MemoryFailureCategory::QueueFull,
                at: Utc::now(),
            });
        }
    }

    /// #2425 regression: neither error arm of `record_memory_durability` may be
    /// swallowed. `observe` cannot propagate (it runs on the turn path), so the
    /// proof has to be read off the child's stderr.
    #[test]
    fn unrecordable_outcomes_are_reported_in_subprocess() {
        let output = Command::new(std::env::current_exe().expect("current test binary"))
            .args([
                "--exact",
                CHILD_TEST_PATH,
                "--nocapture",
                "--test-threads=1",
            ])
            .env(UNRECORDABLE_CHILD_ENV, "1")
            .env("RUST_LOG", "warn")
            .output()
            .expect("run unrecordable outcome child");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "child failed: {stderr}");
        assert_eq!(
            stderr.matches("memory_durability_unrecorded").count(),
            2,
            "both a session_not_found and a bound-exceeded refusal must be \
             reported, not swallowed: {stderr}"
        );
        assert!(
            !stderr.contains(PROMPT_SENTINEL),
            "the warning leaked session content: {stderr}"
        );
    }

    /// How many non-adjacent pending runs the reconciler accepts before it
    /// refuses one — mirrors its own `QUEUE_CAPACITY + 2`.
    fn reorder_bound() -> usize {
        crate::session::memory_sink::QUEUE_CAPACITY + 2
    }
}
