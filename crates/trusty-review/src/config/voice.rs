//! Voice package configuration loading for `ReviewConfig`.
//!
//! Why: extracted from `config/mod.rs` to keep that file under the 500-line
//! cap (#610) after adding voice support (#754/#756).
//! What: `VoiceFileConfig` (the TOML `[voice]` table) and the two loading
//! helpers (`load_voice_package`, `load_voice_principles`).
//! Test: `voice_package_from_env`, `voice_principles_defaults_to_true`,
//! `voice_principles_env_disable` in config/config_tests.rs.

use serde::Deserialize;

/// `[voice]` section of the TOML config file.
///
/// Why: voice package selection is opt-in; storing it in the config file lets
/// teams configure a shared voice without setting env vars on every machine.
/// What: `package` names the voice to load (e.g. `"duetto"`); `principles`
/// toggles the universal principles layer (defaults to `true`).
/// Test: covered indirectly by `ReviewConfig::from_env_and_file`.
#[derive(Debug, Default, Deserialize)]
pub struct VoiceFileConfig {
    /// Name of the voice package to load (e.g. `"duetto"`).
    /// `None` or empty = no voice package.
    #[serde(default)]
    pub package: Option<String>,
    /// Whether to enable the universal best-practices principles layer.
    /// Defaults to `true` when unset (None).
    #[serde(default)]
    pub principles: Option<bool>,
}

/// Resolve the voice package name from env var or config file.
///
/// Why: `TRUSTY_REVIEW_VOICE_PACKAGE` env var wins over the config file
/// `[voice] package` key, following the precedence used by all other fields.
/// What: returns `Some(name)` when either source specifies a non-empty name;
/// `None` when both are absent or empty (no voice package selected).
/// Test: `voice_package_from_env`, `voice_package_from_config_file`.
pub fn load_voice_package(file_voice: Option<&VoiceFileConfig>) -> Option<String> {
    // Env var takes precedence.
    if let Ok(val) = std::env::var("TRUSTY_REVIEW_VOICE_PACKAGE") {
        let trimmed = val.trim().to_string();
        if !trimmed.is_empty() {
            return Some(trimmed);
        }
    }
    // Fall back to config file.
    file_voice
        .and_then(|v| v.package.as_ref())
        .filter(|s| !s.trim().is_empty())
        .cloned()
}

/// Resolve whether the principles layer is enabled from env var or config file.
///
/// Why: `TRUSTY_REVIEW_PRINCIPLES=false` lets operators opt out of the
/// principles layer; defaults to `true` per issue #756 (universal/safe).
/// What: env var overrides config file; both override the default `true`.
/// Test: `voice_principles_defaults_to_true`, `voice_principles_env_disable`.
pub fn load_voice_principles(file_voice: Option<&VoiceFileConfig>) -> bool {
    // Env var: "false" or "0" disables; anything else (incl. absent) keeps on.
    if let Ok(val) = std::env::var("TRUSTY_REVIEW_PRINCIPLES") {
        let lower = val.trim().to_lowercase();
        if lower == "false" || lower == "0" || lower == "no" {
            return false;
        }
        if lower == "true" || lower == "1" || lower == "yes" {
            return true;
        }
    }
    // Config file `[voice] principles = false`.
    if let Some(vf) = file_voice
        && let Some(enabled) = vf.principles
    {
        return enabled;
    }
    // Default: ON.
    true
}
