//! Strongly-typed agent configuration structs parsed from TOML.
//!
//! Why: Sub-agents (and the PM itself) are defined declaratively in TOML so
//! model, prompt, and LLM parameters can evolve without code changes. Keeping
//! the type definitions in a dedicated module keeps the agents module root
//! thin and the loading/resolution logic separate from the data shapes.
//! What: Defines `AgentConfig` and every nested config struct/enum it owns
//! (`AgentInfo`, `LlmParams`, `ToolsConfig`, `RunnerKind`, etc.) plus their
//! serde defaults and small helper impls.
//! Test: Round-trip parsing is exercised by the unit tests in `tests.rs`.

use std::sync::Arc;

use serde::Deserialize;

use super::params::{
    AgentCompressConfig, AgentPluginsConfig, LlmParams, RbacConfig, RunnerConfig,
    SessionCompressionConfig,
};
use crate::llm::adapter::ModelAdapter;

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
    /// trusty-agents core. The `[plugins]` block declares one or more transports
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
    /// Why: Most agents run as trusty-agents subprocess calls that route LLM
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
    /// a new TOML into `.trusty-agents/agents/` and have the PM discover it on
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
/// What: `Subprocess` (default) re-invokes the trusty-agents binary in
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
