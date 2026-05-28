//! Agent configuration loader.
//!
//! Why: Sub-agents (and the PM itself) are defined declaratively in TOML so
//! model, prompt, and LLM parameters can evolve without code changes.
//! What: Parses `config/agents/<name>.toml` into strongly-typed `AgentConfig`.
//! Test: `AgentConfig::load` on bundled `pm.toml` / `python-engineer.toml`
//! returns Ok with expected `agent.name` / `agent.model`.

pub mod claude_code_runner;
pub mod claude_mpm_loader;
pub mod context_filter;
pub mod harness_protocol;
pub mod in_process_runner;
pub mod persona;
pub mod prompt_builder;
pub mod registry;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::llm::adapter::{ModelAdapter, adapter_for_model};

/// Top-level agent config loaded from TOML.
#[derive(Debug, Clone, Deserialize)]
pub struct AgentConfig {
    pub agent: AgentInfo,
    pub llm: LlmParams,
    pub system_prompt: SystemPrompt,
    /// Per-agent tool allowlist. Absent = no restriction.
    ///
    /// Why: Different agents need different capability surfaces; gating by
    /// allowlist at the registry dispatch site prevents, e.g., the planner
    /// from shelling out.
    /// What: `ToolsConfig.allowed` is an optional list of tool names.
    /// Test: `ToolsConfig` parses from TOML; see `tools_config_parses_allowed`.
    #[serde(default)]
    pub tools: ToolsConfig,

    /// Optional `[compress]` section (#135) for send-time prompt compression.
    ///
    /// Why: Long multi-turn conversations and verbose task prompts burn
    /// tokens; opt-in compression shrinks them at send-time without mutating
    /// stored history. Disabled by default so existing agents are unaffected.
    /// What: `AgentCompressConfig` with `enabled`, `token_budget`, `compress_task`.
    /// Test: `compress_config_defaults_disabled`, `compress_config_parses_block`.
    #[serde(default)]
    pub compress: AgentCompressConfig,

    /// Optional `[runner_config]` block (#198) — runner-specific tunables.
    ///
    /// Why: The in-process runner needs its own knobs (max tool calls,
    /// allowed-tools subset) without polluting `[llm]` with fields that don't
    /// apply to the subprocess / claude-code paths. Other runners are free to
    /// honour or ignore these fields.
    /// What: `RunnerConfig` with optional fields; absent defaults to all-None.
    /// Test: `runner_config_parses_max_tool_calls`.
    #[serde(default)]
    pub runner_config: RunnerConfig,

    /// Optional `[session]` block (#448) — multi-turn history summarization.
    ///
    /// Why: Distinct from `[compress]` (deterministic prompt-token shaving),
    /// this controls when conversation history gets collapsed via an LLM
    /// summarization call. Disabled by default; opt-in per agent.
    /// What: `SessionCompressionConfig` with threshold/keep-recent/model.
    /// Test: `session_config_defaults_disabled`,
    /// `session_config_parses_block`.
    #[serde(default)]
    pub session: SessionCompressionConfig,

    /// Agent-scoped tool plugins (#446).
    ///
    /// Why: Operators want to extend an agent with domain-specific tools
    /// (custom reports, lookups against internal APIs) without modifying
    /// open-mpm core. The `[plugins]` block declares one or more transports
    /// (currently only Python subprocess) that the agent loader resolves into
    /// runtime tool objects via `PythonToolPlugin::from_config`.
    /// What: `AgentPluginsConfig` with optional `python` list. Absent = no
    /// plugins; existing TOMLs are unchanged.
    /// Test: `plugins_python_section_parses` in this module's tests.
    #[serde(default)]
    pub plugins: AgentPluginsConfig,

    /// Optional `[rbac]` block (#445) — role-based access control.
    ///
    /// Why: Open-mpm exposes the same agent across multiple transports
    /// (CLI, Slack, Telegram, HTTP). Each transport identifies users
    /// differently; the `[rbac]` block lets operators declare an env-var
    /// allowlist plus default tiers for authenticated vs unauthenticated
    /// users without code changes.
    /// What: `RbacConfig` with all-optional fields. Empty (default) leaves
    /// every user at `ServiceTier::All` (current behavior — no restriction).
    /// Test: `rbac_config_defaults_unrestricted`, `rbac_config_parses_block`.
    #[serde(default)]
    pub rbac: RbacConfig,

    /// Provider-specific behavior adapter derived from `agent.model` (#57).
    ///
    /// Why: Replaces scattered `model_is_anthropic()` branches with a single
    /// object that knows how to format `tool_choice`, inject cache_control,
    /// and parse usage for the active provider family. Populated in
    /// `AgentConfig::load` after TOML parsing so the model string (post
    /// resolution) drives the selection.
    /// What: `Arc<dyn ModelAdapter>` so the config stays `Clone` cheaply.
    /// Skipped by serde (created after parsing); defaults to a generic
    /// adapter so tests that call `toml::from_str` directly still construct
    /// a valid value.
    /// Test: `agent_config_load_populates_adapter`.
    #[serde(skip, default = "default_adapter")]
    pub adapter: Arc<dyn ModelAdapter>,
}

fn default_adapter() -> Arc<dyn ModelAdapter> {
    Arc::new(crate::llm::adapter::GenericAdapter)
}

/// Optional `[tools]` section in agent TOML.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ToolsConfig {
    /// `None` (or missing) means no restriction; all registered tools are
    /// callable. `Some(list)` restricts to exactly those tool names.
    #[serde(default)]
    pub allowed: Option<Vec<String>>,

    /// Glob-style allowlist for persona agents (#255).
    ///
    /// Why: Persona agents (Izzie, CTO assistant) need a small, curated subset
    /// of tools. Listing every `git_*` or `mcp_*` name explicitly is verbose;
    /// glob patterns (`mcp_*`, `git_log`) keep the TOML compact.
    /// What: Optional list of patterns. A trailing `*` is a suffix wildcard
    /// matching any remainder. Exact strings match exactly. `None` means no
    /// glob filtering (preserves backward compat for coding agents).
    /// Test: `tools_config_parses_allow_globs` below; behavior covered by
    /// `filter_tools_by_allow_globs` in `ctrl/mod.rs`.
    #[serde(default)]
    pub allow: Option<Vec<String>>,

    /// Opt-in native-tool capability flags (#133).
    ///
    /// Why: Agents upgrading from shell-based tooling toggle these to
    /// progressively adopt the typed tool surface. Defaults preserve the
    /// prior behavior (search+memory on, ticketing off).
    /// What: A `NativeToolsConfig` record (all booleans).
    /// Test: Parsed via `[tools.native]` in agent TOML.
    #[serde(default)]
    #[allow(dead_code)]
    pub native: NativeToolsConfig,

    /// Shorthand for `[tools.native] ast_native = true` (#347).
    ///
    /// Why: The bake-off and engineer.toml use the compact `[tools]
    /// ast_native = true` form rather than nesting under `[tools.native]`.
    /// Both spellings resolve to the same bundle in `effective_ast_native`.
    /// What: Optional bool; when `Some(true)` (or `native.ast_native`)
    /// the AST tool bundle is registered.
    /// Test: `tools_config_parses_ast_native_shorthand`.
    #[serde(default)]
    pub ast_native: Option<bool>,

    /// OpenRPC scope patterns declared by this agent (#455).
    ///
    /// Why: External JSON-RPC endpoints (trusty-memory, trusty-search,
    /// gworkspace) tag each advertised tool with a scope string
    /// (`memory.read`, `search.read`, `google.gmail.*`). Each agent
    /// declares which of those scope families it should be allowed to
    /// invoke — independent of (and complementary to) the existing
    /// glob-based `allow` / `allowed` lists which gate in-process tool
    /// names. The tool registry consults this list when filtering the
    /// per-agent surface area so a research agent can read memory but
    /// not write it, etc.
    /// What: Optional list of scope patterns. `None` (or empty) means
    /// "no OpenRPC scopes claimed by this agent" — the registry-driven
    /// endpoint scope filter still applies at the endpoint level.
    /// Test: Round-trip parsing covered by the existing
    /// `tools_config_parses_*` cases when the field is present.
    #[serde(default)]
    pub scopes: Option<Vec<String>>,
}

impl ToolsConfig {
    /// Whether the AST-native tool bundle should be registered for this agent.
    ///
    /// Why: There are two equally-valid TOML spellings (`[tools] ast_native = true`
    /// and `[tools.native] ast_native = true`); this collapses them to one
    /// boolean for the registration site.
    /// What: Returns `true` if either is `true`.
    /// Test: Implicit via the parse tests below.
    pub fn effective_ast_native(&self) -> bool {
        self.ast_native.unwrap_or(false) || self.native.ast_native
    }
}

/// `[tools.native]` — opt-in flags for native typed tools (#133).
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct NativeToolsConfig {
    /// Register `search_code`, `search_memory`, `search_skills` (default on).
    #[serde(default = "default_true")]
    pub native_search: bool,

    /// Register `store_memory`, `retrieve_memory`, `list_memory_keys`
    /// (default on).
    #[serde(default = "default_true")]
    pub native_memory: bool,

    /// Register `create_ticket` / `get_ticket` / `close_ticket` /
    /// `list_tickets` / `add_comment`. Default off because a
    /// `TicketingClient` must be configured first.
    #[serde(default)]
    pub native_ticketing: bool,

    /// Register the AST-native tool bundle (#347):
    /// `get_symbol` / `edit_symbol` / `insert_symbol` / `add_import` /
    /// `validate_syntax` / `apply_patch`.
    ///
    /// Why: Lets coding agents replace whole-file `write_file` rewrites with
    /// surgical, syntax-validated edits. Off by default so existing agents
    /// keep their behaviour; flipped on for the engineer agent in #348.
    /// What: Boolean. The flag also accepts the bare `[tools] ast_native`
    /// shorthand via `ToolsConfig::ast_native` for compactness.
    /// Test: `tools_config_parses_ast_native` (parse path);
    /// `build_safe_registry_with_ast_native` (registration path).
    #[serde(default)]
    pub ast_native: bool,
}

impl Default for NativeToolsConfig {
    fn default() -> Self {
        Self {
            native_search: true,
            native_memory: true,
            native_ticketing: false,
            ast_native: false,
        }
    }
}

fn default_true() -> bool {
    true
}

/// `[ticketing]` — provider + credentials for native ticketing tools (#132).
///
/// Why: Lets agents with `native.native_ticketing = true` be handed a
/// preconfigured `TicketingClient` without relying on env var scraping.
/// Missing or partial entries fall back to env vars via
/// `TicketingConfig::from_env()`.
/// What: All fields optional; fields not relevant to the chosen provider
/// are ignored.
/// Test: Construction happy-path covered via `TicketingConfig::build_client`.
#[derive(Debug, Clone, Default, Deserialize)]
#[allow(dead_code)]
pub struct TicketingTomlConfig {
    pub provider: Option<String>,
    pub github_token: Option<String>,
    pub github_repo: Option<String>,
    pub jira_url: Option<String>,
    pub jira_email: Option<String>,
    pub jira_token: Option<String>,
    pub jira_project: Option<String>,
    pub linear_api_key: Option<String>,
    pub linear_team_id: Option<String>,
}

impl TicketingTomlConfig {
    /// Merge TOML-provided fields with env var fallbacks.
    #[allow(dead_code)]
    pub fn into_ticketing_config(self) -> crate::ticketing::TicketingConfig {
        let env = crate::ticketing::TicketingConfig::from_env();
        crate::ticketing::TicketingConfig {
            provider: self.provider.unwrap_or(env.provider),
            github_token: self.github_token.or(env.github_token),
            github_repo: self.github_repo.or(env.github_repo),
            jira_url: self.jira_url.or(env.jira_url),
            jira_email: self.jira_email.or(env.jira_email),
            jira_token: self.jira_token.or(env.jira_token),
            jira_project: self.jira_project.or(env.jira_project),
            linear_api_key: self.linear_api_key.or(env.linear_api_key),
            linear_team_id: self.linear_team_id.or(env.linear_team_id),
            force_gh_cli: false,
        }
    }
}

/// Identity and model selection.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // name/role/description read via config but not yet surfaced in PM flow
pub struct AgentInfo {
    pub name: String,
    pub role: String,
    pub model: String,
    pub description: String,
    /// Whether to retain chat history across multiple calls to this agent
    /// within a single orchestration run.
    ///
    /// Why: Some workflows need an agent to build on earlier context (e.g. a
    /// coder iterating on the same plan across several turns). Keeping this
    /// opt-in preserves the default single-shot semantics for agents that
    /// don't need it.
    /// What: When `true`, the workflow engine (or any caller holding a
    /// `SessionManager`) prepends this agent's past exchanges to the next
    /// call and records the new exchange on success.
    /// Test: `persistent_session_defaults_to_false`.
    #[serde(default)]
    pub persistent_session: bool,

    /// Which `AgentRunner` implementation should execute this agent (#60).
    ///
    /// Why: Most agents run as open-mpm subprocess calls that route LLM
    /// requests through OpenRouter / Anthropic direct using an API key. Claude
    /// Max subscribers can instead spawn the locally-installed `claude` CLI,
    /// which authenticates via OAuth with no API key required. This field
    /// lets the engine pick the right runner per-agent at load time.
    /// What: `RunnerKind` enum serialized as kebab-case. Defaults to
    /// `Subprocess` so existing agent TOMLs keep working unchanged.
    /// Test: `runner_defaults_to_subprocess`, `runner_parses_claude_code`.
    #[serde(default)]
    pub runner: RunnerKind,

    /// Declarative capability tags for dynamic agent discovery (#167).
    ///
    /// Why: The `AgentRegistry` scans a hierarchical set of search paths and
    /// needs to match tasks to agents by role / language / framework / tag
    /// without hard-coding delegation rules in Rust. Keeping capabilities on
    /// `AgentInfo` (under `[agent.capabilities]` in TOML) lets operators drop
    /// a new TOML into `.open-mpm/agents/` and have the PM discover it on
    /// startup.
    /// What: Optional struct; when absent, the agent is treated as having no
    /// declared capabilities and scores zero in `best_match` unless its name
    /// is requested exactly.
    /// Test: `capabilities_parse_from_toml`, `registry_best_match_prefers_specific_over_general`.
    #[serde(default)]
    pub capabilities: Option<AgentCapabilities>,

    /// Optional human-readable display name surfaced by the REPL `/agent`
    /// switch command (#254).
    ///
    /// Why: Persona agents like `personal-assistant` (Izzie) and
    /// `cto-assistant` benefit from a friendly name that the REPL can echo
    /// when the user switches personas, without overloading `name` (which is
    /// a stable identifier used as the TOML filename / lookup key).
    /// What: When set, the REPL prints e.g. `Switched to: Izzie`. Falls back
    /// to `name` when absent.
    /// Test: Loaded via TOML round-trip in persona configs.
    #[serde(default)]
    pub display_name: Option<String>,

    /// Optional short label used as the REPL prompt indicator when this
    /// agent is the active conversation persona (#254).
    ///
    /// Why: Switching to `personal-assistant` should change the prompt to
    /// `izzie>` rather than the long agent name; same for `cto>` for the
    /// CTO assistant. Keeps the REPL UX compact while still distinct.
    /// What: Short string; falls back to `name` when absent.
    /// Test: Loaded via TOML round-trip in persona configs.
    #[serde(default)]
    pub prompt_label: Option<String>,
}

/// Declarative capability tags for `AgentRegistry` matching (#167).
///
/// Why: Replaces hard-coded delegation logic with data-driven agent
/// selection. Each agent declares what roles/languages/frameworks/tags it
/// covers; the PM scores candidates against task signals and picks the best.
/// What: Four string lists; empty lists are valid and mean "no claim".
/// Test: `capabilities_parse_from_toml`, `registry_best_match_prefers_specific_over_general`.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct AgentCapabilities {
    #[serde(default)]
    pub languages: Vec<String>,
    #[serde(default)]
    pub frameworks: Vec<String>,
    #[serde(default)]
    pub roles: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Selects which `AgentRunner` implementation handles an agent (#60).
///
/// Why: Surfacing the choice in config lets individual agents opt into
/// Claude Max OAuth (via the `claude` CLI) without changing code or forcing
/// that path globally.
/// What: `Subprocess` (default) re-invokes the open-mpm binary in
/// `--agent <name>` mode; `Inline` is a placeholder for future in-process
/// runners; `ClaudeCode` spawns the `claude` CLI with `-p ... --output-format
/// stream-json` and parses the final `{"type":"result"}` event.
/// Test: Via TOML round-trip in `runner_defaults_to_subprocess` and
/// `runner_parses_claude_code`.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RunnerKind {
    #[default]
    Subprocess,
    Inline,
    ClaudeCode,
    /// In-process runner (Phase C, #198): runs the agent's LLM tool loop as a
    /// tokio task in the PM process instead of spawning a subprocess.
    ///
    /// Why: Subprocess sub-agents pay a 2–3 second startup tax per delegation
    /// re-loading the embedder and configs. Lightweight read-heavy agents
    /// (docs, qa, plan reviewers) don't need the sandboxing isolation that
    /// the subprocess path provides; running them in-process eliminates the
    /// startup overhead entirely.
    /// What: Routes through `InProcessAgentRunner`, which reuses the PM's
    /// shared `Arc<async_openai::Client>` and only registers a safe subset of
    /// tools (`read_file`, `write_file`, `search_code`, `load_skill`).
    /// Test: `runner_parses_in_process` and `InProcessAgentRunner` unit tests.
    InProcess,
}

/// Optional `[runner_config]` section (#198) — runner-specific settings.
///
/// Why: Different `RunnerKind` implementations need different tuning. Today
/// only the in-process runner uses these fields, but adding the section now
/// keeps later additions (e.g. claude-code-specific timeouts) backwards
/// compatible without re-shaping `[llm]`.
/// What: All fields optional; absent block defaults to all-None.
/// Test: `runner_config_defaults_to_none`, `runner_config_parses_max_tool_calls`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RunnerConfig {
    /// Cap on the number of tool calls the in-process runner will service in
    /// a single agent invocation. When `None`, falls back to `LlmParams.max_turns`.
    ///
    /// Why: The in-process runner is intended for lightweight, read-heavy
    /// agents; capping tool calls at the runner level prevents a misbehaving
    /// agent from saturating PM workers with a long tool loop.
    /// What: When set, used as the `max_turns` ceiling for in-process runs.
    /// Test: `runner_config_parses_max_tool_calls`.
    #[serde(default)]
    pub max_tool_calls: Option<u32>,
}

/// Optional `[plugins]` section (#446) — agent-scoped tool plugin declarations.
///
/// Why: Lets agents extend their tool surface with domain-specific
/// implementations (Python subprocess scripts today) without touching open-mpm
/// core. Future transports (e.g. Rust dylib) plug in here as additional
/// fields.
/// What: A list of `[[plugins.python]]` entries parsed as `PythonPluginConfig`.
/// Test: `plugins_python_section_parses`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AgentPluginsConfig {
    /// One entry per `[[plugins.python]]` table in agent TOML.
    #[serde(default)]
    pub python: Vec<crate::plugins::PythonPluginConfig>,
}

/// Optional `[rbac]` section (#445) — role-based access control rules.
///
/// Why: Lets operators declare per-agent RBAC policy in TOML without code
/// changes. The harness consults this block at transport entry points to
/// decide which `ServiceTier` an inbound user should be assigned to.
/// What: `allowed_users_env` names the env var holding a comma-separated
/// `slack_id:name:tier` allowlist (parsed at startup); `default_tier`
/// applies to authenticated users without an explicit entry;
/// `unauthenticated_tier` applies when the inbound identifier is not in
/// the allowlist. All fields optional — an empty block keeps the previous
/// "everyone is `All`" behavior.
/// Test: `rbac_config_defaults_unrestricted`, `rbac_config_parses_block`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RbacConfig {
    /// Env var name holding the `id:name:tier,id:name:tier,...` allowlist.
    #[serde(default)]
    pub allowed_users_env: Option<String>,
    /// Tier assigned to users that appear in the allowlist without an
    /// explicit tier override. `None` -> `ServiceTier::All`.
    #[serde(default)]
    pub default_tier: Option<crate::rbac::ServiceTier>,
    /// Tier assigned to users that are NOT in the allowlist. `None` ->
    /// `ServiceTier::All` (preserves backwards-compat — operators must opt
    /// in to restriction by setting this).
    #[serde(default)]
    pub unauthenticated_tier: Option<crate::rbac::ServiceTier>,
}

impl RbacConfig {
    /// Effective default tier for authenticated users.
    pub fn effective_default_tier(&self) -> crate::rbac::ServiceTier {
        self.default_tier.clone().unwrap_or_default()
    }

    /// Effective tier for unauthenticated users (transport-default).
    pub fn effective_unauthenticated_tier(&self) -> crate::rbac::ServiceTier {
        self.unauthenticated_tier.clone().unwrap_or_default()
    }
}

/// Optional `[compress]` section (#135) — send-time prompt compression.
///
/// Why: Long prompts and multi-turn histories waste tokens; this block lets
/// an operator opt an agent into deterministic NLP compression at send-time
/// without changing any call sites. Stored history is never mutated.
/// What: `enabled` toggles the whole pipeline; `token_budget` caps the
/// history window in tokens (evicting lowest-scoring middle turns); when
/// `compress_task` is true the latest user task message is also run through
/// the `Compressor`. Disabled by default to preserve existing behavior.
/// Test: `compress_config_defaults_disabled`, `compress_config_parses_block`.
#[derive(Debug, Clone, Deserialize)]
pub struct AgentCompressConfig {
    /// Master switch — when false, no compression happens (default).
    #[serde(default)]
    pub enabled: bool,
    /// History window token budget in approximate tokens (default: 32000).
    #[serde(default = "default_compress_token_budget")]
    pub token_budget: usize,
    /// Whether to also compress the latest user task message (default: false).
    #[serde(default)]
    pub compress_task: bool,
    /// Caveman-style output compression instruction injected into the system
    /// prompt. `none` disables; `lite`/`full`/`ultra` apply increasing density.
    /// Default: `full` (drops articles + filler, keeps fragments).
    #[serde(default)]
    pub output_style: crate::compress::OutputStyle,
}

impl Default for AgentCompressConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            token_budget: default_compress_token_budget(),
            compress_task: false,
            output_style: crate::compress::OutputStyle::default(),
        }
    }
}

fn default_compress_token_budget() -> usize {
    32_000
}

/// Configuration for LLM-driven session history compression (#448).
///
/// Why: Long-running ctrl/PM conversations grow past the model context window;
/// summarizing old turns preserves intent while shrinking token count. Each
/// agent can tune its own trigger threshold and summarization model.
/// What: `enabled` toggles the feature; `compression_threshold` is the total
/// turn count at which the compressor runs; `keep_recent_turns` is the count
/// of trailing turns kept verbatim; `compression_model` overrides the model
/// used for the summarization call (cheap model recommended).
/// Test: `session_config_defaults_disabled`, `session_config_parses_block`.
#[derive(Debug, Clone, Deserialize)]
pub struct SessionCompressionConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_session_threshold")]
    pub compression_threshold: usize,
    #[serde(default = "default_session_keep_recent")]
    pub keep_recent_turns: usize,
    #[serde(default)]
    pub compression_model: Option<String>,
}

impl Default for SessionCompressionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            compression_threshold: default_session_threshold(),
            keep_recent_turns: default_session_keep_recent(),
            compression_model: None,
        }
    }
}

fn default_session_threshold() -> usize {
    40
}

fn default_session_keep_recent() -> usize {
    10
}

/// LLM sampling parameters.
#[derive(Debug, Clone, Deserialize)]
pub struct LlmParams {
    pub temperature: f32,
    pub max_tokens: u32,
    /// Optional TOML-level override for the agent's model.
    ///
    /// Why: #49 — lets a deployment swap models per agent without touching
    /// the `[agent].model` field (which doubles as an identity hint in some
    /// TOMLs). Takes precedence over `[agent].model` but not over the
    /// `OPEN_MPM_MODEL_<UPPER_SNAKE>` env var.
    /// What: Optional string like `"anthropic/claude-haiku-4"`.
    /// Test: See `resolve_model_llm_override_beats_agent_model`.
    #[serde(default)]
    pub model_override: Option<String>,

    /// Whether to enable Anthropic ephemeral prompt caching for this agent (#50).
    ///
    /// Why: Caching materially cuts cost/latency for long system prompts that
    /// are reused across a workflow, but it's Anthropic-only. Defaulting to
    /// `true` is safe because activation is additionally gated on the
    /// adapter reporting `Provider::Anthropic`; for other providers it's a no-op.
    /// What: When `true` AND the model is Anthropic, the LLM client injects
    /// `cache_control: {"type": "ephemeral"}` on the system message.
    /// Test: `llm_params_caching_defaults_true`.
    #[serde(default = "default_prompt_caching")]
    pub enable_prompt_caching: bool,

    /// Maximum number of LLM turns for tool-using agents (#55).
    ///
    /// Why: Some agents (notably `plan-agent`) need more iterations than the
    /// previous hardcoded limit of 12 to complete their work through the
    /// `advance_workflow_phase` tool loop. Exposing it as a config field lets
    /// us tune per-agent without code changes.
    /// What: When absent, defaults to 20. Only consulted by the tool-calling
    /// path (`run_subagent_with_tools`); the single-shot path ignores it.
    /// Test: `llm_params_max_turns_defaults_to_20`.
    #[serde(default = "default_max_turns")]
    pub max_turns: u32,

    /// How to set the `tool_choice` field on chat requests (#57).
    ///
    /// Why: Some agents (plan, research, code) do their work entirely through
    /// tools and should never emit plain text mid-task. Setting `tool_choice`
    /// to "any" forces the model to always call some tool; combined with the
    /// `finish_task` tool it makes task termination explicit instead of
    /// heuristic.
    /// What: Parsed as a lowercase string — `"auto"` (default), `"any"`, or
    /// `"none"`. The actual wire-level shape is resolved by the model adapter.
    /// Test: `llm_params_tool_choice_defaults_auto`, `llm_params_tool_choice_parses_any`.
    #[serde(default)]
    pub tool_choice: ToolChoice,

    /// Whether the `finish_task` terminal tool is auto-injected and honored
    /// as a loop-exit signal for this agent (#57).
    ///
    /// Why: `tool_choice = "any"` needs an escape hatch so the model can
    /// report completion instead of being forced to invent a tool call.
    /// `finish_task(summary=...)` is that escape hatch; turning this on
    /// registers it and makes the loop exit cleanly when the model calls it.
    /// What: `false` by default for backwards compatibility.
    /// Test: `llm_params_use_finish_task_defaults_false`,
    /// `llm_params_use_finish_task_parses_true`.
    #[serde(default)]
    pub use_finish_task: bool,

    /// Route this agent's Anthropic chat calls directly to `api.anthropic.com`
    /// instead of through OpenRouter (#59).
    ///
    /// Why: Users holding an `ANTHROPIC_API_KEY` (including Claude Max
    /// subscribers whose plan includes API access) can bypass OpenRouter's
    /// margin and pick up latest-features first by calling Anthropic natively.
    /// Opt-in so existing deployments don't accidentally redirect traffic.
    /// What: When `true` AND the agent's model routes through `AnthropicAdapter`
    /// AND `ANTHROPIC_API_KEY` is set, the chat loop POSTs to
    /// `api.anthropic.com/v1/messages` in Anthropic's native wire format.
    /// Otherwise (including for non-Anthropic models) we fall through to the
    /// existing OpenRouter path.
    /// Test: `llm_params_use_anthropic_direct_defaults_false`,
    /// `llm_params_use_anthropic_direct_parses_true`.
    #[serde(default)]
    pub use_anthropic_direct: bool,

    /// Restrict which claude CLI tools this agent can use (passed as `--allowedTools`).
    ///
    /// Why: Some agents (e.g. research-agent) should not be able to write files
    /// or run shell commands — only WebSearch/WebFetch. The `claude` CLI's
    /// `--allowedTools` flag accepts a comma-separated allowlist; we expose it
    /// via this field so per-agent restrictions are declarative in TOML.
    /// What: When non-empty AND the agent uses `ClaudeCodeAgentRunner`, the
    /// runner passes `--allowedTools <csv>`. When empty (default), no flag is
    /// passed and the CLI uses its built-in defaults — backwards compatible.
    /// Test: Parsed via standard serde `Vec<String>` default (empty vec).
    #[serde(default)]
    pub claude_allowed_tools: Vec<String>,

    /// AWS profile name for Bedrock-backed agents (#201).
    ///
    /// Why: Operators commonly maintain multiple AWS profiles in
    /// `~/.aws/credentials` (e.g. `default`, `cto`); pinning the profile
    /// per-agent makes it explicit which account a Bedrock call will hit
    /// without depending on `AWS_PROFILE` env var leakage between shells.
    /// What: When set, passed to `aws_config::defaults().profile_name(p)`.
    /// When `None`, falls back to the AWS SDK's default credential chain
    /// (env vars first, then `default` profile).
    /// Test: Field-level parsing covered by the standard serde
    /// `#[serde(default)]` semantics; integration via `bedrock_smoke_test`.
    #[serde(default)]
    pub aws_profile: Option<String>,

    /// AWS region for Bedrock-backed agents (#201).
    ///
    /// Why: Bedrock model availability is region-specific (e.g. Claude 3.5
    /// Haiku is in `us-east-1` but not all regions); declaring it per-agent
    /// avoids surprising 4xx errors when an operator's default region is
    /// unsuitable.
    /// What: When set, used directly as the `Region` for the SDK client.
    /// When `None`, the Bedrock client defaults to `us-east-1`.
    /// Test: Field-level parsing covered by serde defaults.
    #[serde(default)]
    pub aws_region: Option<String>,

    /// Model elevation: failure threshold per file (#231).
    ///
    /// Why: Sonnet is the default for cost/latency; some hard files require
    /// Opus. Rather than running every file on Opus, elevate on demand after
    /// a configurable number of transient retries fail on the same file.
    /// What: When `Some(n)`, the wave-loop retry helper, after exhausting its
    /// transient-error retries on the base model, makes ONE more attempt
    /// using `elevation_model`. When `None` (default), no elevation occurs.
    /// Test: `elevation_triggers_after_n_failures`.
    #[serde(default)]
    pub elevation_threshold: Option<u32>,

    /// Model elevation: target model name (#231).
    ///
    /// Why: Decouples the trigger (`elevation_threshold`) from the chosen
    /// fallback model so deployments can pin to whatever model name their
    /// runner expects (`claude-opus-4-6`, `anthropic/claude-opus-4-6`, etc.).
    /// What: When `Some(name)` AND `elevation_threshold` is also set, the
    /// wave loop retries once on this model after exhausting normal retries.
    /// Test: `elevation_triggers_after_n_failures`.
    #[serde(default)]
    pub elevation_model: Option<String>,

    /// Stop sequences forwarded to the LLM provider (#297).
    ///
    /// Why: Code-returning agents tend to pad explanation prose after the
    /// final code fence. A stop sequence on `"```\n\n"` halts generation as
    /// soon as the closing fence is followed by a blank line, eliminating the
    /// trailing commentary and saving 50–300 output tokens per response.
    /// What: When non-empty, the strings are forwarded as the request's
    /// `stop`/`stop_sequences` parameter on the OpenRouter and direct-Anthropic
    /// paths. Bedrock is not currently wired (TODO).
    /// Test: TOML round-trip via serde defaults.
    #[serde(default)]
    pub stop_sequences: Vec<String>,

    /// Routing model used for initial PM delegation decisions (#298).
    ///
    /// Why: PM orchestrators only need a cheap classifier-grade model to pick
    /// the right specialist agent — paying Sonnet rates for that decision is
    /// wasteful. When set, the first LLM call in the PM loop uses this model
    /// (typically Haiku); subsequent synthesis turns fall back to `model`.
    /// What: Optional model name (e.g. `"anthropic/claude-haiku-4-5"`). When
    /// `None`, the PM uses `model` for every turn (current behavior).
    /// Test: TOML round-trip via serde defaults; runtime wiring in
    /// `src/ctrl/mod.rs` (TODO #298 pending).
    #[serde(default)]
    pub routing_model: Option<String>,

    /// Disable Anthropic extended thinking for this agent (#299).
    ///
    /// Why: Sub-agent task specs are usually deterministic; extended thinking
    /// (CoT scratchpad) inflates token cost 2–4x with marginal quality gain on
    /// well-defined code-writing tasks. Reserve thinking for genuinely
    /// ambiguous reasoning workloads.
    /// What: When `Some(false)`, the harness MUST NOT enable Anthropic's
    /// `thinking` parameter for this agent. When `None` (default) or
    /// `Some(true)`, current behavior (thinking off-by-default at the call
    /// site) is preserved. Wire-up: TODO — currently parsed and stored only.
    /// Test: TOML round-trip via serde defaults.
    #[serde(default)]
    pub thinking_enabled: Option<bool>,
}

/// How to drive the `tool_choice` API field.
///
/// Why: Translates a user-friendly TOML keyword into the provider-agnostic
/// choice consumed by the adapter layer. Kept as a small enum so typos in
/// config fail at load time rather than silently at request time.
/// What: `Auto` (default), `Any` (force some tool), `None_` (forbid tools).
/// Test: `llm_params_tool_choice_parses_any`.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ToolChoice {
    #[default]
    Auto,
    Any,
    None,
}

fn default_prompt_caching() -> bool {
    true
}

fn default_max_turns() -> u32 {
    20
}

/// Where a resolved model name came from. Used for startup logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelSource {
    /// `OPEN_MPM_MODEL_<UPPER_SNAKE>` agent-specific env var.
    AgentEnv,
    /// `[llm] model_override` TOML field.
    LlmOverride,
    /// `[agent] model` TOML field (the default path).
    AgentToml,
    /// `OPEN_MPM_DEFAULT_MODEL` env var (when no agent-specific model is set).
    DefaultEnv,
    /// Hardcoded final fallback.
    Fallback,
}

impl ModelSource {
    /// Human-readable source tag used in startup logs.
    pub fn as_tag(self) -> &'static str {
        match self {
            Self::AgentEnv => "env OPEN_MPM_MODEL_*",
            Self::LlmOverride => "toml [llm].model_override",
            Self::AgentToml => "toml [agent].model",
            Self::DefaultEnv => "env OPEN_MPM_DEFAULT_MODEL",
            Self::Fallback => "fallback",
        }
    }
}

/// Hardcoded final fallback used when no config or env var provides a model.
pub const FALLBACK_MODEL: &str = "anthropic/claude-sonnet-4-6";

/// Built-in TOML for the standalone `ctrl` agent (#240).
///
/// Why: Lets the REPL boot in disconnected mode even when neither
/// `~/.open-mpm/agents/ctrl.toml` nor a project-level `pm.toml` is present.
/// What: Same shape as a hand-authored TOML; consumed by
/// `AgentConfig::ctrl_default`.
/// Test: `agent_config_ctrl_default_loads_with_adapter`.
const CTRL_DEFAULT_TOML: &str = r#"
[agent]
name = "ctrl"
role = "controller"
model = "anthropic/claude-sonnet-4-6"
description = "ctrl — open-mpm coordination layer (assistant + project coordinator)"

[llm]
temperature = 0.5
max_tokens = 4096

[system_prompt]
content = """
You are ctrl — the coordination layer for open-mpm. You sit between the user and the PM orchestrator.

## Your Role

You have two modes, seamlessly integrated:

**Assistant**: You answer questions, discuss ideas, explain concepts, and help the user think through problems directly — no delegation needed.

**Coordinator**: You drive projects to completion. When a task requires code, research, QA, docs, or ops work, you delegate to the PM which routes to the right specialist agent. You track what's in flight, surface blockers, and push work forward.

You are NOT the PM. The PM receives a task and immediately delegates to a specialist agent (python-engineer, research-agent, qa-agent, etc.). You coordinate the PM.

## Connected vs. Standalone

**Standalone (no /connect yet)**: You are a capable assistant. Discuss, plan, and advise — but cannot delegate tasks to agents. When the user wants to act on a project, say: "Run /connect <path> to attach a project and enable agent delegation."

**Connected (after /connect <path>)**: Full coordination mode. Use delegate_to_agent to hand work to the PM. The PM routes to the right specialist.

## Triage Logic

Incoming request — decide:

1. **Simple question or discussion** → respond directly. Don't delegate what you can answer yourself.
2. **Status / project info** → use available tools (list_projects, etc.) to answer directly.
3. **Task requiring code, research, QA, docs, or ops** → delegate to PM via delegate_to_agent.
4. **Ambiguous request** → ask one clarifying question before acting.
5. **Risky or destructive operation** → confirm explicitly before delegating.

## Driving to Completion

After delegating:
- Summarize what the agent did in ≤3 bullet points
- If the project has more phases, propose the next step: "Next: shall I run QA on this?"
- If blocked, state why: "⚠️ BLOCKED: missing API key for X — provide it or skip this step"
- If failed, diagnose and propose recovery

Don't stop mid-project without a clear handoff. If a task spans multiple delegations, track them and keep the user oriented.

## Flagging for Attention

Use `⚠️ Needs your input:` when:
- A decision requires human judgment (architecture choice, credential, external dependency)
- A task has failed and recovery requires guidance
- Requirements are too ambiguous to delegate safely
- An operation is irreversible (deletion, publish, deploy to production)

## Status Tokens

End task summaries with a status token:
- `[DONE]` — complete, no further action needed
- `[RUNNING]` — in flight, more turns coming
- `[BLOCKED]` — cannot proceed without input
- `[FAILED]` — task failed, see details

## Style

- Direct and efficient. No filler ("Great!", "Of course!", "Certainly!").
- Terse between delegations: ≤25 words unless explaining a decision.
- After agent results: crisp summary, not raw output (unless the user asks).
- Slightly opinionated: if something seems wrong, say so.
- Address the user by name if you know it.

## Available Agents (via PM delegation)

research-agent — read-only investigation, codebase analysis
engineer / python-engineer — code implementation, refactoring
plan-agent — architecture and task decomposition
qa-agent — testing, verification
docs-agent — documentation, README
local-ops-agent — bash, Docker, infra, deployment

Do NOT pass tool names (brave_search, search_code, move_file, etc.) as agent_name to delegate_to_agent.
"""
"#;

/// Convert an agent name (e.g. `"python-engineer"`) to its env-var suffix
/// (`"PYTHON_ENGINEER"`).
fn agent_env_suffix(agent_name: &str) -> String {
    agent_name
        .chars()
        .map(|c| if c == '-' { '_' } else { c })
        .collect::<String>()
        .to_uppercase()
}

/// Look up `OPEN_MPM_MODEL_<UPPER_SNAKE>` for the given agent name.
pub(crate) fn agent_model_env(agent_name: &str) -> Option<String> {
    let var = format!("OPEN_MPM_MODEL_{}", agent_env_suffix(agent_name));
    std::env::var(&var).ok().filter(|s| !s.is_empty())
}

/// Core model-resolution logic.
///
/// Why: #49 — centralize the resolution order so all call sites (startup
/// logging, chat dispatch) see the same value.
/// What: Priority order (highest first):
///   1. `OPEN_MPM_MODEL_<UPPER_SNAKE>` env var
///   2. `[llm] model_override` TOML field
///   3. `[agent] model` TOML field
///   4. `OPEN_MPM_DEFAULT_MODEL` env var
///   5. Hardcoded `FALLBACK_MODEL`
/// Test: See unit tests in this module.
pub fn resolve_model(
    agent_name: &str,
    agent_model: &str,
    llm_override: Option<&str>,
) -> (String, ModelSource) {
    if let Some(v) = agent_model_env(agent_name) {
        return (v, ModelSource::AgentEnv);
    }
    if let Some(v) = llm_override.filter(|s| !s.is_empty()) {
        return (v.to_string(), ModelSource::LlmOverride);
    }
    if !agent_model.is_empty() {
        return (agent_model.to_string(), ModelSource::AgentToml);
    }
    if let Ok(v) = std::env::var("OPEN_MPM_DEFAULT_MODEL")
        && !v.is_empty()
    {
        return (v, ModelSource::DefaultEnv);
    }
    (FALLBACK_MODEL.to_string(), ModelSource::Fallback)
}

/// System prompt payload (kept as a struct to allow future fields like
/// skill injection paths without breaking the schema).
#[derive(Debug, Clone, Deserialize)]
pub struct SystemPrompt {
    pub content: String,
    /// Optional list of skill names to resolve and append to `content` at load.
    ///
    /// Why: Agents can declare the domain-knowledge Markdown skills they want
    /// injected without hard-coding them into the system prompt text.
    /// What: Skill names (e.g. `"tdd"`) resolved by a `SkillResolver` and
    /// appended to the effective system prompt string at runtime.
    /// Test: Parse a TOML with `skills = ["foo"]` and assert the field equals
    /// `Some(vec!["foo"])`.
    #[serde(default)]
    pub skills: Option<Vec<String>>,
}

impl AgentConfig {
    /// Load an AgentConfig from a TOML file path.
    ///
    /// Why: Centralizes file-read + parse error handling so callers get one
    /// rich error describing which file failed and why.
    /// What: Reads the file, parses as TOML into `AgentConfig`.
    /// Test: Pass a path to `config/agents/pm.toml` and assert name == "pm".
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read agent config {}", path.display()))?;
        Self::from_toml_str(&raw, path)
    }

    /// Resolve an agent config by short name (e.g. "python-engineer").
    ///
    /// Why: Sub-agent processes are launched with just a name; this avoids
    /// every caller hand-building the same path. MIN-7 (#104): the old
    /// `PathBuf::from(".open-mpm/agents")` was relative to the process CWD,
    /// which broke when the binary was run from outside the repo root.
    /// What: Resolves `<OPEN_MPM_CONFIG_DIR>/<name>.toml` when the env var
    /// is set, otherwise falls back to the CWD-relative `.open-mpm/agents/`
    /// path (with a warn log so the fallback is visible at runtime).
    /// Note on async: this still uses sync `std::fs` via `Self::load`; see
    /// `by_name_async` for a tokio-friendly variant (#96). Sync callers that
    /// live in async contexts should migrate when practical.
    /// Test: `AgentConfig::by_name("pm")` loads without error when run from
    /// the project root.
    pub fn by_name(name: &str) -> Result<Self> {
        // #482: Prefer the directory-package format (`<name>/agent.toml` +
        // `persona.md`) when present; fall back to the flat `<name>.toml`.
        let dir = agents_dir();
        if let Some(cfg) = load_agent_package(&dir, name)? {
            return Ok(cfg);
        }
        Self::load(&dir.join(format!("{name}.toml")))
    }

    /// Built-in default `ctrl` agent config used when no `ctrl.toml` /
    /// `pm.toml` is found on disk (#240, standalone mode).
    ///
    /// Why: When the REPL has no project connected, the controller still
    /// needs an `AgentConfig` to drive the conversational fast path. Bundling
    /// a hardcoded fallback means a fresh checkout works even before the
    /// user creates `~/.open-mpm/agents/ctrl.toml`.
    /// What: Returns an `AgentConfig` with the FALLBACK_MODEL, modest sampling
    /// params, and the canonical ctrl standalone-mode system prompt. Uses
    /// `from_toml_str` under the hood so the adapter is populated identically
    /// to disk-loaded configs.
    /// Test: `agent_config_ctrl_default_loads_with_adapter`.
    pub fn ctrl_default() -> Self {
        Self::from_toml_str(CTRL_DEFAULT_TOML, Path::new("<built-in ctrl default>"))
            .expect("built-in ctrl default TOML must parse")
    }

    /// Async variant of `by_name` that performs its disk read via
    /// `tokio::fs` (#96 / MAJ-4).
    ///
    /// Why: `by_name` calls `std::fs::read_to_string`, which blocks the
    /// current tokio worker thread. Agent-loading happens in async runner
    /// dispatch hot paths (e.g. `DispatchingAgentRunner::run`), so a
    /// blocking read stalls every task on that worker until the read
    /// completes. This variant awaits the read so the runtime can schedule
    /// other work.
    /// What: Reads the resolved TOML path via `tokio::fs::read_to_string`,
    /// then parses + adapter-resolves identically to `Self::load`.
    /// Test: `by_name_async_loads_plan_agent`.
    pub async fn by_name_async(name: &str) -> Result<Self> {
        // #482: Prefer the directory-package format when present. The package
        // loader uses sync `std::fs`; the reads are small config files, so
        // the blocking cost is negligible relative to the LLM dispatch that
        // follows.
        let dir = agents_dir();
        if let Some(cfg) = load_agent_package(&dir, name)? {
            return Ok(cfg);
        }
        let path = dir.join(format!("{name}.toml"));
        match tokio::fs::read_to_string(&path).await {
            Ok(raw) => Self::from_toml_str(&raw, &path),
            Err(e) => {
                // #128: Fallback to claude-mpm agent format (.md + YAML
                // frontmatter) discovered under `.claude/agents/` (project)
                // or `~/.claude/agents/` (user). Lets operators drop in
                // claude-mpm agents without converting to TOML.
                let project_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                if let Some(agent) =
                    crate::agents::claude_mpm_loader::find_agent(name, &project_dir).await
                {
                    tracing::info!(
                        agent = %name,
                        source = %agent.source_path.display(),
                        "loaded claude-mpm agent (fallback from missing TOML)"
                    );
                    return Ok(agent.to_agent_config());
                }
                Err(anyhow::Error::new(e))
                    .with_context(|| format!("failed to read agent config {}", path.display()))
            }
        }
    }

    /// Shared parsing + adapter-resolution path used by both `load` and
    /// `by_name_async`.
    ///
    /// Why: Keeps the TOML-to-`AgentConfig` logic in one place so the sync
    /// and async loaders can't drift in subtle ways (e.g. one populating
    /// the adapter and the other forgetting to).
    /// What: Parses the TOML string, resolves the effective model, picks
    /// the provider adapter, and emits the same startup `tracing::info!`
    /// line as the sync path.
    /// Test: Covered indirectly by `agent_config_load_populates_adapter`
    /// and `by_name_async_loads_plan_agent`.
    fn from_toml_str(raw: &str, path: &Path) -> Result<Self> {
        let mut cfg: AgentConfig = toml::from_str(raw)
            .with_context(|| format!("failed to parse agent TOML {}", path.display()))?;
        // #367: Substitute runtime context variables in the system prompt at
        // load time so every downstream consumer (prompt_builder, claude-code
        // runner, in-process runner, inspection) sees the resolved string.
        // {{OPEN_MPM_VERSION}} → harness version from Cargo.toml.
        cfg.system_prompt.content = cfg
            .system_prompt
            .content
            .replace("{{OPEN_MPM_VERSION}}", env!("CARGO_PKG_VERSION"));
        let (resolved, source) = resolve_model(
            &cfg.agent.name,
            &cfg.agent.model,
            cfg.llm.model_override.as_deref(),
        );
        cfg.agent.model = resolved;
        cfg.adapter = Arc::from(adapter_for_model(&cfg.agent.model));
        // Validate stop_sequences against API limits (#327).
        // Anthropic caps at 8 sequences (≤ 8191 chars each); Bedrock at 4.
        // We use 8 as the permissive upper bound here — the Bedrock caller
        // can enforce its own stricter limit at dispatch time if needed.
        // Fail fast at config load rather than producing a runtime API 400.
        const MAX_STOP_SEQUENCES: usize = 8;
        const MAX_STOP_SEQUENCE_LEN: usize = 8191;
        if cfg.llm.stop_sequences.len() > MAX_STOP_SEQUENCES {
            anyhow::bail!(
                "agent '{}': stop_sequences has {} entries but the API maximum is {} \
                 (in {})",
                cfg.agent.name,
                cfg.llm.stop_sequences.len(),
                MAX_STOP_SEQUENCES,
                path.display()
            );
        }
        for (i, seq) in cfg.llm.stop_sequences.iter().enumerate() {
            if seq.is_empty() {
                anyhow::bail!(
                    "agent '{}': stop_sequences[{}] is empty — empty stop sequences \
                     are rejected by the API (in {})",
                    cfg.agent.name,
                    i,
                    path.display()
                );
            }
            if seq.len() > MAX_STOP_SEQUENCE_LEN {
                anyhow::bail!(
                    "agent '{}': stop_sequences[{}] is {} chars but the API maximum \
                     is {} chars (in {})",
                    cfg.agent.name,
                    i,
                    seq.len(),
                    MAX_STOP_SEQUENCE_LEN,
                    path.display()
                );
            }
        }
        let endpoint = cfg.adapter.api_endpoint(cfg.llm.use_anthropic_direct);
        let endpoint_host = endpoint
            .base_url
            .split("://")
            .nth(1)
            .and_then(|rest| rest.split('/').next())
            .unwrap_or(endpoint.base_url.as_str())
            .to_string();
        let routing = if endpoint.auth_header_name == "x-api-key" {
            "direct"
        } else {
            "openrouter"
        };
        tracing::debug!(
            agent = %cfg.agent.name,
            model = %cfg.agent.model,
            source = source.as_tag(),
            endpoint = %endpoint_host,
            routing = %routing,
            "resolved model"
        );
        Ok(cfg)
    }

    /// Build an `AgentConfig` from the MD-package format (#482).
    ///
    /// Why: The directory-package layout supplies the system prompt as a
    /// separate Markdown file (`persona.md` + optional `skills.md`) rather
    /// than the `[system_prompt] content` TOML key. This reassembles the
    /// two parts into the same in-memory shape produced by `from_toml_str`
    /// so all downstream consumers are unaffected.
    /// What: Parses `agent.toml` as a TOML table, injects the supplied
    /// prompt text under `system_prompt.content`, then delegates to
    /// `from_toml_str` for model resolution, adapter selection, and
    /// validation. `agent.toml` MAY carry a `[system_prompt]` table for
    /// auxiliary keys (e.g. `skills`) but MUST NOT define `content` —
    /// the prompt body belongs in `persona.md`.
    /// Test: `agent_directory_package_loads_correctly`.
    fn from_package_parts(agent_toml: &str, prompt: String, path: &Path) -> Result<Self> {
        let mut table: toml::Table = toml::from_str(agent_toml)
            .with_context(|| format!("failed to parse agent TOML {}", path.display()))?;
        let mut sp = match table.remove("system_prompt") {
            Some(toml::Value::Table(t)) => t,
            Some(_) => anyhow::bail!(
                "agent package {}: [system_prompt] must be a table",
                path.display()
            ),
            None => toml::Table::new(),
        };
        if sp.contains_key("content") {
            anyhow::bail!(
                "agent package {}: agent.toml must not define system_prompt.content \
                 — the system prompt body belongs in persona.md",
                path.display()
            );
        }
        sp.insert("content".to_string(), toml::Value::String(prompt));
        table.insert("system_prompt".to_string(), toml::Value::Table(sp));
        let reassembled = toml::to_string(&table)
            .with_context(|| format!("failed to reassemble agent package {}", path.display()))?;
        Self::from_toml_str(&reassembled, path)
    }
}

/// Resolve the directory holding agent TOML configs, honoring the
/// `OPEN_MPM_CONFIG_DIR` env var with a CWD-relative fallback (MIN-7 / #104).
///
/// Why: Installed binaries rarely share a CWD with the repo; hardcoding a
/// relative path made `open-mpm` fragile when packaged. Honoring an env var
/// lets operators point the loader at a vendored `config/` alongside the
/// binary without code changes.
/// What: Returns `${OPEN_MPM_CONFIG_DIR}/<name>.toml` when the env var is
/// set and non-empty; otherwise logs a warning once per call and returns
/// the legacy `config/agents/<name>.toml` path.
/// Test: Covered by the existing `AgentConfig::by_name("plan-agent")` tests
/// (fallback path) — an explicit env-var test lives in
/// `agent_config_path_honors_env_var`.
static CONFIG_DIR_WARNED: std::sync::OnceLock<()> = std::sync::OnceLock::new();

/// Resolve the agents directory (the parent of every agent config).
///
/// Why: Both the flat `<name>.toml` path and the directory-package
/// (`<name>/`) layout share the same parent directory; centralizing the
/// `OPEN_MPM_CONFIG_DIR` lookup keeps the two resolvers consistent.
/// What: Returns `OPEN_MPM_CONFIG_DIR` when set, else the CWD-relative
/// `.open-mpm/agents` fallback (warning once).
/// Test: Covered by `agent_config_path_honors_env_var`.
fn agents_dir() -> PathBuf {
    match std::env::var("OPEN_MPM_CONFIG_DIR") {
        Ok(s) if !s.is_empty() => PathBuf::from(s),
        _ => {
            CONFIG_DIR_WARNED.get_or_init(|| {
                tracing::warn!(
                    "OPEN_MPM_CONFIG_DIR not set; falling back to .open-mpm/agents/ (this warning appears once)"
                );
            });
            PathBuf::from(".open-mpm/agents")
        }
    }
}

// Why: Helper kept available for ad-hoc tooling that needs the flat
// `<name>.toml` path. No longer invoked by the main loader path which prefers
// the directory-package layout; retained behind `#[allow(dead_code)]` so
// future tools can reuse it without re-deriving the join logic.
#[allow(dead_code)]
fn agent_config_path(name: &str) -> PathBuf {
    agents_dir().join(format!("{name}.toml"))
}

/// Load an agent from the directory-package format if one exists (#482).
///
/// Why: The MD-package layout (`<name>/agent.toml` + `<name>/persona.md`
/// + optional `<name>/skills.md`) keeps the system prompt as editable
/// Markdown instead of an embedded TOML string. The flat `<name>.toml`
/// remains the backward-compatible fallback when no directory is present.
/// What: When `<agents_dir>/<name>/` is a directory, reads `agent.toml`
/// for the struct fields, sets `system_prompt.content` from `persona.md`,
/// and appends `skills.md` (separated by `\n\n---\n\n`) when present.
/// Returns `Ok(None)` when the directory does not exist so the caller can
/// fall back to the flat `<name>.toml` path.
/// Test: `agent_directory_package_loads_correctly`.
fn load_agent_package(dir: &Path, name: &str) -> Result<Option<AgentConfig>> {
    let pkg_dir = dir.join(name);
    if !pkg_dir.is_dir() {
        return Ok(None);
    }
    let toml_path = pkg_dir.join("agent.toml");
    let raw = std::fs::read_to_string(&toml_path)
        .with_context(|| format!("failed to read agent config {}", toml_path.display()))?;
    let persona_path = pkg_dir.join("persona.md");
    let mut prompt = std::fs::read_to_string(&persona_path)
        .with_context(|| format!("failed to read agent persona {}", persona_path.display()))?;
    let skills_path = pkg_dir.join("skills.md");
    if skills_path.exists() {
        let skills = std::fs::read_to_string(&skills_path)
            .with_context(|| format!("failed to read agent skills {}", skills_path.display()))?;
        prompt.push_str("\n\n---\n\n");
        prompt.push_str(&skills);
    }
    let cfg = AgentConfig::from_package_parts(&raw, prompt, &toml_path)?;
    Ok(Some(cfg))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Serialize model-resolution tests because they mutate process-global env.
    use std::sync::Mutex;
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn clear_model_env(agent_name: &str) {
        let var = format!("OPEN_MPM_MODEL_{}", agent_env_suffix(agent_name));
        // SAFETY: test harness, guarded by ENV_LOCK.
        unsafe {
            std::env::remove_var(&var);
            std::env::remove_var("OPEN_MPM_DEFAULT_MODEL");
        }
    }

    #[test]
    fn agent_env_suffix_uppercases_and_replaces_hyphens() {
        assert_eq!(agent_env_suffix("python-engineer"), "PYTHON_ENGINEER");
        assert_eq!(agent_env_suffix("pm"), "PM");
        assert_eq!(agent_env_suffix("research-agent"), "RESEARCH_AGENT");
    }

    #[test]
    fn resolve_model_env_var_beats_toml() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_model_env("python-engineer");
        // SAFETY: guarded by ENV_LOCK
        unsafe {
            std::env::set_var("OPEN_MPM_MODEL_PYTHON_ENGINEER", "env/winner");
        }
        let (m, src) = resolve_model("python-engineer", "toml/model", Some("toml/override"));
        assert_eq!(m, "env/winner");
        assert_eq!(src, ModelSource::AgentEnv);
        clear_model_env("python-engineer");
    }

    #[test]
    fn resolve_model_llm_override_beats_agent_model() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_model_env("x-agent");
        let (m, src) = resolve_model("x-agent", "toml/agent", Some("toml/override"));
        assert_eq!(m, "toml/override");
        assert_eq!(src, ModelSource::LlmOverride);
    }

    #[test]
    fn resolve_model_uses_agent_model_when_no_override() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_model_env("y-agent");
        let (m, src) = resolve_model("y-agent", "toml/agent", None);
        assert_eq!(m, "toml/agent");
        assert_eq!(src, ModelSource::AgentToml);
    }

    #[test]
    fn resolve_model_uses_default_env_when_nothing_else() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_model_env("z-agent");
        // SAFETY: guarded by ENV_LOCK
        unsafe {
            std::env::set_var("OPEN_MPM_DEFAULT_MODEL", "default/model");
        }
        let (m, src) = resolve_model("z-agent", "", None);
        assert_eq!(m, "default/model");
        assert_eq!(src, ModelSource::DefaultEnv);
        // SAFETY: guarded by ENV_LOCK
        unsafe {
            std::env::remove_var("OPEN_MPM_DEFAULT_MODEL");
        }
    }

    #[test]
    fn resolve_model_fallback_when_nothing_set() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_model_env("q-agent");
        let (m, src) = resolve_model("q-agent", "", None);
        assert_eq!(m, FALLBACK_MODEL);
        assert_eq!(src, ModelSource::Fallback);
    }

    #[test]
    fn resolve_model_empty_llm_override_is_ignored() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_model_env("r-agent");
        let (m, src) = resolve_model("r-agent", "toml/agent", Some(""));
        assert_eq!(m, "toml/agent");
        assert_eq!(src, ModelSource::AgentToml);
    }

    #[test]
    fn llm_params_parses_model_override() {
        let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "toml/agent"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024
model_override = "toml/override"

[system_prompt]
content = "base"
"#;
        let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
        assert_eq!(cfg.llm.model_override.as_deref(), Some("toml/override"));
    }

    #[test]
    fn compress_config_defaults_enabled() {
        // When no [compress] section is present, the defaults enable compression
        // so all agents benefit from NLP compression without explicit opt-in.
        // compress_task remains false (aggressive task-text compression stays opt-in).
        let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"
"#;
        let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
        assert!(cfg.compress.enabled);
        assert_eq!(cfg.compress.token_budget, 32_000);
        assert!(!cfg.compress.compress_task);
    }

    #[test]
    fn compress_config_passthrough_when_disabled() {
        // Explicit enabled = false must disable the pipeline (opt-out path).
        let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"

[compress]
enabled = false
"#;
        let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
        assert!(!cfg.compress.enabled);
        assert_eq!(cfg.compress.token_budget, 32_000);
        assert!(!cfg.compress.compress_task);
    }

    #[test]
    fn compress_config_parses_block() {
        // Explicit [compress] block must populate fields.
        let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"

[compress]
enabled = true
token_budget = 12000
compress_task = true
"#;
        let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
        assert!(cfg.compress.enabled);
        assert_eq!(cfg.compress.token_budget, 12000);
        assert!(cfg.compress.compress_task);
    }

    #[test]
    fn session_config_defaults_disabled() {
        let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"
"#;
        let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
        assert!(!cfg.session.enabled);
        assert_eq!(cfg.session.compression_threshold, 40);
        assert_eq!(cfg.session.keep_recent_turns, 10);
        assert!(cfg.session.compression_model.is_none());
    }

    #[test]
    fn session_config_parses_block() {
        let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"

[session]
enabled = true
compression_threshold = 60
keep_recent_turns = 12
compression_model = "claude-haiku-4-5"
"#;
        let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
        assert!(cfg.session.enabled);
        assert_eq!(cfg.session.compression_threshold, 60);
        assert_eq!(cfg.session.keep_recent_turns, 12);
        assert_eq!(
            cfg.session.compression_model.as_deref(),
            Some("claude-haiku-4-5")
        );
    }

    #[test]
    fn tools_config_parses_allowed() {
        let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"

[tools]
allowed = ["web_search", "fetch_url"]
"#;
        let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
        let list = cfg.tools.allowed.expect("allowed present");
        assert_eq!(
            list,
            vec!["web_search".to_string(), "fetch_url".to_string()]
        );
    }

    #[test]
    fn rbac_config_defaults_unrestricted() {
        // No [rbac] block -> default config -> both effective tiers are All.
        let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"
"#;
        let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
        assert_eq!(
            cfg.rbac.effective_default_tier(),
            crate::rbac::ServiceTier::All
        );
        assert_eq!(
            cfg.rbac.effective_unauthenticated_tier(),
            crate::rbac::ServiceTier::All
        );
        assert!(cfg.rbac.allowed_users_env.is_none());
    }

    #[test]
    fn rbac_config_parses_block() {
        let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"

[rbac]
allowed_users_env = "BOT_ALLOWED_USERS"
default_tier = "all"
unauthenticated_tier = "read_only"
"#;
        let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
        assert_eq!(
            cfg.rbac.allowed_users_env.as_deref(),
            Some("BOT_ALLOWED_USERS")
        );
        assert_eq!(
            cfg.rbac.effective_default_tier(),
            crate::rbac::ServiceTier::All
        );
        assert_eq!(
            cfg.rbac.effective_unauthenticated_tier(),
            crate::rbac::ServiceTier::ReadOnly
        );
    }

    #[test]
    fn tools_config_parses_ast_native_shorthand() {
        // #347: `[tools] ast_native = true` shorthand resolves through
        // `effective_ast_native()`.
        let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"

[tools]
ast_native = true
"#;
        let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
        assert!(cfg.tools.effective_ast_native());
    }

    #[test]
    fn tools_config_parses_ast_native_nested() {
        // #347: `[tools.native] ast_native = true` is the long form.
        let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"

[tools.native]
ast_native = true
"#;
        let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
        assert!(cfg.tools.effective_ast_native());
    }

    #[test]
    fn tools_config_parses_allow_globs() {
        // `[tools] allow = [...]` (#255) — glob patterns for persona agents.
        let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"

[tools]
allow = ["mcp_*", "git_log", "git_status"]
"#;
        let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
        let list = cfg.tools.allow.expect("allow present");
        assert_eq!(
            list,
            vec![
                "mcp_*".to_string(),
                "git_log".to_string(),
                "git_status".to_string(),
            ]
        );
        // `allowed` (legacy exact-match) is independent of `allow` (globs).
        assert!(cfg.tools.allowed.is_none());
    }

    #[test]
    fn skills_section_is_ignored_gracefully() {
        // MIN-8 (#105): The `[skills]` section was removed because it was
        // never consumed. Existing TOMLs in the wild may still contain the
        // section; serde should silently tolerate it (we don't set
        // `deny_unknown_fields` on AgentConfig) so agents keep loading until
        // operators clean up their configs.
        let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"

[skills]
auto_load = true
max_auto = 2
"#;
        let cfg: AgentConfig = toml::from_str(toml_str).expect("tolerates legacy [skills]");
        assert_eq!(cfg.agent.name, "x");
    }

    #[test]
    fn llm_params_caching_defaults_true() {
        let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"
"#;
        let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
        assert!(cfg.llm.enable_prompt_caching);
    }

    #[test]
    fn llm_params_max_turns_defaults_to_20() {
        let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"
"#;
        let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
        assert_eq!(cfg.llm.max_turns, 20);
    }

    #[test]
    fn llm_params_max_turns_parses_override() {
        let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024
max_turns = 30

[system_prompt]
content = "base"
"#;
        let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
        assert_eq!(cfg.llm.max_turns, 30);
    }

    #[test]
    fn llm_params_caching_can_be_disabled() {
        let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024
enable_prompt_caching = false

[system_prompt]
content = "base"
"#;
        let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
        assert!(!cfg.llm.enable_prompt_caching);
    }

    #[test]
    fn persistent_session_defaults_to_false() {
        let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"
"#;
        let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
        assert!(!cfg.agent.persistent_session);
    }

    #[test]
    fn persistent_session_parses_when_present() {
        let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"
persistent_session = true

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"
"#;
        let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
        assert!(cfg.agent.persistent_session);
    }

    #[test]
    fn llm_params_tool_choice_defaults_auto() {
        let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"
"#;
        let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
        assert_eq!(cfg.llm.tool_choice, ToolChoice::Auto);
    }

    #[test]
    fn llm_params_tool_choice_parses_any() {
        let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024
tool_choice = "any"

[system_prompt]
content = "base"
"#;
        let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
        assert_eq!(cfg.llm.tool_choice, ToolChoice::Any);
    }

    #[test]
    fn llm_params_use_finish_task_defaults_false() {
        let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"
"#;
        let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
        assert!(!cfg.llm.use_finish_task);
    }

    #[test]
    fn llm_params_use_finish_task_parses_true() {
        let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024
use_finish_task = true

[system_prompt]
content = "base"
"#;
        let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
        assert!(cfg.llm.use_finish_task);
    }

    #[test]
    fn llm_params_use_anthropic_direct_defaults_false() {
        let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"
"#;
        let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
        assert!(!cfg.llm.use_anthropic_direct);
    }

    #[test]
    fn llm_params_use_anthropic_direct_parses_true() {
        let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024
use_anthropic_direct = true

[system_prompt]
content = "base"
"#;
        let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
        assert!(cfg.llm.use_anthropic_direct);
    }

    #[tokio::test]
    async fn by_name_async_loads_plan_agent() {
        // #96: Async loader should produce the same adapter + model as the
        // sync path when OPEN_MPM_CONFIG_DIR is unset (fallback path).
        // Set up env inside a sync scope so the MutexGuard is dropped
        // before we hit any `.await` (avoids await_holding_lock clippy lint).
        {
            let _guard = ENV_LOCK.lock().unwrap();
            clear_model_env("plan-agent");
            // SAFETY: guarded by ENV_LOCK for the duration of this scope.
            unsafe {
                std::env::remove_var("OPEN_MPM_CONFIG_DIR");
            }
        }
        let cfg = AgentConfig::by_name_async("plan-agent")
            .await
            .expect("plan-agent loads async");
        use crate::llm::adapter::Provider;
        assert_eq!(cfg.adapter.provider(), Provider::Anthropic);
    }

    #[test]
    fn agent_directory_package_loads_correctly() {
        // #482: The directory-package format (`<name>/agent.toml` +
        // `persona.md` + optional `skills.md`) must load with the system
        // prompt sourced from persona.md and skills.md appended.
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().expect("create temp dir");
        let agents = tmp.path();
        let pkg = agents.join("cto-assistant");
        std::fs::create_dir(&pkg).expect("create package dir");
        std::fs::write(
            pkg.join("agent.toml"),
            r#"
[agent]
name = "cto-assistant"
role = "assistant"
model = "anthropic/claude-sonnet-4-6"
description = "test agent"

[llm]
temperature = 0.3
max_tokens = 4096
"#,
        )
        .expect("write agent.toml");
        let persona = "You are the CTO Assistant. Be concise and direct.";
        std::fs::write(pkg.join("persona.md"), persona).expect("write persona.md");
        let skills = "## Skill: org chart\nThe SELT has five members.";
        std::fs::write(pkg.join("skills.md"), skills).expect("write skills.md");

        // SAFETY: guarded by ENV_LOCK.
        unsafe {
            std::env::set_var("OPEN_MPM_CONFIG_DIR", agents);
        }
        let cfg = AgentConfig::by_name("cto-assistant").expect("loads package");
        // SAFETY: guarded by ENV_LOCK.
        unsafe {
            std::env::remove_var("OPEN_MPM_CONFIG_DIR");
        }

        assert_eq!(cfg.agent.name, "cto-assistant");
        let expected = format!("{persona}\n\n---\n\n{skills}");
        assert_eq!(cfg.system_prompt.content, expected);
    }

    #[test]
    fn agent_config_path_honors_env_var() {
        // MIN-7 (#104): With OPEN_MPM_CONFIG_DIR set, resolution must use it
        // verbatim instead of the CWD-relative fallback.
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: guarded by ENV_LOCK
        unsafe {
            std::env::set_var("OPEN_MPM_CONFIG_DIR", "/tmp/custom-agents");
        }
        let p = agent_config_path("pm");
        assert_eq!(p, PathBuf::from("/tmp/custom-agents/pm.toml"));
        // SAFETY: guarded by ENV_LOCK
        unsafe {
            std::env::remove_var("OPEN_MPM_CONFIG_DIR");
        }
        let p = agent_config_path("pm");
        assert_eq!(p, PathBuf::from(".open-mpm/agents/pm.toml"));
    }

    #[test]
    fn agent_config_load_populates_adapter() {
        // Loading a real agent TOML should set `adapter` to match the model.
        // `plan-agent` is configured with an Anthropic model.
        let _guard = ENV_LOCK.lock().unwrap();
        clear_model_env("plan-agent");
        let cfg = AgentConfig::by_name("plan-agent").expect("plan-agent loads");
        use crate::llm::adapter::Provider;
        assert_eq!(cfg.adapter.provider(), Provider::Anthropic);
    }

    #[test]
    fn agent_config_ctrl_default_loads_with_adapter() {
        // The built-in ctrl default (#240) must parse and populate an adapter
        // so the controller can boot with zero on-disk config.
        let _guard = ENV_LOCK.lock().unwrap();
        clear_model_env("ctrl");
        let cfg = AgentConfig::ctrl_default();
        assert_eq!(cfg.agent.name, "ctrl");
        assert_eq!(cfg.agent.role, "controller");
        assert!(cfg.system_prompt.content.contains("Standalone"));
        assert!(cfg.system_prompt.content.contains("delegate_to_agent"));
        // Adapter is populated by from_toml_str.
        use crate::llm::adapter::Provider;
        assert_eq!(cfg.adapter.provider(), Provider::Anthropic);
    }

    #[test]
    fn runner_defaults_to_subprocess() {
        let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"
"#;
        let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
        assert_eq!(cfg.agent.runner, RunnerKind::Subprocess);
    }

    #[test]
    fn runner_parses_claude_code() {
        let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"
runner = "claude-code"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"
"#;
        let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
        assert_eq!(cfg.agent.runner, RunnerKind::ClaudeCode);
    }

    #[test]
    fn runner_parses_in_process() {
        // #198 / Phase C: agents opt into the in-process runner via
        // `runner = "in-process"` (kebab-case for the InProcess variant).
        let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"
runner = "in-process"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"
"#;
        let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
        assert_eq!(cfg.agent.runner, RunnerKind::InProcess);
    }

    #[test]
    fn runner_config_defaults_to_none() {
        // No [runner_config] section -> all fields None.
        let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"
"#;
        let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
        assert!(cfg.runner_config.max_tool_calls.is_none());
    }

    #[test]
    fn runner_config_parses_max_tool_calls() {
        let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"

[runner_config]
max_tool_calls = 12
"#;
        let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
        assert_eq!(cfg.runner_config.max_tool_calls, Some(12));
    }

    #[test]
    fn runner_parses_inline() {
        let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"
runner = "inline"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"
"#;
        let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
        assert_eq!(cfg.agent.runner, RunnerKind::Inline);
    }

    #[test]
    fn tools_config_absent_means_no_restriction() {
        let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"
"#;
        let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
        assert!(cfg.tools.allowed.is_none());
    }

    #[test]
    fn stop_sequences_too_many_is_rejected() {
        let seqs: Vec<String> = (0..9).map(|i| format!("seq{}", i)).collect();
        let seqs_toml = seqs
            .iter()
            .map(|s| format!("\"{}\"", s))
            .collect::<Vec<_>>()
            .join(", ");
        let toml_str = format!(
            r#"
[agent]
name = "test-agent"
role = "engineer"
model = "anthropic/claude-sonnet-4-6"
description = "test"

[llm]
temperature = 0.2
max_tokens = 1024
stop_sequences = [{}]

[system_prompt]
content = "test"
"#,
            seqs_toml
        );
        let result = AgentConfig::from_toml_str(&toml_str, Path::new("test.toml"));
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("stop_sequences"),
            "error should mention stop_sequences: {}",
            msg
        );
    }

    #[test]
    fn stop_sequences_over_length_limit_is_rejected() {
        let long_seq = "x".repeat(8192); // one over the limit
        let toml_str = format!(
            r#"
[agent]
name = "test-agent"
role = "engineer"
model = "anthropic/claude-sonnet-4-6"
description = "test"

[llm]
temperature = 0.2
max_tokens = 1024
stop_sequences = ["{}"]

[system_prompt]
content = "test"
"#,
            long_seq
        );
        let result = AgentConfig::from_toml_str(&toml_str, Path::new("test.toml"));
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("stop_sequences"),
            "error should mention stop_sequences: {}",
            msg
        );
    }

    /// Why: #446 — agent TOML must accept the new `[[plugins.python]]` table and
    /// produce a structured `AgentPluginsConfig` with one entry per declaration.
    /// What: Parse a minimal agent TOML with two plugin entries (one using
    /// `schema_file`, one using inline `[plugins.python.schema]`) and assert the
    /// parsed fields, including `restricted_tiers` for the RBAC override path.
    #[test]
    fn plugins_python_section_parses() {
        let toml_str = r#"
[agent]
name = "test"
role = "engineer"
model = "anthropic/claude-sonnet-4-6"
description = "test"

[llm]
temperature = 0.2
max_tokens = 1024

[system_prompt]
content = "test"

[[plugins.python]]
name = "gfa_report"
description = "Git Flow Analytics"
script = "scripts/gfa.py"
schema_file = "scripts/gfa_schema.json"
timeout_secs = 30

[[plugins.python]]
name = "search_email"
description = "Search priority emails"
script = "scripts/email.py"
timeout_secs = 10
restricted_tiers = ["analytics", "read_only"]

[plugins.python.schema]
type = "object"

[plugins.python.schema.properties]
query = { type = "string" }
"#;
        let cfg = AgentConfig::from_toml_str(toml_str, Path::new("test.toml"))
            .expect("plugins.python section must parse");

        assert_eq!(cfg.plugins.python.len(), 2);

        let gfa = &cfg.plugins.python[0];
        assert_eq!(gfa.name, "gfa_report");
        assert_eq!(
            gfa.schema_file.as_deref(),
            Some(std::path::Path::new("scripts/gfa_schema.json"))
        );
        assert_eq!(gfa.timeout_secs, Some(30));
        assert!(gfa.restricted_tiers.is_empty());

        let email = &cfg.plugins.python[1];
        assert_eq!(email.name, "search_email");
        assert_eq!(email.timeout_secs, Some(10));
        assert_eq!(
            email.restricted_tiers,
            vec!["analytics".to_string(), "read_only".to_string()]
        );
        assert!(email.schema.is_some(), "inline schema must be parsed");
    }

    /// Why: An agent TOML with no `[plugins]` section must continue to load
    /// cleanly — the field defaults to an empty `AgentPluginsConfig`. Pins
    /// backward compatibility for the ~30 existing agent TOMLs.
    #[test]
    fn plugins_section_defaults_empty() {
        let toml_str = r#"
[agent]
name = "test"
role = "engineer"
model = "anthropic/claude-sonnet-4-6"
description = "test"

[llm]
temperature = 0.2
max_tokens = 1024

[system_prompt]
content = "test"
"#;
        let cfg = AgentConfig::from_toml_str(toml_str, Path::new("test.toml"))
            .expect("no plugins section must still parse");
        assert!(cfg.plugins.python.is_empty());
    }
}
