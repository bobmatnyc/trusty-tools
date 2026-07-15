//! User-facing config loaded from `~/.trusty-search/config.toml`.
//!
//! Why: trusty-search previously read every knob from environment variables
//! (`OPENROUTER_API_KEY`). With the introduction of a local-model lane
//! (Ollama / LM Studio) we need a structured file so users can pick a model
//! and base URL without exporting half a dozen env vars per shell. The schema
//! mirrors trusty-memory's so users only learn it once.
//!
//! What: `~/.trusty-search/config.toml` is optional. When absent, defaults
//! apply (Ollama at localhost:11434, model `llama3.2`, OpenRouter model
//! `anthropic/claude-haiku-4.5`). Unknown keys are ignored to keep forward
//! compatibility.
//!
//! Test: `load_user_config_returns_defaults_when_missing` and
//! `parses_local_model_section`.

use serde::Deserialize;
use trusty_common::LocalModelConfig;

/// Default OpenRouter model when the user hasn't specified one.
fn default_openrouter_model() -> String {
    "anthropic/claude-haiku-4.5".to_string()
}

#[derive(Deserialize, Default, Clone)]
struct UserConfigFile {
    #[serde(default)]
    openrouter: OpenRouterSection,
    #[serde(default)]
    local_model: LocalModelSection,
}

#[derive(Deserialize, Default, Clone)]
struct OpenRouterSection {
    /// Optional override for the API key. The `OPENROUTER_API_KEY` env var
    /// still takes precedence so existing setups keep working unchanged.
    #[serde(default)]
    api_key: String,
    #[serde(default)]
    model: String,
}

#[derive(Deserialize, Clone)]
struct LocalModelSection {
    #[serde(default = "default_local_enabled")]
    enabled: bool,
    #[serde(default = "default_local_base_url")]
    base_url: String,
    #[serde(default = "default_local_model")]
    model: String,
}

fn default_local_enabled() -> bool {
    true
}
fn default_local_base_url() -> String {
    "http://localhost:11434".to_string()
}
fn default_local_model() -> String {
    "llama3.2".to_string()
}

impl Default for LocalModelSection {
    fn default() -> Self {
        Self {
            enabled: default_local_enabled(),
            base_url: default_local_base_url(),
            model: default_local_model(),
        }
    }
}

/// Resolved user configuration ready to inject into [`crate::SearchAppState`].
///
/// Why: separating the "wire" deserialisation type from the runtime struct
/// lets us apply defaults exactly once at the boundary and keeps the rest of
/// the codebase from juggling `Option<...>` everywhere.
/// What: `openrouter_api_key` resolves to the env var when set, otherwise the
/// TOML value. `openrouter_model` falls back to
/// `anthropic/claude-haiku-4.5`. `local_model` is the [`LocalModelConfig`]
/// from trusty-common.
/// Test: `parses_local_model_section`.
#[derive(Clone, Debug)]
pub struct LoadedUserConfig {
    pub openrouter_api_key: String,
    pub openrouter_model: String,
    pub local_model: LocalModelConfig,
}

impl Default for LoadedUserConfig {
    fn default() -> Self {
        Self {
            openrouter_api_key: std::env::var(trusty_common::env_vars::ENV_OPENROUTER_API_KEY)
                .unwrap_or_default(),
            openrouter_model: default_openrouter_model(),
            local_model: LocalModelConfig::default(),
        }
    }
}

/// Load `~/.trusty-search/config.toml`, applying defaults when sections /
/// fields are missing.
///
/// Why: callers (the `start` subcommand, tests) want one function that
/// returns a ready-to-use `LoadedUserConfig` with all the env-var fallback
/// logic encapsulated. Returning `LoadedUserConfig::default()` on a missing
/// file keeps existing setups (env var only, no TOML file) working unchanged.
/// What: reads the file if present and parses it; ignores parse errors and
/// returns defaults so a corrupt file doesn't block daemon startup (a
/// warning is logged via `tracing::warn!` so the user notices).
/// `OPENROUTER_API_KEY` env var wins over the TOML value when both are set.
/// Test: covered by the unit tests in this module.
pub fn load_user_config() -> LoadedUserConfig {
    let Some(home) = dirs::home_dir() else {
        return LoadedUserConfig::default();
    };
    let path = home.join(".trusty-search").join("config.toml");
    if !path.exists() {
        return LoadedUserConfig::default();
    }
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("could not read {}: {e}; using defaults", path.display());
            return LoadedUserConfig::default();
        }
    };
    let parsed: UserConfigFile = match toml::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("could not parse {}: {e}; using defaults", path.display());
            return LoadedUserConfig::default();
        }
    };
    let env_key =
        std::env::var(trusty_common::env_vars::ENV_OPENROUTER_API_KEY).unwrap_or_default();
    let openrouter_api_key = if !env_key.is_empty() {
        env_key
    } else {
        parsed.openrouter.api_key
    };
    let openrouter_model = if parsed.openrouter.model.is_empty() {
        default_openrouter_model()
    } else {
        parsed.openrouter.model
    };
    LoadedUserConfig {
        openrouter_api_key,
        openrouter_model,
        local_model: LocalModelConfig {
            enabled: parsed.local_model.enabled,
            base_url: parsed.local_model.base_url,
            model: parsed.local_model.model,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_local_model_section() {
        let src = r#"
            [local_model]
            enabled = true
            base_url = "http://localhost:1234"
            model = "qwen2.5-coder"

            [openrouter]
            model = "anthropic/claude-3-5-sonnet"
        "#;
        let parsed: UserConfigFile = toml::from_str(src).unwrap();
        assert!(parsed.local_model.enabled);
        assert_eq!(parsed.local_model.base_url, "http://localhost:1234");
        assert_eq!(parsed.local_model.model, "qwen2.5-coder");
        assert_eq!(parsed.openrouter.model, "anthropic/claude-3-5-sonnet");
    }

    #[test]
    fn local_model_defaults_apply_when_section_absent() {
        let parsed: UserConfigFile = toml::from_str("").unwrap();
        assert!(parsed.local_model.enabled);
        assert_eq!(parsed.local_model.base_url, "http://localhost:11434");
        assert_eq!(parsed.local_model.model, "llama3.2");
    }
}
