//! Pluggable format translation for write_file and agent tools.
//!
//! Why: Agents frequently produce content in one structured format and need to
//! emit another (e.g. Markdown body rendered to HTML for a docs site, or JSON
//! config translated to TOML for a Rust project). Centralizing these
//! conversions behind a small trait keeps the logic testable and deterministic
//! — same input always yields the same output — and avoids scattering
//! ad-hoc `toml::from_str` / `serde_yml::from_str` calls across the codebase.
//! What: Defines `Format` (the supported file formats), a `FormatTranslator`
//! trait, four concrete translators (Markdown→HTML, JSON↔TOML, YAML→JSON),
//! and a `TranslatorRegistry` that looks up a translator by (from, to) pair.
//! Test: Unit tests exercise each translator, verify `Format::from_extension`,
//! and confirm the registry errors on unsupported pairs.

#![allow(dead_code)]
#![allow(clippy::wrong_self_convention)]

use anyhow::{Result, anyhow};

/// Supported formats for translation.
///
/// Why: A closed enum keeps the translator graph auditable and the
/// `from_extension` mapping exhaustive.
/// What: Each variant corresponds to a file-level representation agents
/// commonly emit.
/// Test: `test_format_from_extension` covers the mapping.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Format {
    Markdown,
    Html,
    Json,
    Toml,
    Yaml,
    PlainText,
}

impl Format {
    /// Map a lowercased file extension to a `Format`, or `None` if unsupported.
    ///
    /// Why: Callers (write_file, docs tooling) typically know only the file
    /// extension; centralizing the mapping keeps behavior consistent.
    /// What: Returns `Some(Format::...)` for known extensions, `None` otherwise.
    /// Test: `test_format_from_extension`.
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext.to_lowercase().as_str() {
            "md" | "markdown" => Some(Self::Markdown),
            "html" | "htm" => Some(Self::Html),
            "json" => Some(Self::Json),
            "toml" => Some(Self::Toml),
            "yaml" | "yml" => Some(Self::Yaml),
            "txt" => Some(Self::PlainText),
            _ => None,
        }
    }
}

/// Trait implemented by each deterministic format-conversion pair.
///
/// Why: The trait lets the registry hold heterogeneous translators behind
/// `Box<dyn FormatTranslator>` while keeping the conversion logic colocated
/// with the formats it translates.
/// What: `from_format`/`to_format` declare the edge in the translation graph;
/// `translate` performs the actual conversion.
/// Test: Each translator has a dedicated unit test below.
pub trait FormatTranslator: Send + Sync {
    fn from_format(&self) -> Format;
    fn to_format(&self) -> Format;
    fn translate(&self, input: &str) -> Result<String>;
}

/// Markdown → HTML using comrak with GFM extensions enabled.
///
/// Why: Docs agents emit Markdown; downstream consumers (docs sites, PR
/// previews) typically need HTML, and enabling GFM covers tables, strikethrough,
/// and task lists which show up constantly in LLM output.
/// What: Uses `comrak::markdown_to_html` with `ExtensionOptionsBuilder` set to
/// enable table, strikethrough, tasklist, autolink, and header-ID extensions.
/// Test: `test_markdown_to_html_basic`, `test_markdown_to_html_tables`.
pub struct MarkdownToHtml;

impl FormatTranslator for MarkdownToHtml {
    fn from_format(&self) -> Format {
        Format::Markdown
    }
    fn to_format(&self) -> Format {
        Format::Html
    }
    fn translate(&self, input: &str) -> Result<String> {
        let mut options = comrak::Options::default();
        options.extension.table = true;
        options.extension.strikethrough = true;
        options.extension.tasklist = true;
        options.extension.autolink = true;
        options.extension.header_id_prefix = Some("h-".to_string());
        Ok(comrak::markdown_to_html(input, &options))
    }
}

/// JSON → TOML via `serde_json::Value` round-trip.
///
/// Why: Agents often produce JSON config; Rust projects expect TOML.
/// What: Parses input into `serde_json::Value`, then serializes to TOML.
/// Test: `test_json_to_toml_roundtrip`.
pub struct JsonToToml;

impl FormatTranslator for JsonToToml {
    fn from_format(&self) -> Format {
        Format::Json
    }
    fn to_format(&self) -> Format {
        Format::Toml
    }
    fn translate(&self, input: &str) -> Result<String> {
        let value: serde_json::Value =
            serde_json::from_str(input).map_err(|e| anyhow!("invalid JSON: {e}"))?;
        toml::to_string(&value).map_err(|e| anyhow!("failed to serialize TOML: {e}"))
    }
}

/// TOML → JSON via `toml::Value` → `serde_json::Value`.
///
/// Why: Inverse of JsonToToml. Useful when agents read a Rust project's
/// TOML config and need to hand it to JSON-native downstream tools.
/// What: Parses input as `toml::Value`, then pretty-prints as JSON.
/// Test: `test_toml_to_json`.
pub struct TomlToJson;

impl FormatTranslator for TomlToJson {
    fn from_format(&self) -> Format {
        Format::Toml
    }
    fn to_format(&self) -> Format {
        Format::Json
    }
    fn translate(&self, input: &str) -> Result<String> {
        let value: toml::Value = toml::from_str(input).map_err(|e| anyhow!("invalid TOML: {e}"))?;
        serde_json::to_string_pretty(&value).map_err(|e| anyhow!("failed to serialize JSON: {e}"))
    }
}

/// YAML → JSON via `serde_yml::Value` → `serde_json::Value`.
///
/// Why: YAML is common in CI and k8s ecosystems; translating to JSON lets the
/// downstream tools consume the data without a YAML parser.
/// What: Parses input as `serde_yml::Value`, then pretty-prints as JSON.
/// Test: `test_yaml_to_json`.
pub struct YamlToJson;

impl FormatTranslator for YamlToJson {
    fn from_format(&self) -> Format {
        Format::Yaml
    }
    fn to_format(&self) -> Format {
        Format::Json
    }
    fn translate(&self, input: &str) -> Result<String> {
        let value: serde_json::Value =
            serde_yml::from_str(input).map_err(|e| anyhow!("invalid YAML: {e}"))?;
        serde_json::to_string_pretty(&value).map_err(|e| anyhow!("failed to serialize JSON: {e}"))
    }
}

/// Registry mapping (from, to) to a translator implementation.
///
/// Why: Keeps translator discovery open/closed: callers register translators
/// once and look them up by endpoint pair.
/// What: Linear lookup over `Vec<Box<dyn FormatTranslator>>`. The set is
/// small (4 translators) so a hash map would be overkill.
/// Test: `test_registry_no_translator_errors`, `test_registry_translates`.
pub struct TranslatorRegistry {
    translators: Vec<Box<dyn FormatTranslator>>,
}

impl TranslatorRegistry {
    /// Build the default registry containing the four built-in translators.
    ///
    /// Why: Most callers want the default set; custom registries can add
    /// bespoke translators on top.
    /// What: Boxes and collects MarkdownToHtml, JsonToToml, TomlToJson, YamlToJson.
    /// Test: `test_registry_translates` exercises the default set.
    pub fn default_registry() -> Self {
        Self {
            translators: vec![
                Box::new(MarkdownToHtml),
                Box::new(JsonToToml),
                Box::new(TomlToJson),
                Box::new(YamlToJson),
            ],
        }
    }

    /// Look up and invoke a translator for the given (from, to) pair.
    ///
    /// Why: Callers supply formats rather than concrete translator types so
    /// they can stay agnostic of the registry contents.
    /// What: Linear search for the first translator matching both endpoints;
    /// returns its `translate` result or an error naming the unsupported pair.
    /// Test: `test_registry_no_translator_errors`.
    pub fn translate(&self, input: &str, from: &Format, to: &Format) -> Result<String> {
        for t in &self.translators {
            if t.from_format() == *from && t.to_format() == *to {
                return t.translate(input);
            }
        }
        Err(anyhow!("no translator registered for {from:?} -> {to:?}"))
    }
}

impl Default for TranslatorRegistry {
    fn default() -> Self {
        Self::default_registry()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_markdown_to_html_basic() {
        let out = MarkdownToHtml.translate("# Hello\n").unwrap();
        // comrak emits `<h1>` with a header-id attribute; assert the core markup.
        assert!(out.contains("<h1"), "missing <h1: {out}");
        assert!(out.contains("Hello</h1>"), "missing Hello</h1>: {out}");
    }

    #[test]
    fn test_markdown_to_html_tables() {
        let md = "| a | b |\n|---|---|\n| 1 | 2 |\n";
        let out = MarkdownToHtml.translate(md).unwrap();
        assert!(out.contains("<table>"), "missing table: {out}");
    }

    #[test]
    fn test_json_to_toml_roundtrip() {
        let json = r#"{"key": "value", "n": 42}"#;
        let toml_out = JsonToToml.translate(json).unwrap();
        // Round-trip: convert back and assert structural equality.
        let json_again = TomlToJson.translate(&toml_out).unwrap();
        let a: serde_json::Value = serde_json::from_str(json).unwrap();
        let b: serde_json::Value = serde_json::from_str(&json_again).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn test_toml_to_json() {
        let toml_in = "key = \"value\"\n";
        let out = TomlToJson.translate(toml_in).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["key"], "value");
    }

    #[test]
    fn test_yaml_to_json() {
        let yaml_in = "key: value\n";
        let out = YamlToJson.translate(yaml_in).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["key"], "value");
    }

    #[test]
    fn test_format_from_extension() {
        assert_eq!(Format::from_extension("md"), Some(Format::Markdown));
        assert_eq!(Format::from_extension("markdown"), Some(Format::Markdown));
        assert_eq!(Format::from_extension("json"), Some(Format::Json));
        assert_eq!(Format::from_extension("TOML"), Some(Format::Toml));
        assert_eq!(Format::from_extension("yaml"), Some(Format::Yaml));
        assert_eq!(Format::from_extension("yml"), Some(Format::Yaml));
        assert_eq!(Format::from_extension("html"), Some(Format::Html));
        assert_eq!(Format::from_extension("txt"), Some(Format::PlainText));
        assert_eq!(Format::from_extension("rs"), None);
    }

    #[test]
    fn test_registry_no_translator_errors() {
        let reg = TranslatorRegistry::default_registry();
        let err = reg
            .translate("# hi\n", &Format::Markdown, &Format::Json)
            .unwrap_err();
        assert!(
            err.to_string().contains("no translator"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_registry_translates() {
        let reg = TranslatorRegistry::default_registry();
        let out = reg
            .translate("# Hello\n", &Format::Markdown, &Format::Html)
            .unwrap();
        assert!(out.contains("<h1"), "registry didn't produce HTML: {out}");
    }
}
