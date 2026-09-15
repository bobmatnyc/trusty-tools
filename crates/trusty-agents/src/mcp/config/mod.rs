//! `~/.trusty-agents/config.toml` — this crate's own global settings.
//!
//! Why: one global file that survives a binary upgrade, lets a user change
//! behaviour without recompiling, and gives the prompt builder a stable
//! surface to read. `GlobalConfig` is the on-disk schema (formerly
//! `McpConfig`; renamed in #245 once it composed several subsystems).
//!
//! What it is NO LONGER (#7454, ADR-0060): the MCP server registry. The
//! servers moved to the file `trusty-code` shares
//! (`~/.trusty-tools/mcp/servers.toml`, [`crate::mcp::shared`]), and the
//! per-assistant overrides live in the assistant's own home. What stays under
//! `[mcp]` here is this crate's POLICY over those servers — which agent roles
//! get the prompt layer, and whether a project's `.mcp.json` is trusted — so
//! [`GlobalConfig::servers_for_role`] and
//! [`GlobalConfig::render_prompt_section`] now take the effective server list
//! their caller already resolved rather than reading one of their own.
//!
//! Module layout (see #366 split):
//! - `mod.rs` — `GlobalConfig` struct + load/save/render behavior
//! - `types.rs` — the config sub-section types + defaults
//! - `defaults.rs` — the `DEFAULT_CONFIG_TOML` literal
//! - `tests/` — unit tests
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

pub use types::{GitConfig, LocalInferenceConfig, McpSection, ProvidersSection};

use trusty_mcp::config::McpServerConfig;

use crate::mcp::extensions;

/// Roots of the global config tree (`~/.trusty-agents/config.toml`).
const CONFIG_DIR_NAME: &str = ".trusty-agents";
const CONFIG_FILE_NAME: &str = "config.toml";

/// Top-level shape of `~/.trusty-agents/config.toml` — composes all subsystems.
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
    /// hot path. Enabled by default since #345 — `/local off` or
    /// `enabled = false` turns it back off. (The pre-#345 opt-in wording this
    /// comment carried was corrected by the 2026-08-19 audit, which found the
    /// same claim contradicting `LocalInferenceConfig::default`.)
    /// What: Holds enable flag, model id (`ollama/<name>` form), fallback
    /// behavior, host URL, and a max-token cap to keep local responses snappy.
    /// Test: `local_inference_defaults_apply` round-trips the section;
    /// `local_inference_enabled_defaults_true_when_key_omitted` pins the
    /// default that applies when the section omits `enabled`.
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

    /// `[providers]` section (#3766) — the default inference provider for
    /// agents that declare no pin of their own.
    ///
    /// Why: this section MUST be modelled here, not parsed independently.
    /// `save()` re-serializes only the fields declared on this struct, so a
    /// `[providers]` table it does not know about is dropped from disk by any
    /// unrelated write — `/local on` and the four `mcp_*` mutators all
    /// load-mutate-save.
    /// What: see [`ProvidersSection`]. Absent means unset, which is today's
    /// ambient behaviour.
    /// Test: `providers_section_survives_an_unrelated_save`,
    /// `providers_section_defaults_to_unset`.
    #[serde(default)]
    pub providers: ProvidersSection,

    /// `[[listeners]]` — harness-level eventstream listener definitions
    /// (#3820, DOC-54 SPEC-AGENTS-06 §7.5), stage-one (ingestion) filter.
    ///
    /// Why: Listeners are opt-in, account-specific, and never written by
    /// this codebase — a listener declared here with `enabled = false`
    /// (the field's default) is inert until an operator flips it on by hand.
    /// Defaults to empty so every existing `config.toml` (which predates
    /// this field) keeps parsing unchanged.
    /// What: See `crate::listeners::config::ListenerConfig`.
    ///
    /// DEPRECATED (#7609): this is the legacy spelling of `[[channels]]`, kept
    /// parsing for one release and never dropped from disk. Read
    /// [`GlobalConfig::listeners`] instead of this field — it answers from
    /// `channels`, which is where a migrated entry lands.
    /// Test: `listeners_section_defaults_empty`,
    /// `listeners_section_round_trips`.
    #[serde(default, rename = "listeners")]
    pub legacy_listeners: Vec<crate::listeners::config::ListenerConfig>,

    /// `[[channels]]` — harness-level channels (#7609), the merge of the
    /// listener and channel-binding models.
    ///
    /// Why: one concept, one table. A `[[listeners]]` entry from before the
    /// merge is absorbed into this list on every load, and persisted here once
    /// by [`crate::channels::migrate::migrate_global_if_absent`], so an
    /// operator never hand-edits anything to keep a listener working.
    /// What: [`crate::channels::Channel`] with
    /// [`crate::channels::ChannelScope::Global`]. Empty by default, so every
    /// config that predates this field parses unchanged.
    /// Test: `a_legacy_listeners_table_is_absorbed_into_channels`,
    /// `listeners_section_round_trips`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channels: Vec<crate::channels::Channel>,

    /// `[log_drain]` section (#6537) — periodic upload of this daemon's own
    /// log directory to object storage, mirroring trusty-mpm's `log_drain:`
    /// section.
    ///
    /// Why: modelled here, not parsed independently, for the same reason
    /// `[providers]` is above — `save()` re-serializes only fields this
    /// struct declares, so an unmodelled `[log_drain]` table would be
    /// silently dropped by any unrelated `mcp_*`-tool write.
    /// What: absent or `enabled = false` (the default) means the scheduler
    /// this daemon spawns (`crate::log_drain::spawn`) never uploads anything.
    /// Test: `crate::log_drain::resolve::tests::toml_round_trips_every_field`.
    #[serde(default)]
    pub log_drain: crate::log_drain::LogDrainConfig,
}

impl GlobalConfig {
    /// The harness-level listeners, projected out of `channels`.
    ///
    /// Why (#7609): `[[listeners]]` and `[[channels]]` are one concept now, so
    /// there is one list in memory. This is the derived view every existing
    /// consumer reads — `crate::listeners::poll`, the listener API, the
    /// knowledge pipeline — and it answers exactly what the removed
    /// `listeners` field held, so nothing about polling changed.
    /// What: every `Global`-scope channel as a `ListenerConfig`, in stored
    /// order.
    /// Test: `a_legacy_listeners_table_is_absorbed_into_channels`,
    /// `listeners_section_round_trips`.
    pub fn listeners(&self) -> Vec<crate::listeners::config::ListenerConfig> {
        crate::channels::model::project(
            &self.channels,
            crate::channels::ChannelScope::Global,
            crate::channels::Channel::to_listener_config,
        )
    }

    /// Fold any legacy `[[listeners]]` entry into `channels`, in memory.
    ///
    /// Why (#7609): the on-disk migration only runs on the write path
    /// (`load_or_create`), and `load` has no side effects by design. Absorbing
    /// on every parse is what makes the deprecated table keep working
    /// everywhere, whether or not this process ever wrote the file.
    /// What: scope is set from this file's location, then each legacy listener
    /// with no channel of the same `id` is appended. An entry the operator has
    /// already migrated is NOT duplicated. Warns once per process.
    /// Test: `a_legacy_listeners_table_is_absorbed_into_channels`,
    /// `an_already_migrated_listener_is_not_absorbed_twice`.
    fn absorb_legacy_listeners(&mut self) {
        for channel in &mut self.channels {
            channel.scope = crate::channels::ChannelScope::Global;
        }
        if self.legacy_listeners.is_empty() {
            return;
        }
        crate::channels::migrate::warn_global_listeners_deprecated();
        for listener in self.legacy_listeners.clone() {
            if self.channels.iter().any(|c| c.id == listener.name) {
                continue;
            }
            self.channels.push(crate::channels::Channel::from(listener));
        }
    }

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
    /// What: Returns `$HOME/.trusty-agents/config.toml`, or an error if `$HOME`
    /// can't be determined (extremely rare on real systems but possible in
    /// minimal containers).
    pub fn config_path() -> Result<PathBuf> {
        let home = dirs::home_dir().context("could not determine $HOME directory")?;
        Ok(home.join(CONFIG_DIR_NAME).join(CONFIG_FILE_NAME))
    }

    /// Load the global config, creating it with defaults when absent.
    ///
    /// Why: The first time a user runs trusty-agents there is no config. Rather
    /// than failing or silently using an empty registry, materialize the
    /// defaults to disk so the user can see/edit what's active.
    /// What: Reads `$HOME/.trusty-agents/config.toml`; if missing, writes the
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
            tracing::info!(path = %path.display(), "created default trusty-agents config");
        }
        // #7609: the one-shot `[[listeners]]` -> `[[channels]]` drain. This is
        // already the write path, and it runs BEFORE the read so the parsed
        // config reflects what is now on disk.
        let migration_target = path.clone();
        let migrated = tokio::task::spawn_blocking(move || {
            crate::channels::migrate::migrate_global_if_absent(&migration_target)
        })
        .await;
        match migrated {
            Ok(Ok(Some(report))) => tracing::info!(
                path = %path.display(),
                moved = %report.summary(),
                "channel migration: moved [[listeners]] into [[channels]] (#7609)",
            ),
            Ok(Ok(None)) => {}
            Ok(Err(e)) => tracing::warn!(error = %e, "channel migration: nothing moved"),
            Err(e) => tracing::warn!(error = %e, "channel migration: task failed"),
        }
        let raw = tokio::fs::read_to_string(&path)
            .await
            .with_context(|| format!("failed to read config {}", path.display()))?;
        let mut cfg: Self = toml::from_str(&raw)
            .with_context(|| format!("failed to parse config {}", path.display()))?;
        cfg.absorb_legacy_listeners();
        Ok(cfg)
    }

    /// Parse a config from a TOML string. Used in tests and when a custom
    /// config is provided via env var or programmatic construction.
    pub fn from_toml_str(s: &str) -> Result<Self> {
        let mut cfg: Self = toml::from_str(s).context("failed to parse mcp config TOML")?;
        // #7609: the deprecated `[[listeners]]` table keeps working.
        cfg.absorb_legacy_listeners();
        Ok(cfg)
    }

    /// Default config used as fallback when load encounters issues. (#244, #245)
    ///
    /// Why: `load()` is called fresh on every prompt build to pick up
    /// changes made via the `mcp_*` tools. When the file is missing the
    /// prompt-build path must not fail — and previously it returned an empty
    /// registry (`Self::default()`), which silently dropped the documented
    /// native trusty-mpm defaults from in-memory state. We
    /// now parse `DEFAULT_CONFIG_TOML` so the in-memory defaults match what
    /// `load_or_create` would write to disk. Parse failure (a programmer
    /// error in the literal) falls back to `Self::default()` rather than
    /// panicking.
    /// What: Parses `DEFAULT_CONFIG_TOML` once per call; on parse failure
    /// logs a warning and falls back to `Self::default()`.
    /// Test: `tests::load_returns_documented_defaults_when_absent`.
    fn default_config() -> Self {
        toml::from_str::<Self>(DEFAULT_CONFIG_TOML)
            .map(|mut cfg| {
                cfg.absorb_legacy_listeners();
                cfg
            })
            .unwrap_or_else(|e| {
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
    /// What: Reads `$HOME/.trusty-agents/config.toml` via `tokio::fs`. If the
    /// file is missing returns `default_config()` (the documented defaults
    /// — native gworkspace-mcp). Parse failures log a warning
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
        toml::from_str::<Self>(&content)
            .map(|mut cfg| {
                // #7609: absorb the deprecated table on the read-only path too.
                cfg.absorb_legacy_listeners();
                cfg
            })
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, path = %path.display(), "failed to parse mcp config; using default");
                Self::default_config()
            })
    }

    /// Persist the current state to disk. (#244)
    ///
    /// Why: The `mcp_add`/`mcp_remove`/`mcp_enable`/`mcp_disable` tools
    /// mutate the in-memory config and need to immediately write it back to
    /// `~/.trusty-agents/config.toml` so subsequent processes (and the next
    /// prompt build) see the change.
    /// What: Serializes `self` to pretty TOML and publishes it through
    /// [`crate::state_writer::atomic_update`], which holds the advisory lock on
    /// `config.toml.lock` across the publish and then renames a scratch file
    /// onto `config_path()`. Creates the parent directory if absent.
    ///
    /// #8065: this used to be the ONE `config.toml` writer that took no lock —
    /// its own tmp-then-rename was atomic for a reader but invisible to every
    /// other writer of the same file. Since the #7609 startup drain, every
    /// non-`--api` start writes `config.toml` at boot, so a `save()` from
    /// `repl/commands/routing.rs` could rename over the drain's freshly
    /// published bytes and lose the migration. Sharing the lock closes that.
    /// Note the lock leaves a `config.toml.lock` sibling on disk, which is the
    /// rendezvous point every other writer already uses; it is not scratch.
    ///
    /// `save()` still re-serializes only the fields this struct models — the
    /// other half of #8065's report — which is why a table it does not know
    /// about must be modelled here (see `[providers]`) and why the channel
    /// routes edit the document with `toml_edit` instead of calling this.
    /// Test: `tests::save_and_reload_roundtrip`,
    /// `tests::save_publishes_by_rename_leaving_the_old_file_intact`,
    /// `tests::save_leaves_no_scratch_file_behind`.
    pub async fn save(&self) -> Result<()> {
        let path = Self::config_path()?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("failed to create config dir {}", parent.display()))?;
        }
        let content = toml::to_string_pretty(self).context("failed to serialize mcp config")?;
        // #8065: the shared writer, so this save serializes against the startup
        // drain and every other `config.toml` writer instead of racing them.
        let target = path.clone();
        tokio::task::spawn_blocking(move || {
            crate::state_writer::atomic_update(&target, move |_| Ok(Some(content.into_bytes())))
        })
        .await
        .with_context(|| format!("config save task failed for {}", path.display()))?
        .with_context(|| format!("failed to publish config {}", path.display()))?;
        Ok(())
    }

    /// The servers applicable to `role`, after role gating.
    ///
    /// Why: the MCP prompt layer is role-gated — engineer/coder/qa/ops agents
    /// have their own purpose-built tools and do not benefit from MCP
    /// descriptions in their prompt. Coordinating roles (ctrl, pm, research,
    /// observe) do. The gate is this crate's POLICY, which is why it stayed
    /// here when the servers themselves moved (#7454).
    /// What: empty when `role` is not in `inject_for_roles`; otherwise every
    /// ENABLED server from the effective set the caller already resolved.
    /// Callers pass `resolved.servers`, not a list of their own — see
    /// [`crate::mcp::shared::resolve_for_assistant`].
    /// Test: `tests::render_tests::servers_for_role_gating`,
    /// `tests::render_tests::servers_for_role_drops_disabled`.
    pub fn servers_for_role<'a>(
        &self,
        role: &str,
        servers: &'a [McpServerConfig],
    ) -> Vec<&'a McpServerConfig> {
        if !self.mcp.inject_for_roles.iter().any(|r| r == role) {
            return Vec::new();
        }
        servers.iter().filter(|s| s.enabled).collect()
    }

    /// Render the prompt section listing MCP tools available to `role`.
    ///
    /// Why: the model needs a textual description of what MCP servers exist so
    /// it can request their tools. Without this the agent has no way to
    /// discover external capability.
    /// What: a Markdown block with one heading per server and its declared
    /// tools. A disabled server is still listed, marked disabled — a pane or a
    /// prompt that hides one is the fabrication class DOC-57 §4.4 rules out.
    /// `None` when no server applies to the role, so callers can skip
    /// injecting an empty layer.
    /// Test: `tests::render_prompt_section_*`.
    pub fn render_prompt_section(&self, role: &str, servers: &[McpServerConfig]) -> Option<String> {
        if !self.mcp.inject_for_roles.iter().any(|r| r == role) {
            return None;
        }
        if servers.is_empty() {
            return None;
        }

        let mut out = String::from("## Available External Services (MCP)\n\n");
        out.push_str(
            "The following external service integrations are registered and \
             available. Reference them by name when coordinating work that \
             involves these platforms.\n\n",
        );
        for server in servers {
            let description = extensions::description(server);
            if !server.enabled {
                out.push_str(&format!(
                    "### {} — {description} (disabled)\n\n",
                    server.name
                ));
                out.push_str("_(disabled — not available in this environment)_\n\n");
                continue;
            }
            out.push_str(&format!("### {} — {description}\n\n", server.name));
            let tools = extensions::tools(server);
            if tools.is_empty() {
                out.push_str("_(no tools declared)_\n\n");
                continue;
            }
            let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
            out.push_str(&format!("Tools: {}\n\n", names.join(", ")));
        }
        Some(out.trim_end().to_string())
    }
}
