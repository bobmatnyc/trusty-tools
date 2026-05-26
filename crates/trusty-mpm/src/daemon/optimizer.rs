//! Token-use optimizer: wires CompressionLevel into the PostToolUse relay.
//!
//! Why: compression decisions and config live in one place so the HTTP API,
//! MCP tool, and dashboard all share the same tuning.
//! What: OptimizerConfig (level + per-tool overrides) and
//! optimize_tool_output() which the hook relay calls before push_hook_event.
//! Test: `cargo test -p trusty-mpm-daemon optimizer` verifies that
//! PostToolUse payloads are rewritten when level >= Trim.

use crate::core::compress::{CompressionLevel, compress_output};
use serde_json::Value;

/// Per-tool compression override. Keyed by tool name (e.g. "Bash", "Read").
pub type ToolOverrides = std::collections::HashMap<String, CompressionLevel>;

/// Tuning for the token-use optimizer.
///
/// Why: a single struct lets the HTTP API, MCP backend, and CLI share one
/// view of how aggressively tool outputs should be compressed.
/// What: a default level applied to all tools plus per-tool overrides and a
/// flag controlling redundant-read suppression.
/// Test: `optimize_uses_tool_override`, `get_optimizer_returns_default`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct OptimizerConfig {
    /// Default compression level for all tool outputs.
    pub default_level: CompressionLevel,
    /// Per-tool overrides (tool name → level).
    pub tool_overrides: ToolOverrides,
    /// Whether to suppress redundant Read calls (same path already in window).
    pub suppress_redundant_reads: bool,
}

impl Default for OptimizerConfig {
    fn default() -> Self {
        Self {
            default_level: CompressionLevel::Trim,
            tool_overrides: ToolOverrides::new(),
            suppress_redundant_reads: true,
        }
    }
}

/// On-disk shape of `optimizer.toml` (the framework hook policy file).
///
/// Why: the human-edited policy file uses a friendly `[default] / [tools]`
/// layout that differs from the wire/serde shape of [`OptimizerConfig`]; a
/// dedicated mirror type keeps the file format decoupled from the runtime
/// struct.
/// What: `[default].level` plus a `[tools]` table of name → level overrides.
/// Test: `config_loads_from_toml_file`.
#[derive(Debug, serde::Deserialize)]
struct OptimizerToml {
    #[serde(default)]
    default: OptimizerTomlDefault,
    #[serde(default)]
    tools: std::collections::HashMap<String, TomlLevel>,
}

/// A `CompressionLevel` parsed case-insensitively from the policy file.
///
/// Why: per-tool overrides in `[tools]` accept the same friendly capitalized
/// names as `[default].level`; a newtype lets the same lenient parser apply.
/// What: wraps `CompressionLevel`, deserializing via [`de_level`].
/// Test: `config_loads_from_toml_file`.
#[derive(Debug, Clone, Copy)]
struct TomlLevel(CompressionLevel);

impl<'de> serde::Deserialize<'de> for TomlLevel {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        de_level(deserializer).map(TomlLevel)
    }
}

/// `[default]` table of `optimizer.toml`.
#[derive(Debug, serde::Deserialize)]
struct OptimizerTomlDefault {
    #[serde(default = "default_trim", deserialize_with = "de_level")]
    level: CompressionLevel,
}

impl Default for OptimizerTomlDefault {
    fn default() -> Self {
        Self {
            level: CompressionLevel::Trim,
        }
    }
}

/// `serde` default for the `[default].level` field — `Trim`.
fn default_trim() -> CompressionLevel {
    CompressionLevel::Trim
}

/// Case-insensitively parse a compression level from the policy file.
///
/// Why: the human-edited `optimizer.toml` uses friendly capitalized names
/// (`"Trim"`, `"Caveman"`) while `CompressionLevel`'s wire form is snake_case;
/// accepting either keeps the documented file format working.
/// What: lowercases the string then defers to `CompressionLevel`'s serde impl.
/// Test: `config_loads_from_toml_file`.
fn de_level<'de, D>(deserializer: D) -> Result<CompressionLevel, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    let raw = String::deserialize(deserializer)?;
    let lowered = raw.to_ascii_lowercase();
    CompressionLevel::deserialize(serde::de::value::StrDeserializer::<D::Error>::new(&lowered))
}

impl OptimizerConfig {
    /// Compression level for a given tool name.
    ///
    /// Why: callers need the effective level after overrides are resolved.
    /// What: returns the per-tool override if present, else `default_level`.
    /// Test: `optimize_uses_tool_override`.
    pub fn level_for(&self, tool: &str) -> CompressionLevel {
        self.tool_overrides
            .get(tool)
            .copied()
            .unwrap_or(self.default_level)
    }

    /// Load an optimizer config from a framework `optimizer.toml` file.
    ///
    /// Why: the optimizer policy is framework-managed — it lives on disk under
    /// `~/.trusty-mpm/framework/hooks/` and is edited directly (or reset via
    /// `trusty-mpm install --force`), not mutated through the API.
    /// What: reads `path`, parses the `[default]`/`[tools]` layout, and maps it
    /// onto an [`OptimizerConfig`]. A missing file yields
    /// `Ok(OptimizerConfig::default())` so an un-installed framework still
    /// runs; a malformed file yields an error.
    /// Test: `config_loads_from_toml_file`, `config_missing_file_is_default`.
    pub fn load_from_file(path: &std::path::Path) -> anyhow::Result<Self> {
        let raw = match std::fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(e) => return Err(e.into()),
        };
        let parsed: OptimizerToml = toml::from_str(&raw)?;
        let tool_overrides = parsed
            .tools
            .into_iter()
            .map(|(tool, level)| (tool, level.0))
            .collect();
        Ok(Self {
            default_level: parsed.default.level,
            tool_overrides,
            suppress_redundant_reads: true,
        })
    }
}

/// Apply compression to a PostToolUse hook payload.
///
/// Why: the relay calls this before pushing to the ring buffer so downstream
/// consumers (dashboard, Telegram) and the compacted session history all see
/// the trimmed output.
/// What: looks for `payload["output"]` (string field), compresses it, updates
/// the payload in place. Returns compression stats.
/// Test: `optimize_rewrites_large_output`, `optimize_skips_small_output`.
pub fn optimize_tool_output(
    config: &OptimizerConfig,
    tool_name: &str,
    payload: &mut Value,
) -> crate::core::compress::CompressionStats {
    let level = config.level_for(tool_name);
    if level == CompressionLevel::Off {
        return Default::default();
    }
    if let Some(output) = payload.get("output").and_then(Value::as_str) {
        let (compressed, stats) = compress_output(output, level);
        payload["output"] = Value::String(compressed);
        stats
    } else {
        Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::compress::TRIM_THRESHOLD_BYTES;
    use serde_json::json;

    fn big_output() -> String {
        (0..500)
            .map(|i| format!("line {i} with some padding content"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn optimize_rewrites_large_output_at_trim_level() {
        let cfg = OptimizerConfig::default(); // Trim
        let original = big_output();
        assert!(original.len() > TRIM_THRESHOLD_BYTES);
        let mut payload = json!({ "tool": "Bash", "output": original.clone() });
        let stats = optimize_tool_output(&cfg, "Bash", &mut payload);
        assert!(stats.saved_bytes() > 0);
        let rewritten = payload["output"].as_str().unwrap();
        assert!(rewritten.len() < original.len());
    }

    #[test]
    fn optimize_skips_small_output_at_trim_level() {
        let cfg = OptimizerConfig::default();
        let mut payload = json!({ "tool": "Bash", "output": "tiny output" });
        let stats = optimize_tool_output(&cfg, "Bash", &mut payload);
        assert_eq!(stats.saved_bytes(), 0);
        assert_eq!(payload["output"], "tiny output");
    }

    #[test]
    fn optimize_uses_tool_override() {
        let mut overrides = ToolOverrides::new();
        overrides.insert("Bash".into(), CompressionLevel::Caveman);
        let cfg = OptimizerConfig {
            tool_overrides: overrides,
            ..OptimizerConfig::default()
        };
        // Default level stays Trim; Bash is overridden to Caveman.
        assert_eq!(cfg.level_for("Read"), CompressionLevel::Trim);
        assert_eq!(cfg.level_for("Bash"), CompressionLevel::Caveman);

        let mut payload = json!({ "tool": "Bash", "output": "small" });
        optimize_tool_output(&cfg, "Bash", &mut payload);
        // Caveman replaces even tiny outputs with a summary.
        assert!(payload["output"].as_str().unwrap().contains("suppressed"));
    }

    #[test]
    fn optimize_off_level_is_passthrough() {
        let cfg = OptimizerConfig {
            default_level: CompressionLevel::Off,
            ..OptimizerConfig::default()
        };
        let original = big_output();
        let mut payload = json!({ "tool": "Bash", "output": original.clone() });
        let stats = optimize_tool_output(&cfg, "Bash", &mut payload);
        assert_eq!(stats.saved_bytes(), 0);
        assert_eq!(payload["output"], original);
    }

    #[test]
    fn optimize_caveman_always_replaces() {
        let cfg = OptimizerConfig {
            default_level: CompressionLevel::Caveman,
            ..OptimizerConfig::default()
        };
        let mut payload = json!({ "tool": "Read", "output": "x" });
        optimize_tool_output(&cfg, "Read", &mut payload);
        assert!(payload["output"].as_str().unwrap().contains("suppressed"));
    }

    #[test]
    fn optimize_missing_output_field_is_noop() {
        let cfg = OptimizerConfig::default();
        let mut payload = json!({ "tool": "Bash" });
        let stats = optimize_tool_output(&cfg, "Bash", &mut payload);
        assert_eq!(stats.saved_bytes(), 0);
    }

    #[test]
    fn config_loads_from_toml_file() {
        // The framework `[default]/[tools]` policy layout must map onto an
        // OptimizerConfig with the declared default level and per-tool overrides.
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("optimizer.toml");
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(
            file,
            "[default]\nlevel = \"Caveman\"\n\n[tools]\nRead = \"Off\""
        )
        .unwrap();
        let cfg = OptimizerConfig::load_from_file(&path).unwrap();
        assert_eq!(cfg.default_level, CompressionLevel::Caveman);
        assert_eq!(cfg.level_for("Read"), CompressionLevel::Off);
        assert_eq!(cfg.level_for("Bash"), CompressionLevel::Caveman);
    }

    #[test]
    fn config_missing_file_is_default() {
        // A missing policy file (framework not installed) is not an error — it
        // yields the default config so the daemon still runs.
        let dir = tempfile::tempdir().unwrap();
        let cfg = OptimizerConfig::load_from_file(&dir.path().join("absent.toml")).unwrap();
        assert_eq!(cfg.default_level, OptimizerConfig::default().default_level);
    }
}
