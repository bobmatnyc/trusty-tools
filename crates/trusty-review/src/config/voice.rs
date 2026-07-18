//! Voice package configuration loading for `ReviewConfig`.
//!
//! Why: extracted from `config/mod.rs` to keep that file under the 500-line
//! cap (#610) after adding voice support (#754/#756); extended with a
//! repo-scoped precedence tier for per-project config (#2995).
//! What: `VoiceFileConfig` (the TOML `[voice]` table) and the two loading
//! helpers (`load_voice_package`, `load_voice_principles`), each accepting an
//! optional repo-discovered `VoiceFileConfig` that outranks the env var.
//! Test: inline `mod tests` below.

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

/// Resolve the voice package name from repo file, env var, or config file.
///
/// Why: `repo_voice` (parsed from an auto-discovered project
/// `.trusty-review.toml`, issue #2995) wins over `TRUSTY_REVIEW_VOICE_PACKAGE`
/// so a project's committed voice selection is not silently overridden by a
/// developer's ambient env var; `TRUSTY_REVIEW_VOICE_PACKAGE` continues to win
/// over `file_voice` (the explicit `--config` file or the global XDG default)
/// exactly as before — this preserves the pre-#2995 precedence in full when no
/// repo file exists (zero regression; see `config/mod.rs`'s module doc for the
/// complete four-tier discussion).
/// What: returns `Some(name)` from the first source (repo file → env → file)
/// that specifies a non-empty name; `None` when all three are absent or empty.
/// Test: `voice_package_from_env`, `voice_package_from_config_file`,
/// `voice_package_repo_file_beats_env` (this module's `mod tests`).
pub fn load_voice_package(
    repo_voice: Option<&VoiceFileConfig>,
    file_voice: Option<&VoiceFileConfig>,
) -> Option<String> {
    // Repo-scoped config wins over the env var and the global file (#2995).
    if let Some(name) = repo_voice
        .and_then(|v| v.package.as_ref())
        .filter(|s| !s.trim().is_empty())
    {
        return Some(name.trim().to_string());
    }
    // Env var takes precedence over the (explicit or XDG-default) config file.
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

/// Resolve whether the principles layer is enabled from repo file, env var, or
/// config file.
///
/// Why: `TRUSTY_REVIEW_PRINCIPLES=false` lets operators opt out of the
/// principles layer; defaults to `true` per issue #756 (universal/safe). The
/// repo-scoped `.trusty-review.toml` (#2995) is checked first so a project can
/// pin this setting without an env var, following the same "repo file beats
/// env" rule as `load_voice_package`.
/// What: repo file overrides env var; env var overrides config file; all
/// override the default `true`.
/// Test: `voice_principles_defaults_to_true`, `voice_principles_env_disable`,
/// `voice_principles_repo_file_beats_env` (this module's `mod tests`).
pub fn load_voice_principles(
    repo_voice: Option<&VoiceFileConfig>,
    file_voice: Option<&VoiceFileConfig>,
) -> bool {
    // Repo-scoped config wins over the env var and the global file (#2995).
    if let Some(vf) = repo_voice
        && let Some(enabled) = vf.principles
    {
        return enabled;
    }
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

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // `#[serial_test::serial]` here uses the SAME unnamed/default lock as
    // `review_template.rs`'s and `pipeline::voice_config`'s env-mutating
    // tests — all three modules touch overlapping `TRUSTY_REVIEW_*` env vars,
    // so they must share one lock.

    fn pkg(name: &str) -> VoiceFileConfig {
        VoiceFileConfig {
            package: Some(name.to_string()),
            principles: None,
        }
    }

    #[test]
    #[serial_test::serial]
    fn voice_package_from_env() {
        unsafe {
            std::env::set_var("TRUSTY_REVIEW_VOICE_PACKAGE", "env-voice");
        }
        let result = load_voice_package(None, None);
        unsafe {
            std::env::remove_var("TRUSTY_REVIEW_VOICE_PACKAGE");
        }
        assert_eq!(result.as_deref(), Some("env-voice"));
    }

    #[test]
    #[serial_test::serial]
    fn voice_package_from_config_file() {
        unsafe {
            std::env::remove_var("TRUSTY_REVIEW_VOICE_PACKAGE");
        }
        let file = pkg("file-voice");
        let result = load_voice_package(None, Some(&file));
        assert_eq!(result.as_deref(), Some("file-voice"));
    }

    #[test]
    #[serial_test::serial]
    fn voice_package_repo_file_beats_env() {
        unsafe {
            std::env::set_var("TRUSTY_REVIEW_VOICE_PACKAGE", "env-voice");
        }
        let repo = pkg("repo-voice");
        let result = load_voice_package(Some(&repo), None);
        unsafe {
            std::env::remove_var("TRUSTY_REVIEW_VOICE_PACKAGE");
        }
        assert_eq!(
            result.as_deref(),
            Some("repo-voice"),
            "repo .trusty-review.toml must win over the ambient env var"
        );
    }

    #[test]
    #[serial_test::serial]
    fn voice_principles_defaults_to_true() {
        unsafe {
            std::env::remove_var("TRUSTY_REVIEW_PRINCIPLES");
        }
        assert!(load_voice_principles(None, None));
    }

    #[test]
    #[serial_test::serial]
    fn voice_principles_env_disable() {
        unsafe {
            std::env::set_var("TRUSTY_REVIEW_PRINCIPLES", "false");
        }
        let result = load_voice_principles(None, None);
        unsafe {
            std::env::remove_var("TRUSTY_REVIEW_PRINCIPLES");
        }
        assert!(!result);
    }

    #[test]
    #[serial_test::serial]
    fn voice_principles_repo_file_beats_env() {
        unsafe {
            std::env::set_var("TRUSTY_REVIEW_PRINCIPLES", "true");
        }
        let repo = VoiceFileConfig {
            package: None,
            principles: Some(false),
        };
        let result = load_voice_principles(Some(&repo), None);
        unsafe {
            std::env::remove_var("TRUSTY_REVIEW_PRINCIPLES");
        }
        assert!(
            !result,
            "repo .trusty-review.toml must win over the ambient env var"
        );
    }
}
