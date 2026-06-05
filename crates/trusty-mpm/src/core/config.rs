//! User-facing configuration for the trusty-mpm framework.
//!
//! Why: trusty-mpm had no persistent configuration — every setting was either
//! hard-coded or supplied via environment variable, giving users no canonical
//! way to express preferences like "use haiku for the engineer agent". This
//! module canonicalizes `~/.trusty-mpm/config.toml` as the configuration file
//! and provides a typed loader with graceful fallback (absent file → defaults;
//! malformed file → logged warning + defaults).
//! What: [`MpmConfig`] is the top-level deserialization target for
//! `~/.trusty-mpm/config.toml`; [`MpmConfig::load`] reads and parses it.
//! [`resolve_agent_model`] implements the four-level model precedence used by
//! the session-launch path for issue #390.
//! Test: `config_absent_yields_defaults`, `config_valid_parsed`,
//! `config_malformed_falls_back`, `model_resolution_precedence`.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

// ──────────────────────────────────────────────
// Top-level config sections
// ──────────────────────────────────────────────

/// `[agents]` section — agent discovery sources.
///
/// Why: the framework can pull agents from multiple locations (bundled assets,
/// a user-local directory, an optional registry); this section controls which
/// are active.
/// What: a list of source labels. Recognised values: `"bundled"`, `"user"`,
/// `"registry"`. Unknown values are ignored.
/// Test: `config_valid_parsed` checks the parsed sources list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AgentsConfig {
    /// Ordered list of agent sources to consult.
    ///
    /// Typical default: `["bundled", "user"]`.
    #[serde(default)]
    pub sources: Vec<String>,
}

/// Per-tier model aliases under `[models]`.
///
/// Why: users want to write `tier = "haiku"` in their config rather than a
/// full model id like `claude-haiku-4-5`; the tier table maps short names to
/// the canonical ids used at launch time.
/// What: an optional string for each Claude model family; `None` means "use
/// the framework default for that tier".
/// Test: `config_valid_parsed` checks alias resolution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TierAliases {
    /// Full model id (or alias) for the Haiku family.
    pub haiku: Option<String>,
    /// Full model id (or alias) for the Sonnet family.
    pub sonnet: Option<String>,
    /// Full model id (or alias) for the Opus family.
    pub opus: Option<String>,
}

/// `[models]` section — model selection and tier aliases.
///
/// Why: the framework needs a place to record which model to use per agent
/// (for issue #390) and what the user considers the canonical full id for each
/// tier alias.
/// What: `agents` maps agent names to a model id or tier alias; `tiers`
/// provides the alias → id expansion table; `default` is the fallback when
/// neither the agent override nor the frontmatter supplies a model.
/// Test: `config_valid_parsed`, `model_resolution_precedence`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ModelsConfig {
    /// Per-agent model overrides.
    ///
    /// Key: agent name (e.g. `"engineer"`, `"rust-engineer"`).
    /// Value: model id or tier alias (`"haiku"`, `"sonnet"`, `"opus"`,
    /// `"claude-sonnet-4-5"`, …).
    #[serde(default)]
    pub agents: HashMap<String, String>,

    /// Tier alias → canonical model id expansion table.
    ///
    /// Allows users to pin `haiku = "claude-haiku-4-5"` so short aliases in
    /// `agents.*` resolve to a specific model version.
    #[serde(default)]
    pub tiers: TierAliases,

    /// Default model used when no per-agent override or frontmatter model applies.
    pub default: Option<String>,
}

/// `[skills]` section — skill source configuration.
///
/// Why: forward-compatible placeholder so users can add skill-related config
/// in `config.toml` without breaking the loader.
/// What: currently a no-op struct; future versions will add `sources` and
/// per-skill toggles.
/// Test: `config_valid_parsed` confirms the section deserializes cleanly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SkillsConfig {
    /// Ordered list of skill sources (e.g. `["bundled", "user"]`).
    #[serde(default)]
    pub sources: Vec<String>,
}

/// `[pm]` section — PM-layer toggles.
///
/// Why: the circuit-breaker and other PM-layer features need user-facing
/// on/off knobs; this section provides them without requiring env-var
/// spelunking.
/// What: boolean toggles for the PM-layer features. Defaults leave all
/// features at their compiled-in settings.
/// Test: `config_valid_parsed` checks circuit-breaker toggle parsing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PmConfig {
    /// Enable or disable the agent circuit breaker.
    ///
    /// `None` → use the compiled-in default (enabled). `Some(false)` disables
    /// it globally (not recommended for production).
    pub circuit_breaker: Option<bool>,
}

// ──────────────────────────────────────────────
// Root config
// ──────────────────────────────────────────────

/// The full contents of `~/.trusty-mpm/config.toml`.
///
/// Why: a single top-level struct makes `toml::from_str` the only parsing
/// call; every section has a `Default` impl so absent sections yield
/// sensible values without errors.
/// What: four optional sections (`[agents]`, `[models]`, `[skills]`,
/// `[pm]`); absent sections produce their `Default`.
/// Test: `config_absent_yields_defaults`, `config_valid_parsed`,
/// `config_malformed_falls_back`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MpmConfig {
    /// `[agents]` — agent discovery sources.
    #[serde(default)]
    pub agents: AgentsConfig,

    /// `[models]` — per-agent and tier model configuration.
    #[serde(default)]
    pub models: ModelsConfig,

    /// `[skills]` — skill source configuration.
    #[serde(default)]
    pub skills: SkillsConfig,

    /// `[pm]` — PM-layer feature toggles.
    #[serde(default)]
    pub pm: PmConfig,
}

// ──────────────────────────────────────────────
// Loader
// ──────────────────────────────────────────────

impl MpmConfig {
    /// Load the user config from `~/.trusty-mpm/config.toml`.
    ///
    /// Why: every daemon and CLI path that cares about user preferences calls
    /// this exactly once at startup so configuration is always available via
    /// [`DaemonState`](crate::daemon::state::DaemonState) or a passed-in
    /// reference.
    /// What: reads `<root>/config.toml`; a missing file silently returns
    /// [`MpmConfig::default`]; a malformed file logs a warning at `tracing::warn`
    /// level and returns [`MpmConfig::default`] so startup is never aborted by
    /// a bad config.
    /// Test: `config_absent_yields_defaults`, `config_valid_parsed`,
    /// `config_malformed_falls_back`.
    pub fn load(root: &Path) -> Self {
        let path = root.join("config.toml");
        match std::fs::read_to_string(&path) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                // Absent config is the expected state on a fresh install.
                tracing::debug!("no config.toml found at {}; using defaults", path.display());
                Self::default()
            }
            Err(err) => {
                tracing::warn!(
                    "could not read config.toml at {}: {err}; using defaults",
                    path.display()
                );
                Self::default()
            }
            Ok(raw) => match toml::from_str::<Self>(&raw) {
                Ok(cfg) => {
                    tracing::debug!("loaded config from {}", path.display());
                    cfg
                }
                Err(err) => {
                    tracing::warn!(
                        "config.toml at {} is malformed: {err}; using defaults",
                        path.display()
                    );
                    Self::default()
                }
            },
        }
    }

    /// Load the user config from the canonical `~/.trusty-mpm/` root.
    ///
    /// Why: most callers want the real user config, not a test-time override;
    /// this convenience method resolves the home directory and delegates to
    /// [`load`](Self::load).
    /// What: calls `dirs::home_dir()` to find `~/.trusty-mpm/`; if home is
    /// unavailable (stripped CI), returns [`MpmConfig::default`].
    /// Test: covered indirectly by `config_absent_yields_defaults` (which passes
    /// a temp dir to [`load`]).
    pub fn load_default() -> Self {
        match dirs::home_dir() {
            Some(home) => Self::load(&home.join(".trusty-mpm")),
            None => {
                tracing::warn!("home directory unavailable; using default config");
                Self::default()
            }
        }
    }

    /// Expand a tier alias or pass through a full model id.
    ///
    /// Why: users write short aliases (`"haiku"`, `"sonnet"`, `"opus"`) in
    /// `config.toml`; callers need the canonical model id before passing
    /// `--model` to `claude`.
    /// What: checks `[models.tiers]` for `"haiku"`, `"sonnet"`, `"opus"` and
    /// substitutes the configured id; otherwise returns the input unchanged.
    /// Test: `tier_alias_expansion`.
    pub fn expand_model_alias<'a>(&'a self, alias: &'a str) -> &'a str {
        match alias {
            "haiku" => self
                .models
                .tiers
                .haiku
                .as_deref()
                .unwrap_or("claude-haiku-4-5"),
            "sonnet" => self
                .models
                .tiers
                .sonnet
                .as_deref()
                .unwrap_or("claude-sonnet-4-5"),
            "opus" => self
                .models
                .tiers
                .opus
                .as_deref()
                .unwrap_or("claude-opus-4-5"),
            "auto" => "claude-sonnet-4-5",
            other => other,
        }
    }
}

// ──────────────────────────────────────────────
// Model resolution (issue #390)
// ──────────────────────────────────────────────

/// Resolve the Claude model id to use when launching an agent session.
///
/// Why: Claude Code silently ignores the `model:` field in agent frontmatter,
/// so trusty-mpm must inject the correct model via `--model` at launch time.
/// This function implements the four-level precedence so every call site
/// (CLI launch, daemon session-start, MCP agent_delegate) uses the same
/// resolution logic.
/// What: evaluates four sources in descending priority order:
///
/// 1. `explicit` — a model string explicitly specified by the caller (e.g.,
///    from the `tm session start --model` flag). If `Some`, wins immediately.
/// 2. `config.models.agents.<agent_name>` — the per-agent override in
///    `~/.trusty-mpm/config.toml`.
/// 3. `frontmatter_model` — the `model:` field from the agent's frontmatter
///    (as read from the composed agent `.md` file).
/// 4. `config.models.default` or the built-in tier default (`"sonnet"`).
///
/// All resolved values are expanded through [`MpmConfig::expand_model_alias`]
/// so short aliases (`"haiku"`, `"sonnet"`, `"opus"`) become the canonical
/// model id strings Claude Code accepts.
/// Test: `model_resolution_precedence`.
pub fn resolve_agent_model(
    config: &MpmConfig,
    agent_name: &str,
    frontmatter_model: Option<&str>,
    explicit: Option<&str>,
) -> String {
    // 1. Explicit override always wins.
    if let Some(m) = explicit {
        return config.expand_model_alias(m).to_string();
    }

    // 2. Per-agent config entry.
    if let Some(m) = config.models.agents.get(agent_name) {
        return config.expand_model_alias(m).to_string();
    }

    // 3. Frontmatter model hint.
    if let Some(m) = frontmatter_model {
        return config.expand_model_alias(m).to_string();
    }

    // 4. Config default or built-in fallback.
    let fallback = config
        .models
        .default
        .as_deref()
        .unwrap_or("claude-sonnet-4-5");
    config.expand_model_alias(fallback).to_string()
}

// ──────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: write `content` to `<dir>/config.toml` and load from `dir`.
    fn load_from_str(dir: &Path, content: &str) -> MpmConfig {
        std::fs::write(dir.join("config.toml"), content).unwrap();
        MpmConfig::load(dir)
    }

    #[test]
    fn config_absent_yields_defaults() {
        // An absent config.toml must silently yield the default struct.
        let dir = tempfile::TempDir::new().unwrap();
        let cfg = MpmConfig::load(dir.path());
        assert_eq!(cfg, MpmConfig::default());
        assert!(cfg.agents.sources.is_empty());
        assert!(cfg.models.agents.is_empty());
    }

    #[test]
    fn config_valid_parsed() {
        let dir = tempfile::TempDir::new().unwrap();
        let toml = r#"
[agents]
sources = ["bundled", "user"]

[models]
default = "sonnet"

[models.agents]
engineer = "haiku"
rust-engineer = "opus"

[models.tiers]
haiku = "claude-haiku-4-5"
sonnet = "claude-sonnet-4-5"
opus = "claude-opus-4-5"

[skills]
sources = ["bundled"]

[pm]
circuit_breaker = true
"#;
        let cfg = load_from_str(dir.path(), toml);
        assert_eq!(cfg.agents.sources, vec!["bundled", "user"]);
        assert_eq!(
            cfg.models.agents.get("engineer").map(|s| s.as_str()),
            Some("haiku")
        );
        assert_eq!(
            cfg.models.agents.get("rust-engineer").map(|s| s.as_str()),
            Some("opus")
        );
        assert_eq!(cfg.models.tiers.haiku.as_deref(), Some("claude-haiku-4-5"));
        assert_eq!(cfg.models.default.as_deref(), Some("sonnet"));
        assert_eq!(cfg.skills.sources, vec!["bundled"]);
        assert_eq!(cfg.pm.circuit_breaker, Some(true));
    }

    #[test]
    fn config_malformed_falls_back() {
        // A malformed config.toml must log (tested by absence of panic) and
        // return defaults — the daemon must not crash on a bad file.
        let dir = tempfile::TempDir::new().unwrap();
        let cfg = load_from_str(dir.path(), "this is not toml {{{{");
        assert_eq!(cfg, MpmConfig::default());
    }

    #[test]
    fn config_partial_sections_are_fine() {
        // Users should be able to configure only the sections they care about.
        let dir = tempfile::TempDir::new().unwrap();
        let toml = r#"
[models.agents]
engineer = "haiku"
"#;
        let cfg = load_from_str(dir.path(), toml);
        assert_eq!(
            cfg.models.agents.get("engineer").map(|s| s.as_str()),
            Some("haiku")
        );
        // Other sections must yield defaults.
        assert!(cfg.agents.sources.is_empty());
        assert!(cfg.pm.circuit_breaker.is_none());
    }

    #[test]
    fn tier_alias_expansion() {
        let dir = tempfile::TempDir::new().unwrap();
        let toml = r#"
[models.tiers]
haiku = "claude-haiku-4-5"
sonnet = "claude-sonnet-4-7"
opus = "claude-opus-4-7"
"#;
        let cfg = load_from_str(dir.path(), toml);
        assert_eq!(cfg.expand_model_alias("haiku"), "claude-haiku-4-5");
        assert_eq!(cfg.expand_model_alias("sonnet"), "claude-sonnet-4-7");
        assert_eq!(cfg.expand_model_alias("opus"), "claude-opus-4-7");
        // Full model ids pass through unchanged.
        assert_eq!(cfg.expand_model_alias("claude-opus-4-7"), "claude-opus-4-7");
        // "auto" maps to sonnet.
        assert_eq!(cfg.expand_model_alias("auto"), "claude-sonnet-4-5");
    }

    #[test]
    fn tier_alias_defaults_when_not_configured() {
        // Without explicit tier config, built-in defaults must apply.
        let cfg = MpmConfig::default();
        assert_eq!(cfg.expand_model_alias("haiku"), "claude-haiku-4-5");
        assert_eq!(cfg.expand_model_alias("sonnet"), "claude-sonnet-4-5");
        assert_eq!(cfg.expand_model_alias("opus"), "claude-opus-4-5");
    }

    #[test]
    fn model_resolution_precedence() {
        let dir = tempfile::TempDir::new().unwrap();
        let toml = r#"
[models]
default = "sonnet"

[models.agents]
engineer = "haiku"
"#;
        let cfg = load_from_str(dir.path(), toml);

        // 1. Explicit override wins over everything.
        let m = resolve_agent_model(&cfg, "engineer", Some("opus"), Some("claude-opus-4-5"));
        assert_eq!(m, "claude-opus-4-5");

        // 2. Config per-agent override wins over frontmatter.
        let m = resolve_agent_model(&cfg, "engineer", Some("opus"), None);
        // "engineer" maps to "haiku" → default haiku id.
        assert_eq!(m, "claude-haiku-4-5");

        // 3. Frontmatter hint wins over config default.
        let m = resolve_agent_model(&cfg, "unknown-agent", Some("opus"), None);
        assert_eq!(m, "claude-opus-4-5");

        // 4. Config default is the final fallback for unknown agents.
        let m = resolve_agent_model(&cfg, "unknown-agent", None, None);
        // "sonnet" tier default expands.
        assert_eq!(m, "claude-sonnet-4-5");

        // 5. Built-in fallback when neither config default nor anything else matches.
        let cfg_empty = MpmConfig::default();
        let m = resolve_agent_model(&cfg_empty, "nobody", None, None);
        assert_eq!(m, "claude-sonnet-4-5");
    }
}
