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

use crate::core::sm::config::SessionManagerConfig;

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

/// `[manifest]` section — harness-manifest catalog source (HR-2 / DOC-17).
///
/// Why: HR-2 sources the catalog-layer harness manifest from a configurable
/// claude-mpm checkout. The repo/ref/TTL are already controllable via the
/// `TRUSTY_MPM_CATALOG_REPO` / `_REF` / `_TTL_HOURS` env vars
/// (`crate::content::CatalogSync`); this section gives operators a persistent,
/// file-based way to express the same choice (e.g. point at a user fork) without
/// exporting env vars on every launch. It is read by the catalog-sync
/// construction path; absent values fall back to the env vars, then the
/// compiled-in defaults.
/// What: an optional catalog repo URL, git ref, and TTL (hours). All `None` by
/// default, so an absent section changes nothing.
/// Test: `config_manifest_section_parses`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ManifestConfig {
    /// Catalog repo URL (overrides `TRUSTY_MPM_CATALOG_REPO`'s default).
    pub repo: Option<String>,
    /// Catalog git ref (overrides `TRUSTY_MPM_CATALOG_REF`'s default).
    pub git_ref: Option<String>,
    /// Catalog cache TTL in hours (overrides `TRUSTY_MPM_CATALOG_TTL_HOURS`).
    pub ttl_hours: Option<u64>,
}

/// `[idle_auto_stop]` section — opt-in idle auto-suspend feature (#1816).
///
/// Why: long-lived idle sessions consume Claude Max rate-limit slots and cloud
/// costs without doing useful work. This section lets operators enable a
/// background reaper that automatically stops idle sessions (keeping workspaces
/// intact and resumable) and decommissions done sessions.
/// What: boolean `enabled` flag (default `false`, zero-change), a `dry_run`
/// report-only gate (default `true` — mirrors trusty-search's auto-prune
/// convention, #1782/#1783: even after you enable the loop it only LOGS the
/// stop/decommission it *would* perform until you opt in by setting
/// `dry_run = false`), poll interval, and consecutive-hit thresholds for the
/// stop and decommission decisions.
/// Test: `config_idle_auto_stop_defaults`, `config_idle_auto_stop_section_parses`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdleAutoStopConfig {
    /// Whether the idle auto-stop background loop is active.
    ///
    /// `false` (default) → feature is OFF, zero behavior change.
    /// `true` → background loop polls sessions and classifies idle ones.
    #[serde(default)]
    pub enabled: bool,

    /// Report-only (dry-run) gate — the second, teardown-safe opt-in.
    ///
    /// `true` (default) → when the loop is enabled it only LOGS the
    /// `stop`/`decommission` it *would* perform; no session is ever torn down.
    /// `false` → the loop actually stops idle sessions and decommissions done
    /// ones. This mirrors trusty-search's auto-prune "report-only by default,
    /// opt-in to actually act" convention (#1782): enabling the feature and
    /// enacting destructive teardown are two separate, deliberate steps so an
    /// operator can watch the classifier's decisions for a few cycles before
    /// letting it reap real sessions (#1783).
    #[serde(default = "IdleAutoStopConfig::default_dry_run")]
    pub dry_run: bool,

    /// How often (in seconds) to poll Active sessions and classify them.
    ///
    /// Default: 300 s (5 min). At `idle_consecutive_threshold = 3` this means
    /// sessions are stopped after ~15 min of continuous idleness.
    #[serde(default = "IdleAutoStopConfig::default_poll_interval_secs")]
    pub poll_interval_secs: u64,

    /// Number of consecutive `Idle` verdicts before a session is stopped.
    ///
    /// Default: 3 (at 5-min poll interval → ~15 min of continuous idleness).
    /// Reset to 0 whenever the verdict is NOT `Idle`.
    #[serde(default = "IdleAutoStopConfig::default_idle_consecutive_threshold")]
    pub idle_consecutive_threshold: u32,

    /// Number of consecutive `Done` verdicts before a session is decommissioned.
    ///
    /// Default: 1 (decommission on the first confirmed `Done` verdict).
    #[serde(default = "IdleAutoStopConfig::default_done_consecutive_threshold")]
    pub done_consecutive_threshold: u32,
}

impl IdleAutoStopConfig {
    fn default_dry_run() -> bool {
        true
    }
    fn default_poll_interval_secs() -> u64 {
        300
    }
    fn default_idle_consecutive_threshold() -> u32 {
        3
    }
    fn default_done_consecutive_threshold() -> u32 {
        1
    }
}

impl Default for IdleAutoStopConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            dry_run: Self::default_dry_run(),
            poll_interval_secs: Self::default_poll_interval_secs(),
            idle_consecutive_threshold: Self::default_idle_consecutive_threshold(),
            done_consecutive_threshold: Self::default_done_consecutive_threshold(),
        }
    }
}

/// `[catchup]` section — incremental catch-up runtime (DOC-28 / #1762).
///
/// Why: the catch-up runtime is opt-in and its behaviour (which sources to
/// query, how many items to surface) should be configurable without editing
/// code. This section provides a persistent, file-based alternative to env vars.
/// What: boolean and numeric fields controlling the three catch-up sources
/// (paused sessions, git commits, palace drawers) and the item limits for each.
/// `auto` enables automatic injection of the catch-up digest when a new
/// native `tm` session starts.
/// Test: `config_catchup_defaults_parse`, `config_catchup_section_parses`.
///
// CUTOVER BRIDGE — remove post-migration (#1762)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatchupConfig {
    /// Whether to automatically inject the catch-up digest on session start.
    ///
    /// `true` → catch-up is generated and appended as seed context when a new
    /// `tm session start` is executed. `false` → only the manual
    /// `tm session catchup` command triggers a digest.
    pub auto: bool,
    /// Whether to include git commit history in the digest.
    pub include_git: bool,
    /// Whether to include palace drawer inspection in the digest.
    pub include_palace: bool,
    /// Maximum number of recent git commits to surface.
    pub git_limit: usize,
    /// Maximum number of recent palace drawers to surface.
    pub drawer_limit: usize,
}

impl Default for CatchupConfig {
    fn default() -> Self {
        Self {
            auto: true,
            include_git: true,
            include_palace: true,
            git_limit: 50,
            drawer_limit: 15,
        }
    }
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

/// `[hooks]` section — per-hook opt-outs for the project-tier hook block
/// [`crate::core::session_launch`] writes at every session launch.
///
/// Why (#5034): the `UserPromptSubmit` → `trusty-memory prompt-context` hook
/// injects a measured ~1,211 tokens (median 1,252, range 693–1,438) into EVERY
/// prompt — roughly 24,000 tokens across a 20-turn session, recurring. Issue
/// #4904 measured the precision of that spend at 0 clean matches out of 17
/// curated facts across 1,114 real firings, so an operator may reasonably want
/// it off until relevance improves. Hand-editing `.claude/settings.json` cannot
/// achieve that: the launch writer re-adds its own entries on every launch.
/// This section is the supported off switch.
/// What: boolean toggles, all defaulting to `true`, so an absent `[hooks]`
/// section (and an absent key within a present section) leaves the shipped
/// hook block exactly as it was.
/// Test: `config_hooks_defaults_to_enabled`, `config_hooks_prompt_context_off`,
/// `config_absent_yields_defaults`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HooksConfig {
    /// Write the `UserPromptSubmit` → `trusty-memory prompt-context` hook.
    ///
    /// `true` (default) → unchanged behavior. `false` → the entry is omitted
    /// from the project-tier `.claude/settings.json`, and any entry a previous
    /// launch wrote there is stripped. Every other hook — `SessionStart`, the
    /// PM guard, and the six-event lifecycle triad — is unaffected either way.
    pub prompt_context: bool,
}

impl Default for HooksConfig {
    fn default() -> Self {
        Self {
            prompt_context: true,
        }
    }
}

// ──────────────────────────────────────────────
// Root config
// ──────────────────────────────────────────────

/// The full contents of `~/.trusty-mpm/config.toml`.
///
/// Why: a single top-level struct makes `toml::from_str` the only parsing
/// call; every section has a `Default` impl so absent sections yield
/// sensible values without errors.
/// What: five optional sections (`[agents]`, `[models]`, `[skills]`,
/// `[pm]`, `[session_manager]`); absent sections produce their `Default`.
/// The `[session_manager]` section (DOC-14 spec §10) defaults to disabled, so
/// its mere presence in the struct never changes runtime behavior.
/// Test: `config_absent_yields_defaults`, `config_valid_parsed`,
/// `config_malformed_falls_back`, `config_session_manager_*`.
///
/// Note: `Eq` is intentionally NOT derived — [`SessionManagerConfig`] carries a
/// floating-point `temperature`, and `f32` is not `Eq`. `PartialEq` is retained
/// for the equality assertions in tests.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
// #4837: one container-level `default` replaces the twelve identical
// per-field attributes this struct used to carry — same semantics (the
// derived `Default` is field-wise), and it keeps `config.rs` under the
// 500-SLOC production cap as new sections are added.
#[serde(default)]
pub struct MpmConfig {
    /// `[agents]` — agent discovery sources.
    pub agents: AgentsConfig,

    /// `[models]` — per-agent and tier model configuration.
    pub models: ModelsConfig,

    /// `[skills]` — skill source configuration.
    pub skills: SkillsConfig,

    /// `[manifest]` — harness-manifest catalog source (HR-2 / DOC-17).
    pub manifest: ManifestConfig,

    /// `[pm]` — PM-layer feature toggles.
    pub pm: PmConfig,

    /// `[hooks]` — per-hook opt-outs for the project-tier hook block (#5034).
    ///
    /// Absent section → every hook enabled, byte-identical to the pre-#5034
    /// write. Set `prompt_context = false` to suppress the per-prompt
    /// `trusty-memory prompt-context` injection.
    pub hooks: HooksConfig,

    /// `[session_manager]` — Session Manager agent config (DOC-14 §10).
    ///
    /// Defaults to `enabled = false`; absent or partial sections parse to spec
    /// defaults and leave the legacy overseer path untouched.
    pub session_manager: SessionManagerConfig,

    /// `[control_plane]` — SESSCTL auth + cost guardrails (SPEC-SESSCTL-01 §9,
    /// WI-5 #1596).
    ///
    /// Absent or partial section → spec defaults (concurrency cap 5, launch
    /// stagger 2000 ms, auth timeout 30 s, LLM classifier off). The daemon
    /// injects this into the control-plane `SessionRegistry`.
    pub control_plane: crate::control::config::ControlPlaneConfig,

    /// `[style]` — active output-style selection (HR-4 / DOC-17).
    ///
    /// Absent section → professional default (`trusty-mpm`). See
    /// [`crate::core::output_style::StyleConfig`].
    pub style: crate::core::output_style::StyleConfig,

    /// `[catchup]` — incremental catch-up runtime (DOC-28 / #1762).
    ///
    /// Absent section → all defaults (auto-inject on, all sources enabled,
    /// git_limit=50, drawer_limit=15). Present only during the migration
    /// window.
    // CUTOVER BRIDGE — remove post-migration (#1762)
    pub catchup: CatchupConfig,

    /// `[idle_auto_stop]` — opt-in idle auto-suspend feature (#1816).
    ///
    /// Absent section → `enabled = false` (feature OFF, zero behavior change).
    /// Set `enabled = true` to activate the background idle-reaper loop.
    pub idle_auto_stop: IdleAutoStopConfig,

    /// `[idle_nudge]` — opt-in auto-nudge for idle-parked sessions (#2621).
    ///
    /// Absent section → `enabled = false` (feature OFF, zero behavior change).
    /// Set `enabled = true` to let the daemon nudge parked managed sessions. The
    /// section type lives in [`crate::core::idle_nudge`] alongside the pure
    /// decision logic and ledger it configures.
    pub idle_nudge: crate::core::idle_nudge::IdleNudgeConfig,

    /// `[agent_cost]` — per-subagent context ceiling (#4837).
    ///
    /// Absent section → WARN-ONLY: warn at 250k, no hard stop (`max_tokens =
    /// 0`). The stop is opt-in because the measured transcript distribution
    /// (p50 136k / p90 268k / p95 323k, 3.0% at or above 400k) puts a 400k
    /// ceiling only ~1.24x above p95 — it would deny roughly one dispatch in
    /// 33. Set `max_tokens` to enable it; set `enabled = false` to silence the
    /// warning too. The section type lives in [`crate::core::agent_cost`]
    /// alongside the pure policy it configures.
    pub agent_cost: crate::core::agent_cost::AgentCostConfig,
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
                    // #5207: this parse is deliberately LENIENT (see
                    // `core::config_keys` for why `deny_unknown_fields` here
                    // would be a regression), so a typo'd key is dropped. Report
                    // it instead of dropping it in silence.
                    if let Some(doc) = crate::core::config_keys::toml_document(&raw) {
                        crate::core::config_keys::report_unknown_keys(
                            &path.display().to_string(),
                            &doc,
                            &cfg,
                        );
                    }
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

    /// Load the config and fold in every OTHER default-model layer (#5207).
    ///
    /// Why: `TrustyToolsConfig::default_model` was orphaned — the trusty-console
    /// Config tab and the `config_write` MCP tool both wrote it, under a field
    /// whose own placeholder reads "(unset — uses ~/.trusty-mpm/config.toml)",
    /// and nothing ever read it back. Setting it did nothing. Rather than delete
    /// an operator-facing field three surfaces already expose, this folds it into
    /// the ONE model chain [`resolve_agent_model`] already terminates in, which
    /// is what the owner ruling means by unitary: if it is configurable, every
    /// system uses it. Folding at LOAD time rather than adding a parameter to
    /// [`resolve_agent_model`] is what makes it reach every launch path — the PM
    /// launch, agent delegation, and the daemon — without touching their
    /// signatures.
    /// What: precedence for the effective `models.default`, highest first — the
    /// project's committed `.trusty-mpm.toml`, then
    /// `~/.trusty-tools/trusty-mpm/config.yaml`'s `default_model`, then this
    /// file's own `[models] default`. The YAML sits ABOVE the TOML because that
    /// is the contract its editor already advertises. More SPECIFIC settings — an
    /// explicit `--model`, a per-agent override, agent frontmatter — still win
    /// over any of these, since they are all defaults.
    /// `project_dir` is `None` for a launch with no resolved project.
    /// Test: `load_effective_applies_the_project_layer`,
    /// `project_default_model_tops_the_chain`,
    /// `yaml_default_model_beats_toml_default`,
    /// `default_model_layers_are_a_no_op_when_unset`.
    pub fn load_effective(root: &Path, project_dir: Option<&Path>) -> Self {
        Self::load(root).with_outer_default_model_layers(project_dir)
    }

    /// [`load_effective`](Self::load_effective) against the canonical
    /// `~/.trusty-mpm/` root.
    ///
    /// Why: `launch()` needs the effective config but must not name
    /// `FrameworkPaths::default()` itself — the #4203 source-text guard
    /// (`launch_paths_prepare_through_the_isolated_seam`) forbids that symbol in
    /// the CLI launch paths, because a self-resolved framework root there is how
    /// the deploy-tier bug got reintroduced twice. Resolving the root inside the
    /// config module keeps the guard meaningful instead of merely satisfied.
    /// What: delegates to [`load_default`](Self::load_default) — same
    /// home-directory handling, including the stripped-CI fallback — then folds
    /// on the project and host layers.
    /// Test: `load_effective_applies_the_project_layer` covers the layering;
    /// the root resolution is [`load_default`]'s.
    pub fn load_effective_default(project_dir: Option<&Path>) -> Self {
        Self::load_default().with_outer_default_model_layers(project_dir)
    }

    /// Fold the project and host `default_model` layers onto an already-loaded
    /// config.
    ///
    /// Why: shared by [`load_effective`](Self::load_effective) and
    /// [`load_effective_default`](Self::load_effective_default) so the two entry
    /// points cannot drift in precedence — only in how they find the root.
    /// What: reads the project's `.trusty-mpm.toml` and the host YAML, then
    /// applies [`with_default_model_layers`](Self::with_default_model_layers).
    /// Test: `load_effective_applies_the_project_layer`.
    #[must_use]
    fn with_outer_default_model_layers(self, project_dir: Option<&Path>) -> Self {
        let project_default = project_dir
            .and_then(crate::core::project_config::load_or_report)
            .and_then(|c| c.default_model);
        let host_default =
            crate::core::trusty_tools_config::TrustyToolsConfig::load().default_model;
        self.with_default_model_layers(project_default.as_deref(), host_default.as_deref())
    }

    /// Overlay the project and host default-model layers onto `[models] default`.
    ///
    /// Why: split from [`load_effective`] so the precedence is assertable without
    /// a home directory or a real config file.
    /// What: `project` wins over `host`, which wins over whatever `[models]
    /// default` already held. Both absent → unchanged.
    /// Test: `project_default_model_tops_the_chain`,
    /// `yaml_default_model_beats_toml_default`,
    /// `default_model_layers_are_a_no_op_when_unset`.
    #[must_use]
    pub fn with_default_model_layers(mut self, project: Option<&str>, host: Option<&str>) -> Self {
        if let Some(m) = project.or(host) {
            self.models.default = Some(m.to_string());
        }
        self
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
#[path = "config_tests.rs"]
mod tests;
