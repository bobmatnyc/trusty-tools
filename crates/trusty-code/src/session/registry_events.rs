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

use super::*;

use crate::agent_loop::ContextBudgetSnapshot;

/// `Event::IndexReadiness.state` when the semantic lane is queryable — an
/// empty search result IS evidence of absence.
const READINESS_STATE_READY: &str = "ready";
/// `Event::IndexReadiness.state` while a lane is still building — an empty
/// search result is NOT evidence of absence.
const READINESS_STATE_WARMING: &str = "warming";
/// `Event::IndexReadiness.state` when no index could be probed at all (no
/// daemon / no derivable id) — likewise not evidence of absence.
const READINESS_STATE_UNAVAILABLE: &str = "unavailable";

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
    /// Test: `registry_tests::record_index_readiness_warming_publishes_event`,
    /// `registry_tests::record_index_readiness_ready_publishes_event`,
    /// `registry_tests::record_index_readiness_unavailable_publishes_event`.
    pub fn record_index_readiness(
        &self,
        id: &str,
        readiness: Option<&trusty_common::search_readiness::IndexReadiness>,
        summary: &str,
    ) -> Result<(), RpcError> {
        self.ensure_exists(id)?;
        let event = match readiness {
            Some(r) => Event::IndexReadiness {
                session_id: id.to_string(),
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
            None => Event::IndexReadiness {
                session_id: id.to_string(),
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
        self.record(id, event);
        Ok(())
    }

    /// Record one turn's working-context budget measurement (epic #2343).
    ///
    /// Why: the Infinite Sessions guarantee (working context >= 60%, session
    /// overhead <= 40%) is enforced every single turn and was visible to
    /// nothing — `CadenceOutcome` was computed and dropped. This is the
    /// emission plumbing that lets `task::SessionToolEventSink` forward the
    /// measurement to a `session.attach`ed UI's budget indicator.
    /// What: maps a [`ContextBudgetSnapshot`] field-for-field onto
    /// `Event::ContextBudget`; every value is already computed by the caller,
    /// so this adds no arithmetic of its own. Errors with `session_not_found`
    /// if `id` is unknown.
    /// Test: `registry_tests::record_context_budget_publishes_event`.
    pub fn record_context_budget(
        &self,
        id: &str,
        snapshot: &ContextBudgetSnapshot,
    ) -> Result<(), RpcError> {
        self.ensure_exists(id)?;
        self.record(
            id,
            Event::ContextBudget {
                session_id: id.to_string(),
                context_window_tokens: snapshot.context_window,
                overhead_tokens: snapshot.overhead_tokens,
                overhead_cap_tokens: snapshot.overhead_cap_tokens,
                working_context_pct: snapshot.working_context_pct,
                overhead_pct: snapshot.overhead_pct,
                within_budget: snapshot.within_budget,
                compaction_fired: snapshot.fired,
                compaction_rounds: snapshot.rounds,
            },
        );
        Ok(())
    }

    /// Record a tool invocation starting (#2055 emission plumbing for
    /// #2056's agent loop).
    ///
    /// Why: gives the future agent loop a stable, already-wired call for
    /// `ToolStarted` without it needing to touch the ring buffer, sequencing,
    /// or bus directly.
    /// What: errors with `session_not_found` if `id` is unknown; `args_preview`
    /// is truncated via `crate::events::preview` before emission so a large
    /// argument payload can't blow the ring buffer / wire size.
    /// Test: `registry_tests::record_tool_started_publishes_event`.
    pub fn record_tool_started(
        &self,
        id: &str,
        tool: &str,
        call_id: &str,
        args: &str,
    ) -> Result<(), RpcError> {
        self.ensure_exists(id)?;
        self.record(
            id,
            Event::ToolStarted {
                session_id: id.to_string(),
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
    pub fn record_tool_finished(
        &self,
        id: &str,
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
        tool: &str,
        call_id: &str,
        error: &str,
    ) -> Result<(), RpcError> {
        self.ensure_exists(id)?;
        self.record(
            id,
            Event::ToolError {
                session_id: id.to_string(),
                tool: tool.to_string(),
                call_id: call_id.to_string(),
                error: error.to_string(),
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
