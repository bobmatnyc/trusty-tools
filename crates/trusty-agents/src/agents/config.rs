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
    SessionCompressionConfig, WorkstreamContextConfig,
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
    /// Why: trusty-agents exposes the same agent across multiple transports
    /// (CLI, Slack, Telegram, HTTP). Each transport identifies users
    /// differently; the `[rbac]` block lets operators declare an env-var
    /// allowlist plus default tiers for authenticated vs unauthenticated
    /// users without code changes.
    /// What: `RbacConfig` with all-optional fields. Empty (default) leaves
    /// every user at `ServiceTier::All` (current behavior — no restriction).
    /// Test: `rbac_config_defaults_unrestricted`, `rbac_config_parses_block`.
    #[serde(default)]
    pub rbac: RbacConfig,

    /// Optional `[workstreams]` block (DOC-54 §9.6, demo-day 2026-07-24) —
    /// filterable-context classification tuning.
    ///
    /// Why: The persona-chat dispatch path (`ctrl::pm_task::dispatch::persona`)
    /// in-band classifies every turn into a workstream and, in focused mode,
    /// assembles a per-workstream summary + recent-turn window. Operators
    /// need to tune the summary cadence and focused-window size per agent.
    /// What: `WorkstreamContextConfig` with `enabled`/`summarize_every`/
    /// `recent_window`. Absent section = enabled with the documented
    /// defaults (5-turn cadence, 12-turn window).
    /// Test: `workstream_context_config_defaults`,
    /// `workstream_context_config_parses_block`.
    #[serde(default)]
    pub workstreams: WorkstreamContextConfig,

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

    /// Per-agent listener bindings (#3820, DOC-54 SPEC-AGENTS-04/06)  — the
    /// third leg of the stores/tools/listeners config triple.
    ///
    /// Why: An agent REACTS to inbound events (Gmail, Calendar, …) via
    /// listeners, distinct from how it ACTS via `[tools].allow`. Each entry
    /// names a harness-level listener (defined in `~/.trusty-agents/config.toml`'s
    /// `[[listeners]]`) and further narrows (stage-two filter) which of that
    /// listener's events wake THIS agent. Absent = the agent never wakes for
    /// any event (deny-by-default, matching `[tools].allow`'s posture).
    /// What: `Vec<AgentListenerBinding>` parsed from repeated `[[listeners]]`
    /// tables.
    /// Test: `crate::listeners::config` unit tests cover the binding shape;
    /// `crate::listeners::wake` tests cover matching semantics.
    #[serde(default)]
    pub listeners: Vec<crate::listeners::config::AgentListenerBinding>,

    /// Per-agent OKG store bindings (#3816/#3864, DOC-54 SPEC-AGENTS-04
    /// §5.1) — the FIRST leg of the stores/tools/listeners config triple.
    ///
    /// Why: An agent KNOWS things via a bound OKG store (its knowledge tree
    /// plus the trusty-search index over it, optionally plus a trusty-memory
    /// palace), distinct from how it ACTS (`[tools].allow`) and REACTS
    /// (`[[listeners]]`). Before this field existed there was no way to point
    /// an agent's semantic search at its own corpus — `vector_search` was
    /// hardwired to a CWD-relative local code index (#3864) — and the GUI's
    /// OKG Stores pane could only render a placeholder. Bob's 2026-07-24
    /// decision is exactly ONE store per agent; more than one parses but is
    /// reported by `StoresConfig::validate` and only the first is used as the
    /// default search target.
    /// What: [`crate::stores::StoresConfig`], accepting either the
    /// `[[stores]]` array-of-tables (rich: name/tree/index/palace) or the
    /// product spec's `[stores] allow = [...]` shorthand. Absent = the agent
    /// binds no store, which is valid and leaves every dependent behaviour on
    /// its pre-existing default. Nothing here is fatal: an unresolvable or
    /// malformed binding is surfaced as a disconnected store with a reason
    /// (`crate::stores::status`), never a boot failure.
    /// Test: `crate::stores::config` unit tests cover the binding shapes;
    /// `crate::stores::status` covers live resolution.
    #[serde(default)]
    pub stores: crate::stores::StoresConfig,

    /// Optional `[skills]` section (#3933, DOC-57 §5.4) — capability grants by
    /// skill id instead of raw tool name.
    ///
    /// Why: A skill is the unit a user reasons about ("MTA Train Time"); a tool
    /// name is an implementation detail beneath it. Granting by skill lets the
    /// GUI edit capability in the same vocabulary it renders. Absent = today's
    /// behaviour exactly: the effective tool set is `[tools].allow` verbatim.
    /// What: [`SkillsConfig`], whose `allow` list is expanded to exact tool
    /// names by `skills::manifest::effective_tool_patterns` and UNIONED with
    /// `[tools].allow` before reaching the existing dispatch gate. An id that
    /// resolves to nothing contributes nothing and is reported — never widened
    /// to "all tools".
    /// Test: `skills::manifest::tests`.
    #[serde(default)]
    pub skills: SkillsConfig,

    /// Optional `[permissions]` section (#3936, DOC-57 §7) — the structured
    /// permissions model: scopes, tiers, `user_authority`, autonomy posture,
    /// per-skill grants.
    ///
    /// Why: An agent's permission posture was spread across four unrelated
    /// mechanisms in three locations with three different failure polarities
    /// (DOC-57 §7.1) — `[tools].scopes`, `[rbac]`, and two more with no config
    /// surface at all. `[permissions]` DESCRIBES that union; it invents no new
    /// enforcement (PM-1). Absent = today's behaviour exactly: scope
    /// resolution falls back to `[tools].scopes` verbatim
    /// ([`super::permissions::effective_scopes`]), tiers fall back to
    /// `[rbac]`, and `user_authority` defaults `false` (its documented
    /// default either way).
    /// What: [`super::permissions::PermissionsConfig`].
    /// Test: `super::permissions::tests`.
    #[serde(default)]
    pub permissions: super::permissions::PermissionsConfig,

    // #4026: cross-product subagent grants — the config source for the
    // bridge-layer allow-set (epic #4021, OQ-3/OQ-7).
    /// Optional `[subagents]` section (#4026, epic #4021) — which external
    /// non-coding specialists this agent may target through `dispatch_task`.
    ///
    /// Why: OQ-7's owner ruling puts enforcement at the BRIDGE, fail-closed,
    /// but OQ-3's ruling makes the caller's config the source of the set until
    /// #4030's runtime-built domain authority exists to feed it. This section
    /// is that source. Absent = EMPTY, never "all" — an agent with no
    /// `[subagents]` section cannot reach any cross-product specialist, the
    /// same deny-by-default posture `[skills].allow` takes.
    /// What: [`SubagentsConfig`], whose `allowed` list is intersected with
    /// `tools::cross_product::NON_CODING_TARGETS` by
    /// `tools::subagent_allow::SubagentAllowSet` — config can only ever narrow
    /// that floor, never widen it. ADR-0024 decision 4 adds a SECOND key to
    /// the same section, `delegate_allowed`, applying the identical
    /// floor-narrowing rule to the IN-PROCESS `delegate_to_agent` path over
    /// `agents::delegation::ASSISTANT_REACHABLE_SUBAGENTS`.
    /// Test: `subagents_config_defaults_empty`, `subagents_config_parses_allowed`,
    /// `subagents_config_parses_delegate_allowed`.
    #[serde(default)]
    pub subagents: SubagentsConfig,
}

fn default_adapter() -> Arc<dyn ModelAdapter> {
    Arc::new(crate::llm::adapter::GenericAdapter)
}

// #4026: minimal, forward-compatible shape — a struct (not a bare `Vec`) so
// #4029's Sub-agents configuration section can add `deny`/`domain` keys
// without a breaking config change, exactly as `SkillsConfig` was shaped.
/// Optional `[subagents]` section in agent TOML (#4026, epic #4021).
///
/// Why: kept as its own struct rather than a bare list so #4029's Sub-agents
/// configuration section — and #4030's domain-authority feed — can add fields
/// later without a breaking change, mirroring [`SkillsConfig`]'s rationale.
/// What: TWO independent lists, one per delegation mechanism, deliberately not
/// merged (their target vocabularies do not overlap — see
/// `api::server::agent_subagents`'s module doc):
/// - `allowed` — CROSS-PRODUCT (`dispatch_task` → trusty-code), intersected
///   with `tools::cross_product::NON_CODING_TARGETS`.
/// - `delegate_allowed` — IN-PROCESS (`delegate_to_agent` → trusty-agents),
///   ADR-0024 decision 4's editable whitelist, intersected with
///   `agents::delegation::ASSISTANT_REACHABLE_SUBAGENTS`.
///
/// For BOTH, `None` (missing key) and `Some(vec![])` resolve to an EMPTY
/// allow-set; the distinction is deliberately NOT load-bearing because neither
/// may grant anything (unlike `[skills]`, where absence leaves `[tools].allow`
/// as the sole source). No glob dialect — exact names only.
/// Test: `subagents_config_defaults_empty`, `subagents_config_parses_allowed`,
/// `subagents_config_parses_delegate_allowed`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SubagentsConfig {
    #[serde(default)]
    pub allowed: Option<Vec<String>>,

    /// ADR-0024 decision 4 (RATIFIED 2026-07-29) — the editable whitelist of
    /// IN-PROCESS sub-agent names this agent may reach via `delegate_to_agent`.
    ///
    /// Why: a separate key rather than a reinterpretation of `allowed` because
    /// the two mechanisms take different names for different things
    /// (`research` is a trusty-code specialist; `research-agent` is a
    /// trusty-agents roster entry), and flattening them would make one config
    /// line mean two capabilities. The ADR anticipated the collision and
    /// recorded the alternative (rename the cross-product section, freeing
    /// `[subagents]` for the in-process list); that rename is a separate,
    /// unratified decision, so this change takes the additive key and leaves
    /// the rename to it.
    /// What: exact agent names. Absent = EMPTY = this agent delegates to
    /// NOTHING in-process (decision 4 sub-answer (a), fail-closed); the
    /// bundled assistant personas ship a SEEDED default so that fail-closed
    /// posture is not a silent capability loss on rollout.
    /// Test: `subagents_config_parses_delegate_allowed`,
    /// `bundled_assistant_personas_seed_the_reachable_subagent_whitelist`.
    #[serde(default)]
    pub delegate_allowed: Option<Vec<String>>,

    /// The CONFIGURED default execution style for this agent's coding
    /// delegations (#4350; spec DOC-62 §5.2/§5.3, middle precedence level).
    ///
    /// Why: DOC-62 §5.2 records that no style/rigor field exists anywhere today
    /// and that #4350 introduces one. It lives beside the other cross-product
    /// keys because it configures the SAME surface — what `dispatch_task` hands
    /// to an external target — and an additive key avoids reinterpreting either
    /// existing list. Absent is the safe posture: it falls through to the
    /// built-in `engineer`, which is exactly today's behaviour, so no agent
    /// silently loses ceremony by not being updated.
    /// What: `hack` / `vibe` / `engineer`; anything else is a config parse
    /// error, never a silent default. A per-delegation value from the caller
    /// outranks this, and the target lane's own floor outranks both — this key
    /// can lower ceremony no more than a caller can.
    /// Test: `subagents_config_parses_default_style`,
    /// `subagents_config_rejects_an_unknown_default_style`.
    #[serde(default)]
    pub default_style: Option<crate::tools::execution_style::ExecutionStyle>,
}

/// Optional `[skills]` section in agent TOML (#3933).
///
/// Why: Kept as its own struct rather than a bare `Vec` so the section can gain
/// `deny`/`kind` filters later without a breaking config change.
/// What: `allow` is a list of skill ids. `None` (missing section) and
/// `Some(vec![])` are deliberately different: the first means "this agent does
/// not use skill grants", the second means "this agent grants no skills", and
/// only the first leaves `[tools].allow` untouched as the sole source.
/// Test: `skills_config_parses_allow`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SkillsConfig {
    #[serde(default)]
    pub allow: Option<Vec<String>>,
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

    /// Arbitrary trusty-search indexes attached to this agent (#3232, epic
    /// #4007's tier-2 mechanism).
    ///
    /// Why: `[[stores]]` is the CURATED tier — exactly one OKG store per
    /// agent by the 2026-07-24 owner decision, and that ceiling stays. The
    /// multiplicity an agent needs ("also let me search the APEX repo")
    /// belongs to a second, uncurated tier: a plain list of trusty-search
    /// `index_id`s over any corpus, with no OKG tree, palace, or ingestion
    /// requirement behind them. Declaring them in config (rather than
    /// leaving the model to guess an id that happens to exist) is what makes
    /// them discoverable — `vector_search`'s schema enumerates this list
    /// (#4009) so the model sees real, queryable options.
    /// What: Optional list of index ids (`["apex", "cto-projects"]`).
    /// `None` (missing section/field) means "no attached indexes" and is the
    /// pre-#3232 state exactly. Unioned base-first through `extends` — like
    /// `allow`/`scopes`, an overlay ADDS reach and never silently removes
    /// what its base attached. Use [`Self::resolved_search_indexes`] rather
    /// than reading the field directly, so blank ids and duplicates from a
    /// hand-edited file are normalized once.
    /// Test: `tools_config_parses_search_indexes`,
    /// `extends_unions_search_indexes`,
    /// `resolved_search_indexes_normalizes_blanks_and_dupes`.
    #[serde(default)]
    pub search_indexes: Option<Vec<String>>,

    /// Opt-in allowlist enforcement for `vector_search`'s `index_id` (#4009).
    ///
    /// Why: Owner decision on epic #4007's OQ-2 — enforcement is OPT-IN with
    /// declarative-only as the default at M1. Today `vector_search` forwards
    /// ANY `index_id` string to the daemon unchecked; flipping that to
    /// fail-closed by default would silently break every persona that
    /// currently cross-queries an index it never declared. So the default is
    /// `false` = today's behaviour, byte-identical; an operator who wants a
    /// hard boundary sets `[tools] enforce_search_indexes = true` and gets a
    /// fail-closed allowlist of `{bound OKG index} ∪ search_indexes`.
    /// Schema enrichment (#4009's other half) ships regardless of this flag.
    /// What: `Option<bool>`, read through [`Self::search_indexes_enforced`].
    /// `None`/`Some(false)` = declarative-only. Through `extends`, a child
    /// that declares the key wins; a child that omits it inherits the base's
    /// posture (so a hardened base stays hardened under an overlay).
    /// Test: `tools_config_parses_enforce_search_indexes`,
    /// `extends_child_overrides_enforce_search_indexes`,
    /// `vector_search_default_is_unenforced`.
    #[serde(default)]
    pub enforce_search_indexes: Option<bool>,
}

impl ToolsConfig {
    /// The agent's attached search indexes, normalized (#3232).
    ///
    /// Why: The field is hand-editable TOML and is produced by a union
    /// merge, so it can carry blanks (`""`, `"  "`) and — across a deep
    /// `extends` chain re-declaring the same id — duplicates. Normalizing at
    /// the single read point means neither the tool schema nor the API
    /// response has to re-derive the rule.
    /// What: Trimmed, blank-dropped, first-occurrence-wins dedup, declaration
    /// order preserved. Empty vec when the field is absent.
    /// Test: `resolved_search_indexes_normalizes_blanks_and_dupes`.
    pub fn resolved_search_indexes(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for id in self.search_indexes.iter().flatten() {
            let id = id.trim();
            if id.is_empty() || out.iter().any(|e| e == id) {
                continue;
            }
            out.push(id.to_string());
        }
        out
    }

    /// Whether `vector_search` should fail closed on an undeclared index id
    /// (#4009). Absent = `false` = today's behaviour.
    ///
    /// Test: `tools_config_parses_enforce_search_indexes`.
    pub fn search_indexes_enforced(&self) -> bool {
        self.enforce_search_indexes.unwrap_or(false)
    }

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

/// Default value for `AgentInfo::kind` — see that field's doc comment.
fn default_agent_kind() -> String {
    "assistant".to_string()
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

    /// Whether this agent is hidden from PICKER/roster listings (#3819).
    ///
    /// Why: `role` is dual-purpose — beyond labeling an agent, it also gates
    /// the tool-registry security posture (`runtime::tool_registry::build_registry_for_agent`).
    /// Hiding an agent from the GUI/REPL picker by repurposing `role` would
    /// therefore silently change its tool permissions too — not a safe
    /// mechanism. `hidden` is a dedicated, orthogonal flag: it affects ONLY
    /// listing surfaces (the REPL's `list_assistant_agents_into`, the GUI's
    /// `GET /api/agents` roster), never dispatch, tool gating, or direct
    /// by-name access (`GET/PATCH /api/agents/:name` still work for a hidden
    /// agent — mirrors the existing precedent that a role-based listing
    /// filter narrows the picker's SELECTABLE set, not what's reachable by
    /// name, established for `ctrl`/Concierge).
    /// What: Defaults to `false` (visible) so existing agent TOMLs are
    /// unaffected. An operator sets `hidden = true` in `[agent]` to keep an
    /// agent dispatchable but out of picker UIs (e.g. trimming the demo
    /// roster to Izzie/CTO Assistant/Concierge without touching
    /// `assistant`/`personal-assistant`/`research-agent`'s role or tools).
    /// Test: `parse_agent_toml_hidden_defaults_false_and_parses_true`.
    #[serde(default)]
    pub hidden: bool,

    /// Picker TYPE grouping label (#3819 — Bob's roster-typing directive).
    ///
    /// Why: The GUI agent picker groups entries into labeled sections
    /// ("System Tool", "Assistants") — presentation/typing metadata, kept
    /// deliberately SEPARATE from `role` (which gates the tool-registry
    /// security posture, `runtime::tool_registry::build_registry_for_agent`)
    /// and from `hidden` (which gates picker VISIBILITY, not grouping).
    /// Conflating any of the three would silently change dispatch/security
    /// behavior as a side effect of a display change.
    /// What: A free-form string, default `"assistant"` (so every existing
    /// agent TOML — including ones that will never opt into a group split —
    /// keeps working unchanged). This slice's only other declared value is
    /// `"system-tool"` (set on `ctrl`/Concierge). Not an enum: new picker
    /// sections are meant to be addable by dropping a new `kind` string into
    /// an agent TOML, not by a Rust code change every time.
    /// Test: `parse_agent_toml_kind_defaults_assistant_and_parses_system_tool`.
    #[serde(default = "default_agent_kind")]
    pub kind: String,

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

    /// Name of a base agent this agent inherits from (`extends:`), DOC-41 §2.5
    /// / epic #3052, issue #3055.
    ///
    /// Why: trusty-agents lets a user personalize a stock/bundled base agent
    /// (name it, add tools, override tone) without forking it — they author a
    /// child agent declaring `extends: <base>` and layer personal deltas on
    /// top. Resolution mirrors trusty-mpm's proven `compose_agent`: single
    /// parent, walked base-first at load time (`AgentRegistry::load` /
    /// `AgentConfig::by_name`), never re-resolved per dispatch.
    /// What: When `Some(base)`, the loader resolves `base` (case-insensitively)
    /// and merges it under this agent per the §2.5 merge table — scalars
    /// child-overrides, list fields union, prose base-first concatenation. The
    /// field is consumed during resolution and cleared to `None` on the
    /// flattened result so a resolved config is self-contained. `extends`
    /// itself is never inherited through the chain.
    /// Test: `agents::extends::tests` — see `extends_*` unit tests.
    #[serde(default)]
    pub extends: Option<String>,

    /// Privilege tier declaration (#4168, epic #4167 — L0/L1 orchestration
    /// model), raw as parsed from TOML. Use [`AgentInfo::tier`] rather than
    /// reading this field directly.
    ///
    /// Why: The owner's tiered persona model (#4167) needs a privilege
    /// boundary distinct from `role`. `role` is already triple-overloaded —
    /// it selects the per-agent tool-registry branch
    /// (`runtime::tool_registry::build_registry_for_agent`), gates
    /// `delegate_to_agent`'s target allowlist
    /// (`ASSISTANT_ALLOWED_DELEGATE_ROLES`), and drives capability matching
    /// — and `hidden`'s own doc comment above already names the hazard of
    /// adding a FOURTH meaning to it: "repurposing role would silently
    /// change tool permissions too — not a safe mechanism." A dedicated
    /// field keeps this SPECIFIC boundary (L0 orchestration-tier PM-grants
    /// vs. L1 standard black-box posture) explicit, grep-able, and
    /// independent of any future `role` value — an operator adding a new
    /// specialist `role` can never accidentally grant it orchestration-tier
    /// delegation reach as a side effect.
    /// What: Raw TOML string (`"l0"` / `"orchestration"` for L0,
    /// `"l1"` / `"standard"` for L1, case-insensitive) — an OPTIONAL
    /// OVERRIDE, not the primary source. Resolved by [`AgentInfo::tier`],
    /// which since ADR-0024 decision 3 DERIVES the tier from the agent's
    /// KIND ([`AgentTier::for_kind`]) whenever this field is absent or
    /// blank, and honours an explicit declaration otherwise. A declaration
    /// still FAILS CLOSED: an unrecognized string resolves to
    /// [`AgentTier::L1Standard`], never [`AgentTier::L0Orchestration`], so a
    /// typo can only ever narrow. This is a privilege boundary, not a
    /// convenience default: every call site that needs to know an agent's
    /// tier MUST call `AgentInfo::tier()`, never match on this raw field.
    /// Test: `agent_tier_defaults_to_l1_when_absent`,
    /// `agent_tier_parses_l0_orchestration_aliases`,
    /// `agent_tier_parses_l1_standard_aliases`,
    /// `agent_tier_unknown_value_fails_closed_to_l1`,
    /// `agent_tier_blank_value_fails_closed_to_l1`,
    /// `agent_tier_derives_l0_for_the_assistant_kind`,
    /// `agent_tier_explicit_declaration_overrides_the_derived_kind`.
    #[serde(default)]
    pub tier: Option<String>,
}

/// Persona privilege tier (#4168, epic #4167 — L0/L1 orchestration model).
///
/// Why: The owner decision (2026-07-27) splits personas into two tiers: L0
/// ("orchestration assistant") carries PM-tier grants (GitHub/PR/CI,
/// session-state read, cross-project git, shell/build/test execution,
/// delegation) under a YOLO risk posture the owner explicitly accepts —
/// deliberately NOT sandboxed, but it must never ingest untrusted content
/// (no Gmail/Drive/Calendar surface). L1 ("standard assistant") is today's
/// `assistant`/`cto-assistant`/`izzie`: the documented black-box posture,
/// which DOES ingest untrusted content and therefore must never be able to
/// acquire L0's grants — including via delegation (#4169, epic #4167).
/// This PR defines the tier and its one-directional delegation boundary
/// ONLY; it grants L0 no actual capabilities (those are #4170-#4173) and
/// creates no L0 persona instance.
///
/// ADR-0024 decision 3 (ratified by the owner 2026-07-28) INVERTS that
/// population assignment: every assistant IS L0, and L1 is the sub-agent
/// population (`engineer`, `qa`, `documentation`, `ops`, `planner`,
/// `researcher`, …). The paragraph above is retained verbatim as the record
/// of what #4168 shipped; read it as history, not as the current population.
/// The tier is now DERIVED from kind by [`AgentTier::for_kind`] — see
/// [`AgentInfo::tier`] for why that replaced a per-file `tier = "l0"`
/// literal on every assistant TOML.
/// What: Two variants. `L1Standard` is `#[default]` — every accessor that
/// falls back to `Default::default()` (or an explicit fail-closed match
/// arm) lands on the restricted tier, never the elevated one.
/// Test: See `AgentInfo::tier`'s Test pointer for parsing; delegation-gate
/// behavior is covered by `crate::tools::delegate` tests (search
/// `l1_to_l0` / `l0_to_l1` / `tier`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentTier {
    /// L1 — standard assistant tier. Black-box posture; may ingest
    /// untrusted content (Gmail/Drive/web); MUST NOT reach L0 via
    /// delegation. The fail-closed default.
    #[default]
    L1Standard,
    /// L0 — orchestration tier. PM-tier grants; YOLO risk posture
    /// explicitly accepted by the owner; deliberately not sandboxed but
    /// must never ingest untrusted content. Only reachable by an EXPLICIT
    /// `tier = "l0"` / `tier = "orchestration"` declaration — never a
    /// default, never inferred from `role`.
    L0Orchestration,
}

impl AgentTier {
    /// Resolve a RAW declared tier string, fail-closed (#4168; extracted #4029).
    ///
    /// Why: [`AgentInfo::tier`] was the only place this normalization lived,
    /// which meant any consumer holding a declared tier string WITHOUT a full
    /// [`AgentInfo`] — `GET /api/agents/:name/subagents` (#4029) reporting the
    /// delegation targets the #4169 gate would actually admit is the first —
    /// had to hand-roll the match and could silently drift from the gate. A
    /// privilege boundary must have exactly one parser: if this function and
    /// the gate ever disagreed, the config surface would advertise a target
    /// the gate refuses (or hide one it allows), which is the whole failure
    /// class #4029 exists to avoid. `AgentInfo::tier()` now delegates here, so
    /// there is still only one match in the codebase.
    /// What: Case-insensitive, trimmed match against `"l0"`/`"orchestration"`
    /// (-> [`AgentTier::L0Orchestration`]); EVERYTHING else — `None`, blank, or
    /// an unrecognized string — resolves to [`AgentTier::L1Standard`]. Byte-for
    /// byte the behavior `AgentInfo::tier()` shipped in #4200, which its own
    /// unchanged tests still pin.
    /// Test: `agent_tier_defaults_to_l1_when_absent`,
    /// `agent_tier_parses_l0_orchestration_aliases`,
    /// `agent_tier_parses_l1_standard_aliases`,
    /// `agent_tier_unknown_value_fails_closed_to_l1`,
    /// `agent_tier_blank_value_fails_closed_to_l1`,
    /// `agent_tier_from_declared_matches_agent_info_tier`.
    pub fn from_declared(raw: Option<&str>) -> Self {
        match raw.map(str::trim) {
            Some(s) if s.eq_ignore_ascii_case("l0") || s.eq_ignore_ascii_case("orchestration") => {
                AgentTier::L0Orchestration
            }
            _ => AgentTier::L1Standard,
        }
    }

    /// The tier every agent of this KIND holds (ADR-0024 decision 3).
    ///
    /// Why: ADR-0024 decision 3 — "assistants are a level 0 agent, like a PM,
    /// that can delegate to sub-agents and communicate with each other" —
    /// makes tier a FUNCTION of kind, not an independent per-file fact. The
    /// two shapes available were a `tier = "l0"` literal in every bundled
    /// assistant TOML, or this derivation. The literal loses: `izzie`,
    /// `cto-assistant` and `ctrl` each ship TWO resolvable forms (a directory
    /// package AND a flat `extends`-shadow-fallback TOML), so the literal
    /// would have to be written six-plus times and kept in sync forever, and
    /// a future assistant persona that forgot it would resolve L1 — silently
    /// decorrelating the DATA from the model, which is the exact failure
    /// class ADR-0024's "Why this class of error recurs" section is about.
    /// Deriving cannot drift: there is no second place to forget.
    ///
    /// This narrows #4168's "never inferred from `role`" rule rather than
    /// discarding it, and the property that rule protected is preserved: an
    /// operator adding a NEW SPECIALIST role still lands on `L1Standard`,
    /// because the derivation recognizes exactly one value — the assistant
    /// kind — and nothing else. `role == "assistant"` is already the most
    /// privileged non-orchestrator role in the crate (it is the ONLY role
    /// `build_registry_for_agent` routes into `build_assistant_tier_registry`,
    /// the one branch that registers `delegate_to_agent` and the git tool
    /// surface at all), so deriving L0 from it restates a trust decision the
    /// codebase already makes rather than creating a new escalation path.
    /// #4497: the ORCHESTRATOR kind (`pm`, and a `ctrl` declaring
    /// `role = "controller"`) derives `L0Orchestration` here too, so it is
    /// the same ONE rule that answers "is this agent L0?" for both
    /// populations. It previously reached that tier by a hardcoded literal at
    /// a single dispatch call site (`ctrl_delegate_posture`) while assistants
    /// reached it by this derivation — two mechanisms for one fact, which
    /// meant `AgentInfo::tier()` and the dispatch posture disagreed about
    /// `pm` and a new call site could pick up either answer. The two kinds
    /// stay DISJOINT (`delegation::is_orchestrator_kind` is a named
    /// two-element list, not "not an assistant"), so an orchestrator gains
    /// only the tier — never the assistant-only peer prohibition, never the
    /// assistant-only role allowlist. Nor is it a capability grant: the L0
    /// surfaces (`l0_shell_exec`, the `gh_*` tools, the session-state tools,
    /// cross-project git scoping) are all registered behind a persona's own
    /// `[tools].allow`, and `pm.toml` declares none — see
    /// `bundled_agent_tier_table_is_pinned` and
    /// `bundled_assistant_personas_resolve_l0_and_gain_nothing`, which assert
    /// exactly that.
    ///
    /// What: [`AgentTier::L0Orchestration`] for the assistant kind (via
    /// `delegation::is_assistant_kind`, the ONE definition of that predicate —
    /// the same function `DelegateToAgentTool::execute` and the `/subagents`
    /// route call) and for the orchestrator kind (via
    /// `delegation::is_orchestrator_kind`); [`AgentTier::L1Standard`] for
    /// every other role, including the empty string and every specialist
    /// (sub-agent) role.
    /// Test: `agent_tier_for_kind_is_l0_for_the_assistant_and_orchestrator_kinds`,
    /// `agent_tier_derives_l0_for_the_assistant_kind`,
    /// `agent_tier_derives_l1_for_every_sub_agent_kind`,
    /// `bundled_agent_tier_table_is_pinned`.
    pub fn for_kind(role: &str) -> Self {
        if crate::agents::delegation::is_assistant_kind(role)
            || crate::agents::delegation::is_orchestrator_kind(role)
        {
            AgentTier::L0Orchestration
        } else {
            AgentTier::L1Standard
        }
    }

    /// The canonical lowercase wire label for this tier (`"l0"` / `"l1"`).
    ///
    /// Why: `GET /api/agents/:name/subagents` (#4029) reports both the
    /// delegator's and each target's resolved tier, and a JSON consumer must
    /// not have to parse `Debug` output (`"L0Orchestration"`) or re-derive the
    /// alias set. Returning the SHORT alias — not the long one — keeps the wire
    /// value identical to what an operator writes in `tier = "l0"`.
    /// What: `"l0"` for [`AgentTier::L0Orchestration`], `"l1"` for
    /// [`AgentTier::L1Standard`]. Round-trips through [`Self::from_declared`].
    /// Test: `agent_tier_wire_label_round_trips_through_from_declared`.
    pub fn wire_label(self) -> &'static str {
        match self {
            AgentTier::L0Orchestration => "l0",
            AgentTier::L1Standard => "l1",
        }
    }
}

impl AgentInfo {
    /// Resolve this agent's privilege tier: declared wins, else derived from
    /// KIND (#4168; ADR-0024 decision 3).
    ///
    /// Why: The single resolution point for an agent's tier — see
    /// [`Self::tier`]'s field doc and [`AgentTier::for_kind`] for the full
    /// rationale. Every consumer (the delegation gate's defense-in-depth
    /// layer, the `/subagents` route, the L0-only session-state surface) MUST
    /// call this rather than matching on the raw field, so "what counts as L0"
    /// can never drift between call sites.
    ///
    /// ADR-0024 decision 3: assistant-kind agents ARE L0. That is expressed
    /// here as a derivation rather than as a `tier = "l0"` literal in each
    /// bundled assistant TOML because there is no single such file — `izzie`,
    /// `cto-assistant` and `ctrl` each ship a directory package AND a flat
    /// fallback, and a literal that must be repeated is a literal that will
    /// eventually disagree with itself. See `AgentTier::for_kind`.
    /// What: An explicit, non-blank `[agent].tier` declaration WINS and is
    /// parsed by [`AgentTier::from_declared`] — case-insensitive, trimmed,
    /// fail-closed (an unrecognized value resolves to
    /// [`AgentTier::L1Standard`], so a typo can only narrow, never elevate).
    /// An ABSENT or blank declaration derives from `role` via
    /// [`AgentTier::for_kind`]: the assistant kind and (#4497) the
    /// orchestrator kind resolve [`AgentTier::L0Orchestration`], every other
    /// role [`AgentTier::L1Standard`]. The explicit branch is retained so an
    /// operator can still pin a genuinely-L0 persona of some other kind, or
    /// deliberately narrow one assistant back to L1 — both are declared
    /// intent, never an accident of omission.
    /// Test: `agent_tier_defaults_to_l1_when_absent`,
    /// `agent_tier_parses_l0_orchestration_aliases`,
    /// `agent_tier_parses_l1_standard_aliases`,
    /// `agent_tier_unknown_value_fails_closed_to_l1`,
    /// `agent_tier_blank_value_fails_closed_to_l1`,
    /// `agent_tier_derives_l0_for_the_assistant_kind`,
    /// `agent_tier_derives_l1_for_every_sub_agent_kind`,
    /// `agent_tier_explicit_declaration_overrides_the_derived_kind`,
    /// `bundled_assistant_personas_resolve_l0_and_gain_nothing`,
    /// `bundled_agent_tier_table_is_pinned`.
    ///
    /// #4029: the DECLARED-string match lives in [`AgentTier::from_declared`]
    /// so a consumer holding only a raw tier string shares one parser instead
    /// of hand-rolling a second one. That function is deliberately left as a
    /// pure parser of the raw value — it has no `role` to consult and must not
    /// grow one; the kind derivation lives here, where the role is in scope.
    pub fn tier(&self) -> AgentTier {
        match self.tier.as_deref().map(str::trim) {
            Some(declared) if !declared.is_empty() => AgentTier::from_declared(Some(declared)),
            // ADR-0024 decision 3: an assistant IS tier L0, derived from kind
            // so the fact lives in one place instead of in every persona file.
            _ => AgentTier::for_kind(&self.role),
        }
    }

    /// The human-facing speaker label for this persona (#3738).
    ///
    /// Why: The chat surface (GUI per-message speaker attribution, the REPL
    /// `/switch` "Switched to:" line, the `GET /api/agents` catalog) all need
    /// the SAME "Assistant" / "Izzie" / "CTO Bot" string, resolved one way:
    /// the persona's declared `display_name`, falling back to its stable
    /// `name` id when a persona (e.g. a worker agent) declares none. Centralizing
    /// this here is the single canonical accessor the owner decision asked for,
    /// so the backend and every front end stay in agreement without each
    /// re-deriving the mapping (which is how "CTO Assistant" vs "CTO Bot" drift
    /// creeps in).
    /// What: Returns `display_name` when set, else `name`.
    /// Test: `bundled_personas_expose_expected_display_names` (prefers
    /// display_name), `display_label_falls_back_to_name_when_unset`.
    pub fn display_label(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.name)
    }
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
