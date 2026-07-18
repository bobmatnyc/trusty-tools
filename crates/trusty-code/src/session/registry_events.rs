//! `SessionRegistry`'s #2055 tool/log/progress/message event-recording
//! plumbing. Split out of `registry.rs` (#2344) purely to keep that
//! production file under the crate's 500-SLOC cap as the #2344
//! persistent-transcript methods grew it past the limit — this is a child
//! module of `registry` (declared via `#[path = ...] mod events;`), so it
//! shares full access to `SessionRegistry`'s private `lock`/`record` helpers
//! exactly as if these methods were still defined in that file.
//!
//! Why: gives the agent loop a stable, already-wired call for each #2055
//! event kind without it needing to touch the ring buffer, sequencing, or
//! bus directly.
//! What: `record_tool_started`/`record_tool_finished`/`record_tool_error`/
//! `record_log`/`record_progress`/`record_message`, plus the shared
//! `ensure_exists` existence guard they all call before recording.
//! Test: `registry_tests::record_tool_started_publishes_event`,
//! `registry_tests::record_tool_finished_publishes_event`,
//! `registry_tests::record_tool_error_publishes_event`,
//! `registry_tests::record_log_publishes_event`,
//! `registry_tests::record_progress_publishes_event`,
//! `registry_tests::record_message_publishes_event`.

use serde::Serialize;

use super::*;
use crate::tools::telemetry::{RecallTelemetry, SearchTelemetry};

use crate::events::{ContextBudgetSnapshot, IndexReadinessSnapshot};

/// `Event::IndexReadiness.state` when the semantic lane is queryable — an
/// empty search result IS evidence of absence.
const READINESS_STATE_READY: &str = "ready";
/// `Event::IndexReadiness.state` while a lane is still building — an empty
/// search result is NOT evidence of absence.
const READINESS_STATE_WARMING: &str = "warming";
/// `Event::IndexReadiness.state` when no index could be probed at all (no
/// daemon / no derivable id) — likewise not evidence of absence.
const READINESS_STATE_UNAVAILABLE: &str = "unavailable";

/// `session.get_readiness`'s response payload (DOC-39 §5.6 Slice D).
///
/// Why: a session's readiness is a single cached value, not a collection —
/// `session.get_goals`'s "empty array means nothing yet" convention doesn't
/// fit here, and a bare `Option` serialising to `null` would be exactly the
/// kind of untyped "nothing yet" signal the design explicitly rules out (a
/// consumer can't tell "never probed" from "probed, and every field happens
/// to be absent" without a discriminant). This is a thin, tagged wrapper
/// around [`IndexReadinessSnapshot`] — the SAME struct
/// `SessionRegistry::record_index_readiness` caches and `Event::IndexReadiness`
/// derives from — so the query and event surfaces can never independently
/// drift on field names.
/// What: `#[serde(tag = "status")]` (internally tagged, matching `Event`'s own
/// `tag = "type"` convention) so the wire shape is
/// `{"status":"probed","state":...,"index_id":...,...}` for a session with a
/// recorded snapshot, or `{"status":"never_probed"}` for one with none.
/// Test: `registry_tests::record_index_readiness_caches_snapshot_for_late_query`,
/// `protocol_readiness::tests::get_readiness_never_probed_session_returns_never_probed`.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ReadinessQuery {
    /// A probe has landed for this session; wraps the last-recorded snapshot.
    Probed(IndexReadinessSnapshot),
    /// No `record_index_readiness` call has ever landed for this session.
    NeverProbed,
}

/// `session.get_context_budget`'s response payload (issue #3015).
///
/// Why: a session's working-context budget is a single cached value, not a
/// collection — the same "typed nothing-yet, never a bare `null`" rationale
/// as [`ReadinessQuery`] applies verbatim: a consumer must be able to tell
/// "no turn has completed yet" from "a turn completed and every field
/// happens to be zero" without a discriminant.
/// What: `#[serde(tag = "status", rename_all = "snake_case")]` so the wire
/// shape is `{"status":"recorded", ...snapshot fields}` for a session with a
/// cached measurement, or `{"status":"never_recorded"}` for one with none.
/// Test: `registry_tests::record_context_budget_caches_snapshot_for_late_query`,
/// `protocol_budget::tests::get_context_budget_never_recorded_session_returns_never_recorded`.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ContextBudgetQuery {
    /// A turn has completed for this session; wraps the last-recorded
    /// snapshot.
    Recorded(ContextBudgetSnapshot),
    /// No `record_context_budget` call has ever landed for this session.
    NeverRecorded,
}

impl SessionRegistry {
    /// Record a project's trusty-search index readiness (issue #2784's
    /// UI Phase-1 follow-up).
    ///
    /// Why: `trusty_common::search_readiness` already probes this per lane and
    /// then only logged it to stderr, where no API consumer could reach it.
    /// The UI is a thin client over this daemon — anything it renders must
    /// arrive as an event — so a warming index was structurally unrenderable.
    /// Critically, this is what lets a consumer tell a COLD index from a READY
    /// one with genuinely zero hits: both return an empty search, but they mean
    /// opposite things, and conflating them is exactly what makes a model
    /// conclude "nothing there" and hand-explore to the wrong target.
    /// What: takes the probe's `Option<IndexReadiness>` — `None` (fail-open: no
    /// daemon, undrivable id, HTTP error) maps to `state: "unavailable"` with
    /// every lane `false` and no index fields, which is itself the useful
    /// signal rather than something to swallow. `Some` maps to `"ready"` when
    /// `semantic_search_ready()` and `"warming"` otherwise, carrying the
    /// per-lane flags through verbatim. Errors with `session_not_found` if `id`
    /// is unknown.
    /// (DOC-39 §5.6 Slice D) Also caches the resulting [`IndexReadinessSnapshot`]
    /// on the session (last-writer-wins), so a client that attaches AFTER this
    /// one-time event fires can still retrieve it via
    /// [`Self::get_readiness`]/`session.get_readiness`.
    /// Test: `registry_tests::record_index_readiness_warming_publishes_event`,
    /// `registry_tests::record_index_readiness_ready_publishes_event`,
    /// `registry_tests::record_index_readiness_unavailable_publishes_event`,
    /// `registry_tests::record_index_readiness_caches_snapshot_for_late_query`.
    pub fn record_index_readiness(
        &self,
        id: &str,
        readiness: Option<&trusty_common::search_readiness::IndexReadiness>,
        summary: &str,
    ) -> Result<(), RpcError> {
        self.ensure_exists(id)?;
        let snapshot = match readiness {
            Some(r) => IndexReadinessSnapshot {
                state: if r.semantic_search_ready() {
                    READINESS_STATE_READY
                } else {
                    READINESS_STATE_WARMING
                }
                .to_string(),
                index_id: Some(r.index_id.clone()),
                lifecycle_status: Some(r.lifecycle_status.clone()),
                chunk_count: Some(r.chunk_count),
                lexical_ready: r.lexical_ready,
                semantic_ready: r.semantic_ready,
                graph_ready: r.graph_ready,
                summary: summary.to_string(),
            },
            None => IndexReadinessSnapshot {
                state: READINESS_STATE_UNAVAILABLE.to_string(),
                index_id: None,
                lifecycle_status: None,
                chunk_count: None,
                lexical_ready: false,
                semantic_ready: false,
                graph_ready: false,
                summary: summary.to_string(),
            },
        };
        {
            let mut sessions = self.lock();
            if let Some(entry) = sessions.get_mut(id) {
                entry.readiness = Some(snapshot.clone());
            }
        }
        self.record(
            id,
            Event::IndexReadiness {
                session_id: id.to_string(),
                state: snapshot.state,
                index_id: snapshot.index_id,
                lifecycle_status: snapshot.lifecycle_status,
                chunk_count: snapshot.chunk_count,
                lexical_ready: snapshot.lexical_ready,
                semantic_ready: snapshot.semantic_ready,
                graph_ready: snapshot.graph_ready,
                summary: snapshot.summary,
            },
        );
        Ok(())
    }

    /// `session.get_readiness`: return the last-recorded index-readiness
    /// probe for a session, for clients that attach after the one-time
    /// `Event::IndexReadiness` already fired (DOC-39 §5.6 Slice D).
    ///
    /// Why: [`Self::record_index_readiness`] emits its event exactly once, at
    /// task start (see `run_task::ensure_project_indexed_in_background`'s
    /// call site in `task::executor`). A UI that connects even a moment later
    /// has no query surface to ask "what is the readiness state right now" —
    /// only a stream it may have missed. This is that query surface.
    /// What: `Err(session_not_found)` if `id` is unknown. Otherwise
    /// [`ReadinessQuery::Probed`] wrapping the cached [`IndexReadinessSnapshot`]
    /// if a probe has ever landed for this session, or
    /// [`ReadinessQuery::NeverProbed`] — a distinct, clearly-typed state,
    /// never a bare `null` — if no `record_index_readiness` call has happened
    /// yet (e.g. a projectless session, or one queried before its background
    /// indexing probe completes).
    /// Test: `registry_tests::record_index_readiness_caches_snapshot_for_late_query`,
    /// `protocol_readiness::tests::get_readiness_never_probed_session_returns_never_probed`,
    /// `registry_tests::get_readiness_unknown_session_errors`.
    pub fn get_readiness(&self, id: &str) -> Result<ReadinessQuery, RpcError> {
        self.ensure_exists(id)?;
        Ok(self
            .lock()
            .get(id)
            .and_then(|e| e.readiness.clone())
            .map(ReadinessQuery::Probed)
            .unwrap_or(ReadinessQuery::NeverProbed))
    }

    /// Record one turn's working-context budget measurement (epic #2343;
    /// caches the snapshot for late queries as of issue #3015).
    ///
    /// Why: the Infinite Sessions guarantee (working context >= 60%, session
    /// overhead <= 40%) is enforced every single turn and was visible to
    /// nothing — `CadenceOutcome` was computed and dropped. This is the
    /// emission plumbing that lets `task::SessionToolEventSink` forward the
    /// measurement to a `session.attach`ed UI's budget indicator. (issue
    /// #3015) Also caches the resulting [`ContextBudgetSnapshot`] on the
    /// session (last-writer-wins), mirroring [`Self::record_index_readiness`],
    /// so a client that attaches or reconnects AFTER a turn's event already
    /// fired can still retrieve it via [`Self::get_context_budget`]/
    /// `session.get_context_budget` — the API PR #3014's GUI status bar
    /// polls instead of leaving "budget: unavailable" permanently.
    /// What: maps the caller's `agent_loop::ContextBudgetSnapshot` field-for-
    /// field onto a cached [`ContextBudgetSnapshot`] first, then derives
    /// `Event::ContextBudget` from that cached value so the two can never
    /// disagree; every value is already computed by the caller, so this adds
    /// no arithmetic of its own. Errors with `session_not_found` if `id` is
    /// unknown.
    /// Test: `registry_tests::record_context_budget_publishes_event`,
    /// `registry_tests::record_context_budget_caches_snapshot_for_late_query`.
    pub fn record_context_budget(
        &self,
        id: &str,
        measurement: &crate::agent_loop::ContextBudgetSnapshot,
    ) -> Result<(), RpcError> {
        self.ensure_exists(id)?;
        let snapshot = ContextBudgetSnapshot {
            context_window_tokens: measurement.context_window,
            overhead_tokens: measurement.overhead_tokens,
            overhead_cap_tokens: measurement.overhead_cap_tokens,
            working_context_pct: measurement.working_context_pct,
            overhead_pct: measurement.overhead_pct,
            within_budget: measurement.within_budget,
            compaction_fired: measurement.fired,
            compaction_rounds: measurement.rounds,
        };
        {
            let mut sessions = self.lock();
            if let Some(entry) = sessions.get_mut(id) {
                entry.context_budget = Some(snapshot);
            }
        }
        self.record(
            id,
            Event::ContextBudget {
                session_id: id.to_string(),
                context_window_tokens: snapshot.context_window_tokens,
                overhead_tokens: snapshot.overhead_tokens,
                overhead_cap_tokens: snapshot.overhead_cap_tokens,
                working_context_pct: snapshot.working_context_pct,
                overhead_pct: snapshot.overhead_pct,
                within_budget: snapshot.within_budget,
                compaction_fired: snapshot.compaction_fired,
                compaction_rounds: snapshot.compaction_rounds,
            },
        );
        Ok(())
    }

    /// `session.get_context_budget`: return the last-recorded working-context
    /// budget measurement for a session, for clients that attach after the
    /// per-turn `Event::ContextBudget` already fired (issue #3015).
    ///
    /// Why: [`Self::record_context_budget`] emits its event once per PM turn
    /// boundary — a client that attaches (or reconnects) between turns has no
    /// query surface to ask "what is the budget right now", only a stream it
    /// may have missed. This is that query surface, the exact counterpart to
    /// [`Self::get_readiness`].
    /// What: `Err(session_not_found)` if `id` is unknown. Otherwise
    /// [`ContextBudgetQuery::Recorded`] wrapping the cached
    /// [`ContextBudgetSnapshot`] if a turn has ever completed for this
    /// session, or [`ContextBudgetQuery::NeverRecorded`] — a distinct,
    /// clearly-typed state, never a bare `null` — if no
    /// `record_context_budget` call has happened yet (e.g. a session queried
    /// before its first PM turn completes, or a delegated sub-agent session,
    /// which never emits cadence budget events at all).
    /// Test: `registry_tests::record_context_budget_caches_snapshot_for_late_query`,
    /// `protocol_budget::tests::get_context_budget_never_recorded_session_returns_never_recorded`,
    /// `registry_tests::get_context_budget_unknown_session_errors`.
    pub fn get_context_budget(&self, id: &str) -> Result<ContextBudgetQuery, RpcError> {
        self.ensure_exists(id)?;
        Ok(self
            .lock()
            .get(id)
            .and_then(|e| e.context_budget)
            .map(ContextBudgetQuery::Recorded)
            .unwrap_or(ContextBudgetQuery::NeverRecorded))
    }

    /// Record a tool invocation starting (#2055 emission plumbing for
    /// #2056's agent loop).
    ///
    /// Why: gives the future agent loop a stable, already-wired call for
    /// `ToolStarted` without it needing to touch the ring buffer, sequencing,
    /// or bus directly.
    /// What: errors with `session_not_found` if `id` is unknown; `args_preview`
    /// is truncated via `crate::events::preview` before emission so a large
    /// argument payload can't blow the ring buffer / wire size. `agent`
    /// (UI Phase 1) is the dispatching agent's name — see
    /// [`crate::events::Event::ToolStarted`]. `agent_id` (DOC-39 AC-13) is
    /// that same agent's stable per-spawn id.
    /// Test: `registry_tests::record_tool_started_publishes_event`.
    pub fn record_tool_started(
        &self,
        id: &str,
        agent: &str,
        agent_id: &str,
        tool: &str,
        call_id: &str,
        args: &str,
    ) -> Result<(), RpcError> {
        self.ensure_exists(id)?;
        self.record(
            id,
            Event::ToolStarted {
                session_id: id.to_string(),
                agent: agent.to_string(),
                agent_id: agent_id.to_string(),
                tool: tool.to_string(),
                call_id: call_id.to_string(),
                args_preview: crate::events::preview(args, 500),
            },
        );
        Ok(())
    }

    /// Record a tool invocation finishing (#2055 emission plumbing for
    /// #2056's agent loop). See [`Self::record_tool_started`].
    /// Test: `registry_tests::record_tool_finished_publishes_event`.
    /// (DOC-39 AC-13) `agent_id` pushed this past clippy's
    /// `too_many_arguments` gate; every parameter is a plain, independent
    /// piece of the event and none pair naturally into a sub-struct, so the
    /// attribute is the pragmatic choice here (mirrors
    /// `serve::transport::run_loop`'s same allowance).
    #[allow(clippy::too_many_arguments)]
    pub fn record_tool_finished(
        &self,
        id: &str,
        agent: &str,
        agent_id: &str,
        tool: &str,
        call_id: &str,
        success: bool,
        result: &str,
    ) -> Result<(), RpcError> {
        self.ensure_exists(id)?;
        self.record(
            id,
            Event::ToolFinished {
                session_id: id.to_string(),
                agent: agent.to_string(),
                agent_id: agent_id.to_string(),
                tool: tool.to_string(),
                call_id: call_id.to_string(),
                success,
                result_preview: crate::events::preview(result, 500),
            },
        );
        Ok(())
    }

    /// Record a tool invocation raising an exceptional error (#2055 emission
    /// plumbing for #2056's agent loop). See [`Self::record_tool_started`].
    /// Test: `registry_tests::record_tool_error_publishes_event`.
    pub fn record_tool_error(
        &self,
        id: &str,
        agent: &str,
        agent_id: &str,
        tool: &str,
        call_id: &str,
        error: &str,
    ) -> Result<(), RpcError> {
        self.ensure_exists(id)?;
        self.record(
            id,
            Event::ToolError {
                session_id: id.to_string(),
                agent: agent.to_string(),
                agent_id: agent_id.to_string(),
                tool: tool.to_string(),
                call_id: call_id.to_string(),
                error: error.to_string(),
            },
        );
        Ok(())
    }

    /// Record a `search_code` call's real lane + hit count + per-hit
    /// path/score + latency (UI Phase 1; `hits` added DOC-39 Slice B). See
    /// [`crate::events::Event::SearchPerformed`].
    ///
    /// Why: emitted ALONGSIDE the generic tool events so the UI can trace a
    /// change back to the searches that drove it without re-parsing prose.
    /// What: same existence guard + ring-buffer/sequencing path as every
    /// other `record_*`, so a structured event replays on `session.attach`
    /// exactly like the generic ones.
    /// Test: `registry_tests::record_search_performed_publishes_event`.
    pub fn record_search_performed(
        &self,
        id: &str,
        agent: &str,
        agent_id: &str,
        telemetry: &SearchTelemetry,
    ) -> Result<(), RpcError> {
        self.ensure_exists(id)?;
        self.record(
            id,
            Event::SearchPerformed {
                session_id: id.to_string(),
                agent: agent.to_string(),
                agent_id: agent_id.to_string(),
                lane: telemetry.lane.clone(),
                query: telemetry.query.clone(),
                hit_count: telemetry.hit_count,
                hits: telemetry.hits.clone(),
                latency_ms: telemetry.latency_ms,
            },
        );
        Ok(())
    }

    /// Record a `recall_session` call's results and which ones entered
    /// context (UI Phase 1). See [`crate::events::Event::MemoryRecalled`].
    ///
    /// Why: the `injected` flag is the UI's differentiating surface —
    /// injected memories vs ones recalled and held back.
    /// What: as [`Self::record_search_performed`].
    /// Test: `registry_tests::record_memory_recalled_publishes_event`.
    pub fn record_memory_recalled(
        &self,
        id: &str,
        agent: &str,
        agent_id: &str,
        telemetry: &RecallTelemetry,
    ) -> Result<(), RpcError> {
        self.ensure_exists(id)?;
        self.record(
            id,
            Event::MemoryRecalled {
                session_id: id.to_string(),
                agent: agent.to_string(),
                agent_id: agent_id.to_string(),
                query: telemetry.query.clone(),
                results: telemetry.results.clone(),
            },
        );
        Ok(())
    }

    /// Record a session-scoped diagnostic log line (#2055 emission plumbing).
    /// Test: `registry_tests::record_log_publishes_event`.
    pub fn record_log(&self, id: &str, level: &str, message: &str) -> Result<(), RpcError> {
        self.ensure_exists(id)?;
        self.record(
            id,
            Event::Log {
                session_id: id.to_string(),
                level: level.to_string(),
                message: message.to_string(),
            },
        );
        Ok(())
    }

    /// Record a coarse progress update (#2055 emission plumbing).
    /// Test: `registry_tests::record_progress_publishes_event`.
    pub fn record_progress(
        &self,
        id: &str,
        message: &str,
        percent: Option<f32>,
    ) -> Result<(), RpcError> {
        self.ensure_exists(id)?;
        self.record(
            id,
            Event::Progress {
                session_id: id.to_string(),
                message: message.to_string(),
                percent,
            },
        );
        Ok(())
    }

    /// Record a generic freeform session message (#2055 emission plumbing).
    /// Test: `registry_tests::record_message_publishes_event`.
    pub fn record_message(&self, id: &str, text: &str) -> Result<(), RpcError> {
        self.ensure_exists(id)?;
        self.record(
            id,
            Event::Message {
                session_id: id.to_string(),
                text: text.to_string(),
            },
        );
        Ok(())
    }

    /// Return `Ok(())` if `id` exists, `Err(session_not_found)` otherwise.
    ///
    /// Why: every `record_*` plumbing method needs the same existence guard
    /// before recording; centralising it keeps each one a two-line body.
    fn ensure_exists(&self, id: &str) -> Result<(), RpcError> {
        if self.lock().contains_key(id) {
            Ok(())
        } else {
            Err(RpcError::session_not_found(id))
        }
    }
}
