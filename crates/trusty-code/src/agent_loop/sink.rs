//! Optional tool-event observation hook for `AgentLoop` (#2056).
//!
//! Why: #2055 built the session event taxonomy and `SessionRegistry::record_tool_*`
//! emission plumbing, but `AgentLoop` had no seam to call it from — its tool
//! dispatch (`dispatch_all`) was a closed loop over `ToolRegistry::dispatch_gated`
//! with no observer. Rather than fork the loop for daemon-driven runs, this adds
//! ONE optional hook: a handler invoked around every tool dispatch. `run_task`'s
//! existing CLI path (which never sets a sink) is completely unaffected — the
//! hook defaults to `None` and costs nothing when absent.
//! What: [`ToolEventSink`] is the trait; `crate::task::SessionToolEventSink`
//! (#2056) is the concrete implementation that forwards to
//! `session::registry::SessionRegistry::record_tool_*`. Kept in `agent_loop`
//! (not `session`) so this crate's lower-level engine module has no dependency
//! on the higher-level session/daemon layer — `session` depends on
//! `agent_loop`, never the reverse.
//! Test: exercised via `agent_loop::tests` (a recording sink asserts hook call
//! order) and `crate::task` tests (the real `SessionToolEventSink`).

use async_trait::async_trait;

/// Observes tool dispatch lifecycle events from an `AgentLoop` run.
///
/// Why: The only way to make an agent loop's tool activity observable to an
/// external subscriber (a `session.attach`ed client) without forking the loop
/// or the tool registry.
/// What: Three hooks, one per #2055 taxonomy kind; `call_id` is the
/// `ToolCall::id` the model assigned, correlating start/finish/error for the
/// same invocation. All are `&self` (not `&mut self`) so a sink can be shared
/// as `Arc<dyn ToolEventSink>` across a PM loop and every delegated sub-agent
/// loop.
/// Test: `crate::task::sink::tests::*`.
#[async_trait]
pub trait ToolEventSink: Send + Sync {
    /// A tool invocation is about to run.
    async fn tool_started(&self, call_id: &str, tool: &str, args_preview: &str);

    /// A tool invocation completed (successfully or with a recoverable error —
    /// `success` distinguishes the two). Use [`Self::tool_error`] instead for an
    /// exceptional (non-recoverable) failure.
    async fn tool_finished(&self, call_id: &str, tool: &str, success: bool, result_preview: &str);

    /// A tool invocation raised an exceptional (non-recoverable) error.
    async fn tool_error(&self, call_id: &str, tool: &str, error: &str);
}
