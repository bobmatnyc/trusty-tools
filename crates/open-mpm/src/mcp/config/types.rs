//! On-disk config sub-section types for `~/.open-mpm/config.toml`.
//!
//! Why: `GlobalConfig` composes several independent sections (MCP registry,
//! local-inference fast-path, git tools). Defining each section's shape +
//! defaults here keeps `mod.rs` focused on load/save/render behavior.
//! What: `LocalInferenceConfig`, `GitConfig`, `McpSection`, `McpService`,
//! `McpTool`, plus the `serde` default helpers they share.
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
/// Test: Defaults exercised via `LocalInferenceConfig::default`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LocalInferenceConfig {
    /// Enable local Ollama fast-path (default: true — enabled by default, #345).
    #[serde(default)]
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

/// `[mcp]` section — the remote/service-tier MCP registry.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McpSection {
    #[serde(default = "default_inject_roles")]
    pub inject_for_roles: Vec<String>,
    #[serde(default)]
    pub services: Vec<McpService>,
}

impl Default for McpSection {
    fn default() -> Self {
        Self {
            inject_for_roles: default_inject_roles(),
            services: Vec::new(),
        }
    }
}

/// A single registered MCP service.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McpService {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// For HTTP-transport services, the endpoint URL. Stdio services leave
    /// this `None` and use `command` + `args` instead.
    #[serde(default)]
    pub url: Option<String>,
    /// "stdio" | "http". Currently only `stdio` is implemented in `kuzu.rs`.
    pub transport: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub tools: Vec<McpTool>,
}

/// A single tool advertised by an MCP service.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McpTool {
    pub name: String,
    pub description: String,
}
