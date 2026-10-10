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
//! `[search]` (#9258) sets the lexical-lane defaults; a per-query value wins.
//! A malformed or out-of-range `[search]` value is a startup error, never a
//! fallback to defaults.
//!
//! Test: `parses_local_model_section`,
//! `search_section_sets_the_lexical_lane_defaults`,
//! `a_malformed_search_setting_refuses_to_start`.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use trusty_common::LocalModelConfig;

use crate::core::indexer::{
    check_lexical_limit, LexicalLaneDefaults, LexicalLimitError, ORIGIN_CONFIG,
};

/// Default OpenRouter model when the user hasn't specified one.
fn default_openrouter_model() -> String {
    "anthropic/claude-haiku-4.5".to_string()
}

/// The sections parsed with serde. `[search]` is read separately, by
/// [`search_defaults`], so its errors can never be swallowed with these.
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

/// Resolved user configuration ready to inject into
/// [`SearchAppState`](crate::service::SearchAppState).
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
    /// `[search]` lexical-lane defaults (#9258), validated at load.
    pub lexical_defaults: LexicalLaneDefaults,
}

impl Default for LoadedUserConfig {
    fn default() -> Self {
        Self {
            openrouter_api_key: std::env::var(trusty_common::env_vars::ENV_OPENROUTER_API_KEY)
                .unwrap_or_default(),
            openrouter_model: default_openrouter_model(),
            local_model: LocalModelConfig::default(),
            lexical_defaults: LexicalLaneDefaults::default(),
        }
    }
}

/// A `config.toml` the daemon refuses to start on (#9258).
///
/// Why: a `[search]` value the daemon cannot honour used to reset the whole
/// file to defaults, so `ripgrep_fallback = "false"` left the content-scan
/// lane on although the config said off.
/// What: the file path plus the value-free [`SearchConfigError`].
/// Test: `a_malformed_search_setting_refuses_to_start`.
#[derive(Debug, thiserror::Error)]
#[error("refusing to start: {}: {problem}", path.display())]
pub struct UserConfigError {
    /// The config file that holds the bad section.
    pub path: PathBuf,
    /// What is wrong with `[search]`.
    pub problem: SearchConfigError,
}

/// What is wrong with `[search]`. Names the key, never the rejected value
/// (the #9603 rule for config errors).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SearchConfigError {
    /// The file is not valid TOML and declares `[search]`, so the section
    /// cannot be read; ignoring it would silently restore the default lane.
    #[error("the file is not valid TOML (line {line}) and it declares [search]")]
    Unparseable { line: usize },
    /// A value of the wrong TOML type.
    #[error("{setting} must be {expected}, got a TOML {found}")]
    WrongType {
        setting: &'static str,
        expected: &'static str,
        found: &'static str,
    },
    /// A negative `lexical_limit`.
    #[error("{ORIGIN_CONFIG} must be at least 1, got a negative integer")]
    NegativeLimit,
    /// `lexical_limit` outside `1..=MAX_LEXICAL_LIMIT`.
    #[error(transparent)]
    Limit(#[from] LexicalLimitError),
}

const SETTING_SECTION: &str = "daemon config [search]";
const SETTING_RIPGREP: &str = "daemon config [search].ripgrep_fallback";

fn wrong_type(
    setting: &'static str,
    expected: &'static str,
    found: &toml::Value,
) -> SearchConfigError {
    SearchConfigError::WrongType {
        setting,
        expected,
        found: found.type_str(),
    }
}

/// Read `[search]` from a parsed document.
///
/// Why: #9258 — a bad value must stop startup, so this section is validated
/// on its own, never through the serde pass whose failure falls back.
/// What: absent section or key → [`LexicalLaneDefaults::default`]'s value.
/// `ripgrep_fallback` must be a boolean; `lexical_limit` must be an integer
/// accepted by [`check_lexical_limit`] with the config origin. Unknown keys
/// are ignored, like every other section.
/// Test: `a_malformed_search_setting_refuses_to_start`,
/// `search_section_sets_the_lexical_lane_defaults`.
fn search_defaults(doc: &toml::Table) -> Result<LexicalLaneDefaults, SearchConfigError> {
    let mut out = LexicalLaneDefaults::default();
    let Some(section) = doc.get("search") else {
        return Ok(out);
    };
    let table = section
        .as_table()
        .ok_or_else(|| wrong_type(SETTING_SECTION, "a table", section))?;
    if let Some(v) = table.get("ripgrep_fallback") {
        out.ripgrep_fallback = v
            .as_bool()
            .ok_or_else(|| wrong_type(SETTING_RIPGREP, "a boolean", v))?;
    }
    if let Some(v) = table.get("lexical_limit") {
        let n = v
            .as_integer()
            .ok_or_else(|| wrong_type(ORIGIN_CONFIG, "an integer", v))?;
        let n = usize::try_from(n).map_err(|_| SearchConfigError::NegativeLimit)?;
        out.lexical_limit = Some(check_lexical_limit(n, ORIGIN_CONFIG)?);
    }
    Ok(out)
}

/// Whether some line of `raw` opens or assigns the top-level `search` key.
///
/// Why: on a TOML syntax error the document cannot be read, so this textual
/// check decides whether a `[search]` section would be lost (#9258).
/// What: ignores whitespace, one or two leading `[`, and a leading quote;
/// then looks for `search` followed by `]`, `.`, `=` or a quote. Comment
/// lines never match. A false positive only refuses an already broken file.
/// Test: `a_malformed_search_setting_refuses_to_start`.
fn declares_search(raw: &str) -> bool {
    raw.lines().any(|line| {
        let compact: String = line.chars().filter(|c| !c.is_whitespace()).collect();
        let key = compact
            .trim_start_matches('[')
            .trim_start_matches(['"', '\'']);
        key.strip_prefix("search")
            .is_some_and(|rest| rest.starts_with([']', '.', '=', '"', '\'']))
    })
}

/// Parse a config file's text into the runtime config.
///
/// Why: the one place that decides which errors stop startup (#9258).
/// What: `[search]` errors — including a TOML syntax error in a file that
/// declares `[search]` — return [`UserConfigError`]. Any other syntax error,
/// or a bad `[openrouter]` / `[local_model]`, keeps the earlier behaviour:
/// warn and use defaults for those sections. The validated `[search]` values
/// survive that fallback.
/// Test: `a_malformed_search_setting_refuses_to_start`,
/// `a_bad_other_section_keeps_the_search_settings`.
fn parse_user_config(raw: &str, path: &Path) -> Result<LoadedUserConfig, UserConfigError> {
    let refuse = |problem: SearchConfigError| UserConfigError {
        path: path.to_path_buf(),
        problem,
    };
    let doc: toml::Table = match toml::from_str(raw) {
        Ok(v) => v,
        Err(e) if declares_search(raw) => {
            let start = e.span().map_or(0, |s| s.start);
            let line = raw.get(..start).map_or(0, |s| s.matches('\n').count()) + 1;
            return Err(refuse(SearchConfigError::Unparseable { line }));
        }
        Err(e) => {
            tracing::warn!("could not parse {}: {e}; using defaults", path.display());
            return Ok(LoadedUserConfig::default());
        }
    };
    let lexical_defaults = search_defaults(&doc).map_err(refuse)?;
    let parsed: UserConfigFile = match toml::Value::Table(doc).try_into::<UserConfigFile>() {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("could not parse {}: {e}; using defaults", path.display());
            return Ok(LoadedUserConfig {
                lexical_defaults,
                ..LoadedUserConfig::default()
            });
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
    Ok(LoadedUserConfig {
        openrouter_api_key,
        openrouter_model,
        local_model: LocalModelConfig {
            enabled: parsed.local_model.enabled,
            base_url: parsed.local_model.base_url,
            model: parsed.local_model.model,
        },
        lexical_defaults,
    })
}

/// Load `~/.trusty-search/config.toml`, applying defaults when sections /
/// fields are missing.
///
/// Why: callers (the `start` subcommand, tests) want one function that
/// returns a ready-to-use `LoadedUserConfig` with all the env-var fallback
/// logic encapsulated. Returning `LoadedUserConfig::default()` on a missing
/// file keeps existing setups (env var only, no TOML file) working unchanged.
/// What: reads the file if present and hands it to [`parse_user_config`]. A
/// read error, or a parse error outside `[search]`, logs a warning and keeps
/// defaults. A bad `[search]` is an error the caller must refuse to start on
/// (#9258). `OPENROUTER_API_KEY` env var wins over the TOML value.
/// Test: covered by the unit tests in this module.
pub fn load_user_config() -> Result<LoadedUserConfig, UserConfigError> {
    let Some(home) = dirs::home_dir() else {
        return Ok(LoadedUserConfig::default());
    };
    let path = home.join(".trusty-search").join("config.toml");
    if !path.exists() {
        return Ok(LoadedUserConfig::default());
    }
    match std::fs::read_to_string(&path) {
        Ok(raw) => parse_user_config(&raw, &path),
        Err(e) => {
            tracing::warn!("could not read {}: {e}; using defaults", path.display());
            Ok(LoadedUserConfig::default())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::indexer::MAX_LEXICAL_LIMIT;

    fn parse(src: &str) -> Result<LoadedUserConfig, UserConfigError> {
        parse_user_config(src, Path::new("/home/u/.trusty-search/config.toml"))
    }

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
    fn search_section_sets_the_lexical_lane_defaults() {
        let src = "[search]\nripgrep_fallback = false\nlexical_limit = 25\n";
        let d = parse(src).expect("a valid [search]").lexical_defaults;
        assert!(!d.ripgrep_fallback);
        assert_eq!(d.lexical_limit, Some(25));
        // A file written before #9258 has no `[search]` and keeps today's lane.
        let old = parse("[openrouter]\nmodel = \"m\"\n").expect("no [search]");
        assert_eq!(old.lexical_defaults, LexicalLaneDefaults::default());
        assert_eq!(old.openrouter_model, "m");
    }

    /// #9258: every malformed or out-of-range `[search]` value is an error
    /// naming the key and the config origin — never a fallback to defaults.
    #[test]
    fn a_malformed_search_setting_refuses_to_start() {
        let too_big = format!("[search]\nlexical_limit = {}\n", MAX_LEXICAL_LIMIT + 1);
        let cases: [(&str, &str); 7] = [
            ("[search]\nlexical_limit = -1\n", "[search].lexical_limit"),
            (
                "[search]\nlexical_limit = \"50\"\n",
                "[search].lexical_limit",
            ),
            ("[search]\nlexical_limit = 0\n", "[search].lexical_limit"),
            (too_big.as_str(), "[search].lexical_limit"),
            (
                "[search]\nripgrep_fallback = \"false\"\n",
                "[search].ripgrep_fallback",
            ),
            ("search = 5\n", "daemon config [search]"),
            ("[search]\nripgrep_fallback = fals\n", "declares [search]"),
        ];
        // Every case is checked before failing, so one run names them all.
        let mut loaded = Vec::new();
        for (src, key) in cases {
            let Err(err) = parse(src) else {
                loaded.push(src);
                continue;
            };
            let msg = err.to_string();
            assert!(msg.contains(key), "{src:?}: {msg}");
            assert!(msg.contains("config.toml"), "{src:?}: {msg}");
            assert!(!msg.contains("\"50\""), "the value is never echoed: {msg}");
        }
        assert!(loaded.is_empty(), "loaded instead of refusing: {loaded:?}");
    }

    /// The pre-#9258 warn-and-default path for the other sections stays, and
    /// a valid `[search]` survives it.
    #[test]
    fn a_bad_other_section_keeps_the_search_settings() {
        let src = "[search]\nripgrep_fallback = false\n[local_model]\nenabled = \"yes\"\n";
        let cfg = parse(src).expect("a bad [local_model] still loads");
        assert!(!cfg.lexical_defaults.ripgrep_fallback);
        // The pre-#9258 fallback: the whole runtime default, not the serde one.
        assert_eq!(cfg.local_model.model, LocalModelConfig::default().model);
        let broken = parse("[openrouter]\nmodel = \n").expect("no [search]: defaults");
        assert_eq!(broken.lexical_defaults, LexicalLaneDefaults::default());
    }

    #[test]
    fn local_model_defaults_apply_when_section_absent() {
        let parsed: UserConfigFile = toml::from_str("").unwrap();
        assert!(parsed.local_model.enabled);
        assert_eq!(parsed.local_model.base_url, "http://localhost:11434");
        assert_eq!(parsed.local_model.model, "llama3.2");
    }
}
