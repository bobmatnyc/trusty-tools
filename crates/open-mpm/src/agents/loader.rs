//! Disk loading and parsing of agent configs.
//!
//! Why: Centralizes file-read + parse + adapter-resolution so callers get one
//! rich error per failing file and the sync/async loaders cannot drift. Path
//! resolution honours `OPEN_MPM_CONFIG_DIR` so installed binaries find their
//! bundled config anywhere on disk.
//! What: Implements `AgentConfig::{load, by_name, by_name_async, ctrl_default}`
//! plus the directory-package (#482) loader and the agents-directory resolver.
//! Test: See `tests.rs` (`agent_config_*`, `by_name_async_loads_plan_agent`,
//! `agent_directory_package_loads_correctly`, `agent_config_path_honors_env_var`).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};

use super::config::AgentConfig;
use super::model::{CTRL_DEFAULT_TOML, resolve_model};
use crate::llm::adapter::adapter_for_model;

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
    pub(super) fn from_toml_str(raw: &str, path: &Path) -> Result<Self> {
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
pub(crate) fn agent_config_path(name: &str) -> PathBuf {
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
