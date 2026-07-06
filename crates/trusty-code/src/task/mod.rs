//! Daemon-owned task execution: `task.run` + background agent-loop
//! orchestration (#2056, M1 control-plane cut line).
//!
//! Why: this module is where a `task.run` (or a future `session.send`
//! auto-trigger) call actually turns into a running PM -> engineer agent
//! loop, streaming live `tool_started`/`tool_finished`/`tool_error` and
//! session-lifecycle events (#2055) to any `session.attach`ed client (#2054),
//! without blocking the triggering RPC. It is glue, not a fork: the engine
//! itself (`crate::agent_loop`, `crate::runner`, `crate::tools`, `crate::llm`)
//! is reused verbatim, extended (in #2056) with an optional tool-event sink
//! and cancellation flag rather than duplicated.
//! What: [`protocol::register`] wires the `task.run` JSON-RPC method;
//! [`executor::spawn_task_run`] is the orchestration entry point it calls;
//! [`sink::SessionToolEventSink`] is the concrete `agent_loop::ToolEventSink`
//! forwarding to `session::SessionRegistry::record_tool_*`; [`mock_llm`]
//! provides the offline-testable "echo" `LlmClientTrait` selected via
//! `TCODE_MOCK_LLM=echo` so the whole flow is black-box testable without a
//! live model.
//! Test: `task::executor::tests::*`, `task::protocol::tests::*`,
//! `task::sink::tests::*`, `task::mock_llm::tests::*`; the full flow
//! end-to-end (a real subprocess) in `tests/task_e2e.rs`.

pub mod executor;
pub mod mock_llm;
pub mod protocol;
pub mod sink;

pub use executor::{TaskRunParams, spawn_task_run};
pub use sink::SessionToolEventSink;
