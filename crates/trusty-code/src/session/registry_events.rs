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

impl SessionRegistry {
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
