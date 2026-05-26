//! Overseer policy configuration loaded from `overseer.toml`.
//!
//! Why: the session overseer is opt-in and rule-driven; its blocklist,
//! auto-approve list, rate limit, token budget, and auto-responses must be
//! editable without recompiling the daemon. A framework-managed `overseer.toml`
//! file (alongside `optimizer.toml`) keeps the policy declarative.
//! What: [`OverseerConfig`] mirrors the on-disk TOML layout
//! (`[overseer]`, `[deterministic]`, `[auto_responses]`) and
//! [`OverseerConfig::load_from`] reads it, falling back to defaults when the
//! file is missing or malformed so the daemon always starts.
//! Test: `cargo test -p trusty-mpm-core overseer_config` covers TOML parsing
//! and the missing-file fallback.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Rule-based overseer tuning (the `[deterministic]` table).
///
/// Why: the [`crate::core::deterministic_overseer::DeterministicOverseer`] needs a
/// blocklist, an auto-approve list, a per-session rate limit, and a token
/// budget; grouping them keeps the policy file readable.
/// What: substring lists plus two numeric limits.
/// Test: `default_deterministic_is_sane`, `config_loads_from_toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeterministicConfig {
    /// Substrings that, when found in a tool input, block the tool call.
    #[serde(default)]
    pub blocklist: Vec<String>,
    /// Substrings that, when found in a tool input, auto-allow the tool call.
    #[serde(default)]
    pub auto_approve: Vec<String>,
    /// Maximum tool calls a single session may make within a sliding minute.
    #[serde(default = "default_rate_limit")]
    pub max_tool_calls_per_minute: u32,
    /// Token budget; sessions exceeding it should be halted (monitoring hook).
    #[serde(default = "default_token_budget")]
    pub token_budget_limit: u64,
}

/// `serde` default for `max_tool_calls_per_minute`.
fn default_rate_limit() -> u32 {
    120
}

/// `serde` default for `token_budget_limit`.
fn default_token_budget() -> u64 {
    200_000
}

impl Default for DeterministicConfig {
    fn default() -> Self {
        Self {
            blocklist: Vec::new(),
            auto_approve: Vec::new(),
            max_tool_calls_per_minute: default_rate_limit(),
            token_budget_limit: default_token_budget(),
        }
    }
}

/// `[overseer]` table — the top-level enable switch.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
struct OverseerSection {
    #[serde(default)]
    enabled: bool,
}

/// LLM-overseer tuning (the `[llm]` table).
///
/// Why: the LLM-backed overseer calls OpenRouter to make nuanced allow/block
/// decisions; the model name, the env var holding the API key, and the
/// opt-in flag must be editable without recompiling.
/// What: an `enabled` switch, the OpenRouter model id, and the name of the
/// environment variable that carries the API key.
/// Test: `llm_config_loads_from_toml`, `default_llm_is_disabled`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmConfig {
    /// Whether the LLM overseer is active. Opt-in: defaults to `false`.
    #[serde(default)]
    pub enabled: bool,
    /// OpenRouter model id, e.g. `meta-llama/llama-3.1-8b-instruct:free`.
    #[serde(default = "default_llm_model")]
    pub model: String,
    /// Name of the environment variable that holds the OpenRouter API key.
    #[serde(default = "default_api_key_env")]
    pub api_key_env: String,
}

/// `serde` default for `LlmConfig::model`.
fn default_llm_model() -> String {
    "meta-llama/llama-3.1-8b-instruct:free".to_string()
}

/// `serde` default for `LlmConfig::api_key_env`.
fn default_api_key_env() -> String {
    "OPENROUTER_API_KEY".to_string()
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            model: default_llm_model(),
            api_key_env: default_api_key_env(),
        }
    }
}

/// On-disk shape of `overseer.toml`.
///
/// Why: TOML tables (`[overseer]`, `[deterministic]`, `[auto_responses]`) map
/// onto nested structs; a dedicated mirror keeps the file layout decoupled from
/// the flat runtime [`OverseerConfig`].
/// What: the three policy tables, each defaulted so partial files still parse.
/// Test: `config_loads_from_toml`.
#[derive(Debug, Default, Deserialize)]
struct OverseerToml {
    #[serde(default)]
    overseer: OverseerSection,
    #[serde(default)]
    deterministic: DeterministicConfig,
    #[serde(default)]
    llm: LlmConfig,
    #[serde(default)]
    auto_responses: HashMap<String, String>,
}

/// Complete overseer policy, as consumed by the daemon at runtime.
///
/// Why: a single value the daemon builds once at startup and passes to the
/// overseer; keeping it flat (rather than the nested TOML mirror) simplifies
/// every call site.
/// What: the enable flag, the deterministic tuning, and the
/// question-pattern → response map.
/// Test: `default_is_disabled`, `config_loads_from_toml`,
/// `load_from_missing_file_is_default`.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverseerConfig {
    /// Whether oversight is active. Opt-in: defaults to `false`.
    pub enabled: bool,
    /// Rule-based overseer tuning.
    pub deterministic: DeterministicConfig,
    /// LLM-backed overseer tuning (the `[llm]` table).
    #[serde(default)]
    pub llm: LlmConfig,
    /// Question-substring → canned-response map for auto-answering sessions.
    pub auto_responses: HashMap<String, String>,
}

impl OverseerConfig {
    /// Load the overseer policy from an `overseer.toml` file.
    ///
    /// Why: the policy is framework-managed (`~/.trusty-mpm/framework/hooks/`)
    /// and edited directly; the daemon must reflect it without an API call, and
    /// must never fail to start because the file is absent or malformed.
    /// What: reads `path` and parses the `[overseer]`/`[deterministic]`/
    /// `[auto_responses]` layout. A missing *or* malformed file yields
    /// [`OverseerConfig::default`] (logged for the malformed case).
    /// Test: `config_loads_from_toml`, `load_from_missing_file_is_default`,
    /// `load_from_malformed_file_is_default`.
    pub fn load_from(path: &Path) -> Self {
        let raw = match std::fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(e) => {
                tracing::warn!(
                    "failed to read overseer config {}: {e}; using defaults",
                    path.display()
                );
                return Self::default();
            }
        };
        match toml::from_str::<OverseerToml>(&raw) {
            Ok(parsed) => Self {
                enabled: parsed.overseer.enabled,
                deterministic: parsed.deterministic,
                llm: parsed.llm,
                auto_responses: parsed.auto_responses,
            },
            Err(e) => {
                tracing::warn!(
                    "malformed overseer config {}: {e}; using defaults",
                    path.display()
                );
                Self::default()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_disabled() {
        let cfg = OverseerConfig::default();
        assert!(!cfg.enabled);
        assert!(cfg.auto_responses.is_empty());
    }

    #[test]
    fn default_deterministic_is_sane() {
        let d = DeterministicConfig::default();
        assert_eq!(d.max_tool_calls_per_minute, 120);
        assert_eq!(d.token_budget_limit, 200_000);
        assert!(d.blocklist.is_empty());
    }

    #[test]
    fn config_loads_from_toml() {
        // A full policy file must map onto an OverseerConfig with every table.
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("overseer.toml");
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(
            file,
            "[overseer]\nenabled = true\n\n\
             [deterministic]\nblocklist = [\"rm -rf /\"]\nauto_approve = [\"ls\"]\n\
             max_tool_calls_per_minute = 30\ntoken_budget_limit = 1000\n\n\
             [auto_responses]\n\"shall i proceed\" = \"yes, proceed\""
        )
        .unwrap();

        let cfg = OverseerConfig::load_from(&path);
        assert!(cfg.enabled);
        assert_eq!(cfg.deterministic.blocklist, vec!["rm -rf /".to_string()]);
        assert_eq!(cfg.deterministic.auto_approve, vec!["ls".to_string()]);
        assert_eq!(cfg.deterministic.max_tool_calls_per_minute, 30);
        assert_eq!(cfg.deterministic.token_budget_limit, 1000);
        assert_eq!(
            cfg.auto_responses
                .get("shall i proceed")
                .map(String::as_str),
            Some("yes, proceed")
        );
    }

    #[test]
    fn load_from_missing_file_is_default() {
        // A missing policy file (framework not installed) is not an error.
        let dir = tempfile::tempdir().unwrap();
        let cfg = OverseerConfig::load_from(&dir.path().join("absent.toml"));
        assert_eq!(cfg, OverseerConfig::default());
    }

    #[test]
    fn load_from_malformed_file_is_default() {
        // A malformed file must fall back to defaults rather than panic.
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("overseer.toml");
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(file, "this is = not valid = toml [[[").unwrap();
        let cfg = OverseerConfig::load_from(&path);
        assert_eq!(cfg, OverseerConfig::default());
    }

    #[test]
    fn default_llm_is_disabled() {
        // The LLM overseer is opt-in: a default config has it off but still
        // carries a sane model id and the standard env var name.
        let llm = LlmConfig::default();
        assert!(!llm.enabled);
        assert_eq!(llm.api_key_env, "OPENROUTER_API_KEY");
        assert!(llm.model.contains("llama") || llm.model.contains("haiku"));
    }

    #[test]
    fn llm_config_loads_from_toml() {
        // A `[llm]` table maps onto `OverseerConfig::llm`.
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("overseer.toml");
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(
            file,
            "[overseer]\nenabled = true\n\n\
             [llm]\nenabled = true\nmodel = \"claude-haiku-4-5\"\n\
             api_key_env = \"MY_KEY\""
        )
        .unwrap();
        let cfg = OverseerConfig::load_from(&path);
        assert!(cfg.llm.enabled);
        assert_eq!(cfg.llm.model, "claude-haiku-4-5");
        assert_eq!(cfg.llm.api_key_env, "MY_KEY");
    }

    #[test]
    fn llm_table_uses_field_defaults() {
        // A `[llm]` table with only `enabled` set defaults model + env var.
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("overseer.toml");
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(file, "[llm]\nenabled = true").unwrap();
        let cfg = OverseerConfig::load_from(&path);
        assert!(cfg.llm.enabled);
        assert_eq!(cfg.llm.api_key_env, "OPENROUTER_API_KEY");
    }

    #[test]
    fn partial_toml_uses_field_defaults() {
        // A file with only `[overseer]` must still parse, defaulting the rest.
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("overseer.toml");
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(file, "[overseer]\nenabled = true").unwrap();
        let cfg = OverseerConfig::load_from(&path);
        assert!(cfg.enabled);
        assert_eq!(cfg.deterministic, DeterministicConfig::default());
    }
}
