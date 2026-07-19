//! Disk loading and parsing of agent configs.
//!
//! Why: Centralizes file-read + parse + adapter-resolution so callers get one
//! rich error per failing file and the sync/async loaders cannot drift. Path
//! resolution honours `TAGENT_CONFIG_DIR` so installed binaries find their
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
    /// `PathBuf::from(".trusty-agents/agents")` was relative to the process CWD,
    /// which broke when the binary was run from outside the repo root.
    /// What: Resolves `<TAGENT_CONFIG_DIR>/<name>.toml` when the env var
    /// is set, otherwise falls back to the CWD-relative `.trusty-agents/agents/`
    /// path (with a warn log so the fallback is visible at runtime).
    /// Note on async: this still uses sync `std::fs` via `Self::load`; see
    /// `by_name_async` for a tokio-friendly variant (#96). Sync callers that
    /// live in async contexts should migrate when practical.
    /// Test: `AgentConfig::by_name("pm")` loads without error when run from
    /// the project root.
    pub fn by_name(name: &str) -> Result<Self> {
        let (cfg, from_package) = Self::by_name_unresolved_src(name)?;
        // #3055: resolve an `extends` chain at load time (DOC-41 §2.5). An
        // agent with no `extends` is returned unchanged (its `extends` is
        // already `None`); one that inherits is flattened base-first against
        // sibling agents loaded by name.
        if cfg.agent.extends.is_none() {
            return Ok(cfg);
        }
        let lookup = |n: &str| Self::by_name_unresolved(n).ok();
        match crate::agents::extends::resolve(name, &lookup) {
            Ok(resolved) => Ok(resolved.finalize_extends()),
            Err(e) => Self::extends_shadow_fallback(name, from_package, e),
        }
    }

    /// Load a single agent config by name WITHOUT resolving its `extends`
    /// chain (#3055).
    ///
    /// Why: [`AgentConfig::resolve`](crate::agents::extends::resolve) walks the
    /// chain recursively and must fetch each ancestor in its raw, still-
    /// `extends`-bearing form; resolving inside the per-name loader would
    /// double-resolve.
    /// What: thin wrapper over [`Self::by_name_unresolved_src`] discarding the
    /// source-provenance flag, for the recursive ancestor lookup where only the
    /// config matters.
    /// Test: covered by the `extends_*` by-name tests and the pre-existing
    /// `by_name` tests.
    fn by_name_unresolved(name: &str) -> Result<Self> {
        Self::by_name_unresolved_src(name).map(|(cfg, _)| cfg)
    }

    /// Load a single agent by name (no `extends` resolution), reporting whether
    /// it came from a directory package.
    ///
    /// Why: the flat `.md` personalization format (`<name>.md`) is the PRIMARY
    /// user surface for `extends:` overlays (DOC-41 §2.5.1), yet the pre-#3055
    /// per-name loader only tried `<name>/agent.toml` and `<name>.toml` — so a
    /// user's `~/.trusty-agents/agents/my-assistant.md` was visible in the
    /// registry roster but FILE-NOT-FOUND at dispatch (every runner loads via
    /// `by_name`/`by_name_async`, not the informational registry). This adds the
    /// missing flat-`.md` tier so the documented flow actually dispatches. The
    /// `from_package` flag feeds [`Self::extends_shadow_fallback`], which must
    /// not let an unresolvable directory package silently shadow a complete flat
    /// `<name>.toml`.
    /// What: tries, in order under `agents_dir()`: (1) the directory package
    /// (`<name>/agent.toml` + `persona.md`), (2) flat `<name>.toml`, (3) flat
    /// `<name>.md` via [`parse_md_agent`](crate::agents::registry::parse_md_agent),
    /// (4) the synchronous claude-mpm `.md` fallback (kept in lock-step with the
    /// async loader's tiers so ancestor resolution is symmetric — see the
    /// async/sync drift note in `by_name_async_unresolved`). Returns
    /// `(config, came_from_package)`.
    /// Test: `by_name_flat_md_extends_dispatches`,
    /// `by_name_package_extends_shadow_falls_back_to_flat` (tests.rs).
    fn by_name_unresolved_src(name: &str) -> Result<(Self, bool)> {
        // #482: Prefer the directory-package format when present.
        let dir = agents_dir();
        if let Some(cfg) = load_agent_package(&dir, name)? {
            return Ok((cfg, true));
        }
        let toml_path = dir.join(format!("{name}.toml"));
        if toml_path.exists() {
            return Ok((Self::load(&toml_path)?, false));
        }
        // #3055: flat `.md` personalization overlay — the primary user surface.
        let md_path = dir.join(format!("{name}.md"));
        if md_path.exists() {
            let cfg = crate::agents::registry::parse_md_agent(&md_path)
                .with_context(|| format!("failed to load agent md {}", md_path.display()))?;
            return Ok((cfg, false));
        }
        // claude-mpm compatibility tier (sync), symmetric with the async loader.
        if let Some(cfg) = crate::agents::claude_mpm_loader::find_agent_sync(name) {
            return Ok((cfg, false));
        }
        // Preserve the historical missing-`<name>.toml` error shape when nothing
        // resolved.
        Ok((Self::load(&toml_path)?, false))
    }

    /// Recover from a directory-package `extends` that could not be resolved by
    /// falling back to a complete flat `<name>.toml` shadowing it (#3055).
    ///
    /// Why: `by_name_unresolved_src` prefers `<name>/agent.toml` over the flat
    /// `<name>.toml`. If the package manifest declares an `extends` that fails
    /// to resolve (target missing / cycle / depth), serving the unresolved,
    /// permission-widened PARTIAL package over a complete flat config is a
    /// silent correctness/security hazard (a `[tools]`-less `izzie/agent.toml`
    /// shadowing a locked-down flat `izzie.toml`). Once the resolver works
    /// end-to-end the normal path resolves the package and this fires only on
    /// genuine failures.
    /// What: when the offending config came from a package AND a flat
    /// `<name>.toml` exists alongside, logs a `warn` naming both paths and loads
    /// the flat file (resolving its OWN `extends` if any, forcing `name` to bind
    /// to the flat file rather than the shadowing package). Otherwise the
    /// original resolution error is returned with context.
    /// Test: `by_name_package_extends_shadow_falls_back_to_flat`.
    fn extends_shadow_fallback(
        name: &str,
        from_package: bool,
        err: crate::agents::extends::AgentExtendsError,
    ) -> Result<Self> {
        let dir = agents_dir();
        let flat = dir.join(format!("{name}.toml"));
        if from_package && flat.exists() {
            tracing::warn!(
                agent = %name,
                package = %dir.join(name).display(),
                flat = %flat.display(),
                error = %err,
                "directory-package `extends` failed to resolve; falling back to the \
                 complete flat <name>.toml (refusing to serve the unresolved partial package)"
            );
            return Self::by_name_flat_toml(name, &flat);
        }
        Err(anyhow::Error::new(err))
            .with_context(|| format!("failed to resolve `extends` chain for agent '{name}'"))
    }

    /// Load ONLY the flat `<name>.toml`, resolving its own `extends` chain.
    ///
    /// Why: the shadow-fallback ([`Self::extends_shadow_fallback`]) must bind
    /// `name` to the flat file, not the package that `by_name_unresolved_src`
    /// would prefer — otherwise the resolver would re-load the broken package.
    /// What: loads `flat` directly; if it declares no `extends`, returns it; if
    /// it does, resolves via a lookup that forces `name` → the flat file and
    /// routes ancestors through the normal per-name loader, then finalizes.
    /// Test: `by_name_package_extends_shadow_falls_back_to_flat`.
    fn by_name_flat_toml(name: &str, flat: &Path) -> Result<Self> {
        let cfg = Self::load(flat)?;
        if cfg.agent.extends.is_none() {
            return Ok(cfg);
        }
        let lookup = |n: &str| {
            if n.eq_ignore_ascii_case(name) {
                Self::load(flat).ok()
            } else {
                Self::by_name_unresolved(n).ok()
            }
        };
        let resolved = crate::agents::extends::resolve(name, &lookup).with_context(|| {
            format!("failed to resolve flat `<name>.toml` extends for '{name}'")
        })?;
        Ok(resolved.finalize_extends())
    }

    /// Re-run model + adapter resolution after an `extends` merge (#3055).
    ///
    /// Why: `extends` resolution ([`crate::agents::extends`]) folds a base and
    /// child at the struct level without touching model resolution. A `.md`
    /// child that omitted `model` inherits the base's, and a child that
    /// declared one carries it raw — either way the merged config must run
    /// through `resolve_model` (for `TAGENT_MODEL_*` / default-env overrides
    /// keyed on the FINAL agent name) and re-derive the provider adapter so it
    /// matches a normally-loaded config.
    /// What: resolves `agent.model` via [`resolve_model`] and rebuilds
    /// `adapter` from the result. Idempotent for an already-resolved model.
    /// Test: `extends_resolved_child_inherits_base_model` (registry tests).
    pub(crate) fn finalize_extends(mut self) -> Self {
        let (resolved, _src) = resolve_model(
            &self.agent.name,
            &self.agent.model,
            self.llm.model_override.as_deref(),
        );
        self.agent.model = resolved;
        self.adapter = Arc::from(adapter_for_model(&self.agent.model));
        self
    }

    /// Built-in default `ctrl` agent config used when no `ctrl.toml` /
    /// `pm.toml` is found on disk (#240, standalone mode).
    ///
    /// Why: When the REPL has no project connected, the controller still
    /// needs an `AgentConfig` to drive the conversational fast path. Bundling
    /// a hardcoded fallback means a fresh checkout works even before the
    /// user creates `~/.trusty-agents/agents/ctrl.toml`.
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
        let (cfg, from_package) = Self::by_name_async_unresolved(name, &dir).await?;
        // #3055: resolve `extends` at load time (DOC-41 §2.5). Base lookups use
        // the sync unresolved loader — which now carries the SAME tier set as
        // the async loader (package → flat toml → flat md → claude-mpm), so a
        // child extending a base only discoverable via the claude-mpm fallback
        // no longer gets an asymmetric `ExtendsNotFound`. The reads are small
        // config files, negligible next to the LLM dispatch that follows.
        if cfg.agent.extends.is_none() {
            return Ok(cfg);
        }
        let lookup = |n: &str| Self::by_name_unresolved(n).ok();
        match crate::agents::extends::resolve(name, &lookup) {
            Ok(resolved) => Ok(resolved.finalize_extends()),
            Err(e) => Self::extends_shadow_fallback(name, from_package, e),
        }
    }

    /// Async single-agent load WITHOUT `extends` resolution (#3055 companion to
    /// [`Self::by_name_unresolved_src`]).
    ///
    /// Why: `by_name_async` needs the raw, still-`extends`-bearing child before
    /// it walks the chain.
    /// What: mirrors [`Self::by_name_unresolved_src`]'s tier ORDER exactly so
    /// sync and async resolve the same agents: directory package → flat
    /// `<name>.toml` → flat `<name>.md` (#3055 personalization overlay) →
    /// claude-mpm `.md` fallback. Returns `(config, came_from_package)`.
    /// Test: `by_name_async_loads_plan_agent`,
    /// `by_name_async_flat_md_extends_dispatches`.
    async fn by_name_async_unresolved(name: &str, dir: &Path) -> Result<(Self, bool)> {
        if let Some(cfg) = load_agent_package(dir, name)? {
            return Ok((cfg, true));
        }
        let path = dir.join(format!("{name}.toml"));
        match tokio::fs::read_to_string(&path).await {
            Ok(raw) => Ok((Self::from_toml_str(&raw, &path)?, false)),
            Err(e) => {
                // #3055: flat `.md` personalization overlay tier, BEFORE the
                // claude-mpm fallback — matching `by_name_unresolved_src`.
                let md_path = dir.join(format!("{name}.md"));
                if md_path.exists() {
                    let cfg =
                        crate::agents::registry::parse_md_agent(&md_path).with_context(|| {
                            format!("failed to load agent md {}", md_path.display())
                        })?;
                    return Ok((cfg, false));
                }
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
                    return Ok((agent.to_agent_config(), false));
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
        // {{TAGENT_VERSION}} → harness version from Cargo.toml.
        cfg.system_prompt.content = cfg
            .system_prompt
            .content
            .replace("{{TAGENT_VERSION}}", env!("CARGO_PKG_VERSION"));
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
/// `TAGENT_CONFIG_DIR` env var with a CWD-relative fallback (MIN-7 / #104).
///
/// Why: Installed binaries rarely share a CWD with the repo; hardcoding a
/// relative path made `trusty-agents` fragile when packaged. Honoring an env var
/// lets operators point the loader at a vendored `config/` alongside the
/// binary without code changes.
/// What: Returns `${TAGENT_CONFIG_DIR}/<name>.toml` when the env var is
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
/// `TAGENT_CONFIG_DIR` lookup keeps the two resolvers consistent.
/// What: Returns `TAGENT_CONFIG_DIR` when set, else the CWD-relative
/// `.trusty-agents/agents` fallback (warning once).
/// Test: Covered by `agent_config_path_honors_env_var`.
fn agents_dir() -> PathBuf {
    match crate::env_compat::env_var("TAGENT_CONFIG_DIR", "OPEN_MPM_CONFIG_DIR") {
        Ok(s) if !s.is_empty() => PathBuf::from(s),
        _ => {
            CONFIG_DIR_WARNED.get_or_init(|| {
                tracing::warn!(
                    "TAGENT_CONFIG_DIR not set; falling back to .trusty-agents/agents/ \
                     (or .trusty-agents/agents/ if .trusty-agents does not exist). \
                     This warning appears once."
                );
            });
            // Prefer .trusty-agents; fall back to legacy .trusty-agents for compatibility.
            let new_dir = PathBuf::from(".trusty-agents/agents");
            if !new_dir.exists() {
                let legacy = PathBuf::from(".trusty-agents/agents");
                if legacy.exists() {
                    return legacy;
                }
            }
            new_dir
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
