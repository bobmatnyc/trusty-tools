//! trusty-code — per-project Claude-Code-compatible MPM orchestration harness.
//!
//! # Why
//!
//! open-mpm is the general-purpose MPM orchestration platform, but each project
//! needs a harness that is *already* wired to its own `.claude` configuration:
//! agents, skills, MCP connections, CLAUDE.md, and permissions. `trusty-code`
//! fills that role. It is the Claude-Code-native orchestration entry point —
//! driven by API, CLI, or TUI — that runs the PM main-loop, enforces the
//! mandatory workflow, and delegates authority to typed sub-agents according to
//! MPM protocols. Extraction from open-mpm is tracked in epic #587.
//!
//! # Design constraints
//!
//! * **Claude-Code compatible** — reads `.claude/` config, agents, skills, MCP
//!   descriptors, `CLAUDE.md`, and permission grants exactly as Claude Code does.
//! * **API / CLI / TUI driven** — no hooks support (hooks are a Claude Code
//!   shell-level feature; `tcode` operates above that layer via its event bus).
//! * **Per-agent model routing** — each agent may specify its own model,
//!   independently choosing between AWS Bedrock models and OpenRouter models.
//! * **Single-instance per project** — one `tcode serve` process per `.claude/`
//!   root; multiple CLI or TUI clients may attach to it.
//! * **No `unwrap()` in library code** — all fallible paths use `?` with
//!   `thiserror`-derived error types (once errors exist to derive); application
//!   entry points use `anyhow::Result`.
//!
//! # What
//!
//! Phase 1 public surface (leaf/protocol modules extracted from open-mpm per
//! #640):
//!
//! * [`events`] — process-global broadcast event bus.
//! * [`ipc`] — NDJSON IPC protocol for PM ↔ sub-agent communication.
//! * [`perf`] — per-phase latency + token/cost instrumentation.
//! * [`intent`] — pure-Rust heuristic intent classifier.
//! * [`progress`] — real-time phase progress reporter.
//! * [`build_info`] — monotonic build counter + version banner.
//!
//! Phase 2 public surface (tools layer, per #641):
//!
//! * [`tools`] — `ToolExecutor` / `AgentRunner` / `SearchProvider` traits,
//!   `ToolRegistry` dispatcher, `DelegateToAgentTool`, `ToolResult`.
//! * [`rbac`] — `ServiceTier`, `UserIdentity`, access-control helpers.
//!
//! Phase 3 public surface (agents + LLM layer, per #642):
//!
//! * [`agents`] — `AgentConfig` TOML schema, `discover_agents`, `load_all_agents`.
//! * [`identity`] — `CallerIdentity`, `RecallCeiling` for memory scoping.
//! * [`logging`] — tracing init helpers (`init_tracing`, `init_tracing_for_test`).
//!
//! # Test
//!
//! `cargo test -p trusty-code` — all modules carry their own unit tests.

// ── Phase 1 leaf/protocol modules (extracted from open-mpm per #640) ──

/// Process-wide broadcast event bus for real-time UI streaming.
///
/// Why: Centralises telemetry emission so any code path can publish events
/// without threading a sender through dozens of call sites.
/// What: `Event` enum, `publish`/`subscribe`/`emit` helpers, the
/// `EVENT_LINE_PREFIX` constant for subprocess relay.
/// Test: `events::tests::publish_round_trips_through_subscribe`.
pub mod events;

/// NDJSON IPC protocol for PM ↔ sub-agent communication.
///
/// Why: Provides a framing-safe wire protocol over stdin/stdout pipes so the
/// PM and sub-agent subprocesses exchange structured messages without ambiguity.
/// What: `IpcMessage` enum (Task/Result/Error), `HistoryMessage` wire type,
/// `serialize_message`/`parse_message` helpers.
/// Test: `ipc::tests::*` round-trips every variant.
pub mod ipc;

/// Per-phase latency + token/cost instrumentation.
///
/// Why: Tracks how long each workflow phase takes, how many tokens it consumed,
/// and the resulting USD cost so runs can be compared build-over-build.
/// What: `TokenUsage`, `PhaseRecord`, `PerfRecord`, `PerfTotals`,
/// `PerfCollector`, `cost_usd`.
/// Test: `perf::tests::*`.
pub mod perf;

/// Pure-Rust heuristic intent classifier for PM fast-pathing.
///
/// Why: Avoids routing trivial conversational inputs through the full
/// subprocess pipeline.
/// What: `IntentClass` enum, `classify_intent` function.
/// Test: `intent::classifier_tests::*`.
pub mod intent;

/// Real-time phase progress reporter to stderr.
///
/// Why: Workflow runs take 20–70 minutes; users need live feedback without
/// polluting stdout.
/// What: `ProgressReporter` struct with phase/wave lifecycle hooks and
/// `format_duration` helper.
/// Test: `progress::tests::*`.
pub mod progress;

/// Build and version tracking.
///
/// Why: A monotonic build counter pairs with semver for log correlation.
/// What: `BuildInfo` struct, `VERSION`/`GIT_HASH`/`PKG_NAME` constants,
/// `version_string` helper.
/// Test: `build_info::tests::*`.
pub mod build_info;

// ── Phase 2 tools layer (per #641) ──

/// Tool system: traits, registry, and the delegate tool.
///
/// Why: The PM loop needs a polymorphic tool dispatch layer so new capabilities
/// plug in without touching orchestration code.
/// What: `ToolExecutor`, `AgentRunner`, `RunContext`, `AgentOutput`,
/// `SearchProvider`, `SkillResolver`, `ToolResult`, `ToolRegistry`,
/// `DelegateToAgentTool`.
/// Test: `tools::traits::tests::*`, `tools::registry::tests::*`,
/// `tools::delegate::tests::*`.
pub mod tools;

/// Role-based access control for tool execution.
///
/// Why: tcode exposes tools over multiple surfaces; RBAC gates execution on a
/// stable tier ladder without per-deployment code branches.
/// What: `ServiceTier`, `UserIdentity`, `filter_tools_for_user`,
/// `can_access_tier`.
/// Test: `rbac::tests::*`.
pub mod rbac;

// ── Phase 3 agents + LLM layer (per #642) ──

/// Native OpenRouter LLM client.
///
/// Why: trusty-code agents need to invoke LLMs via the OpenRouter API without
/// depending on third-party Rust SDK crates that pin us to specific provider
/// contracts. A thin native client gives full control over the wire format,
/// headers, and error handling.
/// What: Exports `LlmClient`, `LlmClientConfig`, all request/response types
/// (`ChatRequest`, `ChatResponse`, `ChatMessage`, `ToolDefinition`, …), and
/// `LlmError`. The API key is injected at construction time.
/// Test: `cargo test -p trusty-code` covers serialisation, deserialisation,
/// and error-mapping unit tests. `--include-ignored` adds the live HTTP test.
pub mod llm;

/// Agent configuration loading.
///
/// Why: Sub-agents are defined declaratively in TOML files under
/// `.claude/agents/` so model, prompt, and parameters can evolve without code
/// changes.
/// What: `AgentConfig`, `AgentInfo`, `LlmParams`, `SystemPrompt`, `ToolsConfig`,
/// `RunnerConfig`, `RunnerKind`, `discover_agents`, `load_all_agents`.
/// Test: `agents::tests::*`.
pub mod agents;

/// Caller identity hierarchy for memory scoping.
///
/// Why: Memory must be scoped according to who is calling — operator, PM, or
/// sub-agent — so agents cannot self-elevate their recall scope.
/// What: `CallerIdentity` enum, `RecallCeiling`, env-var constructors.
/// Test: `identity::tests::*`.
pub mod identity;

/// Tracing and logging initialisation.
///
/// Why: All binaries need consistent stderr-bound tracing setup; centralising
/// it prevents duplicated setup across entry points.
/// What: `init_tracing`, `init_tracing_for_test`, `DEFAULT_LOG_LEVEL`.
/// Test: `logging::tests::*`.
pub mod logging;

// ── Package-level re-exports ──

/// Version string, re-exported so integration tests can assert it without
/// hard-coding the constant.
///
/// Why: Single source of truth for the version across CLI and any future API
/// responses that embed it.
/// What: The `CARGO_PKG_VERSION` compile-time env var, captured at build time.
/// Test: `cargo run -p trusty-code -- --version` must print this value.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_non_empty() {
        // Why: guard against accidental blank version strings.
        // What: asserts that VERSION is not the empty string.
        // Test: this test itself.
        assert!(!VERSION.is_empty());
    }
}
