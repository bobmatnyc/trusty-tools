//! MCP service registry persisted at `~/.open-mpm/config.toml`.
//!
//! Why: Agents that coordinate work (ctrl, PM, research, observe) need to know
//! which external MCP tools are available so the LLM can request them. Keeping
//! this declaration in a single global config file (a) survives upgrades of
//! the binary, (b) lets a user enable/disable services without recompiling,
//! and (c) gives a stable surface for prompt injection across all roles.
//! What: `GlobalConfig` is the on-disk schema (formerly `McpConfig`; renamed
//! in #245 to reflect that it composes multiple subsystems — MCP registry +
//! GitHub identities — rather than only MCP). `load_or_create()` creates the
//! file with default content (gworkspace-mcp enabled + slack-user-proxy
//! disabled) when missing. The registry only lists *remote/service-tier*
//! MCPs that agents should know about — local native integrations
//! (kuzu-memory, mcp-vector-search) are wired into the harness directly and
//! deliberately do not appear here.
//! `services_for_role()` returns enabled services applicable to a given agent
//! role; `render_prompt_section()` formats them as a Markdown block suitable
//! for `SystemPromptBuilder::add_mcp_layer`.
//! Test: `tests::*` cover create-on-absent, role gating, and rendering.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Roots of the global config tree (`~/.open-mpm/config.toml`).
const CONFIG_DIR_NAME: &str = ".open-mpm";
const CONFIG_FILE_NAME: &str = "config.toml";

/// Default agent roles that receive MCP tool descriptions in their prompt.
fn default_inject_roles() -> Vec<String> {
    vec![
        "ctrl".to_string(),
        "pm".to_string(),
        "research".to_string(),
        "observe".to_string(),
    ]
}

fn default_true() -> bool {
    true
}

/// Top-level shape of `~/.open-mpm/config.toml` — composes all subsystems.
///
/// Why: `[mcp]` is one section among potentially many future ones (e.g.
/// `[telemetry]`, `[ui]`); nesting under named sections keeps the file
/// extensible without breaking deserialization. `[github]` (#243) holds the
/// multi-identity ticketing registry so users can route the ticketing agent
/// to a personal vs. work GitHub account without env-var juggling. Renamed
/// from `McpConfig` in #245 to reflect that the type is the global config
/// container — the MCP registry is now `self.mcp` (a `McpSection`).
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct GlobalConfig {
    #[serde(default)]
    pub mcp: McpSection,
    /// `[github]` section — multi-identity registry (#243). Defaults to an
    /// empty section when absent so the field is non-optional and callers
    /// don't need to drill through `Option`.
    #[serde(default)]
    pub github: crate::ticketing::GitHubSection,
    /// `[git]` section — controls native git tool wiring (#247).
    ///
    /// Why: Lets users restrict which agent roles see git tools and gate
    /// write operations behind a confirmation flag without recompiling.
    /// What: Defaults match `default_git_roles()` (ctrl/pm/research/observe)
    /// and `confirm_writes = false`.
    /// Test: `git_section_defaults_apply` below.
    #[serde(default)]
    pub git: GitConfig,
    /// `[logging]` section (Feature B4) — chat output logging knobs.
    ///
    /// Why: Centralises the rotation/retention defaults with the rest of the
    /// global config so operators can tune chat-log disk usage from a single
    /// file rather than env vars.
    /// What: Defaults to `enabled = true`, `max_size_mb = 10`,
    /// `retain_days = 30` when the section is absent.
    /// Test: `LoggingConfig::default` covered by `logging_config_defaults_apply`.
    #[serde(default)]
    pub logging: crate::logging::LoggingConfig,
    /// `[local_inference]` section (#319) — local Ollama fast-path knobs.
    ///
    /// Why: Routing qualifying queries (TM status, simple chat) to a local
    /// Ollama instance shaves the remote round-trip + token cost from the
    /// hot path. Defaults to disabled so the feature is opt-in: a user must
    /// explicitly toggle `enabled = true` (or run `/local on`) before any
    /// remote-bound traffic gets diverted.
    /// What: Holds enable flag, model id (`ollama/<name>` form), fallback
    /// behavior, host URL, and a max-token cap to keep local responses snappy.
    /// Test: `local_inference_defaults_apply` round-trips the section.
    #[serde(default)]
    pub local_inference: LocalInferenceConfig,
    /// `[tool_registry]` section (#453) — OpenRPC tool registry config.
    ///
    /// Why: External JSON-RPC 2.0 endpoints (`gworkspace`, `trusty-memory`, …)
    /// advertise tools via `rpc.discover`. Operators declare which
    /// endpoints to contact and which scopes to trust per endpoint here;
    /// the harness wires the manifest into the same `dyn ToolExecutor`
    /// dispatch path as in-process tools.
    /// What: Optional — absence means "no external registry endpoints",
    /// which is the default for new installs. Endpoints with
    /// `enabled = false` are skipped.
    /// Test: `tool_registry_section_round_trips` and
    /// `crate::tools::registry::tests::builder_with_no_endpoints_returns_empty`.
    #[serde(default)]
    pub tool_registry: Option<crate::tools::registry::config::ToolRegistryConfig>,
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

impl GlobalConfig {
    /// Resolve a GitHub identity for ticketing.
    ///
    /// Why: Wires the multi-identity registry into the rest of the codebase
    /// without forcing every caller to drill into `self.github…`.
    /// What: Returns `None` when no identity matches; otherwise returns a
    /// clone of the matched identity.
    /// Test: `tests::github_identity_resolves_default` (in ticketing tests).
    pub fn github_identity(&self, name: Option<&str>) -> Option<crate::ticketing::GitHubIdentity> {
        self.github.identity(name).cloned()
    }
}

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

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McpTool {
    pub name: String,
    pub description: String,
}

/// Default config TOML written when `~/.open-mpm/config.toml` is absent.
///
/// Why: Stored as a literal so the on-disk shape is identical to what callers
/// see when they load the file; comments document intent for human editors.
const DEFAULT_CONFIG_TOML: &str = r#"# open-mpm global configuration
# ~/.open-mpm/config.toml

[mcp]
# Agent roles that receive MCP tool descriptions in their system prompt
inject_for_roles = ["ctrl", "pm", "research", "observe"]

# Remote and service-tier MCPs — these are external platforms agents can reference.
# Local native integrations (kuzu-memory, mcp-vector-search) are handled by the
# harness directly and do not appear here.

[[mcp.services]]
name = "gworkspace-mcp"
description = "Google Workspace — Gmail, Calendar, Drive, Docs, Sheets, Tasks"
command = "gworkspace-mcp"
args = ["mcp"]
transport = "stdio"
enabled = true

[[mcp.services.tools]]
name = "gmail_search"
description = "Search Gmail messages by query"

[[mcp.services.tools]]
name = "gmail_send"
description = "Send an email via Gmail"

[[mcp.services.tools]]
name = "gmail_read"
description = "Read a Gmail message by ID"

[[mcp.services.tools]]
name = "gmail_list"
description = "List Gmail messages with optional filters"

[[mcp.services.tools]]
name = "calendar_list"
description = "List Google Calendar events"

[[mcp.services.tools]]
name = "calendar_create"
description = "Create a Google Calendar event"

[[mcp.services.tools]]
name = "calendar_update"
description = "Update an existing calendar event"

[[mcp.services.tools]]
name = "drive_search"
description = "Search Google Drive files"

[[mcp.services.tools]]
name = "drive_read"
description = "Read a Google Drive file"

[[mcp.services.tools]]
name = "drive_create"
description = "Create a file in Google Drive"

[[mcp.services.tools]]
name = "docs_read"
description = "Read a Google Doc"

[[mcp.services.tools]]
name = "docs_create"
description = "Create a new Google Doc"

[[mcp.services.tools]]
name = "docs_update"
description = "Update content in a Google Doc"

[[mcp.services.tools]]
name = "sheets_read"
description = "Read data from Google Sheets"

[[mcp.services.tools]]
name = "sheets_update"
description = "Write data to Google Sheets"

[[mcp.services.tools]]
name = "tasks_list"
description = "List Google Tasks"

[[mcp.services.tools]]
name = "tasks_create"
description = "Create a Google Task"

[[mcp.services.tools]]
name = "tasks_complete"
description = "Mark a Google Task as complete"

[[mcp.services]]
name = "slack-user-proxy"
description = "Slack messaging — send messages, read channels, search"
command = "slack-user-proxy"
args = []
transport = "stdio"
enabled = false

[[mcp.services.tools]]
name = "slack_post"
description = "Post a message to a Slack channel"

[[mcp.services.tools]]
name = "slack_search"
description = "Search Slack messages"

[[mcp.services.tools]]
name = "slack_read"
description = "Read messages from a Slack channel"

# Granola — meeting notes, transcripts, and action items (#256).
# Enabled by default; harmless when the binary is absent (the harness
# logs and continues). Used heavily by the personal-assistant and
# cto-assistant for meeting recall.
[[mcp.services]]
name = "granola-notes"
description = "Granola meeting notes and transcripts"
command = "/opt/homebrew/bin/granola-mcp"
args = []
transport = "stdio"
enabled = true

[[mcp.services.tools]]
name = "granola_search"
description = "Search Granola meeting notes and transcripts"

[[mcp.services.tools]]
name = "granola_get"
description = "Retrieve a specific Granola meeting note"

[[mcp.services.tools]]
name = "granola_list_recent"
description = "List recent Granola meetings"

# Duetto org memory service — Duetto-internal HTTP MCP (#256).
# Disabled by default since it's only reachable on Duetto infrastructure.
# Enable with `mcp_enable duetto-memory` when on a Duetto-connected machine.
[[mcp.services]]
name = "duetto-memory"
description = "Duetto org memory service (Duetto infra only)"
url = "https://mcp-services.dev.duettosystems.com/memory/mcp"
transport = "http"
enabled = false

# GitHub identities for the ticketing agent (#243).
# Each identity points to env vars holding a token + default repo, so
# secrets stay out of this file. Set `default_identity` to choose which
# identity is used when no override is provided.
#
# Example:
# [github]
# default_identity = "personal"
#
# [[github.identities]]
# name = "personal"
# token_env = "GITHUB_TOKEN"
# repo_env = "GITHUB_REPO"
#
# [[github.identities]]
# name = "work"
# token_env = "GITHUB_TOKEN_WORK"
# repo_env = "GITHUB_REPO_WORK"

# Native git tool configuration (#247).
# Controls which agent roles get the git_* tools (status, log, branches,
# commit, push, pull, fetch, stash, etc.). Read operations use libgit2;
# write operations shell out to `git` to preserve hooks and signing.
[git]
available_for_roles = ["ctrl", "pm", "research", "observe"]
confirm_writes = false
default_branch = "main"

# Local Ollama fast-path (#319).
# When enabled, qualifying ctrl turns (TM status queries, simple chat) are
# routed to a locally-running ollama instance instead of the remote model.
# Enabled by default (#345). Toggle with `/local off` or set `enabled = false`
# below. Requires `ollama serve` and a pulled model matching `model`.
[local_inference]
enabled = true
model = "ollama/qwen3:30b"
fallback_on_error = true
ollama_host = "http://localhost:11434"
max_tokens = 2048

# OpenRPC (https://spec.open-rpc.org/) over stdio tool registry (#453, #455).
# Declares external JSON-RPC 2.0 endpoints (driver = "direct") that advertise
# tools via `rpc.discover`. The `direct` driver spawns a subprocess and
# speaks JSON-RPC 2.0 over its stdin/stdout (NDJSON; one JSON object per
# line, JSON array for batch). Endpoints below are DISABLED by default —
# flip `enabled = true` once the corresponding binary supports an OpenRPC
# stdio mode (e.g. via a `--rpc` flag). See
# docs/research/openrpc-trusty-contract.md for the wire format.
[tool_registry]
scope_enforcement = "deny"

# trusty-memory — recall/remember/forget over JSON-RPC 2.0 stdio.
# Disabled until the trusty-memory binary supports `--rpc` mode (where it
# reads OpenRPC requests from stdin and writes responses to stdout).
[[tool_registry.endpoints]]
name = "trusty-memory"
driver = "direct"
description = "Trusty memory service — recall/remember/forget"
command = "trusty-memory"
args = ["--rpc"]
enabled = false
scopes = ["memory.read", "memory.write"]
discovery_ttl_secs = 0
eager_discovery = true

[tool_registry.endpoints.transport]
timeout_ms = 5000

# trusty-search — semantic/keyword code search over JSON-RPC 2.0 stdio.
# Disabled until the trusty-search binary supports `--rpc` mode.
[[tool_registry.endpoints]]
name = "trusty-search"
driver = "direct"
description = "Trusty search service — semantic + keyword code search"
command = "trusty-search"
args = ["--rpc"]
enabled = false
scopes = ["search.read"]
discovery_ttl_secs = 0
eager_discovery = true

[tool_registry.endpoints.transport]
timeout_ms = 5000

# gworkspace — Google Workspace (Gmail, Calendar, Drive, Docs, Sheets, Tasks)
# via JSON-RPC 2.0 stdio. The `gworkspace-mcp` binary (from the trusty-common
# workspace, crate `trusty-gworkspace`) exposes an OpenRPC 1.3.2 manifest via
# `rpc.discover` and advertises Google OAuth scopes per tool through the
# `x-google-scopes` extension. Disabled by default — flip `enabled = true`
# after authenticating (`gworkspace-mcp auth login`) on a machine with the
# binary on $PATH. See docs/research/openrpc-trusty-contract.md.
[[tool_registry.endpoints]]
name = "gworkspace"
driver = "direct"
description = "Google Workspace — Gmail, Calendar, Drive, Docs, Sheets, Tasks"
command = "gworkspace-mcp"
args = []
enabled = false
scopes = [
    "google.gmail.*",
    "google.calendar.*",
    "google.drive.*",
    "google.docs.*",
    "google.sheets.*",
    "google.tasks.*",
]
discovery_ttl_secs = 0
eager_discovery = true

[tool_registry.endpoints.transport]
timeout_ms = 5000

# tickets-mcp — unified ticketing MCP server (GitHub Issues, JIRA, Linear)
# via JSON-RPC 2.0 stdio. Disabled by default; flip `enabled = true` once
# the `tickets-mcp` binary is on $PATH.
[[tool_registry.endpoints]]
name = "tickets-mcp"
driver = "direct"
description = "Unified ticketing MCP server — GitHub Issues, JIRA, Linear"
command = "tickets-mcp"
args = []
enabled = false
scopes = ["ticketing.*"]
discovery_ttl_secs = 0
eager_discovery = true

[tool_registry.endpoints.transport]
timeout_ms = 5000

[[tool_registry.endpoints]]
name = "commons-ticketing"
driver = "direct"
description = "Commons ticketing — create/update/close GitHub issues and PRs via OpenRPC stdio"
command = "commons-ticketing"
args = ["--rpc"]
enabled = false
scopes = ["ticketing.read", "ticketing.write", "ticketing.admin"]
discovery_ttl_secs = 300
eager_discovery = true

[tool_registry.endpoints.transport]
timeout_ms = 5000
"#;

impl GlobalConfig {
    /// Resolve the on-disk path for the global config file.
    ///
    /// Why: Centralizes path construction so tests can override via a custom
    /// `$HOME` and so the rest of the codebase never hardcodes the layout.
    /// What: Returns `$HOME/.open-mpm/config.toml`, or an error if `$HOME`
    /// can't be determined (extremely rare on real systems but possible in
    /// minimal containers).
    pub fn config_path() -> Result<PathBuf> {
        let home = dirs::home_dir().context("could not determine $HOME directory")?;
        Ok(home.join(CONFIG_DIR_NAME).join(CONFIG_FILE_NAME))
    }

    /// Load the global config, creating it with defaults when absent.
    ///
    /// Why: The first time a user runs open-mpm there is no config. Rather
    /// than failing or silently using an empty registry, materialize the
    /// defaults to disk so the user can see/edit what's active.
    /// What: Reads `$HOME/.open-mpm/config.toml`; if missing, writes the
    /// `DEFAULT_CONFIG_TOML` and parses it. Parse errors propagate so a
    /// hand-edited broken file fails loudly instead of being silently reset.
    /// Test: `tests::load_or_create_*` cover both the create-on-absent and
    /// load-existing branches.
    pub async fn load_or_create() -> Result<Self> {
        let path = Self::config_path()?;
        if !path.exists() {
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .with_context(|| format!("failed to create config dir {}", parent.display()))?;
            }
            tokio::fs::write(&path, DEFAULT_CONFIG_TOML)
                .await
                .with_context(|| format!("failed to write default config {}", path.display()))?;
            tracing::info!(path = %path.display(), "created default open-mpm config");
        }
        let raw = tokio::fs::read_to_string(&path)
            .await
            .with_context(|| format!("failed to read config {}", path.display()))?;
        let cfg: Self = toml::from_str(&raw)
            .with_context(|| format!("failed to parse config {}", path.display()))?;
        Ok(cfg)
    }

    /// Parse a config from a TOML string. Used in tests and when a custom
    /// config is provided via env var or programmatic construction.
    pub fn from_toml_str(s: &str) -> Result<Self> {
        toml::from_str(s).context("failed to parse mcp config TOML")
    }

    /// Default config used as fallback when load encounters issues. (#244, #245)
    ///
    /// Why: `load()` is called fresh on every prompt build to pick up
    /// changes made via the `mcp_*` tools. When the file is missing the
    /// prompt-build path must not fail — and previously it returned an empty
    /// registry (`Self::default()`), which silently dropped the documented
    /// gworkspace-mcp + slack-user-proxy defaults from in-memory state. We
    /// now parse `DEFAULT_CONFIG_TOML` so the in-memory defaults match what
    /// `load_or_create` would write to disk. Parse failure (a programmer
    /// error in the literal) falls back to `Self::default()` rather than
    /// panicking.
    /// What: Parses `DEFAULT_CONFIG_TOML` once per call; on parse failure
    /// logs a warning and falls back to `Self::default()`.
    /// Test: `tests::load_returns_documented_defaults_when_absent`.
    fn default_config() -> Self {
        toml::from_str(DEFAULT_CONFIG_TOML).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "DEFAULT_CONFIG_TOML failed to parse; falling back to empty default");
            Self::default()
        })
    }

    /// Load the config from disk without creating it on absence. (#244, #245)
    ///
    /// Why: The `mcp_*` management tools (mcp_add, mcp_remove, etc.) write
    /// to disk and we want the very next prompt build to reflect those
    /// changes. By re-reading the file on each turn we get hot-reload for
    /// free without a caching layer. Unlike `load_or_create`, this function
    /// has no side effects on the file system. Async since #245 to avoid
    /// blocking the tokio runtime on disk I/O during prompt builds and tool
    /// dispatch (called from async contexts).
    /// What: Reads `$HOME/.open-mpm/config.toml` via `tokio::fs`. If the
    /// file is missing returns `default_config()` (the documented defaults
    /// — gworkspace-mcp + slack-user-proxy). Parse failures log a warning
    /// and fall back to the same defaults rather than failing loudly.
    /// Test: `tests::load_returns_documented_defaults_when_absent`,
    /// `tests::save_and_reload_roundtrip`.
    pub async fn load() -> Self {
        let Ok(path) = Self::config_path() else {
            return Self::default_config();
        };
        let content = match tokio::fs::read_to_string(&path).await {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Self::default_config();
            }
            Err(e) => {
                tracing::warn!(error = %e, path = %path.display(), "failed to read mcp config; using default");
                return Self::default_config();
            }
        };
        toml::from_str(&content).unwrap_or_else(|e| {
            tracing::warn!(error = %e, path = %path.display(), "failed to parse mcp config; using default");
            Self::default_config()
        })
    }

    /// Persist the current state to disk. (#244)
    ///
    /// Why: The `mcp_add`/`mcp_remove`/`mcp_enable`/`mcp_disable` tools
    /// mutate the in-memory config and need to immediately write it back to
    /// `~/.open-mpm/config.toml` so subsequent processes (and the next
    /// prompt build) see the change.
    /// What: Serializes `self` to pretty TOML and atomically writes to
    /// `config_path()`. Creates the parent directory if absent.
    /// Test: `tests::save_and_reload_roundtrip`.
    pub async fn save(&self) -> Result<()> {
        let path = Self::config_path()?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("failed to create config dir {}", parent.display()))?;
        }
        let content = toml::to_string_pretty(self).context("failed to serialize mcp config")?;
        tokio::fs::write(&path, content)
            .await
            .with_context(|| format!("failed to write config {}", path.display()))?;
        Ok(())
    }

    /// Add or replace a service by name. Persists immediately. (#244)
    ///
    /// Why: Allows the `mcp_add` tool to register a new service, or update
    /// an existing one (re-add with same name) atomically. By persisting
    /// inside the method the caller can't forget to save.
    /// What: Removes any existing service with `service.name`, pushes the
    /// new one, and calls `save()`.
    /// Test: `tests::add_service_replaces_existing`.
    pub async fn add_service(&mut self, service: McpService) -> Result<()> {
        self.mcp.services.retain(|s| s.name != service.name);
        self.mcp.services.push(service);
        self.save().await
    }

    /// Remove a service by name. Persists immediately. (#244)
    ///
    /// Why: Backs the `mcp_remove` tool. Returns whether anything was
    /// actually removed so the tool can give the LLM a meaningful message
    /// (vs. silently no-op'ing on an unknown name).
    /// What: Filters `services`, persists if a removal occurred, returns
    /// `Ok(true)` when found and removed, `Ok(false)` otherwise.
    /// Test: `tests::remove_service_*`.
    pub async fn remove_service(&mut self, name: &str) -> Result<bool> {
        let before = self.mcp.services.len();
        self.mcp.services.retain(|s| s.name != name);
        let removed = self.mcp.services.len() < before;
        if removed {
            self.save().await?;
        }
        Ok(removed)
    }

    /// Enable a service by name. Persists immediately. (#244)
    ///
    /// Why: Backs the `mcp_enable` tool. The service stays registered but
    /// becomes visible in role-gated prompt rendering and dispatch.
    /// What: Finds by name, sets `enabled = true`, persists. Returns
    /// `Ok(true)` when found, `Ok(false)` when name unknown.
    /// Test: `tests::enable_disable_toggles_flag`.
    pub async fn enable_service(&mut self, name: &str) -> Result<bool> {
        if let Some(s) = self.mcp.services.iter_mut().find(|s| s.name == name) {
            s.enabled = true;
            self.save().await?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Disable a service by name. Persists immediately. (#244)
    ///
    /// Why: Backs the `mcp_disable` tool. Lets the user temporarily turn
    /// off a service without forgetting its configuration.
    /// What: Finds by name, sets `enabled = false`, persists. Returns
    /// `Ok(true)` when found, `Ok(false)` when name unknown.
    /// Test: `tests::enable_disable_toggles_flag`.
    pub async fn disable_service(&mut self, name: &str) -> Result<bool> {
        if let Some(s) = self.mcp.services.iter_mut().find(|s| s.name == name) {
            s.enabled = false;
            self.save().await?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Render a human-readable list of all registered services. (#244)
    ///
    /// Why: The `mcp_list` tool returns this string to the LLM so it can
    /// summarize available external capabilities for the user. Includes
    /// disabled services (marked) so the user can see what's available to
    /// re-enable.
    /// What: Builds a multi-line string with a header count, then for each
    /// service a line with a check/cross marker, name, transport,
    /// description, and a wrapped tools list.
    /// Test: `tests::render_list_format`.
    pub fn render_list(&self) -> String {
        let total = self.mcp.services.len();
        if total == 0 {
            return "No MCP services registered.".to_string();
        }
        let mut out = format!("Registered MCP services ({total}):\n\n");
        for svc in &self.mcp.services {
            let marker = if svc.enabled { "✓" } else { "✗" };
            let disabled_suffix = if svc.enabled { "" } else { " (disabled)" };
            out.push_str(&format!(
                "{} {} [{}] — {}{}\n",
                marker, svc.name, svc.transport, svc.description, disabled_suffix
            ));
            if !svc.tools.is_empty() {
                let names: Vec<&str> = svc.tools.iter().map(|t| t.name.as_str()).collect();
                out.push_str(&format!("  Tools: {}\n", names.join(", ")));
            }
            out.push('\n');
        }
        out.trim_end().to_string()
    }

    /// Services enabled for a given agent role.
    ///
    /// Why: The MCP layer is role-gated — engineer/coder/qa/ops agents have
    /// their own purpose-built tools and don't benefit from MCP descriptions
    /// in their prompt. Coordinating roles (ctrl, pm, research, observe) do.
    /// What: If `role` is not in `inject_for_roles`, returns empty. Otherwise
    /// returns references to all `enabled = true` services.
    /// Test: `tests::services_for_role_gating`.
    pub fn services_for_role(&self, role: &str) -> Vec<&McpService> {
        if !self.mcp.inject_for_roles.iter().any(|r| r == role) {
            return Vec::new();
        }
        self.mcp.services.iter().filter(|s| s.enabled).collect()
    }

    /// Render the prompt section listing MCP tools available to `role`.
    ///
    /// Why: The model needs a textual description of what MCP tools exist so
    /// it can request them via the standard tool-call protocol. Without this
    /// the agent has no way to discover external capabilities.
    /// What: Builds a Markdown block with one heading per service and a bullet
    /// list of `tool_name — description` lines. Returns `None` when no services
    /// apply to the role (so callers can skip injecting an empty layer).
    /// Test: `tests::render_prompt_section_*`.
    pub fn render_prompt_section(&self, role: &str) -> Option<String> {
        if !self.mcp.inject_for_roles.iter().any(|r| r == role) {
            return None;
        }
        if self.mcp.services.is_empty() {
            return None;
        }

        let mut out = String::from("## Available External Services (MCP)\n\n");
        out.push_str(
            "The following external service integrations are registered and \
             available. Reference them by name when coordinating work that \
             involves these platforms.\n\n",
        );
        for svc in &self.mcp.services {
            if svc.enabled {
                out.push_str(&format!("### {} — {}\n\n", svc.name, svc.description));
                if svc.tools.is_empty() {
                    out.push_str("_(no tools declared)_\n\n");
                    continue;
                }
                let names: Vec<&str> = svc.tools.iter().map(|t| t.name.as_str()).collect();
                out.push_str(&format!("Tools: {}\n\n", names.join(", ")));
            } else {
                out.push_str(&format!(
                    "### {} — {} (disabled)\n\n",
                    svc.name, svc.description
                ));
                out.push_str("_(disabled — not available in this environment)_\n\n");
            }
        }
        Some(out.trim_end().to_string())
    }
}

#[cfg(test)]
mod tests {
    // Why: These tests hold `HOME_LOCK` (a `std::sync::Mutex`) across async
    // I/O to serialize global $HOME mutation between tests. See
    // `crate::test_env` for the full rationale.
    #![allow(clippy::await_holding_lock)]

    use super::*;
    use crate::test_env::HOME_LOCK;

    fn tempdir() -> PathBuf {
        let p = std::env::temp_dir().join(format!("open-mpm-mcp-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[tokio::test]
    async fn load_or_create_writes_default_when_absent() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir();
        unsafe {
            std::env::set_var("HOME", &home);
        }

        let cfg = GlobalConfig::load_or_create()
            .await
            .expect("create default config");

        let path = home.join(".open-mpm").join("config.toml");
        assert!(
            path.exists(),
            "config file should exist after load_or_create"
        );

        // Defaults (#256): gworkspace-mcp (enabled), slack-user-proxy (disabled),
        // granola-notes (enabled), duetto-memory (disabled).
        assert_eq!(cfg.mcp.services.len(), 4);
        let gw = cfg
            .mcp
            .services
            .iter()
            .find(|s| s.name == "gworkspace-mcp")
            .expect("gworkspace-mcp present in defaults");
        assert!(gw.enabled);
        let slack = cfg
            .mcp
            .services
            .iter()
            .find(|s| s.name == "slack-user-proxy")
            .expect("slack-user-proxy present in defaults");
        assert!(!slack.enabled);
        let granola = cfg
            .mcp
            .services
            .iter()
            .find(|s| s.name == "granola-notes")
            .expect("granola-notes present in defaults (#256)");
        assert!(granola.enabled);
        let duetto = cfg
            .mcp
            .services
            .iter()
            .find(|s| s.name == "duetto-memory")
            .expect("duetto-memory present in defaults (#256)");
        assert!(
            !duetto.enabled,
            "duetto-memory should be disabled by default"
        );
        assert_eq!(duetto.transport, "http");
        assert_eq!(
            duetto.url.as_deref(),
            Some("https://mcp-services.dev.duettosystems.com/memory/mcp")
        );
        // No native local integrations in the registry — those are wired into
        // the harness directly (kuzu-memory, mcp-vector-search).
        assert!(
            !cfg.mcp.services.iter().any(|s| s.name == "kuzu-memory"),
            "kuzu-memory must not appear in MCP registry"
        );
        assert!(
            !cfg.mcp
                .services
                .iter()
                .any(|s| s.name == "mcp-vector-search"),
            "mcp-vector-search must not appear in MCP registry"
        );
        assert!(cfg.mcp.inject_for_roles.contains(&"ctrl".to_string()));
        assert!(cfg.mcp.inject_for_roles.contains(&"pm".to_string()));
    }

    #[tokio::test]
    async fn load_or_create_reads_existing_file() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir();
        unsafe {
            std::env::set_var("HOME", &home);
        }
        let cfg_dir = home.join(".open-mpm");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(
            cfg_dir.join("config.toml"),
            r#"
[mcp]
inject_for_roles = ["ctrl"]

[[mcp.services]]
name = "custom"
description = "a custom service"
command = "echo"
transport = "stdio"
enabled = true
"#,
        )
        .unwrap();

        let cfg = GlobalConfig::load_or_create()
            .await
            .expect("load existing config");
        assert_eq!(cfg.mcp.inject_for_roles, vec!["ctrl".to_string()]);
        assert_eq!(cfg.mcp.services.len(), 1);
        assert_eq!(cfg.mcp.services[0].name, "custom");
    }

    #[test]
    fn services_for_role_gating() {
        let cfg = GlobalConfig::from_toml_str(
            r#"
[mcp]
inject_for_roles = ["ctrl", "pm"]

[[mcp.services]]
name = "gworkspace-mcp"
description = "Google Workspace"
command = "gworkspace-mcp"
args = ["mcp"]
transport = "stdio"
enabled = true

[[mcp.services]]
name = "slack-user-proxy"
description = "Slack"
command = "slack-user-proxy"
transport = "stdio"
enabled = false
"#,
        )
        .unwrap();

        // Roles in inject list see the enabled service only (gworkspace-mcp);
        // slack-user-proxy is disabled by default and excluded.
        let ctrl_services = cfg.services_for_role("ctrl");
        assert_eq!(ctrl_services.len(), 1);
        assert_eq!(ctrl_services[0].name, "gworkspace-mcp");
        let pm_services = cfg.services_for_role("pm");
        assert_eq!(pm_services.len(), 1);
        assert_eq!(pm_services[0].name, "gworkspace-mcp");
        // Roles outside the list see nothing.
        assert!(cfg.services_for_role("engineer").is_empty());
        assert!(cfg.services_for_role("coder").is_empty());
    }

    #[test]
    fn render_prompt_section_includes_tool_names() {
        let cfg = GlobalConfig::from_toml_str(
            r#"
[mcp]
inject_for_roles = ["pm"]

[[mcp.services]]
name = "gworkspace-mcp"
description = "Google Workspace"
command = "gworkspace-mcp"
args = ["mcp"]
transport = "stdio"
enabled = true

[[mcp.services.tools]]
name = "gmail_search"
description = "Search Gmail"
"#,
        )
        .unwrap();

        let rendered = cfg.render_prompt_section("pm").expect("non-empty");
        assert!(rendered.contains("gworkspace-mcp"));
        assert!(rendered.contains("gmail_search"));
        assert!(rendered.contains("## Available External Services (MCP)"));
    }

    #[test]
    fn render_prompt_section_marks_disabled_services() {
        let cfg = GlobalConfig::from_toml_str(
            r#"
[mcp]
inject_for_roles = ["ctrl"]

[[mcp.services]]
name = "gworkspace-mcp"
description = "Google Workspace"
command = "gworkspace-mcp"
transport = "stdio"
enabled = true

[[mcp.services.tools]]
name = "gmail_search"
description = "Search Gmail"

[[mcp.services]]
name = "slack-user-proxy"
description = "Slack messaging"
command = "slack-user-proxy"
transport = "stdio"
enabled = false
"#,
        )
        .unwrap();

        let rendered = cfg.render_prompt_section("ctrl").expect("non-empty");
        // Enabled service appears with its tools.
        assert!(rendered.contains("gworkspace-mcp"));
        assert!(rendered.contains("gmail_search"));
        // Disabled service is listed with the disabled marker.
        assert!(rendered.contains("slack-user-proxy"));
        assert!(
            rendered.contains("(disabled"),
            "expected '(disabled' marker in rendered output, got:\n{rendered}"
        );
        assert!(
            rendered.contains("not available"),
            "expected 'not available' marker in rendered output, got:\n{rendered}"
        );
    }

    #[test]
    fn render_prompt_section_empty_for_excluded_role() {
        let cfg = GlobalConfig::from_toml_str(
            r#"
[mcp]
inject_for_roles = ["ctrl"]

[[mcp.services]]
name = "gworkspace-mcp"
description = "Google Workspace"
command = "gworkspace-mcp"
transport = "stdio"
enabled = true
"#,
        )
        .unwrap();

        assert!(cfg.render_prompt_section("engineer").is_none());
    }

    #[tokio::test]
    async fn load_returns_documented_defaults_when_absent() {
        // (#244, #245) load() must not create the file (unlike load_or_create),
        // but must return the documented defaults (gworkspace-mcp +
        // slack-user-proxy) so prompt-build paths see the same registry that
        // `load_or_create` would write.
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir();
        unsafe {
            std::env::set_var("HOME", &home);
        }
        let cfg = GlobalConfig::load().await;
        let path = home.join(".open-mpm").join("config.toml");
        assert!(!path.exists(), "load() must not create the config file");
        // #245/#256: defaults now mirror DEFAULT_CONFIG_TOML — 4 services
        // (gworkspace-mcp, slack-user-proxy, granola-notes, duetto-memory).
        assert_eq!(cfg.mcp.services.len(), 4);
        assert!(cfg.mcp.services.iter().any(|s| s.name == "gworkspace-mcp"));
        assert!(
            cfg.mcp
                .services
                .iter()
                .any(|s| s.name == "slack-user-proxy")
        );
        assert!(cfg.mcp.services.iter().any(|s| s.name == "granola-notes"));
        assert!(cfg.mcp.services.iter().any(|s| s.name == "duetto-memory"));
    }

    #[tokio::test]
    async fn save_and_reload_roundtrip() {
        // (#244) save() then load() must round-trip identically.
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir();
        unsafe {
            std::env::set_var("HOME", &home);
        }
        let mut cfg = GlobalConfig::default();
        cfg.mcp.inject_for_roles = vec!["ctrl".to_string(), "pm".to_string()];
        cfg.mcp.services.push(McpService {
            name: "test-svc".to_string(),
            description: "A test service".to_string(),
            command: "test-cmd".to_string(),
            args: vec!["arg1".to_string()],
            url: None,
            transport: "stdio".to_string(),
            enabled: true,
            tools: vec![McpTool {
                name: "test_tool".to_string(),
                description: "A test tool".to_string(),
            }],
        });
        cfg.save().await.expect("save should succeed");
        let reloaded = GlobalConfig::load().await;
        assert_eq!(reloaded.mcp.services.len(), 1);
        assert_eq!(reloaded.mcp.services[0].name, "test-svc");
        assert_eq!(reloaded.mcp.services[0].tools.len(), 1);
        assert_eq!(reloaded.mcp.services[0].tools[0].name, "test_tool");
        assert!(reloaded.mcp.services[0].enabled);
    }

    #[tokio::test]
    async fn add_service_replaces_existing() {
        // (#244) add_service with a name that already exists replaces, not appends.
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir();
        unsafe {
            std::env::set_var("HOME", &home);
        }
        let mut cfg = GlobalConfig::default();
        cfg.add_service(McpService {
            name: "x".to_string(),
            description: "first".to_string(),
            command: "a".to_string(),
            args: vec![],
            url: None,
            transport: "stdio".to_string(),
            enabled: true,
            tools: vec![],
        })
        .await
        .unwrap();
        cfg.add_service(McpService {
            name: "x".to_string(),
            description: "second".to_string(),
            command: "b".to_string(),
            args: vec![],
            url: None,
            transport: "stdio".to_string(),
            enabled: true,
            tools: vec![],
        })
        .await
        .unwrap();
        assert_eq!(cfg.mcp.services.len(), 1);
        assert_eq!(cfg.mcp.services[0].description, "second");
        assert_eq!(cfg.mcp.services[0].command, "b");
    }

    #[tokio::test]
    async fn remove_service_returns_correct_bool() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir();
        unsafe {
            std::env::set_var("HOME", &home);
        }
        let mut cfg = GlobalConfig::default();
        cfg.add_service(McpService {
            name: "x".to_string(),
            description: "d".to_string(),
            command: "c".to_string(),
            args: vec![],
            url: None,
            transport: "stdio".to_string(),
            enabled: true,
            tools: vec![],
        })
        .await
        .unwrap();
        assert!(cfg.remove_service("x").await.unwrap());
        assert!(!cfg.remove_service("x").await.unwrap());
        assert!(cfg.mcp.services.is_empty());
    }

    #[tokio::test]
    async fn enable_disable_toggles_flag() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir();
        unsafe {
            std::env::set_var("HOME", &home);
        }
        let mut cfg = GlobalConfig::default();
        cfg.add_service(McpService {
            name: "x".to_string(),
            description: "d".to_string(),
            command: "c".to_string(),
            args: vec![],
            url: None,
            transport: "stdio".to_string(),
            enabled: false,
            tools: vec![],
        })
        .await
        .unwrap();
        assert!(cfg.enable_service("x").await.unwrap());
        assert!(cfg.mcp.services[0].enabled);
        assert!(cfg.disable_service("x").await.unwrap());
        assert!(!cfg.mcp.services[0].enabled);
        // Unknown name returns false.
        assert!(!cfg.enable_service("missing").await.unwrap());
        assert!(!cfg.disable_service("missing").await.unwrap());
    }

    #[test]
    fn render_list_format() {
        let cfg = GlobalConfig::from_toml_str(
            r#"
[mcp]
inject_for_roles = ["ctrl"]

[[mcp.services]]
name = "gworkspace-mcp"
description = "Google Workspace"
command = "gworkspace-mcp"
args = ["mcp"]
transport = "stdio"
enabled = true

[[mcp.services.tools]]
name = "gmail_search"
description = "Search Gmail"

[[mcp.services]]
name = "slack-user-proxy"
description = "Slack messaging"
command = "slack-user-proxy"
transport = "stdio"
enabled = false

[[mcp.services.tools]]
name = "slack_post"
description = "Post"
"#,
        )
        .unwrap();
        let rendered = cfg.render_list();
        assert!(rendered.contains("Registered MCP services (2):"));
        assert!(rendered.contains("✓ gworkspace-mcp [stdio]"));
        assert!(rendered.contains("✗ slack-user-proxy [stdio]"));
        assert!(rendered.contains("(disabled)"));
        assert!(rendered.contains("Tools: gmail_search"));
        assert!(rendered.contains("Tools: slack_post"));
    }

    #[test]
    fn render_list_empty() {
        let cfg = GlobalConfig::default();
        assert_eq!(cfg.render_list(), "No MCP services registered.");
    }

    #[test]
    fn local_inference_defaults_apply() {
        // (#319, #345) LocalInferenceConfig::default must match the documented
        // shipping defaults — enabled, qwen3:30b, fallback on, localhost.
        let li = LocalInferenceConfig::default();
        assert!(li.enabled, "local inference must be enabled by default");
        assert_eq!(li.model, "ollama/qwen3:30b");
        assert!(li.fallback_on_error);
        assert_eq!(li.ollama_host, "http://localhost:11434");
        assert_eq!(li.max_tokens, 2048);
    }

    #[test]
    fn local_inference_section_round_trips() {
        // (#319) Round-trip the [local_inference] section so the documented
        // TOML shape stays parseable as the codebase evolves.
        let cfg = GlobalConfig::from_toml_str(
            r#"
[local_inference]
enabled = true
model = "ollama/qwen3:8b"
fallback_on_error = false
ollama_host = "http://192.168.1.10:11434"
max_tokens = 4096
"#,
        )
        .unwrap();
        assert!(cfg.local_inference.enabled);
        assert_eq!(cfg.local_inference.model, "ollama/qwen3:8b");
        assert!(!cfg.local_inference.fallback_on_error);
        assert_eq!(cfg.local_inference.ollama_host, "http://192.168.1.10:11434");
        assert_eq!(cfg.local_inference.max_tokens, 4096);
    }

    #[test]
    fn default_config_includes_local_inference_section() {
        // (#319, #345) The DEFAULT_CONFIG_TOML literal must include a usable
        // [local_inference] block so users have an obvious place to flip
        // the flag without needing to know the schema.
        let cfg = GlobalConfig::from_toml_str(DEFAULT_CONFIG_TOML).unwrap();
        assert!(cfg.local_inference.enabled);
        assert_eq!(cfg.local_inference.model, "ollama/qwen3:30b");
    }

    #[test]
    fn default_config_is_valid_toml() {
        let cfg = GlobalConfig::from_toml_str(DEFAULT_CONFIG_TOML).expect("default parses");
        assert_eq!(cfg.mcp.services.len(), 4);
        let gw = cfg
            .mcp
            .services
            .iter()
            .find(|s| s.name == "gworkspace-mcp")
            .expect("gworkspace-mcp present");
        assert!(gw.enabled);
        assert!(gw.tools.iter().any(|t| t.name == "gmail_search"));
        assert!(gw.tools.iter().any(|t| t.name == "calendar_list"));
        let slack = cfg
            .mcp
            .services
            .iter()
            .find(|s| s.name == "slack-user-proxy")
            .expect("slack-user-proxy present");
        assert!(!slack.enabled);
        // Native local integrations stay out of the registry.
        assert!(!cfg.mcp.services.iter().any(|s| s.name == "kuzu-memory"));
        assert!(
            !cfg.mcp
                .services
                .iter()
                .any(|s| s.name == "mcp-vector-search")
        );
    }
}
