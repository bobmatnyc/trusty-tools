//! `BiomeTool` — TypeScript/JavaScript diagnostics via `biome check`.
//!
//! Why: biome is a fast Rust-based linter/formatter with a structured JSON
//! reporter, a good fit for on-demand analysis.
//! What: runs `biome check --reporter=json <file>` and maps each diagnostic.
//! Test: `parse_biome_json_extracts_diagnostic` parses a captured payload.

use std::path::Path;

use anyhow::Result;
use serde_json::Value;

use super::run_command;
use crate::core::tools::{Severity, StaticTool, ToolDiagnostic};

/// TypeScript/JavaScript static-analysis tool backed by `biome`.
pub struct BiomeTool;

impl StaticTool for BiomeTool {
    fn name(&self) -> &str {
        "biome"
    }

    fn language(&self) -> &str {
        "typescript"
    }

    /// Why: biome lints JavaScript as well as TypeScript, but the router maps
    /// `.js/.jsx/.mjs/.cjs` to the `"javascript"` tag. Without this alias that
    /// bucket holds no tool and every JS file is silently skipped — the JS half
    /// of the #963 class of "routed-but-no-tool" bug.
    fn aliases(&self) -> &[&str] {
        &["javascript"]
    }

    fn is_available(&self) -> bool {
        which::which("biome").is_ok()
    }

    fn run(&self, file: &Path, _content: &str) -> Result<Vec<ToolDiagnostic>> {
        let dir = file.parent().unwrap_or_else(|| Path::new("."));
        let path = file.to_string_lossy();
        let out = match run_command("biome", &["check", "--reporter=json", &path], dir) {
            Ok(o) => o,
            Err(e) => {
                tracing::debug!("biome invocation failed: {e}");
                return Ok(Vec::new());
            }
        };
        Ok(parse_biome_json(&out.stdout))
    }
}

/// Parse biome's JSON report into diagnostics.
fn parse_biome_json(stdout: &str) -> Vec<ToolDiagnostic> {
    let Ok(root) = serde_json::from_str::<Value>(stdout.trim()) else {
        return Vec::new();
    };
    let Some(diagnostics) = root.get("diagnostics").and_then(Value::as_array) else {
        return Vec::new();
    };
    diagnostics.iter().filter_map(biome_diag_to_diag).collect()
}

/// Convert a single biome diagnostic object into a `ToolDiagnostic`.
fn biome_diag_to_diag(d: &Value) -> Option<ToolDiagnostic> {
    let location = d.get("location")?;
    let file = location
        .get("path")
        .and_then(|p| p.get("file"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    // biome encodes the span as a byte offset pair; line/col are not directly
    // present, so we fall back to 0 when the offset cannot be resolved.
    let line = location
        .get("span")
        .and_then(Value::as_array)
        .and_then(|s| s.first())
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let severity = severity_from_str(d.get("severity").and_then(Value::as_str).unwrap_or(""));
    let message = extract_description(d);
    let code = d
        .get("category")
        .and_then(Value::as_str)
        .map(str::to_string);
    Some(ToolDiagnostic {
        tool: "biome".into(),
        file,
        line,
        col: 0,
        severity,
        code,
        message,
    })
}

/// Extract a human-readable description from a biome diagnostic.
fn extract_description(d: &Value) -> String {
    if let Some(s) = d.get("description").and_then(Value::as_str) {
        return s.to_string();
    }
    // Newer biome versions nest the message under `message[].content`.
    if let Some(parts) = d.get("message").and_then(Value::as_array) {
        let joined: String = parts
            .iter()
            .filter_map(|p| p.get("content").and_then(Value::as_str))
            .collect();
        if !joined.is_empty() {
            return joined;
        }
    }
    String::new()
}

/// Map a biome severity string to a `Severity`.
fn severity_from_str(s: &str) -> Severity {
    match s {
        "error" | "fatal" => Severity::Error,
        "warning" => Severity::Warning,
        "information" | "info" => Severity::Info,
        _ => Severity::Hint,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_biome_json_extracts_diagnostic() {
        let json = r#"{"diagnostics":[{"severity":"error","description":"unexpected token","category":"parse","location":{"path":{"file":"a.ts"},"span":[10,12]}}]}"#;
        let diags = parse_biome_json(json);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].file, "a.ts");
        assert_eq!(diags[0].severity, Severity::Error);
        assert_eq!(diags[0].message, "unexpected token");
    }

    #[test]
    fn parse_biome_json_tolerates_garbage() {
        assert!(parse_biome_json("not json").is_empty());
        assert!(parse_biome_json("{}").is_empty());
    }

    #[test]
    fn severity_from_str_maps_known_values() {
        assert_eq!(severity_from_str("error"), Severity::Error);
        assert_eq!(severity_from_str("warning"), Severity::Warning);
        assert_eq!(severity_from_str("information"), Severity::Info);
        assert_eq!(severity_from_str("other"), Severity::Hint);
    }

    #[test]
    fn biome_covers_typescript_and_javascript() {
        // biome lints both; the router maps .ts/.tsx → "typescript" and
        // .js/.jsx/.mjs/.cjs → "javascript", so the tool must claim both or
        // JS files are silently skipped.
        assert_eq!(BiomeTool.language(), "typescript");
        assert_eq!(BiomeTool.aliases(), &["javascript"]);
    }
}
