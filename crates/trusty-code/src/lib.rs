//! trusty-code — per-project Claude-Code-compatible MPM orchestration harness.
//!
//! # Why
//!
//! Each Claude-Code project needs a harness that is *already* wired to its
//! own `.claude` configuration: agents, skills, MCP connections, CLAUDE.md,
//! and permissions. `trusty-code` fills that role as an original, purpose-built
//! coding harness. It is the Claude-Code-native orchestration entry point —
//! driven by API, CLI, or TUI — that runs the PM main-loop, enforces the
//! mandatory workflow, and delegates authority to typed sub-agents according to
//! MPM protocols. Its non-coding sibling, trusty-agents, is a
//! separately-installable general-purpose orchestration platform for
//! personal-productivity workflows; the two share architectural DNA but
//! trusty-code is not extracted from it. Tracked under epic #2052.
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
//! Phase 1 public surface (leaf/protocol modules, per #640):
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

// ── Phase 1 leaf/protocol modules (per #640) ──

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
/// `DelegateToAgentTool`, `UseSkillTool` (#2069's progressive-disclosure
/// on-invoke skill-body loader).
/// Test: `tools::traits::tests::*`, `tools::registry::tests::*`,
/// `tools::delegate::tests::*`, `tools::skill::tests::*`.
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

/// LLM transports for trusty-code.
///
/// Why: trusty-code agents invoke LLMs via OpenAI-compatible endpoints
/// (OpenRouter / Fireworks) and AWS Bedrock. Since #2406 (epic #2400) the
/// OpenAI-compatible HTTP mechanics live in the shared `trusty_common::inference`
/// adapter layer rather than a bespoke tcode client, so credential resolution,
/// error classification, and the wire schema are shared across the ecosystem.
/// What: Exports `OpenAiCompatClient` (shared OpenRouter/Fireworks transport),
/// `DispatchingLlmClient` (slug-routed transport), `BedrockChatClient`, all
/// request/response types (`ChatRequest`, `ChatResponse`, `ChatMessage`,
/// `ToolDefinition`, …), and `LlmError`. Credentials resolve via the shared
/// 3-tier chain (env > `.env.local` > secure store) at first use.
/// Test: `cargo test -p trusty-code` covers serialisation, deserialisation,
/// type-conversion, and error-mapping unit tests plus the offline black-box
/// e2e (`tests/inference_shared_adapter_e2e.rs`). `--include-ignored` adds the
/// live provider tests.
pub mod llm;

/// Embedded default agents & skills (#2895).
///
/// Why: A fresh project has no `.claude/agents/` or `.claude/skills/` yet;
/// bundling a working default set at compile time (mirroring, not sharing,
/// `trusty-mpm`'s embed pattern) gives every project a usable harness before
/// any project-level config exists. Disk-based config always wins when present.
/// What: `EmbeddedAgent`/`DEFAULT_AGENTS` (three native-TOML tcode agents);
/// `EmbeddedSkill`/`DEFAULT_SKILLS` (trusty-mpm's universal skill set, minus
/// the `tm-*` orchestration skills).
/// Test: `assets::tests::*`.
pub mod assets;

/// Agent configuration loading.
///
/// Why: Sub-agents are defined declaratively in TOML files under
/// `.claude/agents/` so model, prompt, and parameters can evolve without code
/// changes.
/// What: `AgentConfig`, `AgentInfo`, `LlmParams`, `SystemPrompt`, `ToolsConfig`,
/// `RunnerConfig`, `RunnerKind`, `discover_agents`, `load_all_agents`.
/// Test: `agents::tests::*`.
pub mod agents;

/// Progressive-disclosure skill discovery for `.claude/skills/` (#2069,
/// vision spec §5.3, Resolved Decision #7).
///
/// Why: A skill catalog's full Markdown bodies cost far more tokens than the
/// PM/agents need up front — most skills in a session are never invoked.
/// Splitting discovery into cheap, always-cached metadata (name +
/// description) and an on-demand-loaded body is the token-efficiency layer
/// P1B wires into `prompt::assemble_system_prompt_for_mode`'s `DailyDriver`
/// arm.
/// What: `SkillMetadata`, `locate_skills_dir` (`.claude/skills`, the
/// Claude-Code-compatible path), `discover_skill_metadata`,
/// `load_skill_body` (the lazy on-invoke loader), `format_skill_catalog`
/// (prompt-injection rendering), and `FsSkillResolver` (the concrete
/// `tools::SkillResolver` impl the `use_skill` tool is built on).
/// Test: `skills::tests::*`.
pub mod skills;

/// Harness mode selection: `daily-driver` (default, token-efficiency
/// consumption point) vs `parity` (full-schema benchmark mode) — #2059,
/// vision spec §5.9.
///
/// Why: reconciles the parity spec's byte-identical-schema requirement with
/// the production harness's future token-efficiency work by making the
/// choice an explicit, resolvable, queryable mode rather than picking one
/// behaviour permanently.
/// What: `HarnessMode`, `resolve_mode` (the three-tier precedence: env var >
/// `task.run` param > `.claude/settings.json` > default).
/// Test: `mode::tests::*`.
pub mod mode;

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

/// The typed project binding — projectless / non-git dir / git repo.
///
/// Why: "project" was one concept split across two disagreeing API surfaces —
/// `task.run` demanded a path it could not omit, `session.create` accepted a
/// label it could not index. Projectless was therefore inexpressible, which made
/// the shell's entry state (spec DOC-39 screen 7a) unimplementable. This module
/// is the single object both surfaces now converge on.
/// What: `ProjectBinding` (`None` | `Directory` | `GitRepo`), `BindingError`,
/// and `is_git_worktree` — the crate's one git-detection implementation.
/// Binding is NOT gated on `.git`: a non-git directory is a bound, indexing
/// state (#2728/#2747).
/// Test: `binding::tests::*`.
pub mod binding;

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

/// Verify-before-finish gate (#2279): rejects a premature `finish_task` with
/// a recoverable retry when a named test command was never run.
///
/// Why: (bake-off L2 diagnosis) An engineer that never runs the visible,
/// prompt-named test suite can still call `finish_task` successfully today;
/// this module gives both the delegated engineer's loop and the delegating
/// PM's loop a turn-boundary hook to reject that specific case.
/// What: [`agent_loop::FinishGate`] constructors `default_finish_gate` (own
/// transcript) and `pm_finish_gate` (externally shared engineer transcript),
/// plus the pure `names_test_command`/`is_test_command` regex predicates
/// they share.
/// Test: `verify_gate::tests::*`.
pub mod verify_gate;

/// Redundant full-suite test re-run suppression (#2682): the complement to
/// [`verify_gate`] — it stops the suite running MORE than it needs to.
///
/// Why: (bake-off L1 diagnosis) The engineer re-runs the full suite 7-14x per
/// run as repeated "one final run to confirm" passes, even when no code has
/// changed since the last clean pass, inflating tcode's turn count far above
/// claude-code's on the same challenge.
/// What: The pure `is_redundant_test_rerun` predicate the agent loop consults
/// (when suppression is enabled) to short-circuit a redundant test re-run with
/// [`redundant_run::REDUNDANT_RERUN_MESSAGE`] instead of spawning the suite.
/// Test: `redundant_run::tests::*`.
pub mod redundant_run;

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

/// Shared trusty-memory `tools/call` envelope helpers (#2424).
///
/// Why: trusty-memory direct-dispatches only its `TOOL_METHODS` allowlist —
/// every `chat_*` tool is reachable ONLY via the MCP `tools/call` envelope,
/// which the #2343 soak found the turn recorder was not using (100% of
/// durable writes failed `-32601`). One shared implementation of the
/// envelope build/unwrap keeps the write side (`session::memory_sink`) and
/// the read side (`tools::recall_session`) on the same verified shape.
/// What: `tools_call_params`, `parse_tools_call_envelope`,
/// `call_tool_wrapped`.
/// Test: `memory_envelope::tests::*`.
pub mod memory_envelope;

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

/// Daemon-side directory inspection for the UI's project picker (UI Phase-1).
///
/// Why: the UI is a thin client — no UI target (web OR Tauri) touches the
/// filesystem directly, so browsing the local disk to pick a project has to be
/// a daemon API. One call serves both the picker's rendering (screen 7a's
/// breadcrumb + per-entry `git` badge) and the three-state project binding
/// model (projectless → non-git dir → git repo).
/// What: `list_dir`, `DirListing`, `DirEntryInfo`, `ListDirError`, and
/// `fs_browse::protocol::register` (wires `fs.list_dir` onto a `Router`).
/// Test: `fs_browse::tests::*`, `fs_browse::protocol::tests::*`.
pub mod fs_browse;

/// Daemon-owned session model + attach/detach protocol (#2054, Axiom 4).
///
/// Why: "The Daemon OWNS Sessions; CLI Attaches Over the API" — sessions
/// are first-class objects living inside `tcode serve`, reachable via
/// `session.*` JSON-RPC methods over both transports.
/// What: `Session`, `SessionStatus`, `SessionRegistry`, and
/// `session::protocol::register` (wires the seven `session.*` methods onto
/// a `Router`).
/// Test: `session::model::tests::*`, `session::registry::registry_tests::*`,
/// `session::protocol::tests::*`; API-driven end-to-end coverage in
/// `tests/session_e2e.rs`.
pub mod session;

/// Daemon-owned task execution: `task.run` + background agent-loop
/// orchestration (#2056, M1 control-plane cut line).
///
/// Why: wires the session/event layer to the existing engine
/// (`agent_loop`/`runner`/`tools`/`llm`) so a task actually EXECUTES through
/// the daemon and streams live tool/lifecycle events to attached clients —
/// the keystone that closes the M1 control-plane cut line.
/// What: `task.run` JSON-RPC method, background execution orchestration, the
/// concrete `SessionToolEventSink`, and the offline "echo" mock LLM
/// (`TCODE_MOCK_LLM=echo`) that makes the whole flow black-box testable
/// without a live model.
/// Test: `task::executor::tests::*`, `task::protocol::tests::*`,
/// `task::sink::tests::*`, `task::mock_llm::tests::*`; API-driven end-to-end
/// coverage in `tests/task_e2e.rs`.
pub mod task;

/// Thin JSON-RPC client the `tcode` CLI binary drives against its own
/// spawned `tcode serve --stdio` child (#2060, vision spec §4.4 "CLI as
/// Thin Client" / Axiom 3 "API-First").
///
/// Why: the CLI must hold no orchestration logic of its own — every
/// session/task operation goes through the SAME JSON-RPC surface `tcode
/// serve` exposes, so a human, a script, and a future TUI see identical
/// behaviour.
/// What: `cli_client::stdio::StdioRpcClient` (spawn + NDJSON request/
/// response/notification I/O) and `cli_client::render` (pure formatting for
/// `session list`/`attach`/`run-task`/`transcript`).
/// Test: `cli_client::stdio::tests::*`, `cli_client::render::tests::*`;
/// end-to-end coverage in `tests/cli_e2e.rs`.
pub mod cli_client;

// ── Package-level re-exports ──

/// Version string, re-exported so integration tests can assert it without
/// hard-coding the constant.
///
/// Why: Single source of truth for the version across CLI and any future API
/// responses that embed it.
/// What: The `CARGO_PKG_VERSION` compile-time env var, captured at build time.
/// Test: `cargo run -p trusty-code -- --version` must print this value.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Test-only cross-module log-capture support for the #2857 observability
/// tests (`*_logs_warn`/`*_logs_info` across `agent_loop`, `verify_gate`,
/// `recall_session`, and `tools::delegate`).
///
/// Why: An EARLIER version of this support used
/// `tracing::subscriber::set_default` (a per-thread SCOPED override),
/// installed/removed fresh by every capturing test. `tracing`'s callsite
/// "interest" cache is process-GLOBAL and is only recomputed when a
/// subscriber is installed or removed; with many capture tests concurrently
/// installing/removing their own scoped subscriber across a wide
/// `cargo test` thread pool, the cache would intermittently settle on
/// "nobody is interested" for a callsite at the exact moment a genuinely
/// interested thread's event fired, silently dropping it — reproduced
/// directly (an empty capture buffer in ~1-in-3 to 1-in-2 full-suite runs at
/// `--test-threads` >= 4, but NEVER at `--test-threads=1`). Serializing the
/// scoped installs did not fix it, because interest churn from ANY
/// concurrently active install/remove anywhere in the process could still
/// race the check.
/// What: Installs exactly ONE global default subscriber, exactly once, for
/// the whole test-binary process ([`ensure_global_capture_installed`], via
/// `std::sync::Once`) — no scoped per-thread overrides, no repeated
/// install/remove churn, so the callsite interest cache settles to "always
/// interested" the first time each callsite is hit and never gets
/// invalidated again. Its layer ([`GlobalCaptureLayer`]) writes every
/// event's level + message into a `thread_local!` buffer — thread-local
/// storage is inherently race-free across concurrent test threads (each
/// thread only ever sees its OWN events), and a fresh `#[test]`/
/// `#[tokio::test]` on a REUSED pool thread runs strictly after the
/// previous one on that same thread finished, so [`begin_capture`]'s clear
/// can never race a still-running previous test. [`captured_at_least`]
/// reads back messages at `min_level` or more severe.
/// `logging::tests::init_tracing_for_test_is_idempotent` also calls
/// [`begin_capture`] before its own `try_init()`, guaranteeing this
/// module's global install always wins the one-shot `set_global_default`
/// race regardless of which test the harness happens to schedule first.
/// Test: exercised indirectly by every `*_logs_warn`/`*_logs_info` test in
/// this crate; a regression here would reintroduce the intermittent
/// empty-capture failure this module exists to eliminate.
#[cfg(test)]
pub(crate) mod test_support {
    use std::cell::RefCell;
    use std::sync::Once;

    thread_local! {
        static CAPTURED: RefCell<Vec<(tracing::Level, String)>> = const { RefCell::new(Vec::new()) };
    }

    /// The permanent, process-wide capture layer (see module docs).
    struct GlobalCaptureLayer;

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for GlobalCaptureLayer {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            struct MessageVisitor(String);
            impl tracing::field::Visit for MessageVisitor {
                fn record_debug(
                    &mut self,
                    field: &tracing::field::Field,
                    value: &dyn std::fmt::Debug,
                ) {
                    if field.name() == "message" {
                        self.0 = format!("{value:?}");
                    }
                }
            }
            let mut visitor = MessageVisitor(String::new());
            event.record(&mut visitor);
            let level = *event.metadata().level();
            CAPTURED.with(|c| c.borrow_mut().push((level, visitor.0)));
        }
    }

    static INSTALL_ONCE: Once = Once::new();

    /// Install [`GlobalCaptureLayer`] as the process's global default
    /// subscriber, exactly once. Idempotent and safe to call from every
    /// capturing test's setup.
    pub(crate) fn ensure_global_capture_installed() {
        INSTALL_ONCE.call_once(|| {
            use tracing_subscriber::layer::SubscriberExt as _;
            let subscriber = tracing_subscriber::registry().with(GlobalCaptureLayer);
            // Best-effort: every subscriber-touching test in this crate calls
            // this function (directly or via `begin_capture`) before doing
            // anything else tracing-related, so this install always wins the
            // one-shot `set_global_default` race in practice.
            let _ = tracing::subscriber::set_global_default(subscriber);
        });
    }

    /// Ensure the global capture layer is installed and clear THIS thread's
    /// captured-event buffer, ready to record a fresh test's events.
    ///
    /// Why/What: see module docs above.
    pub(crate) fn begin_capture() {
        ensure_global_capture_installed();
        CAPTURED.with(|c| c.borrow_mut().clear());
    }

    /// This thread's captured event messages at `min_level` or more severe,
    /// recorded since the last [`begin_capture`] call on this thread.
    ///
    /// Why/What: see module docs above.
    pub(crate) fn captured_at_least(min_level: tracing::Level) -> Vec<String> {
        CAPTURED.with(|c| {
            c.borrow()
                .iter()
                .filter(|(level, _)| *level <= min_level)
                .map(|(_, msg)| msg.clone())
                .collect()
        })
    }
}

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
