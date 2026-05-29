//! LLM sampling parameters and optional per-agent tuning blocks.
//!
//! Why: The `[llm]`, `[runner_config]`, `[plugins]`, `[rbac]`, `[compress]`,
//! and `[session]` TOML sections are independent tunables that evolve on their
//! own cadence; grouping them here keeps the core `AgentConfig` identity shape
//! (in `config.rs`) readable and under the file-size cap.
//! What: Defines `LlmParams`, `ToolChoice`, `RunnerConfig`, `AgentPluginsConfig`,
//! `RbacConfig`, `AgentCompressConfig`, and `SessionCompressionConfig` plus
//! their serde defaults and small helper impls.
//! Test: Round-trip parsing is exercised by the unit tests in `tests.rs`.

use serde::Deserialize;

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
