//! Stable plugin API surface shared between `trusty-agents` and external agent crates.
//!
//! Why: The original design placed `ToolExecutor` / `AgentPlugin` / `ToolResult`
//!      inside the host crate's `lib.rs`. That created a hard cargo dependency
//!      cycle (trusty-agents → cto-assistant → trusty-agents), because external agent
//!      crates need the trait to implement it AND `trusty-agents` needs the agent
//!      crate to inject the plugin at startup. Cargo cannot resolve circular
//!      path dependencies even when they are logically one-directional at the
//!      binary level. Extracting the minimal trait surface into this tiny
//!      crate breaks the cycle: both `trusty-agents` and every agent crate depend
//!      on `trusty-agents-common`, but never on each other through the lib.
//! What: Re-defines the previously trusty-agents-internal types — `ToolExecutor`
//!       trait, `ToolResult` enum, `ToolExecutionTier` enum, `ServiceTier`
//!       enum (RBAC tiers), and `AgentPlugin` struct — as the public surface.
//!       Also hosts the harness-adapter framework (`adapters`) and the
//!       JSON-backed session ledger (`session_registry`), both moved here in
//!       Wave 1 of the trusty-agents-common build-out (issue #862, refs #830/#832).
//!       `trusty-agents` re-exports them via `trusty_agents::agent_api`,
//!       `trusty_agents::adapters`, and `trusty_agents::session_registry` for
//!       source-level compatibility with the existing call sites in
//!       `crates/trusty-agents/src/**`.
//! Test: Compile-tested transitively via `crates/cto-assistant` (downstream
//!       agent) and `crates/trusty-agents` (host).

/// Portable perf value types: `TokenUsage`, `PhaseRecord`, `PerfTotals`, `PerfRecord`.
///
/// Why: Moved to trusty-agents-common in Wave 2 (issue #867, refs #830/#832) so
///      external crates and the runner seam can reference `TokenUsage` (used in
///      `AgentOutput`) without depending on the full `trusty-agents` binary crate.
///      `PerfCollector` (stateful, tokio-dependent) stays in `trusty-agents::perf`.
/// What: The four portable plain-data types for per-phase token counting,
///       cost tracking, and full run record serialisation.
/// Test: Unit tests in `perf::tests` plus compile-tested via `trusty-agents`.
pub mod perf;

/// AgentRunner DI seam: `HistoryMessage`, `RunContext`, `AgentOutput`, and
/// the `AgentRunner` async trait.
///
/// Why: Moved to trusty-agents-common in Wave 2 (issue #867, refs #830/#832)
///      so external crates that need to implement or test against `AgentRunner`
///      can depend on this lightweight crate without pulling in the full
///      `trusty-agents` binary crate.
/// What: The runner seam — once `TokenUsage` (perf) is here, the only
///       remaining dependencies are `std`, `anyhow`, `async-trait`, and
///       `serde`. `HistoryMessage` is the portable IPC wire form
///       (`{role, content}` + serde); its `into_typed()` conversion
///       (requiring `async-openai`) stays in `trusty-agents::session`.
/// Test: `test_run_with_history_forwards_ctx` in `runner::tests` (bug #122
///       regression guard); compile-tested via `trusty-agents`.
pub mod runner;

/// Harness adapter framework: `HarnessAdapter` trait, value types, pattern
/// helpers, `AdapterRegistry`, and 7 concrete adapters.
///
/// Why: Moved to trusty-agents-common in Wave 1 (issue #862) so external
///      crates that need to implement or enumerate adapters can do so without
///      depending on the full `trusty-agents` binary crate. Zero internal
///      `crate::` dependencies confirmed pre-move.
/// What: Re-exports everything that was under `trusty-agents::adapters`.
/// Test: All unit tests in the submodules; `cargo test -p trusty-agents-common`
///       exercises them in-place.
pub mod adapters;

/// JSON-backed session ledger (`SessionsRegistry` + `SessionEntry`).
///
/// Why: Moved to trusty-agents-common in Wave 1 (issue #862) alongside the
///      adapter framework. The registry is purely `std`/`anyhow`/`chrono`/`serde`
///      — no host-crate dependencies — making it a clean extraction.
/// What: `SessionsRegistry` provides `open`, `record_start`, `record_end`,
///       `list` over a flat `sessions.json` file.
/// Test: `record_start_appends_entry`, `record_end_updates_status`, etc. in
///       the `tests` module of `session_registry.rs`.
pub mod session_registry;

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Structured result of a tool execution.
///
/// Why: Hard-failing the LLM loop on every tool error is brittle — the model
///      often can recover (retry with different args, fall back to another
///      tool, or explain the failure in its final answer). Returning a
///      structured `Error { recoverable }` lets us surface the failure back
///      to the LLM as a `tool_result` with `is_error: true` while keeping the
///      loop running, unless `recoverable = false` in which case callers may
///      choose to stop.
/// What: `Success(String)` carries a successful textual result; `Error`
///       carries a message plus a `recoverable` flag.
/// Test: `ToolResult::err(...).is_error()` is true; `ok(...).content()`
///       returns the success string. Exercised across `trusty-agents/tools/**`.
#[derive(Debug)]
pub enum ToolResult {
    Success(String),
    Error { message: String, recoverable: bool },
}

impl ToolResult {
    /// Success with a textual payload.
    ///
    /// Why: Single canonical happy-path constructor used by every tool.
    /// What: Wraps `s` in `Success`.
    /// Test: Trivially exercised by every successful tool execute().
    pub fn ok(s: impl Into<String>) -> Self {
        ToolResult::Success(s.into())
    }

    /// Recoverable error: loop continues, LLM sees `is_error: true`.
    ///
    /// Why: Most tool failures are non-fatal — wrong arg, transient network,
    ///      empty result. We want the model to see the error and decide.
    /// What: Wraps `msg` with `recoverable = true`.
    /// Test: Exercised by tool error tests across the workspace.
    pub fn err(msg: impl Into<String>) -> Self {
        ToolResult::Error {
            message: msg.into(),
            recoverable: true,
        }
    }

    /// Fatal (non-recoverable) error: callers may choose to stop the loop.
    ///
    /// Why: Some failures (invariant violations, credential rejection) shouldn't
    ///      be retried by the LLM; callers should surface them and bail.
    /// What: Wraps `msg` with `recoverable = false`.
    /// Test: Used by `is_fatal` tests in trusty-agents.
    pub fn fatal(msg: impl Into<String>) -> Self {
        ToolResult::Error {
            message: msg.into(),
            recoverable: false,
        }
    }

    /// Whether this result is an error variant.
    ///
    /// Why: Dispatch paths need a cheap predicate to log/branch on failure.
    /// What: Returns `true` for any `Error`, `false` for `Success`.
    /// Test: `cto_assistant::execute_outcome_maps_to_tool_result` checks this.
    pub fn is_error(&self) -> bool {
        matches!(self, ToolResult::Error { .. })
    }

    /// Whether this error is fatal (not recoverable). `false` for Success.
    ///
    /// Why: Callers that distinguish fatal-vs-recoverable need this to decide
    ///      whether to retry or bail.
    /// What: True only for `Error { recoverable: false, .. }`.
    /// Test: `cto_assistant::execute_outcome_maps_to_tool_result`.
    pub fn is_fatal(&self) -> bool {
        matches!(
            self,
            ToolResult::Error {
                recoverable: false,
                ..
            }
        )
    }

    /// Access the inner textual content (success body or error message).
    ///
    /// Why: The LLM tool-result payload is always a string; this lets callers
    ///      treat success/error uniformly when serialising.
    /// What: Returns the success body or the error message.
    /// Test: Implicit in every test that asserts on `result.content()`.
    pub fn content(&self) -> &str {
        match self {
            Self::Success(s) => s,
            Self::Error { message, .. } => message,
        }
    }
}

/// Two-tier tool execution model (trusty-agents #447).
///
/// Why: The dispatch path treats always-on tools fundamentally differently
///      from on-demand tools — they run automatically, their output becomes
///      context rather than a `tool_result`, and they must not appear in the
///      LLM's tool list. Encoding the distinction as an enum on the trait
///      makes it impossible to accidentally schedule an `AlwaysOn` tool as
///      `OnDemand` or vice-versa.
/// What: `OnDemand` is the default (current behavior); `AlwaysOn` opts the
///       tool into the pre-LLM context-building pipeline.
/// Test: Default exercised by every existing tool; `AlwaysOn` exercised by
///       `trusty-agents`'s `tools/always_on::build_live_context_*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToolExecutionTier {
    #[default]
    OnDemand,
    AlwaysOn,
}

/// RBAC service tier (trusty-agents #445).
///
/// Why: Different transports (CLI, Slack, Telegram, HTTP) expose the same
///      tool registry to users with different trust levels. Tools opt into
///      RBAC by listing the tiers that must be denied access. Defined here
///      (not in `trusty-agents/rbac`) because the `ToolExecutor::restricted_tiers`
///      signature returns `&[ServiceTier]` — external agent crates would not
///      be able to implement the trait without seeing the enum.
/// What: `All` (full access — controller / authenticated operator),
///       `Analytics` (read + analytical queries, no mutations), `ReadOnly`
///       (passive observation only, the strictest tier).
///       Serializes as `snake_case` so TOML/JSON authors can write
///       `tier = "read_only"` rather than the variant name. `Default` is
///       `All` so callsites that forget to set a tier degrade open at the
///       controller (unauthenticated transports MUST set a stricter default).
/// Test: `trusty-agents/rbac` covers serde + ordering; `trusty-agents/tools/mod::dispatch_for_user_*`
///       covers integration with dispatch.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ServiceTier {
    /// Full access — the controller / authenticated operator.
    #[default]
    All,
    /// Analytics-only tier — read + analytical queries, no mutations.
    Analytics,
    /// Read-only tier — passive observation only. The strictest tier.
    ReadOnly,
}

/// A tool invocable by an LLM through function calling.
///
/// Why: Replaces hardcoded string-match dispatch with polymorphic execution.
///      Living in `trusty-agents-common` (not in `trusty-agents`) so external agent
///      crates can implement it without depending on the full host crate,
///      breaking the cargo dependency cycle.
/// What: Supplies OpenAI-compatible JSON schema via `schema()` and executes
///       parsed arguments in `execute()`. Returns a structured `ToolResult`
///       so failures can be surfaced back to the LLM without tearing down
///       the loop.
/// Test: See unit tests in `trusty-agents/tools/mod.rs` for `ToolRegistry`.
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    /// Tool name — must match `function.name` in the schema and the LLM's
    /// `tool_call.name`.
    fn name(&self) -> &str;

    /// Full OpenAI-compatible tool schema object (`{"type":"function", ...}`).
    fn schema(&self) -> Value;

    /// Execute the tool with already-parsed JSON arguments.
    ///
    /// Why: Returning `ToolResult` rather than `Result<String>` means
    ///      transient / user-visible failures (missing arg, HTTP 500, refused
    ///      command) flow back to the LLM as structured errors instead of
    ///      aborting the whole turn.
    /// What: Returns `ToolResult::Success` on success or `ToolResult::Error`
    ///       on failure.
    /// Test: Each concrete impl has tests; registry dispatches through this.
    async fn execute(&self, args: Value) -> ToolResult;

    /// Tiers that are NOT permitted to invoke this tool.
    ///
    /// Why: RBAC at the dispatch boundary; see `ServiceTier`.
    /// What: Default returns empty (no restriction). Concrete tools override.
    /// Test: `trusty-agents/tools/mod::filter_tools_for_user_*`.
    fn restricted_tiers(&self) -> &[ServiceTier] {
        &[]
    }

    /// Whether this tool is `AlwaysOn` or `OnDemand`.
    ///
    /// Why: Always-on tools run automatically before each LLM call; on-demand
    ///      tools appear in the LLM's tool list. See `ToolExecutionTier`.
    /// What: Default returns `OnDemand`.
    /// Test: `trusty-agents/tools/always_on::build_live_context_*`.
    fn execution_tier(&self) -> ToolExecutionTier {
        ToolExecutionTier::OnDemand
    }
}

/// Named bundle of `ToolExecutor`s for a specific persona.
///
/// Why: Replaces hard-coded persona-to-tool branches in `trusty-agents`'s
///      `ctrl/mod.rs` with a data-driven injection point. New agent crates
///      register by adding themselves to the plugin list constructed in
///      `trusty-agents`'s `main.rs`; ctrl never needs to learn their names.
///      Lives here (not in `trusty-agents`) so agent crates can construct one
///      without depending on the host.
/// What: Holds the persona name the plugin's tools apply to plus an
///       `Arc<dyn ToolExecutor>` per tool. Cloning is cheap (Arc reference
///       counts) so the plugin can be reused across sessions.
/// Test: `cargo test -p cto-assistant agent_plugin_targets_cto_assistant`.
#[derive(Clone)]
pub struct AgentPlugin {
    /// Persona name (e.g. `"cto-assistant"`) this plugin's tools belong to.
    pub persona_name: String,
    /// Tool executors to register when the named persona becomes active.
    pub tools: Vec<Arc<dyn ToolExecutor>>,
}

impl AgentPlugin {
    /// Construct a plugin for the named persona.
    ///
    /// Why: Single canonical constructor keeps callers from accidentally
    ///      leaving fields uninitialised when the struct grows.
    /// What: Stores the persona name (converting `impl Into<String>` so
    ///       call sites can pass `&str` literals) and the tool vector.
    /// Test: Indirectly via `cto_assistant::agent_plugin()`.
    pub fn new(persona_name: impl Into<String>, tools: Vec<Arc<dyn ToolExecutor>>) -> Self {
        Self {
            persona_name: persona_name.into(),
            tools,
        }
    }
}
