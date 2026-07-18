//! User config (`~/.trusty-memory/config.toml`) loading + `DreamConfig`
//! derivation.
//!
//! Why: split out of `helpers.rs` (issue #2593 follow-up, code review on
//! #2977) to keep that file under the 500-SLOC production cap after the
//! `dream_config_from_user_config` addition pushed it over. This is also a
//! cohesive unit on its own: "read config.toml" and "translate it into the
//! shapes downstream consumers need" belong together, separate from the
//! unrelated preview/snippet/palace-info transforms that fill the rest of
//! `helpers.rs`.
//! What: `UserConfigMin`/`OpenRouterMin`/`LocalModelMin` (the minimal TOML
//! mirror), `LoadedUserConfig` (the public, normalised shape), `load_user_config`
//! (file → `LoadedUserConfig`), and `dream_config_from_user_config`
//! (`LoadedUserConfig` → `DreamConfig`, used by both the idle dream scheduler
//! and the on-demand `dream_consolidate_room`/`palace_dream` tools). Re-exported
//! from `service::mod` unchanged so `crate::service::{load_user_config,
//! dream_config_from_user_config, LoadedUserConfig}` keeps resolving exactly as
//! before this split — no public API change.
//! Test: `dream_config_from_user_config_prefers_local_model_when_resolved`,
//! `dream_config_from_user_config_prefers_openrouter_model_with_key`,
//! `dream_config_from_user_config_prefers_openrouter_model_with_env_key`.

use serde::Deserialize;
use trusty_common::memory_core::dream::DreamConfig;
use trusty_common::memory_core::semantic_consolidation::SemanticConsolidationConfig;

/// Minimal mirror of the user-config schema.
#[derive(Deserialize, Default, Clone)]
struct UserConfigMin {
    #[serde(default)]
    openrouter: OpenRouterMin,
    #[serde(default)]
    local_model: LocalModelMin,
}

#[derive(Deserialize, Default, Clone)]
struct OpenRouterMin {
    #[serde(default)]
    api_key: String,
    #[serde(default)]
    model: String,
}

#[derive(Deserialize, Clone)]
struct LocalModelMin {
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

impl Default for LocalModelMin {
    fn default() -> Self {
        Self {
            enabled: default_local_enabled(),
            base_url: default_local_base_url(),
            model: default_local_model(),
        }
    }
}

/// Loaded user config (mirrors the public `LoadedUserConfig` from `web.rs`).
#[derive(Clone)]
pub struct LoadedUserConfig {
    pub openrouter_api_key: String,
    pub openrouter_model: String,
    pub local_model: trusty_common::LocalModelConfig,
}

impl Default for LoadedUserConfig {
    fn default() -> Self {
        Self {
            openrouter_api_key: String::new(),
            openrouter_model: "anthropic/claude-3-5-sonnet".to_string(),
            local_model: trusty_common::LocalModelConfig::default(),
        }
    }
}

/// Read the user's `~/.trusty-memory/config.toml`, falling back to defaults.
///
/// Why: shared between HTTP config endpoint, chat tool dispatch, and
/// provider auto-detection.
/// What: returns `Some(LoadedUserConfig)` even when the file is missing
/// (so callers see defaults consistently); `None` only when the home
/// directory itself can't be resolved.
/// Test: indirectly via `config_endpoint_returns_payload`.
pub fn load_user_config() -> Option<LoadedUserConfig> {
    let home = dirs::home_dir()?;
    let path = home.join(".trusty-memory").join("config.toml");
    if !path.exists() {
        return Some(LoadedUserConfig::default());
    }
    let raw = std::fs::read_to_string(&path).ok()?;
    let parsed: UserConfigMin = toml::from_str(&raw).unwrap_or_default();
    let model = if parsed.openrouter.model.is_empty() {
        "anthropic/claude-3-5-sonnet".to_string()
    } else {
        parsed.openrouter.model
    };
    Some(LoadedUserConfig {
        openrouter_api_key: parsed.openrouter.api_key,
        openrouter_model: model,
        local_model: trusty_common::LocalModelConfig {
            enabled: parsed.local_model.enabled,
            base_url: parsed.local_model.base_url,
            model: parsed.local_model.model,
        },
    })
}

/// Derive a `DreamConfig` seed from the user's loaded config (OpenRouter key,
/// local-model flag, and local-model id).
///
/// Why (issue #2593): the idle dream scheduler and the on-demand
/// `dream_consolidate_room`/`palace_dream` tools both need to translate
/// `LoadedUserConfig` into `DreamConfig` identically, or the two paths
/// silently diverge — the idle scheduler previously used
/// `DreamConfig::default()` outright (ignoring the user's config.toml
/// entirely), and the on-demand path forwarded the OpenRouter key and the
/// local-model flag but dropped `local_model.model`, leaving
/// `semantic.model` on the OpenRouter-style default even when consolidation
/// resolved to a local Ollama backend — the exact misconfiguration
/// `validate_ollama_model` now rejects. Centralising the derivation also
/// pins the "which model string goes with which backend" decision: it
/// mirrors `build_consolidator_from_config`'s own branch
/// (`local_model_enabled && api_key.is_empty()` => Ollama) so the model
/// forwarded here always matches the backend the dream cycle will actually
/// select. The key-presence half of that branch is resolved via the SAME
/// `trusty_common::memory_core::semantic_consolidation::resolve_openrouter_api_key`
/// that `build_consolidator_from_config` itself calls — an earlier version
/// of this function checked only `cfg.openrouter_api_key` (config.toml),
/// which diverged from `build_consolidator_from_config`'s env-var-fallback
/// resolution whenever `OPENROUTER_API_KEY` was set in the daemon's process
/// environment but absent from config.toml: this function picked the
/// local-model id while the consolidator built the OpenRouter backend,
/// silently sending an Ollama tag to OpenRouter every cycle.
/// What: sets `openrouter_api_key`, `local_model_enabled`, and
/// `semantic.model` (the local-model id when the local path resolves, the
/// OpenRouter model id otherwise); every other `DreamConfig` field keeps its
/// default.
/// Test: `dream_config_from_user_config_prefers_local_model_when_resolved`,
/// `dream_config_from_user_config_prefers_openrouter_model_with_key`,
/// `dream_config_from_user_config_prefers_openrouter_model_with_env_key`.
pub fn dream_config_from_user_config(cfg: &LoadedUserConfig) -> DreamConfig {
    let resolved_api_key =
        trusty_common::memory_core::semantic_consolidation::resolve_openrouter_api_key(
            &cfg.openrouter_api_key,
        );
    let resolves_local = cfg.local_model.enabled && resolved_api_key.is_empty();
    let model = if resolves_local {
        cfg.local_model.model.clone()
    } else {
        cfg.openrouter_model.clone()
    };

    DreamConfig {
        openrouter_api_key: cfg.openrouter_api_key.clone(),
        local_model_enabled: cfg.local_model.enabled,
        semantic: SemanticConsolidationConfig {
            model,
            ..SemanticConsolidationConfig::default()
        },
        ..DreamConfig::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why (issue #2593): the exact production gap — the idle scheduler and
    /// the on-demand tool both need `local_model.model` to reach
    /// `DreamConfig.semantic.model` when the local Ollama path is what will
    /// actually resolve (local model enabled, no OpenRouter key ANYWHERE —
    /// neither config.toml nor the real process environment; without the
    /// `EnvVarGuard::remove` this test is order-dependent on whatever the
    /// ambient shell happens to export, since `dream_config_from_user_config`
    /// now resolves the env-var fallback too — code review follow-up on
    /// #2977). `#[serial]` avoids racing other tests that read/write the
    /// same real env var.
    /// What: builds a `LoadedUserConfig` with a configured local model id and
    /// no OpenRouter key; asserts the derived `DreamConfig.semantic.model`
    /// equals the configured local model, not the OpenRouter-style default.
    #[test]
    #[serial_test::serial]
    fn dream_config_from_user_config_prefers_local_model_when_resolved() {
        let _guard = EnvVarGuard::remove("OPENROUTER_API_KEY");
        let cfg = LoadedUserConfig {
            openrouter_api_key: String::new(),
            openrouter_model: "anthropic/claude-3-5-sonnet".to_string(),
            local_model: trusty_common::LocalModelConfig {
                enabled: true,
                base_url: "http://localhost:11434".to_string(),
                model: "llama3.2".to_string(),
            },
        };

        let dream_cfg = dream_config_from_user_config(&cfg);

        assert_eq!(dream_cfg.semantic.model, "llama3.2");
        assert!(dream_cfg.local_model_enabled);
        assert!(dream_cfg.openrouter_api_key.is_empty());
    }

    /// Why: when an OpenRouter key is configured, consolidation resolves to
    /// the OpenRouter backend regardless of `local_model.enabled` (mirrors
    /// `build_consolidator_from_config`'s branch), so the OpenRouter model
    /// id must be forwarded instead of the local-model id.
    /// What: builds a `LoadedUserConfig` with both a local model AND an
    /// OpenRouter key configured; asserts the derived `semantic.model` is
    /// the OpenRouter model, not the local one.
    #[test]
    fn dream_config_from_user_config_prefers_openrouter_model_with_key() {
        let cfg = LoadedUserConfig {
            openrouter_api_key: "sk-test-key".to_string(),
            openrouter_model: "anthropic/claude-3-5-sonnet".to_string(),
            local_model: trusty_common::LocalModelConfig {
                enabled: true,
                base_url: "http://localhost:11434".to_string(),
                model: "llama3.2".to_string(),
            },
        };

        let dream_cfg = dream_config_from_user_config(&cfg);

        assert_eq!(dream_cfg.semantic.model, "anthropic/claude-3-5-sonnet");
        assert_eq!(dream_cfg.openrouter_api_key, "sk-test-key");
    }

    /// Why (code review follow-up on #2977): `config.toml` may have no
    /// OpenRouter key while the daemon's process environment carries
    /// `OPENROUTER_API_KEY` — `build_consolidator_from_config` (trusty-common)
    /// resolves that env-var fallback before choosing a backend, so this
    /// helper must resolve it identically or it picks the local-model id
    /// while the consolidator builds the OpenRouter backend, silently
    /// sending an Ollama tag to OpenRouter every cycle. `#[serial]` +
    /// `EnvVarGuard` (mirroring the pattern in
    /// `trusty_common::memory_core::dream::tests`) avoid racing other tests
    /// that read/write the same real env var.
    /// What: empty `openrouter_api_key` in config, `OPENROUTER_API_KEY` set
    /// in the real environment; asserts the OpenRouter model is chosen, not
    /// the local one.
    #[test]
    #[serial_test::serial]
    fn dream_config_from_user_config_prefers_openrouter_model_with_env_key() {
        let _guard = EnvVarGuard::set("OPENROUTER_API_KEY", "sk-from-env");

        let cfg = LoadedUserConfig {
            openrouter_api_key: String::new(),
            openrouter_model: "anthropic/claude-3-5-sonnet".to_string(),
            local_model: trusty_common::LocalModelConfig {
                enabled: true,
                base_url: "http://localhost:11434".to_string(),
                model: "llama3.2".to_string(),
            },
        };

        let dream_cfg = dream_config_from_user_config(&cfg);

        assert_eq!(
            dream_cfg.semantic.model, "anthropic/claude-3-5-sonnet",
            "an env-supplied OpenRouter key must resolve the OpenRouter \
             model, not the local one, even though config.toml has no key"
        );
    }

    // ─── RAII env-var guard for tests (mirrors
    // trusty_common::memory_core::dream::tests::EnvVarGuard) ───────────────
    //
    // Safety: test-only; `#[serial_test::serial]` on every caller serialises
    // access to the real process environment across test threads.

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var(key).ok();
            // Safety: test-only; caller is `#[serial]`.
            unsafe { std::env::set_var(key, value) };
            Self { key, previous }
        }

        fn remove(key: &'static str) -> Self {
            let previous = std::env::var(key).ok();
            // Safety: test-only; caller is `#[serial]`.
            unsafe { std::env::remove_var(key) };
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            // Safety: test-only; caller is `#[serial]`.
            match &self.previous {
                Some(v) => unsafe { std::env::set_var(self.key, v) },
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }
}
