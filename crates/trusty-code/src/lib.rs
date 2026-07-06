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

/// Provider abstraction and per-agent model routing.
///
/// Why: Each agent routes to its own model, possibly behind a different backend
/// (OpenRouter today, AWS Bedrock later). The agent loop depends on the
/// `Provider` trait rather than branching on the backend, and a factory maps a
/// model slug to the right implementation.
/// What: `Provider`, `ToolChoice`, `OpenRouterProvider`, `BedrockProvider`,
/// `provider_for`, `resolve_model`, `DEFAULT_MODEL`.
/// Test: `provider::*` submodule tests.
pub mod provider;

/// Project-context ingestion — load `CLAUDE.md` for prompt injection (#1033).
///
/// Why: A Claude-Code-compatible harness must give the PM and every sub-agent
/// the same project-specific rules a human Claude Code session sees — the
/// project-root `CLAUDE.md`. This module reads that file, bounds its size so a
/// runaway file cannot blow the model context budget, and degrades gracefully
/// when the file is absent.
/// What: `load_project_context` (root then `.claude/` lookup, size-capped with a
/// provenance note on truncation) and `MAX_CONTEXT_BYTES`.
/// Test: `project_context::tests::*`.
pub mod project_context;

// ── Phase 4 orchestration layer (per #1028) ──

/// Multi-turn agent loop driving an LLM through tool calls to completion.
///
/// Why: A task needing file reads, tool runs, and reaction to results cannot be
/// done in one chat call; the loop is the bounded control structure that calls
/// the model, dispatches gated tool calls, feeds results back, and iterates.
/// What: `AgentLoop`, `AgentLoopConfig`, `AgentLoopError`, and `Transcript`. The
/// loop is bounded by a turn cap and a wall-clock timeout, accrues token usage
/// via `PerfCollector`, and returns an `AgentOutput`.
/// Test: `agent_loop::tests::*` (stubbed two-turn flow, turn-cap abort,
/// recoverable tool error, usage accrual) plus an `#[ignore]`-gated live test.
pub mod agent_loop;

/// In-process agent runner — the runtime execution layer (per #1029).
///
/// Why: The PM's `delegate_to_agent` tool dispatches to an `AgentRunner`; the M1
/// harness runs the delegated sub-agent *in-process* so a delegation cycle is
/// cheap, fully observable, and rolls the sub-agent's token usage onto the same
/// shared LLM client as the PM. This module is the production implementation of
/// that seam — and `RunnerKind::InProcess` is the real default backend.
/// What: `InProcessAgentRunner` (implements `tools::AgentRunner`), the
/// `RegistryFactory` DI seam for sub-agent tool assembly, `InProcessRunnerConfig`
/// (default turn/timeout budget), `RunnerError`, and `agent_config_exists`. Each
/// `run` loads the agent config, gates its tools by `tools.allowed`, resolves the
/// model + assembled system prompt, drives an `AgentLoop`, and returns its output.
/// Test: `runner::tests::*` (stubbed-PM delegation, return-to-PM output,
/// `tools.allowed` enforcement, usage roll-up) — all offline.
pub mod runner;

/// `tcode run-task` end-to-end execution layer (#1034, #1035).
///
/// Why: This is the orchestration closer that makes the `tcode` binary run a task
/// for real — load project context, assemble the PM prompt, run the PM loop, let
/// it delegate to the engineer in-process, capture the diff + transcript + usage,
/// and render a human or JSON report with meaningful exit codes. The LLM client is
/// injected so the whole path is testable offline with a scripted mock.
/// What: `RunTaskParams`, `execute_run_task`, `RunReport`, `ExitCode`, and the
/// transcript types (`TurnRecord`, `SharedTranscript`, `RecordingLlmClient`).
/// Test: `run_task::tests::*` (all offline, mocked LLM).
pub mod run_task;

/// DOC-28 catch-up context injection for the PM prompt (#1762, PR2).
///
/// Why: The PM agent needs recent project-activity context (paused sessions,
/// git commits, memory palace entries) at task start so it can orient without
/// interrogating the operator. This module wraps the shared engine in
/// trusty-common and exposes a single `pm_catchup_context` fn that is wired
/// into the PM prompt path only (sub-agents receive `None`).
/// What: `pm_catchup_context(project_dir) -> Option<String>` — async,
/// fail-open (returns `None` on any error or empty result), never advances the
/// watermark (advancement is owned by trusty-mpm `session_launch`).
/// Test: `catchup::tests::pm_catchup_context_does_not_panic_on_empty_repo`.
pub mod catchup;

/// System-prompt assembly layer implementing the parity spec.
///
/// Why: The cross-model comparison harness must assemble the same fixed
/// instruction surface for every model so the comparison measures the model,
/// not the scaffolding (`docs/trusty-code/parity-spec.md` §1). This module owns
/// that surface: the byte-identical BASE preamble, its version token, and the
/// fixed-order assembler that merges BASE + agent prompt + project `CLAUDE.md`
/// context + optional per-tier fallback guidance (the #1023 seam).
/// What: `BASE_PREAMBLE`, `BASE_PREAMBLE_VERSION`, `PromptAssembler`, and
/// `assemble_system_prompt`.
/// Test: `prompt::tests::*`.
pub mod prompt;

// ── Phase 5 daemon transport layer (#2053, M1 control-plane cut line) ──

/// JSON-RPC 2.0 core: wire types (re-exported from `trusty_common::mcp`) plus
/// the tcode-specific `Router`/`RpcError` extensibility seam.
///
/// Why: `tcode serve` must speak the same JSON-RPC-over-STDIO contract as
/// the rest of the trusty-* family; the `Router` is the new piece that lets
/// later tickets register `session.*`/`task.*`/`harness.describe` methods
/// without touching the transport loop.
/// What: `Request`, `Response`, `error_codes` (re-exported), `RpcError`,
/// `MethodHandler`, `Router`.
/// Test: `jsonrpc::error::tests::*`, `jsonrpc::router::tests::*`.
pub mod jsonrpc;

/// `tcode serve` daemon: STDIO + HTTP JSON-RPC transports + proof-of-life
/// methods.
///
/// Why: the foundation of the M1 control-plane cut line (#2053) — proves the
/// transport + router work end-to-end via `ping`/`health` before `session.*`
/// (#2054), `task.*` (#2056), and `harness.describe` (#2066) land on top.
/// Both transports dispatch through the same `Router`, so the method
/// surface can never drift between `--stdio` and `--http`.
/// What: `build_router`, `run_stdio`, `run_http`, `DEFAULT_HTTP_PORT`, and
/// the `methods`/`transport`/`http` submodules.
/// Test: `serve::tests::*`, `serve::methods::tests::*`,
/// `serve::transport::tests::*`, `serve::http::tests::*`.
pub mod serve;

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
