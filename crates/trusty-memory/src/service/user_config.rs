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
//! Test: `dream_config_is_off_and_names_no_local_model_by_default`,
//! `dream_config_from_user_config_prefers_openrouter_model_with_key`,
//! `semantic_consolidation_is_off_without_a_config_file`.

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
    /// `[semantic_consolidation]` — the switch for the dream cycle's LLM phase.
    /// Absent from the schema until #5188, so a `config.toml` asking for the
    /// phase to be off was parsed and discarded while the phase ran anyway.
    /// `[semantic]` is accepted as an alias: it matches `DreamConfig`'s field
    /// name, so both spellings are in circulation and silently dropping either
    /// one is the defect this table exists to fix.
    #[serde(default, alias = "semantic")]
    semantic_consolidation: SemanticConsolidationMin,
    /// `[dream]` — the #6652 kg.redb prune-and-compact tunables. Absent means
    /// the `DreamConfig` defaults: compaction on, 90-day history retention,
    /// a 64 MiB file floor, backups kept.
    #[serde(default)]
    dream: DreamMin,
}

/// `[dream]` — the kg.redb prune-and-compact tunables (#6652).
///
/// Why: the compaction rewrites the palace's whole knowledge graph, so every
/// knob that decides whether and how aggressively it runs has to be settable
/// without recompiling. Each field is `Option` so an absent key inherits
/// [`DreamConfig::default`] rather than resetting it to this struct's own
/// default — the difference matters when only one key is present.
/// What: mirrors four `DreamConfig` fields by name.
/// Test: `dream_table_overrides_the_compaction_defaults`,
/// `an_absent_dream_table_leaves_every_default`.
#[derive(Deserialize, Default, Clone)]
struct DreamMin {
    #[serde(default)]
    compact: Option<bool>,
    #[serde(default)]
    prune_history_after_days: Option<i64>,
    #[serde(default)]
    compact_min_bytes: Option<u64>,
    #[serde(default)]
    compact_keep_backup: Option<bool>,
}

#[derive(Deserialize, Default, Clone)]
struct OpenRouterMin {
    #[serde(default)]
    api_key: String,
    #[serde(default)]
    model: String,
}

/// `[local_model]` — a local OpenAI-compatible server (Ollama, LM Studio).
///
/// #5188: `enabled` now defaults to FALSE. It defaulted to true, which is how a
/// daemon with no config file at all decided a local model was available.
#[derive(Deserialize, Clone)]
struct LocalModelMin {
    #[serde(default)]
    enabled: bool,
    #[serde(default = "default_local_base_url")]
    base_url: String,
    #[serde(default = "default_local_model")]
    model: String,
}

impl Default for LocalModelMin {
    fn default() -> Self {
        Self {
            // #5188: opt-in, so an absent `[local_model]` table means "no".
            enabled: false,
            base_url: default_local_base_url(),
            model: default_local_model(),
        }
    }
}

/// `[semantic_consolidation]` — the dream cycle's LLM phase (#5188).
#[derive(Deserialize, Default, Clone)]
struct SemanticConsolidationMin {
    /// Defaults to false: the phase costs money and calls an external model,
    /// so the file has to ask for it.
    #[serde(default)]
    enabled: bool,
    /// Model id. Empty falls back to `[openrouter] model`. An `ollama/` or
    /// `local/` prefix is the only thing that selects a local model server.
    #[serde(default)]
    model: String,
}

fn default_local_base_url() -> String {
    "http://localhost:11434".to_string()
}
fn default_local_model() -> String {
    "llama3.2".to_string()
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
            // #5188: NOT `LocalModelConfig::default()`, whose `enabled: true`
            // is what let a daemon with no config file probe a local Ollama.
            // trusty-search shares that struct, so the default stays as it is
            // and trusty-memory states its own answer here.
            local_model: trusty_common::LocalModelConfig {
                enabled: false,
                base_url: default_local_base_url(),
                model: default_local_model(),
            },
        }
    }
}

/// Path of the user config file this module reads.
///
/// Why (#5188): `load_user_config` and `load_semantic_consolidation_config`
/// project two different shapes out of the same file; one path expression
/// keeps them from drifting apart.
/// What: `~/.trusty-memory/config.toml`; `None` when the home directory
/// cannot be resolved.
fn user_config_path() -> Option<std::path::PathBuf> {
    Some(dirs::home_dir()?.join(".trusty-memory").join("config.toml"))
}

/// Parse the whole config file into its minimal mirror.
///
/// Why (#5188): the single reader for `~/.trusty-memory/config.toml`. A
/// malformed file yields defaults rather than an error, matching the
/// pre-existing behaviour of `load_user_config` — the daemon starts either way.
/// What: `None` when the home directory cannot be resolved or the file cannot
/// be read; `Some(UserConfigMin::default())` when the file is absent or
/// unparseable.
fn read_user_config_min() -> Option<UserConfigMin> {
    let path = user_config_path()?;
    if !path.exists() {
        return Some(UserConfigMin::default());
    }
    let raw = std::fs::read_to_string(&path).ok()?;
    Some(toml::from_str(&raw).unwrap_or_default())
}

/// Read the `[semantic_consolidation]` table into a
/// [`SemanticConsolidationConfig`].
///
/// Why (#5188): the dream cycle's LLM phase had no config key at all — the
/// struct's `enabled` field was hardcoded true by `dream_config_from_user_config`
/// and a `[semantic_consolidation]` block in the file was silently discarded.
/// This is the key that turns the phase on.
/// What: `enabled` and `model` come from the file; every other field keeps its
/// [`SemanticConsolidationConfig::default`] value. An empty `model` is left
/// empty for [`dream_config_from_user_config`] to fill from `[openrouter]`.
/// Returns the all-default (disabled) config when the file is absent.
/// Test: `semantic_consolidation_is_off_without_a_config_file`.
pub fn load_semantic_consolidation_config() -> SemanticConsolidationConfig {
    load_semantic_consolidation_config_from(&read_user_config_min().unwrap_or_default())
}

/// The pure projection behind [`load_semantic_consolidation_config`].
///
/// Why (#5188): separates "read the file" from "read the table" so a test can
/// state its own input instead of asserting against the developer's real
/// `~/.trusty-memory/config.toml`.
/// Test: `semantic_consolidation_is_off_without_a_config_file`.
fn load_semantic_consolidation_config_from(parsed: &UserConfigMin) -> SemanticConsolidationConfig {
    SemanticConsolidationConfig {
        enabled: parsed.semantic_consolidation.enabled,
        model: parsed.semantic_consolidation.model.clone(),
        ..SemanticConsolidationConfig::default()
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
    let parsed = read_user_config_min()?;
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

/// Derive a `DreamConfig` seed from the user's config file.
///
/// Why (#2593): the idle dream scheduler and the on-demand
/// `dream_consolidate_room`/`palace_dream` tools must translate the user's
/// config into `DreamConfig` identically, or the two paths silently diverge —
/// the idle scheduler once used `DreamConfig::default()` outright and never
/// saw `config.toml` at all.
///
/// Why (#5188): the semantic phase's enable switch and model id now come from
/// `[semantic_consolidation]` rather than being hardcoded. Two behaviours
/// changed here. The phase is off unless the file says otherwise, and
/// `[local_model] model` no longer leaks into `semantic.model`: forwarding it
/// meant "no OpenRouter key" chose a local model server by itself, which is
/// how an unconfigured daemon loaded a 45 GB model into a crash loop. A local
/// server is now named explicitly — `model = "ollama/llama3.2"` — and
/// `[local_model] enabled` only permits that choice.
/// What: `semantic.enabled` and `semantic.model` come from
/// [`load_semantic_consolidation_config`], with an empty model falling back to
/// `[openrouter] model`. `openrouter_api_key` and `local_model_enabled` come
/// from `cfg`. Every other `DreamConfig` field keeps its default.
/// Test: `dream_config_is_off_and_names_no_local_model_by_default`,
/// `dream_config_forwards_an_explicit_ollama_model`,
/// `dream_config_from_user_config_prefers_openrouter_model_with_key`.
pub fn dream_config_from_user_config(cfg: &LoadedUserConfig) -> DreamConfig {
    let parsed = read_user_config_min().unwrap_or_default();
    dream_config_from_parts(
        cfg,
        load_semantic_consolidation_config_from(&parsed),
        parsed.dream.clone(),
    )
}

/// [`dream_config_from_user_config`] with the semantic section passed in.
///
/// Why (#5188): `load_semantic_consolidation_config` reads the developer's real
/// `~/.trusty-memory/config.toml`, so a test driving the public wrapper asserts
/// against whatever that machine happens to hold. Splitting the file read from
/// the derivation lets the tests state their own input.
/// What: pure — no file, no environment.
/// Test: `dream_config_is_off_and_names_no_local_model_by_default`,
/// `dream_config_forwards_an_explicit_ollama_model`.
fn dream_config_from_parts(
    cfg: &LoadedUserConfig,
    semantic: SemanticConsolidationConfig,
    dream: DreamMin,
) -> DreamConfig {
    let defaults = DreamConfig::default();
    // #5188: an empty `[semantic_consolidation] model` inherits the OpenRouter
    // model id — never the local-model id, which would pick a local backend
    // nobody asked for.
    let model = if semantic.model.trim().is_empty() {
        cfg.openrouter_model.clone()
    } else {
        semantic.model.clone()
    };

    DreamConfig {
        openrouter_api_key: cfg.openrouter_api_key.clone(),
        local_model_enabled: cfg.local_model.enabled,
        semantic: SemanticConsolidationConfig { model, ..semantic },
        // #6652: an absent key inherits the default rather than zeroing it.
        compact: dream.compact.unwrap_or(defaults.compact),
        prune_history_after_days: dream
            .prune_history_after_days
            .unwrap_or(defaults.prune_history_after_days),
        compact_min_bytes: dream
            .compact_min_bytes
            .unwrap_or(defaults.compact_min_bytes),
        compact_keep_backup: dream
            .compact_keep_backup
            .unwrap_or(defaults.compact_keep_backup),
        ..defaults
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn openrouter_only_cfg() -> LoadedUserConfig {
        LoadedUserConfig {
            openrouter_api_key: String::new(),
            openrouter_model: "anthropic/claude-3-5-sonnet".to_string(),
            local_model: trusty_common::LocalModelConfig {
                enabled: false,
                base_url: "http://localhost:11434".to_string(),
                model: "llama3.2".to_string(),
            },
        }
    }

    /// Why (#5188): the reported repro — no `~/.trusty-memory/config.toml`, no
    /// provider key — must produce a `DreamConfig` that cannot reach a local
    /// model server. Before the fix this config had `semantic.enabled = true`,
    /// `local_model_enabled = true`, and `semantic.model = "qwen3:30b"`, which
    /// is exactly what drove a 45 GB model into a crash loop.
    /// What: derives from the all-defaults user config and asserts the phase is
    /// off, the local backend is not permitted, and no local model id was
    /// forwarded.
    #[test]
    fn dream_config_is_off_and_names_no_local_model_by_default() {
        let dream_cfg = dream_config_from_parts(
            &LoadedUserConfig::default(),
            load_semantic_consolidation_config_from(&UserConfigMin::default()),
            DreamMin::default(),
        );

        assert!(
            !dream_cfg.semantic.enabled,
            "semantic consolidation must be off until a config key enables it"
        );
        assert!(
            !dream_cfg.local_model_enabled,
            "a local model server must not be permitted by default"
        );
        assert!(
            !dream_cfg.semantic.model.starts_with("ollama/")
                && !dream_cfg.semantic.model.starts_with("local/"),
            "no local model id may be forwarded by default, got {:?}",
            dream_cfg.semantic.model
        );
    }

    /// Why (#5188): `LoadedUserConfig::default()` is what `load_user_config`
    /// returns when the file is absent, so its `local_model.enabled` IS the
    /// no-config-file answer.
    #[test]
    fn loaded_user_config_default_disables_the_local_model() {
        assert!(!LoadedUserConfig::default().local_model.enabled);
    }

    /// Why (#5188): an absent `[local_model]` table must mean "no", not
    /// "yes" — that default is how the daemon decided a local model existed.
    #[test]
    fn absent_local_model_table_parses_as_disabled() {
        let parsed: UserConfigMin = toml::from_str("").expect("empty config parses");
        assert!(!parsed.local_model.enabled);
        assert!(!parsed.semantic_consolidation.enabled);
    }

    /// Why (#5188): the `[semantic_consolidation]` table was not in the schema
    /// at all, so a file asking for the phase was — like a file asking against
    /// it — silently discarded. Pins that both directions now parse.
    #[test]
    fn semantic_consolidation_table_is_read_from_the_file() {
        let parsed: UserConfigMin = toml::from_str(
            r#"
[semantic_consolidation]
enabled = true
model = "ollama/llama3.2"
"#,
        )
        .expect("config parses");
        assert!(parsed.semantic_consolidation.enabled);
        assert_eq!(parsed.semantic_consolidation.model, "ollama/llama3.2");
    }

    /// Why (#5188): `[semantic]` matches `DreamConfig`'s field name, so an
    /// operator reading the struct writes that spelling. Accepting only
    /// `[semantic_consolidation]` would drop it silently — the same failure
    /// this table was added to fix.
    #[test]
    fn semantic_table_alias_is_accepted() {
        let parsed: UserConfigMin = toml::from_str(
            r#"
[semantic]
enabled = true
"#,
        )
        .expect("config parses");
        assert!(parsed.semantic_consolidation.enabled);
    }

    /// Why (#5188): a local model server is reachable only when the operator
    /// names it. Pins that the explicit `ollama/` id survives the derivation
    /// verbatim — the prefix is what `resolve_consolidation_provider` reads.
    #[test]
    fn dream_config_forwards_an_explicit_ollama_model() {
        let mut cfg = openrouter_only_cfg();
        cfg.local_model.enabled = true;
        let semantic = SemanticConsolidationConfig {
            enabled: true,
            model: "ollama/llama3.2".to_string(),
            ..SemanticConsolidationConfig::default()
        };

        let dream_cfg = dream_config_from_parts(&cfg, semantic, DreamMin::default());

        assert!(dream_cfg.semantic.enabled);
        assert_eq!(dream_cfg.semantic.model, "ollama/llama3.2");
        assert!(dream_cfg.local_model_enabled);
    }

    /// Why (#5188): with the phase enabled but no model named, the id must come
    /// from `[openrouter]` — never from `[local_model]`, which is how "no key"
    /// used to select a local backend on its own.
    #[test]
    fn empty_semantic_model_inherits_the_openrouter_model_not_the_local_one() {
        let mut cfg = openrouter_only_cfg();
        cfg.local_model.enabled = true;
        cfg.local_model.model = "qwen3:30b".to_string();
        let semantic = SemanticConsolidationConfig {
            enabled: true,
            model: String::new(),
            ..SemanticConsolidationConfig::default()
        };

        let dream_cfg = dream_config_from_parts(&cfg, semantic, DreamMin::default());

        assert_eq!(dream_cfg.semantic.model, "anthropic/claude-3-5-sonnet");
    }

    /// Why: an OpenRouter key configured in the file must reach `DreamConfig`
    /// so the consolidator can build the OpenRouter backend.
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
        let semantic = SemanticConsolidationConfig {
            enabled: true,
            // No `[semantic_consolidation] model`, so `[openrouter] model` fills in.
            model: String::new(),
            ..SemanticConsolidationConfig::default()
        };

        let dream_cfg = dream_config_from_parts(&cfg, semantic, DreamMin::default());

        assert_eq!(dream_cfg.semantic.model, "anthropic/claude-3-5-sonnet");
        assert_eq!(dream_cfg.openrouter_api_key, "sk-test-key");
    }

    /// Why (#5188): `load_semantic_consolidation_config` reads the developer's
    /// real config file, so the hermetic half of its contract — "an absent or
    /// empty file yields a disabled phase" — is asserted through the same
    /// projection with a stated input.
    #[test]
    fn semantic_consolidation_is_off_without_a_config_file() {
        let cfg = load_semantic_consolidation_config_from(&UserConfigMin::default());
        assert!(!cfg.enabled);
        assert!(cfg.model.is_empty());
    }
}
