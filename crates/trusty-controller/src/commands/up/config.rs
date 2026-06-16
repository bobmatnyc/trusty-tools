//! Boot-policy configuration (DOC-12 §8, #1220).
//!
//! Why: `tctl up` must read boot policy from the #1220 cross-crate convention
//! `~/.trusty-tools/trusty-controller/config.yaml` so a user can toggle
//! `with_mpm` / `claude_code.policy` / `analyze_core` (also editable from the
//! trusty-console config UI). CLI flags win over config, which wins over
//! built-in defaults (DOC-12 §8 precedence table).
//!
//! What: Defines the `BootConfig` deserialisation tree (with the §8 example
//! keys), `load_boot_config()` (reads the YAML, tolerating an absent file), and
//! `ResolvedBootPolicy::resolve()` which folds flags > config > defaults into
//! the effective boot policy the orchestrator runs against.
//!
//! Test: `super::tests` covers absent-file defaults, YAML parsing, and the
//! flag-over-config precedence in `resolve`.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The `[analyze_core]` tri-state from DOC-12 §3.5 / §8.
///
/// Why: STAGE 2's analysis content can block boot (wait for the snapshot), run
/// in the background (default), or be skipped entirely.
///
/// What: Mirrors the `blocking | background | skip` value set; `Default` is
/// `Background` per the §3.5 locked default.
///
/// Test: `super::tests::analyze_mode_parses` and the resolve precedence tests.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AnalyzeMode {
    /// Wait for the core-analysis snapshot before proceeding.
    Blocking,
    /// Run the snapshot in the background, non-gating (DOC-12 §3.5 default).
    #[default]
    Background,
    /// Omit the boot-analysis pass entirely.
    Skip,
}

impl AnalyzeMode {
    /// Parse a CLI `--analyze-core` value into an `AnalyzeMode`.
    ///
    /// Why: clap hands the flag through as an already-validated value enum in
    /// the command layer, but the orchestrator also accepts a string from the
    /// config path; one parser keeps both consistent.
    ///
    /// What: Maps `blocking|background|skip` (case-insensitive) to the variant;
    /// any other value returns `None`.
    ///
    /// Test: `super::tests::analyze_mode_parses`.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "blocking" => Some(Self::Blocking),
            "background" => Some(Self::Background),
            "skip" => Some(Self::Skip),
            _ => None,
        }
    }
}

/// The `boot.claude_code` sub-table (DOC-12 §8 / §6).
///
/// Why: Claude Code upgrade behaviour (whether to check latest, whether to skip
/// the upgrade entirely) is user-tunable, while *consent before applying* stays
/// fixed (RESOLVED Q3).
///
/// What: `policy` is `latest` (check + prompt-to-upgrade) or `present` (accept
/// installed); `skip_upgrade` short-circuits the upgrade step.
///
/// Test: `super::tests::config_parses_claude_code`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ClaudeCodeConfig {
    /// `latest` | `present` (DOC-12 §6 `min_policy` default = `latest`).
    pub policy: String,
    /// Skip the upgrade step even when stale (DOC-12 §6 `--skip-claude-upgrade`).
    pub skip_upgrade: bool,
}

impl Default for ClaudeCodeConfig {
    fn default() -> Self {
        Self {
            policy: "latest".to_owned(),
            skip_upgrade: false,
        }
    }
}

/// The `boot` sub-table of the controller config (DOC-12 §8).
///
/// Why: Groups all boot-policy keys under one namespace mirroring the §8 YAML
/// example.
///
/// What: `with_mpm` (opt-in orchestrator default), `analyze_core` (tri-state),
/// and the nested `claude_code` table.
///
/// Test: `super::tests::config_parses_full`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct BootSection {
    /// §4 opt-in orchestrator default for `tctl up` (flags override).
    pub with_mpm: bool,
    /// §3.5 analyze blocking | background | skip default.
    pub analyze_core: AnalyzeMode,
    /// §6 Claude Code policy sub-table.
    pub claude_code: ClaudeCodeConfig,
}

impl Default for BootSection {
    fn default() -> Self {
        Self {
            with_mpm: false,
            analyze_core: AnalyzeMode::Background,
            claude_code: ClaudeCodeConfig::default(),
        }
    }
}

/// The `[controller]` config section (#1316).
///
/// Why: The #1316 opt-in auto-update CHECK is gated by a config flag distinct
/// from boot policy, so it lives in its own `controller` namespace. The default
/// is OFF — tctl must never probe crates.io in the background, let alone apply
/// anything, unless the operator opts in.
///
/// What: `auto_update_check` enables the notify-only background update check
/// (see `commands::auto_update`). It only *notifies*; applying always requires
/// explicit confirmation via `tctl upgrade`.
///
/// Test: `super::tests::config_parses_controller`, `super::tests::controller_default_off`.
#[derive(Clone, Debug, PartialEq, Eq, Default, Deserialize)]
#[serde(default)]
pub struct ControllerSection {
    /// Enable the opt-in notify-only update check (default `false`).
    pub auto_update_check: bool,
}

/// The controller's on-disk config (DOC-12 §8 / #1220 / #1316).
///
/// Why: The deserialisation root for
/// `~/.trusty-tools/trusty-controller/config.yaml`.
///
/// What: Holds the `boot` section, the optional `analyze_core_targets` override
/// (empty = use the §3.3 default set), and the `controller` section (#1316
/// auto-update opt-in).
///
/// Test: `super::tests::config_parses_full`, `super::tests::config_parses_controller`.
#[derive(Clone, Debug, PartialEq, Eq, Default, Deserialize)]
#[serde(default)]
pub struct BootConfig {
    /// Boot-policy keys (§8).
    pub boot: BootSection,
    /// Override of the §3.3 core-analysis target set (empty = default).
    pub analyze_core_targets: Vec<String>,
    /// Controller-level settings (#1316 auto-update opt-in).
    pub controller: ControllerSection,
}

/// Compute the #1220 controller config path: `~/.trusty-tools/trusty-controller/config.yaml`.
///
/// Why: Centralises the one path the loader reads so tests and the loader agree.
///
/// What: Joins `dirs::home_dir()` with `.trusty-tools/trusty-controller/config.yaml`.
/// Returns `None` only when the home directory cannot be resolved.
///
/// Test: `super::tests::config_path_shape`.
pub fn config_path() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(
        home.join(".trusty-tools")
            .join("trusty-controller")
            .join("config.yaml"),
    )
}

/// Load the boot config from a specific path, tolerating an absent file.
///
/// Why: An absent config file is the common case (zero-config boot) and must
/// fall back to defaults rather than error — boot is the zero-effort path.
///
/// What: Reads `path`; if it does not exist, returns `BootConfig::default()`.
/// A present-but-malformed file is a real error (surfaced to the caller) so a
/// typo is not silently ignored.
///
/// Test: `super::tests::load_absent_is_default`, `super::tests::load_parses_yaml`.
pub fn load_boot_config_from(path: &Path) -> anyhow::Result<BootConfig> {
    if !path.exists() {
        return Ok(BootConfig::default());
    }
    let raw = std::fs::read_to_string(path)?;
    if raw.trim().is_empty() {
        return Ok(BootConfig::default());
    }
    let cfg: BootConfig = serde_yaml::from_str(&raw)?;
    Ok(cfg)
}

/// Load the boot config from the #1220 default path.
///
/// Why: The orchestrator entry point wants a one-call "load my config" that
/// resolves the path itself.
///
/// What: Resolves `config_path()` then delegates to `load_boot_config_from`;
/// returns defaults when the home dir is unresolvable (headless/CI).
///
/// Test: Covered indirectly; the path-resolving variant is exercised in the
/// orchestrator integration test under `super::tests`.
pub fn load_boot_config() -> anyhow::Result<BootConfig> {
    match config_path() {
        Some(p) => load_boot_config_from(&p),
        None => Ok(BootConfig::default()),
    }
}
