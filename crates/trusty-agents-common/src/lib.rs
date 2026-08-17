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
//! Test: Compile-tested transitively via `crates/trusty-agents` (host);
//!       `ToolResult`'s predicates are covered by
//!       `tool_result_is_error_distinguishes_variants`.

// docs.rs builds a release's documentation once, from the uploaded tarball,
// so a broken intra-doc link is baked into that version forever and only a new
// release can correct it. Deny keeps this crate at zero rather than letting the
// ratchet in `scripts/check_rustdoc_links.sh` absorb a new one.
#![deny(rustdoc::broken_intra_doc_links)]

pub mod perf;

pub mod runner;

pub mod adapters;

pub mod session_registry;

pub mod events;

pub mod harness_doc;

pub mod agent_assets;

pub mod compress;

pub mod agents;

pub mod skills;

pub mod connectors;

pub mod workstreams;

pub mod transport;

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
    /// Test: `tool_result_is_error_distinguishes_variants`.
    pub fn is_error(&self) -> bool {
        matches!(self, ToolResult::Error { .. })
    }

    /// Whether this error is fatal (not recoverable). `false` for Success.
    ///
    /// Why: Callers that distinguish fatal-vs-recoverable need this to decide
    ///      whether to retry or bail.
    /// What: True only for `Error { recoverable: false, .. }`.
    /// Test: `tool_result_is_fatal_only_for_non_recoverable`.
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
    /// Test: exercised by `trusty-agents`'s `tools/mod::filter_tools_for_user_*`.
    fn restricted_tiers(&self) -> &[ServiceTier] {
        &[]
    }

    /// Whether this tool is `AlwaysOn` or `OnDemand`.
    ///
    /// Why: Always-on tools run automatically before each LLM call; on-demand
    ///      tools appear in the LLM's tool list. See `ToolExecutionTier`.
    /// What: Default returns `OnDemand`.
    /// Test: exercised by `trusty-agents`'s `tools/always_on::build_live_context_*`.
    fn execution_tier(&self) -> ToolExecutionTier {
        ToolExecutionTier::OnDemand
    }

    /// The OpenRPC scope this tool was discovered under (trusty-agents #453,
    /// #3208), e.g. `"google.gmail.read"`.
    ///
    /// Why: Only tools sourced from the OpenRPC tool registry (endpoints like
    ///      `gworkspace`, `trusty-memory`) carry a meaningful scope —
    ///      in-process tools (git, delegate_to_agent, shell, ...) are gated
    ///      entirely by the existing name/glob allowlist + RBAC-tier checks
    ///      and have no scope concept. Defaulting to `None` means "not part
    ///      of the scoped surface, not subject to scope-pattern gating"
    ///      rather than "unscoped == open" — callers that DO consult scopes
    ///      only apply the check when this returns `Some`.
    /// What: Default returns `None`. `RegistryToolExecutor` overrides this
    ///       with its `DiscoveredTool`'s `scope` field.
    /// Test: `trusty-agents/tools/registry/adapter` round-trips the override;
    ///       `trusty-agents/ctrl/pm_task/dispatch/persona::filter_persona_tool_names_*`
    ///       covers the enforcement consumer.
    fn scope(&self) -> Option<&str> {
        None
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
    /// Test: Indirectly via `agent_plugin_lookup_returns_matching_plugin`
    ///       (`trusty-agents`), which constructs plugins through this ctor.
    pub fn new(persona_name: impl Into<String>, tools: Vec<Arc<dyn ToolExecutor>>) -> Self {
        Self {
            persona_name: persona_name.into(),
            tools,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies `is_error` splits `Success` from both `Error` flavours.
    ///
    /// Why: Dispatch paths branch on this predicate to decide whether to feed
    ///      the LLM an `is_error: true` tool result. This assertion used to
    ///      live in `crates/cto-assistant`, deleted in #3732 — it belongs
    ///      beside the type it covers, not in a downstream agent crate.
    /// What: Asserts `ok` is not an error while `err`/`fatal` both are.
    /// Test: self.
    #[test]
    fn tool_result_is_error_distinguishes_variants() {
        assert!(!ToolResult::ok("done").is_error());
        assert!(ToolResult::err("retry me").is_error());
        assert!(ToolResult::fatal("bad creds").is_error());
    }

    /// Verifies `is_fatal` is true only for the non-recoverable error.
    ///
    /// Why: Callers stop the loop on fatal and keep going on recoverable;
    ///      conflating the two either hangs on unrecoverable failures or
    ///      aborts on transient ones.
    /// What: Asserts `fatal` is fatal while `err` and `ok` are not.
    /// Test: self.
    #[test]
    fn tool_result_is_fatal_only_for_non_recoverable() {
        assert!(ToolResult::fatal("bad creds").is_fatal());
        assert!(!ToolResult::err("retry me").is_fatal());
        assert!(!ToolResult::ok("done").is_fatal());
    }
}
