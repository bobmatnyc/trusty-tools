//! On-disk config sub-section types for `~/.trusty-agents/config.toml`.
//!
//! Why: `GlobalConfig` composes several independent sections (MCP registry,
//! local-inference fast-path, git tools). Defining each section's shape +
//! defaults here keeps `mod.rs` focused on load/save/render behavior.
//! What: `LocalInferenceConfig`, `GitConfig`, `McpSection` (MCP POLICY only
//! since #7454 — the servers live in `trusty_mcp`'s shared file),
//! `ProvidersSection`, plus the `serde` default helpers they share.
//! Test: Defaults + round-trips exercised in `config::tests`.

use serde::{Deserialize, Serialize};

/// Default agent roles that receive MCP tool descriptions in their prompt.
pub(super) fn default_inject_roles() -> Vec<String> {
    vec![
        "ctrl".to_string(),
        "pm".to_string(),
        "research".to_string(),
        "observe".to_string(),
    ]
}

pub(super) fn default_true() -> bool {
    true
}

/// Local Ollama fast-path configuration (#319).
///
/// Why: Frequent ctrl turns (TM status, project lists, "what can you help me
/// with") don't need a remote LLM round-trip. Routing them to a locally-running
/// Ollama instance gives sub-second feedback and zero token cost — but only
/// when the user opts in (the user might not have ollama installed, or might
/// prefer the remote model for everything). This struct captures the knobs.
/// What: `enabled` gates the entire fast-path. `model` is the ollama-prefixed
/// model id. `fallback_on_error` controls whether a local failure retries
/// remotely. `ollama_host` overrides the default localhost URL.
/// `max_tokens` caps the local response (small for speed).
/// Test: Defaults exercised via `LocalInferenceConfig::default`;
/// `local_inference_enabled_defaults_true_when_key_omitted` and
/// `local_inference_explicit_false_still_wins` pin the `enabled` serde default
/// to the documented value.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LocalInferenceConfig {
    /// Enable local Ollama fast-path (default: true — enabled by default, #345).
    // audit 2026-08-19: was `#[serde(default)]`, which resolves to `false` and
    // silently disabled the fast-path for any section omitting the key.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Ollama model id, in `ollama/<name>` form (default: ollama/qwen3:30b).
    #[serde(default = "default_local_model")]
    pub model: String,
    /// Fall back to remote if local call fails (default: true).
    #[serde(default = "default_true")]
    pub fallback_on_error: bool,
    /// Ollama base URL (default: http://localhost:11434).
    #[serde(default = "default_ollama_host")]
    pub ollama_host: String,
    /// Max tokens for local inference (kept small for snappy local replies).
    #[serde(default = "default_local_max_tokens")]
    pub max_tokens: u32,
}

fn default_local_model() -> String {
    "ollama/qwen3:30b".to_string()
}

fn default_ollama_host() -> String {
    "http://localhost:11434".to_string()
}

fn default_local_max_tokens() -> u32 {
    2048
}

impl Default for LocalInferenceConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            model: default_local_model(),
            fallback_on_error: true,
            ollama_host: default_ollama_host(),
            max_tokens: default_local_max_tokens(),
        }
    }
}

/// Git tool configuration section (#247).
///
/// Why: Native git tools are powerful and can mutate the working tree;
/// this struct gives operators a single place to scope which roles see
/// them and whether write operations require user confirmation.
/// What: `available_for_roles` is the inject-set; `confirm_writes` toggles
/// pre-commit/pre-push prompts (currently advisory — wired into the
/// future ctrl confirmation UI); `default_branch` is the branch tools
/// should treat as the trunk for advisory checks.
/// Test: Round-trip parsing covered by the existing config tests once
/// `[git]` is present in `DEFAULT_CONFIG_TOML`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GitConfig {
    /// Agent roles that get git tools available.
    #[serde(default = "default_git_roles")]
    pub available_for_roles: Vec<String>,
    /// Require user confirmation before write operations (commit, push).
    #[serde(default)]
    pub confirm_writes: bool,
    /// Branch name to treat as the trunk for advisory checks.
    #[serde(default = "default_git_branch")]
    pub default_branch: String,
}

fn default_git_roles() -> Vec<String> {
    vec![
        "ctrl".to_string(),
        "pm".to_string(),
        "research".to_string(),
        "observe".to_string(),
    ]
}

fn default_git_branch() -> String {
    "main".to_string()
}

impl Default for GitConfig {
    fn default() -> Self {
        Self {
            available_for_roles: default_git_roles(),
            confirm_writes: false,
            default_branch: default_git_branch(),
        }
    }
}

/// `[mcp]` section — this crate's MCP POLICY, not its server list (#7454).
///
/// Why: the servers themselves moved to the file shared with `trusty-code`
/// (ADR-0060, [`crate::mcp::shared`]), so what remains here is the policy this
/// crate applies to them: which agent roles get the MCP prompt layer, and
/// whether a project's repo-controlled `.mcp.json` may contribute servers at
/// all. Keeping the section rather than deleting it means every existing
/// `config.toml` still parses, and `trust_project_mcp_json` — a security gate
/// with a deliberate `false` default — keeps its documented spelling.
/// What: `inject_for_roles` gates the prompt layer. A `[[mcp.services]]` array
/// left over from before #7454 is IGNORED by this struct and drained exactly
/// once by [`crate::mcp::shared::migrate`].
/// Test: `crate::mcp::tests::migrate_tests::migrates_once_and_never_again`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McpSection {
    #[serde(default = "default_inject_roles")]
    pub inject_for_roles: Vec<String>,
    /// Trust gate for auto-spawning servers discovered from a project's
    /// `.mcp.json` (security-critical, see #3266 BLOCK remediation).
    ///
    /// Why: A `.mcp.json` file is project-controlled, not operator-curated —
    /// cloning a hostile repo that ships a malicious `.mcp.json` would
    /// otherwise cause trusty-agents to auto-spawn arbitrary binaries on
    /// every persona turn with zero gating. Defaulting this to `false`
    /// means the effective server set only ever comes from the shared,
    /// operator-curated file and this assistant's own `[mcp]` overrides
    /// unless the operator explicitly opts a project in.
    /// What: When `false` (the default),
    /// [`crate::mcp::shared::merge_mcp_json`] contributes nothing. When
    /// `true`, `.mcp.json` entries are merged in BEHIND the shared file, so a
    /// shared-file entry still wins a name collision.
    /// Test: `crate::mcp::tests::global_tests::untrusted_mcp_json_contributes_nothing`,
    /// `crate::mcp::tests::global_tests::mcp_json_entries_are_marked_and_ranked_last`.
    #[serde(default)]
    pub trust_project_mcp_json: bool,
}

impl Default for McpSection {
    fn default() -> Self {
        Self {
            inject_for_roles: default_inject_roles(),
            trust_project_mcp_json: false,
        }
    }
}

/// `[providers]` section (#3766) — the default inference provider for agents
/// that pin none of their own.
///
/// Why: the bundled agent templates ship bare model slugs
/// (`model = "claude-sonnet-4-6"`) and no `[agent].provider_id`, so which
/// provider served them was decided by whichever ambient credential the
/// harness happened to find first. The owner's ruling on #3766 puts that
/// choice in configuration. It is a field on [`super::GlobalConfig`] rather
/// than a second parser over the same file because `GlobalConfig::save()`
/// re-serializes only its declared fields: an unmodelled `[providers]` table
/// would be erased from disk by any unrelated write — `/local on`
/// (`repl::commands::routing`) and the four `mcp_*` mutators all
/// load-mutate-save.
/// What: `default_provider_id` names a provider from
/// `trusty_common::inference::registry`. Absent (the default) means UNSET and
/// preserves the ambient behaviour exactly. The precedence rules live in
/// [`crate::llm::provider_policy`], which reads this field.
/// Test: `providers_section_survives_an_unrelated_save`,
/// `providers_section_defaults_to_unset`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct ProvidersSection {
    /// The provider that unpinned, provider-agnostic agents default to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_provider_id: Option<String>,
}
