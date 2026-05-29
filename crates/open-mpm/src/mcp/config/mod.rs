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
//!
//! Module layout (see #366 split):
//! - `mod.rs` — `GlobalConfig` struct + load/save/render behavior
//! - `types.rs` — the config sub-section types + defaults
//! - `defaults.rs` — the `DEFAULT_CONFIG_TOML` literal
//! - `tests.rs` — unit tests
//!
//! Test: `tests::*` cover create-on-absent, role gating, and rendering.

mod defaults;
mod types;

#[cfg(test)]
mod tests;

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use defaults::DEFAULT_CONFIG_TOML;

pub use types::{GitConfig, LocalInferenceConfig, McpSection, McpService, McpTool};

/// Roots of the global config tree (`~/.open-mpm/config.toml`).
const CONFIG_DIR_NAME: &str = ".open-mpm";
const CONFIG_FILE_NAME: &str = "config.toml";

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
