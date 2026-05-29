//! In-process `AgentRunner` (Phase C, #198) — runs an agent's LLM tool loop
//! as a tokio task in the PM process instead of spawning a subprocess.
//!
//! Why: Subprocess sub-agents pay a 2–3 second startup tax per delegation,
//! re-loading the embedder and configs from scratch. Lightweight, read-heavy
//! agents (docs reviewer, qa pass-through, plan reviewer) don't need the
//! sandboxing isolation that the subprocess path provides; running them
//! in-process eliminates the startup overhead entirely. Heavier agents
//! (anything that shells out, runs cargo, executes user code) stay on the
//! subprocess path because process isolation is part of their contract.
//!
//! What: `InProcessAgentRunner` implements `AgentRunner` by:
//!   1. Loading the agent TOML via `AgentConfig::by_name_async`.
//!   2. Building a per-call `ToolRegistry` containing only the safe subset:
//!      `read_file`, `list_dir`, `grep_files`, `write_file` (CWD-scoped),
//!      `search_code`, `load_skill`, `list_skills`, plus `finish_task` when
//!      the agent opts in.
//!   3. Constructing the layered system prompt with the same builder used by
//!      the subprocess path (BASE_PROTOCOL + CLAUDE.md walk + skill layers).
//!   4. Calling `llm::chat_with_tools_gated` with the shared
//!      `Arc<async_openai::Client>` so we don't pay client construction
//!      cost again.
//!
//! Test: `runner_construction_uses_shared_client` constructs a runner with a
//! pre-built client and verifies the registry is built; `dispatcher_routes_in_process`
//! (in `claude_code_runner.rs`) verifies dispatch wiring.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use async_openai::Client;
use async_openai::config::OpenAIConfig;
use async_openai::types::{
    ChatCompletionRequestMessage, ChatCompletionRequestSystemMessageArgs,
    ChatCompletionRequestUserMessageArgs,
};
use async_trait::async_trait;

use crate::agents::AgentConfig;
use crate::agents::harness_protocol::{BASE_PROTOCOL, FINISH_TASK_PROTOCOL};
use crate::agents::prompt_builder::SystemPromptBuilder;
use crate::ipc::extract_summary;
use crate::skills::registry::SkillRegistry as TagSkillRegistry;
use crate::tools::ToolRegistry;
use crate::tools::fs_reader::{GrepFilesTool, ListDirTool, ReadFileTool};
use crate::tools::native_search::SearchCodeTool;
use crate::tools::skill_loader::{FsSkillResolver, SkillListTool, SkillLoaderTool};
use crate::tools::traits::{AgentOutput, AgentRunner, RunContext, SkillResolver};
use crate::tools::write_file::WriteFileTool;

/// Scope-guarded env-var setter for Bedrock AWS profile/region (#201).
///
/// Why: `chat_with_tools_gated` consults `OPEN_MPM_AWS_PROFILE` /
/// `OPEN_MPM_AWS_REGION` to construct the Bedrock client without taking new
/// arguments. The runner sets these for the duration of one call and
/// restores the prior values on drop so concurrent agents don't trample
/// each other's state. Single-threaded test gating isn't required because
/// each agent run sets its own values before the await point.
/// What: On install, snapshots the current values, writes the agent's. On
/// drop, restores. Safe for our use: each agent call sets values before
/// awaiting the LLM and the guard outlives that await.
/// Test: Indirectly via `bedrock_smoke_test` (manual).
// Why: Widened from `pub(crate)` to `pub` so the `open-mpm` binary (now a
//      separate crate consuming `open-mpm` as a library) can construct one
//      via `open_mpm::agents::in_process_runner::BedrockEnvGuard::install`.
// What: Public struct with a `pub` install constructor; Drop is automatically
//       reachable.
// Test: Existing call sites in `main.rs` (bin) and `ctrl/mod.rs` (lib).
pub struct BedrockEnvGuard {
    prev_profile: Option<String>,
    prev_region: Option<String>,
}

impl BedrockEnvGuard {
    pub fn install(profile: Option<&str>, region: Option<&str>) -> Self {
        let prev_profile = std::env::var("OPEN_MPM_AWS_PROFILE").ok();
        let prev_region = std::env::var("OPEN_MPM_AWS_REGION").ok();
        // SAFETY: env mutation is process-global; documented above.
        unsafe {
            match profile {
                Some(p) => std::env::set_var("OPEN_MPM_AWS_PROFILE", p),
                None => std::env::remove_var("OPEN_MPM_AWS_PROFILE"),
            }
            match region {
                Some(r) => std::env::set_var("OPEN_MPM_AWS_REGION", r),
                None => std::env::remove_var("OPEN_MPM_AWS_REGION"),
            }
        }
        Self {
            prev_profile,
            prev_region,
        }
    }
}

impl Drop for BedrockEnvGuard {
    fn drop(&mut self) {
        // SAFETY: same as install — process-global env mutation.
        unsafe {
            match &self.prev_profile {
                Some(v) => std::env::set_var("OPEN_MPM_AWS_PROFILE", v),
                None => std::env::remove_var("OPEN_MPM_AWS_PROFILE"),
            }
            match &self.prev_region {
                Some(v) => std::env::set_var("OPEN_MPM_AWS_REGION", v),
                None => std::env::remove_var("OPEN_MPM_AWS_REGION"),
            }
        }
    }
}

/// Default in-process tool surface — read/search/write/skill helpers only.
///
/// Why: Documents the safe subset that the in-process runner registers without
/// per-agent build branches. Heavier capabilities (shell exec, web search,
/// memory recall) stay on the subprocess path where their dependencies are
/// already wired and their failure modes are isolated.
/// What: A static name list that matches the registered tools below; useful
/// for asserting the surface in tests.
pub const SAFE_TOOL_NAMES: &[&str] = &[
    "read_file",
    "list_dir",
    "grep_files",
    "write_file",
    "search_code",
    "load_skill",
    "list_skills",
    "analyze_file",
    "get_complexity_hotspots",
    "find_smells",
];

/// `AgentRunner` that executes the agent inline on the calling tokio runtime.
///
/// Why: Eliminates the 2–3s startup tax per delegation by reusing the PM's
/// already-initialized `async_openai::Client`. Agent TOML still drives model,
/// max_tokens, system prompt, and tool allowlist, so the contract observed by
/// the model is identical to the subprocess path — only the transport changes.
/// What: Holds an `Arc<Client>` (the shared LLM client), an `Arc<dyn SkillResolver>`
/// (so skill loading also avoids repeated filesystem scans), and a shared
/// `Arc<TagSkillRegistry>` so in-process agents get the same tag-ranked,
/// persisted-index-backed `list_skills(tags=[...])` as subprocess agents
/// (#170/#173). Constructed once per workflow run by `build_runner_for_workflow`.
/// Test: `runner_construction_uses_shared_client`, `in_process_list_skills_uses_tag_registry`.
pub struct InProcessAgentRunner {
    client: Arc<Client<OpenAIConfig>>,
    skill_resolver: Arc<dyn SkillResolver>,
    /// Tag-indexed catalog shared across every in-process agent run. Built once
    /// at runner construction (scan + persisted-index merge) so per-call
    /// registry assembly is allocation-free. Empty when no skills are found,
    /// in which case `build_safe_registry` falls back to the flat lister.
    tag_registry: Arc<TagSkillRegistry>,
}

impl InProcessAgentRunner {
    /// Build with an explicit shared client and skill resolver.
    ///
    /// Why: Lets callers (the workflow runner builder, tests) inject the
    /// already-constructed client + resolver. Using the same `Arc<Client>`
    /// across in-process and subprocess paths means HTTP keepalive / TLS
    /// state is reused.
    /// What: Plain constructor; both args required so the runner cannot be
    /// silently mis-configured with a fresh client. The shared tag registry is
    /// built once here via `load_with_index` (scan + persisted-index merge) so
    /// in-process `list_skills(tags=[...])` is tag-ranked and learning-aware.
    /// Test: `runner_construction_uses_shared_client`.
    pub fn new(client: Arc<Client<OpenAIConfig>>, skill_resolver: Arc<dyn SkillResolver>) -> Self {
        let tag_registry = Arc::new(TagSkillRegistry::load_with_index(
            &crate::default_bundled_config_dir(),
        ));
        Self {
            client,
            skill_resolver,
            tag_registry,
        }
    }

    /// Construct with an explicit, pre-built tag registry (tests / advanced callers).
    ///
    /// Why: `new` scans the filesystem to build the tag registry, which tests
    /// that want to assert tag-ranked behaviour need to control. This seam lets
    /// them inject a deterministic registry without touching the real config dir.
    /// What: Plain constructor storing the supplied registry verbatim.
    /// Test: `in_process_list_skills_uses_tag_registry`.
    pub fn with_tag_registry(
        client: Arc<Client<OpenAIConfig>>,
        skill_resolver: Arc<dyn SkillResolver>,
        tag_registry: Arc<TagSkillRegistry>,
    ) -> Self {
        Self {
            client,
            skill_resolver,
            tag_registry,
        }
    }

    /// Convenience constructor pulling the default skill resolver.
    ///
    /// Why: Most callers (the PM/workflow path) want the standard
    /// project-then-home skill search; only tests inject a mock resolver.
    /// What: Builds an `FsSkillResolver::from_defaults()` and wraps it.
    /// Test: Used by `build_runner_for_workflow` integration in `main.rs`.
    pub fn with_default_resolver(client: Arc<Client<OpenAIConfig>>) -> Self {
        let resolver: Arc<dyn SkillResolver> = Arc::new(FsSkillResolver::from_defaults());
        Self::new(client, resolver)
    }

    /// Build the safe-subset tool registry for an in-process agent.
    ///
    /// Why: Centralises tool wiring so the same set is registered for every
    /// in-process agent; per-agent `[tools].allowed` still gates which of
    /// these the LLM may call. Keeping the surface small keeps the in-process
    /// path predictable: no shell exec, no web tools, no memory mutations.
    /// What: Registers the ten tools listed in `SAFE_TOOL_NAMES`. The
    /// `WriteFileTool` is rooted at `working_dir` (CWD by default) so writes
    /// are scoped to the project tree. The `SearchCodeTool` is constructed
    /// via `new_auto_with_local` (when `local_indexer` is supplied) or
    /// `new_auto` so it auto-detects the search daemon and only falls back
    /// to grep when neither a daemon nor a local indexer is available
    /// (#376 A1, was previously hardcoded to the grep-only `new()` path).
    /// #170/#173: When `tag_registry` is non-empty, `list_skills` is wired via
    /// `SkillListTool::with_tag_registry` so `tags=[...]` returns tag-ranked,
    /// persisted-index-aware results — identical to the subprocess path. An
    /// empty registry falls back to the flat resolver lister so single-skill
    /// projects (and tests with no skills) keep working.
    /// Test: `safe_registry_includes_expected_tools`,
    /// `in_process_list_skills_uses_tag_registry`.
    pub async fn build_safe_registry(
        skill_resolver: Arc<dyn SkillResolver>,
        working_dir: PathBuf,
        local_indexer: Option<Arc<crate::search::indexer::CodeIndexer>>,
        tag_registry: Arc<TagSkillRegistry>,
    ) -> ToolRegistry {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(ReadFileTool::new()));
        reg.register(Arc::new(ListDirTool::new()));
        reg.register(Arc::new(GrepFilesTool::new()));
        reg.register(Arc::new(WriteFileTool::new(working_dir.clone())));
        let search_tool = if let Some(idx) = local_indexer {
            SearchCodeTool::new_auto_with_local(&working_dir, idx).await
        } else {
            SearchCodeTool::new_auto(&working_dir).await
        };
        reg.register(Arc::new(search_tool));
        reg.register(Arc::new(SkillLoaderTool::new(skill_resolver.clone())));
        if tag_registry.is_empty() {
            reg.register(Arc::new(SkillListTool::new(skill_resolver)));
        } else {
            reg.register(Arc::new(SkillListTool::with_tag_registry(
                skill_resolver,
                None,
                tag_registry,
            )));
        }
        // #373: non-destructive analysis tools available to every safe-mode
        // agent (analysis-agent, etc.). These are read-only AST analyzers
        // and don't mutate disk.
        reg.register(Arc::new(crate::tools::analysis::AnalyzeFileTool));
        reg.register(Arc::new(crate::tools::analysis::GetComplexityHotspotsTool));
        reg.register(Arc::new(crate::tools::analysis::FindSmellsTool));
        reg
    }

    /// Run an in-process agent end-to-end with an optional `RunContext`.
    ///
    /// Why: Shared by `run`, `run_with_context`, and `run_with_history` so all
    /// three honour `working_dir` / `model` / `max_turns_override`
    /// uniformly. Avoids the bug pattern from #122 where overrides were
    /// silently dropped on certain code paths.
    /// What: Loads the agent config (model resolved by the loader), applies
    /// any context overrides, builds the system prompt, registers safe tools,
    /// and dispatches to `llm::chat_with_tools_gated`. Returns an `AgentOutput`
    /// with `content`, `summary` (extracted via `extract_summary`), and `usage`.
    /// Test: Direct unit tests use a mock LLM-free path (config + registry
    /// only); end-to-end is validated through manual workflow runs.
    async fn run_inner(
        &self,
        agent_name: &str,
        task: &str,
        ctx: &RunContext,
    ) -> Result<AgentOutput> {
        let mut cfg = AgentConfig::by_name_async(agent_name)
            .await
            .with_context(|| format!("failed to load agent config for '{agent_name}'"))?;

        // Apply per-call model override (carried via RunContext).
        if let Some(model) = &ctx.model
            && !model.is_empty()
        {
            cfg.agent.model = model.clone();
            cfg.adapter = Arc::from(crate::llm::adapter::adapter_for_model(&cfg.agent.model));
        }

        // Resolve the effective max_turns: ctx > runner_config.max_tool_calls > llm.max_turns.
        let max_turns = ctx
            .max_turns_override
            .or(cfg.runner_config.max_tool_calls)
            .unwrap_or(cfg.llm.max_turns);

        // Pick the working directory for write_file scoping.
        let working_dir = ctx
            .working_dir
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

        // Build the layered system prompt the same way the subprocess path does.
        let mut builder = SystemPromptBuilder::new(cfg.system_prompt.content.clone())
            .walk_project_instructions(&working_dir)
            .add_harness_layer(BASE_PROTOCOL);
        if cfg.llm.use_finish_task {
            builder = builder.add_harness_layer(FINISH_TASK_PROTOCOL);
        }
        if let Some(skills) = &cfg.system_prompt.skills {
            for s in skills {
                if let Some(text) = self.skill_resolver.resolve(s) {
                    builder = builder.add_skill(format!("# Skill: {s}\n\n{text}"));
                }
            }
        }
        // #420: Inject caveman output-style fragment so in-process agents
        // get the same compression as subprocess-runner agents.
        builder = builder.with_output_style(cfg.compress.output_style);
        let system_prompt = builder.build();

        // Build the safe tool registry, optionally appending finish_task. The
        // shared tag registry powers tag-ranked `list_skills` (#170/#173).
        let mut registry = Self::build_safe_registry(
            self.skill_resolver.clone(),
            working_dir,
            None,
            self.tag_registry.clone(),
        )
        .await;
        if cfg.llm.use_finish_task {
            registry.register(Arc::new(crate::tools::finish_task::FinishTaskTool::new()));
        }
        // #347: Optionally register the AST-native tool bundle. Triggered by
        // `[tools] ast_native = true`, `[tools.native] ast_native = true`, or
        // the process-wide `--ast-native` CLI override (#348).
        if cfg.tools.effective_ast_native() || crate::ast::is_ast_native_overridden() {
            for t in crate::tools::ast_tools::ast_native_tools() {
                registry.register(t);
            }
        }

        // Compose the message list (system + user). In-process runner does not
        // currently splice persistent-session history; if/when that's needed
        // it slots in here, mirroring `run_subagent_with_tools` in main.rs.
        let system_msg: ChatCompletionRequestMessage =
            ChatCompletionRequestSystemMessageArgs::default()
                .content(system_prompt.as_str())
                .build()
                .context("failed to build system message")?
                .into();
        let user_msg: ChatCompletionRequestMessage =
            ChatCompletionRequestUserMessageArgs::default()
                .content(task)
                .build()
                .context("failed to build user message")?
                .into();
        let messages = vec![system_msg, user_msg];

        let tool_choice = match cfg.llm.tool_choice {
            crate::agents::ToolChoice::Auto => cfg.adapter.tool_choice_auto(),
            crate::agents::ToolChoice::Any => cfg.adapter.tool_choice_any(),
            crate::agents::ToolChoice::None => Some(serde_json::Value::String("none".to_string())),
        };

        // #201: Bedrock-routed agents read AWS profile/region from env vars
        // set here. Using a process-scope env handoff (rather than threading
        // a new arg through `chat_with_tools_gated`) keeps the signature
        // stable for the OpenRouter / Anthropic-direct paths which never
        // observe these variables.
        let _aws_env_guard = if cfg.adapter.provider() == crate::llm::adapter::Provider::Bedrock {
            Some(BedrockEnvGuard::install(
                cfg.llm.aws_profile.as_deref(),
                cfg.llm.aws_region.as_deref(),
            ))
        } else {
            None
        };

        let (content, usage) = crate::llm::chat_with_tools_gated(
            self.client.as_ref(),
            &cfg.agent.model,
            cfg.adapter.as_ref(),
            messages,
            Arc::new(registry),
            cfg.tools.allowed.clone(),
            cfg.llm.temperature,
            cfg.llm.max_tokens,
            max_turns,
            cfg.llm.enable_prompt_caching,
            tool_choice,
            cfg.llm.use_finish_task,
            cfg.llm.use_anthropic_direct,
            &cfg.llm.stop_sequences,
        )
        .await
        .with_context(|| format!("in-process agent '{agent_name}' LLM loop failed"))?;

        let summary = extract_summary(&content);
        let summary_opt = if summary.is_empty() {
            None
        } else {
            Some(summary)
        };
        Ok(AgentOutput {
            content,
            summary: summary_opt,
            usage,
        })
    }
}

#[async_trait]
impl AgentRunner for InProcessAgentRunner {
    async fn run(&self, agent_name: &str, task: &str) -> Result<AgentOutput> {
        self.run_inner(agent_name, task, &RunContext::default())
            .await
    }

    async fn run_with_context(
        &self,
        agent_name: &str,
        task: &str,
        ctx: &RunContext,
    ) -> Result<AgentOutput> {
        self.run_inner(agent_name, task, ctx).await
    }

    async fn run_with_history(
        &self,
        agent_name: &str,
        task: &str,
        history: &[crate::session::HistoryMessage],
        ctx: &RunContext,
    ) -> Result<AgentOutput> {
        if !history.is_empty() {
            tracing::warn!(
                agent = %agent_name,
                turns = history.len(),
                "in-process runner does not yet thread session history; ignoring"
            );
        }
        self.run_inner(agent_name, task, ctx).await
    }
}

#[cfg(test)]
#[path = "in_process_runner_tests.rs"]
mod tests;
