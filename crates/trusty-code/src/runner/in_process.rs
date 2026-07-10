//! In-process agent runner — drives a sub-agent's own loop without a subprocess.
//!
//! Why: The model-comparison harness (issue #1029) must run a delegated
//! sub-agent *inside the same process* as the PM, so a single run is cheap,
//! fully observable, and free of IPC/process-spawn flakiness. The PM's
//! `delegate_to_agent` tool calls an `AgentRunner`; this is the production
//! implementation of that seam — for each call it loads the sub-agent's config,
//! builds the sub-agent's gated tool registry, resolves its model and assembled
//! system prompt, and drives an `AgentLoop` to completion, returning the
//! sub-agent's `AgentOutput` (including its accrued token usage) back to the PM.
//! What: `InProcessAgentRunner` implements `AgentRunner`. It holds an
//! `Arc<dyn LlmClientTrait>` (shared with the PM so usage rolls up onto one
//! transcript), an `Arc<dyn RegistryFactory>` (the DI seam for how a sub-agent's
//! tools are assembled — fs/bash scoped to the working dir, plus a nested
//! delegate), the agents config directory, optional project `CLAUDE.md` context,
//! and an `InProcessRunnerConfig` (default turn/timeout budget). `RunnerKind`'s
//! default is `InProcess` (config.rs), making this the real default backend.
//! Test: `runner::tests` — stubbed-PM delegation, return-to-PM `AgentOutput`,
//! `tools.allowed` enforcement, and usage roll-up — all offline via a scripted
//! `LlmClientTrait` mock.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use anyhow::Result;
use async_trait::async_trait;

use crate::agent_loop::{AgentLoop, AgentLoopConfig, ToolEventSink};
use crate::agents::AgentConfig;
use crate::llm::LlmClientTrait;
use crate::mode::HarnessMode;
use crate::prompt::assemble_system_prompt_for_mode;
use crate::provider::{resolve_context_window, resolve_max_tokens, resolve_model};
use crate::runner::error::RunnerError;
use crate::tools::{AgentOutput, AgentRunner, RunContext, ToolRegistry};

/// Builds the tool registry a sub-agent is allowed to use for one invocation.
///
/// Why: How a sub-agent's tools are wired (fs/bash scoped to the run's working
/// directory, a nested `delegate_to_agent`, search providers, …) is an
/// orchestration concern that varies by deployment and must not be hard-coded
/// into the runner. Injecting a factory keeps the runner policy-free and lets
/// the orchestrator (#1030) and tests supply exactly the registry they want.
/// What: `build` receives the resolved `AgentConfig` and the per-call
/// `RunContext` and returns the *full* registry of tools available to that
/// agent before allowlist gating; the runner then applies `tools.allowed`.
/// Test: `runner::tests` use a closure-backed factory; production wiring lives
/// in the orchestrator / `tm meta` layer.
#[async_trait]
pub trait RegistryFactory: Send + Sync {
    /// Build the (ungated) tool registry for `agent` under `ctx`.
    ///
    /// Why: The factory owns tool construction; the runner owns gating. Keeping
    /// them separate means a single factory serves every agent and the runner
    /// applies each agent's own `tools.allowed` uniformly.
    /// What: Returns an `Arc<ToolRegistry>` containing all tools the deployment
    /// offers this agent; the runner narrows it to `tools.allowed`.
    /// Test: Closure-backed impl in `runner::tests`.
    async fn build(&self, agent: &AgentConfig, ctx: &RunContext) -> Arc<ToolRegistry>;
}

/// Blanket impl so a plain async closure can serve as a `RegistryFactory`.
///
/// Why: Most call sites (and every test) want to supply registry assembly as a
/// one-line closure rather than declaring a named type; this keeps wiring terse.
/// What: Any `Fn(&AgentConfig, &RunContext) -> Fut` returning a future of
/// `Arc<ToolRegistry>` implements `RegistryFactory`.
/// Test: `runner::tests::delegate_runs_engineer_loop` constructs the runner from
/// a closure.
#[async_trait]
impl<F, Fut> RegistryFactory for F
where
    F: Fn(AgentConfig, RunContext) -> Fut + Send + Sync,
    Fut: std::future::Future<Output = Arc<ToolRegistry>> + Send,
{
    async fn build(&self, agent: &AgentConfig, ctx: &RunContext) -> Arc<ToolRegistry> {
        self(agent.clone(), ctx.clone()).await
    }
}

/// Static tuning for an in-process runner's default loop budget.
///
/// Why: The turn cap and timeout are the dials a deployment most often varies;
/// grouping them keeps the runner constructor stable. Per-call `RunContext`
/// overrides still take precedence over these defaults.
/// What: `max_turns` and `timeout_secs` seed each invocation's `AgentLoopConfig`
/// unless `RunContext.max_turns_override` overrides the turn cap.
/// Test: `runner::tests::run_context_overrides_max_turns`.
#[derive(Debug, Clone)]
pub struct InProcessRunnerConfig {
    /// Default maximum LLM round-trips per sub-agent run.
    pub max_turns: u32,
    /// Default wall-clock budget per sub-agent run, in seconds.
    pub timeout_secs: u64,
}

impl Default for InProcessRunnerConfig {
    /// Generous-but-bounded defaults for a delegated sub-agent's own loop.
    ///
    /// Why: (bake-off L1 diagnosis) The former default of 8 turns was tuned for
    /// a toy/single-file task; a realistic multi-file package build (reading
    /// several existing files, writing/editing several more, running the
    /// project's own tests before finishing per the strengthened finish-gate
    /// convention) routinely needs more than 8 LLM round-trips. Exhausting the
    /// cap mid-task forces a `TurnCapExceeded` abort, which historically
    /// triggered a destructive PM re-delegation that discarded correct partial
    /// work and rebuilt a simpler (sometimes incomplete) solution from scratch
    /// — see `tools::delegate::redelegation_hint`, the companion fix that makes
    /// re-delegation reuse-aware regardless of this budget. 40 turns is
    /// deliberately generous: the tradeoff is a higher worst-case cost/latency
    /// ceiling per delegation in exchange for comfortably fitting a real
    /// package-sized task without truncation. Still fully overridable per call
    /// via `RunContext.max_turns_override` (`run_pipeline`) or per-runner via
    /// `with_config`/`InProcessRunnerConfig::default().max_turns = N`.
    /// What: 40 turns, 120s timeout.
    /// Test: `runner::tests::config_default_is_sane`,
    /// `runner::tests::default_max_turns_is_generous`.
    fn default() -> Self {
        Self {
            max_turns: 40,
            timeout_secs: 120,
        }
    }
}

/// Runs a delegated sub-agent in-process by driving its own `AgentLoop`.
///
/// Why: This is the concrete `AgentRunner` the PM's `delegate_to_agent` tool
/// dispatches to in the M1 harness. Running in-process (vs. subprocess) keeps a
/// comparison run a single, observable process and lets the sub-agent's token
/// usage accrue onto the same shared client the PM uses, so the orchestrator can
/// roll engineer cost up into the PM's transcript (#1030).
/// What: Holds the shared LLM client, a `RegistryFactory`, the agents config
/// directory, optional project context, and the default loop budget. For each
/// `run`, it loads `<config_dir>/<agent_name>.toml`, asks the factory for the
/// agent's tools, gates them by `tools.allowed`, resolves the model and
/// assembled system prompt, then runs an `AgentLoop` and returns its output.
/// Test: `runner::tests::*` cover the full delegation path offline.
pub struct InProcessAgentRunner {
    llm: Arc<dyn LlmClientTrait>,
    registry_factory: Arc<dyn RegistryFactory>,
    config_dir: PathBuf,
    project_context: Option<String>,
    config: InProcessRunnerConfig,
    /// #2056: propagated to every sub-agent `AgentLoop` this runner drives, so
    /// a delegated sub-agent's tool activity is observable to the same sink
    /// the delegating (PM) loop uses. `None` (the default) matches the
    /// pre-#2056 behaviour exactly.
    sink: Option<Arc<dyn ToolEventSink>>,
    /// #2056: propagated to every sub-agent `AgentLoop`, so cancelling the
    /// PM's run also stops any in-flight delegated sub-agent loop.
    cancel: Option<Arc<AtomicBool>>,
    /// #2059: the resolved `HarnessMode` for the delegating (PM) run,
    /// propagated to every sub-agent `AgentLoop` this runner drives so a
    /// delegated sub-agent's prompt/tool-schema assembly uses the SAME mode
    /// as its delegator. Defaults to `HarnessMode::default()`, matching
    /// pre-#2059 behaviour exactly (both modes are functionally identical
    /// in M1 regardless).
    mode: HarnessMode,
    /// #2069: the rendered, metadata-only skill catalog
    /// (`skills::format_skill_catalog`) threaded into every sub-agent prompt
    /// this runner assembles, in `HarnessMode::DailyDriver` only (the mode
    /// arm ignores it entirely in `Parity` — see
    /// `assemble_system_prompt_for_mode`'s docs). `None` until
    /// `with_skills_catalog` is called, matching pre-#2069 behaviour exactly.
    skills_catalog: Option<String>,
}

impl InProcessAgentRunner {
    /// Construct a runner from its collaborators.
    ///
    /// Why: Constructor injection keeps the runner testable — production passes a
    /// real `LlmClient` and a real registry factory; tests pass a scripted mock
    /// client and a closure factory.
    /// What: Stores the shared client, the registry factory, the agents config
    /// directory, and a default `InProcessRunnerConfig`. Project context is
    /// `None` until `with_project_context` is called.
    /// Test: `runner::tests::delegate_runs_engineer_loop`.
    pub fn new(
        llm: Arc<dyn LlmClientTrait>,
        registry_factory: Arc<dyn RegistryFactory>,
        config_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            llm,
            registry_factory,
            config_dir: config_dir.into(),
            project_context: None,
            config: InProcessRunnerConfig::default(),
            sink: None,
            cancel: None,
            mode: HarnessMode::default(),
            skills_catalog: None,
        }
    }

    /// Attach a [`ToolEventSink`] every delegated sub-agent loop this runner
    /// drives will notify around its own tool dispatches (#2056).
    ///
    /// Why: Makes a delegated sub-agent's tool activity observable to the
    /// same sink the delegating loop uses, without the delegate tool or the
    /// PM loop needing to know about it.
    /// What: Builder-style setter; returns `self` for chaining.
    /// Test: `runner::tests::sink_reaches_delegated_loop`.
    pub fn with_tool_event_sink(mut self, sink: Arc<dyn ToolEventSink>) -> Self {
        self.sink = Some(sink);
        self
    }

    /// Attach a cancellation flag every delegated sub-agent loop this runner
    /// drives will check (#2056).
    ///
    /// Why: Cancelling the delegating (PM) run must also stop an in-flight
    /// delegated sub-agent loop, not just the PM's own turns.
    /// What: Builder-style setter; returns `self` for chaining.
    /// Test: `runner::tests::cancel_flag_reaches_delegated_loop`.
    pub fn with_cancel_flag(mut self, cancel: Arc<AtomicBool>) -> Self {
        self.cancel = Some(cancel);
        self
    }

    /// Set the resolved `HarnessMode` every sub-agent `AgentLoop` this runner
    /// drives will use (#2059).
    ///
    /// Why: a delegated sub-agent's prompt/tool-schema assembly must use the
    /// SAME resolved mode as its delegator — a daily-driver PM run should not
    /// silently delegate to a parity-mode engineer, or vice versa.
    /// What: Builder-style setter; returns `self` for chaining.
    /// Test: `runner::tests::with_mode_completes_a_delegated_run_in_both_modes`.
    pub fn with_mode(mut self, mode: HarnessMode) -> Self {
        self.mode = mode;
        self
    }

    /// Attach project `CLAUDE.md` context injected into every assembled prompt.
    ///
    /// Why: The parity prompt assembler (#1032) merges verbatim project context
    /// as a fixed section; threading it here means every sub-agent the runner
    /// drives sees the same project rules the PM does.
    /// What: Stores `context` to pass to `assemble_system_prompt`.
    /// Test: `runner::tests::project_context_reaches_prompt` (via the system
    /// prompt captured by the scripted client).
    pub fn with_project_context(mut self, context: impl Into<String>) -> Self {
        self.project_context = Some(context.into());
        self
    }

    /// Attach the rendered skill catalog (#2069) injected into every
    /// `HarnessMode::DailyDriver` prompt this runner assembles.
    ///
    /// Why: A delegated sub-agent should see the same cheap, metadata-only
    /// skill catalog the delegating PM does, without the runner needing to
    /// know how the catalog was discovered (the caller renders it via
    /// `skills::format_skill_catalog` and passes the result straight
    /// through).
    /// What: Builder-style setter; returns `self` for chaining. Ignored
    /// entirely by `HarnessMode::Parity` at prompt-assembly time.
    /// Test: `runner::tests::skills_catalog_reaches_daily_driver_prompt`,
    /// `runner::tests::skills_catalog_ignored_in_parity`.
    pub fn with_skills_catalog(mut self, catalog: impl Into<String>) -> Self {
        self.skills_catalog = Some(catalog.into());
        self
    }

    /// Override the default loop budget (turn cap + timeout).
    ///
    /// Why: Deployments and tests may want a tighter or looser default than the
    /// built-in 40 turns / 120s.
    /// What: Replaces `self.config`.
    /// Test: `runner::tests::run_context_overrides_max_turns` sets a config then
    /// overrides it per-call.
    pub fn with_config(mut self, config: InProcessRunnerConfig) -> Self {
        self.config = config;
        self
    }

    /// Override only the wall-clock timeout of the default loop budget (#2207).
    ///
    /// Why: `run_task`/`task::executor` resolve a single run-wide deadline
    /// (`crate::provider::resolve_deadline_secs`) that must apply to the
    /// delegated engineer's loop as well as the delegating PM's own — but
    /// they have no reason to touch `max_turns`. A dedicated setter avoids
    /// forcing every call site to reconstruct a whole `InProcessRunnerConfig`
    /// (and risk silently resetting `max_turns` to its default) just to
    /// change one field.
    /// What: Builder-style setter; returns `self` for chaining. Leaves
    /// `self.config.max_turns` untouched.
    /// Test: `runner::tests::with_timeout_secs_overrides_only_timeout`.
    pub fn with_timeout_secs(mut self, timeout_secs: u64) -> Self {
        self.config.timeout_secs = timeout_secs;
        self
    }

    /// Load the agent config for `agent_name` from the config directory.
    ///
    /// Why: Each delegation targets a named agent; its model, prompt, and
    /// `tools.allowed` come from `<config_dir>/<agent_name>.toml`. Distinguishing
    /// "no such file" (`UnknownAgent`) from "file present but unparseable"
    /// (`ConfigLoad`) gives the caller actionable errors.
    /// What: Builds the path, returns `UnknownAgent` if it does not exist, else
    /// loads it (mapping a parse failure to `ConfigLoad`).
    /// Test: `runner::tests::unknown_agent_errors`,
    /// `runner::tests::delegate_runs_engineer_loop`.
    fn load_agent(&self, agent_name: &str) -> Result<AgentConfig, RunnerError> {
        let path = self.config_dir.join(format!("{agent_name}.toml"));
        if !path.exists() {
            return Err(RunnerError::UnknownAgent {
                name: agent_name.to_string(),
                dir: self.config_dir.clone(),
            });
        }
        AgentConfig::load(&path).map_err(|source| RunnerError::ConfigLoad {
            name: agent_name.to_string(),
            source,
        })
    }

    /// Resolve the gated registry, model, and assembled prompt, then run the loop.
    ///
    /// Why: Factoring the per-call pipeline out of the trait methods lets both
    /// `run` and `run_with_context` share one implementation; the only
    /// difference between them is the `RunContext` they pass in.
    /// What: Loads the agent config, builds and gates its registry (honouring
    /// `tools.allowed`), resolves the model (`RunContext` > agent > default) and
    /// turn cap (`RunContext.max_turns_override` > runner default), assembles the
    /// system prompt (BASE + agent prompt + project context), runs the
    /// `AgentLoop` — attaching (#2279) `verify_gate::default_finish_gate` so a
    /// premature `finish_task` gets a recoverable retry rather than
    /// terminating the loop — and maps any loop error to `RunnerError::Loop`.
    /// Test: `runner::tests::*`.
    async fn run_pipeline(
        &self,
        agent_name: &str,
        task: &str,
        ctx: &RunContext,
    ) -> Result<AgentOutput, RunnerError> {
        let agent = self.load_agent(agent_name)?;

        // Build the agent's full tool set, then gate it by `tools.allowed`.
        let full_registry = self.registry_factory.build(&agent, ctx).await;
        let registry = gate_registry(&full_registry, &agent);

        // Resolve the model and the per-call turn cap (RunContext wins).
        let model = resolve_model(&agent, Some(ctx));
        let max_turns = ctx.max_turns_override.unwrap_or(self.config.max_turns);
        // #run_task-maxtokens-bug: the delegated agent's own `[llm].max_tokens`
        // must reach its loop — previously this was dropped entirely and every
        // sub-agent turn was silently capped at the agent-loop default.
        let max_tokens = resolve_max_tokens(&agent);

        // #2308: derive the compaction threshold from this agent's ACTUAL
        // resolved context window rather than the flat, model-blind
        // `CompactionConfig::default()` — see that impl's doc for the root
        // cause (a 6_000-token threshold is ~3% of a real 200K-token Bedrock
        // Claude Sonnet window).
        let compaction =
            crate::agent_loop::CompactionConfig::for_context_window(resolve_context_window(&model));

        let loop_config = AgentLoopConfig {
            max_turns,
            timeout_secs: self.config.timeout_secs,
            model,
            max_tokens,
            mode: self.mode,
            compaction,
        };

        // Assemble the mode-branched system prompt (BASE + agent prompt +
        // project ctx) — see `assemble_system_prompt_for_mode`'s docs (#2059).
        // Fallback guidance (the #1023 per-tier seam) is not wired here yet.
        // `skills_catalog` (#2069) is appended only in `HarnessMode::DailyDriver`.
        let system = assemble_system_prompt_for_mode(
            self.mode,
            &agent,
            self.project_context.as_deref(),
            None,
            self.skills_catalog.as_deref(),
        );

        let mut agent_loop = AgentLoop::new(loop_config, Arc::clone(&self.llm), registry)
            // #2279: every sub-agent this runner drives gets the default
            // verify-before-finish gate — its own `bash` calls land in its
            // own `Transcript`, so no external state is needed. This is the
            // ONE production construction site for every delegated
            // sub-agent loop (both `run_task` and `task::executor` delegate
            // through this runner), so wiring it here covers both entry
            // points without duplicating the attachment at each call site.
            .with_finish_gate(crate::verify_gate::default_finish_gate());
        if let Some(sink) = &self.sink {
            agent_loop = agent_loop.with_tool_event_sink(Arc::clone(sink));
        }
        if let Some(cancel) = &self.cancel {
            agent_loop = agent_loop.with_cancel_flag(Arc::clone(cancel));
        }

        agent_loop
            .run(&system, task)
            .await
            .map_err(|source| RunnerError::Loop {
                name: agent_name.to_string(),
                source,
            })
    }
}

/// Narrow a full registry to the agent's `tools.allowed`, if any.
///
/// Why: The agent loop dispatches with no per-call allowlist, so the enforcement
/// point for `tools.allowed` (issue #1029) is handing the loop a registry that
/// only contains the permitted tools. An absent (`None`) allowlist means "all
/// tools the factory built are permitted".
/// What: If `tools.allowed` is `Some(list)`, returns `Arc::new(full.gated(list))`;
/// otherwise returns the full registry unchanged (cheap `Arc` clone).
/// Test: `runner::tests::tools_allowed_is_enforced`,
/// `runner::tests::no_allowlist_permits_all`.
fn gate_registry(full: &Arc<ToolRegistry>, agent: &AgentConfig) -> Arc<ToolRegistry> {
    match agent.tools.as_ref().and_then(|t| t.allowed.as_ref()) {
        Some(allowed) => Arc::new(full.gated(allowed)),
        None => Arc::clone(full),
    }
}

#[async_trait]
impl AgentRunner for InProcessAgentRunner {
    /// Run `task` against the named agent with default per-call context.
    ///
    /// Why: The simplest entry point the delegate tool calls; no overrides.
    /// What: Delegates to `run_pipeline` with a default `RunContext`, converting
    /// `RunnerError` into the trait's `anyhow::Error`.
    /// Test: `runner::tests::delegate_runs_engineer_loop`.
    async fn run(&self, agent_name: &str, task: &str) -> Result<AgentOutput> {
        let ctx = RunContext::default();
        Ok(self.run_pipeline(agent_name, task, &ctx).await?)
    }

    /// Run `task` against the named agent honouring per-call `ctx` overrides.
    ///
    /// Why: The orchestrator may pin a model, working directory, or turn cap for
    /// a single delegation; `RunContext` carries those without global state.
    /// What: Delegates to `run_pipeline` with the supplied `ctx`.
    /// Test: `runner::tests::run_context_overrides_max_turns`,
    /// `runner::tests::run_context_overrides_model`.
    async fn run_with_context(
        &self,
        agent_name: &str,
        task: &str,
        ctx: &RunContext,
    ) -> Result<AgentOutput> {
        Ok(self.run_pipeline(agent_name, task, ctx).await?)
    }
}

/// Convenience: discover whether an agent config exists for `name` under `dir`.
///
/// Why: Callers (e.g. the orchestrator wiring the delegate tool) sometimes need
/// to pre-check an agent name without constructing a runner; exposing the same
/// path rule the runner uses keeps the two in lockstep.
/// What: Returns `true` iff `<dir>/<name>.toml` exists.
/// Test: `runner::tests::agent_config_exists_detects_present_and_absent`.
pub fn agent_config_exists(dir: &Path, name: &str) -> bool {
    dir.join(format!("{name}.toml")).exists()
}
