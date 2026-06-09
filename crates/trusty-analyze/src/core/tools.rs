//! `StaticTool` plugin trait and `ToolDiagnostic` types.
//!
//! Why: trusty-analyzer's tree-sitter adapters give a uniform structural
//! baseline, but real-world linters (clippy, ruff, biome, ...) catch issues a
//! grammar walk never will. This module defines the minimal surface every
//! external static-analysis tool exposes so the orchestration layer can probe,
//! select, and run them without knowing which binary backs each one.
//!
//! What: a `Severity` enum, a `ToolDiagnostic` record (one finding), and the
//! `StaticTool` trait (`name`/`language`/`is_available`/`run`).
//!
//! Test: `severity_serializes_lowercase` pins the wire format; the per-tool
//! impl tests in `tool_impls` exercise `run` against captured fixtures.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Severity of a single tool diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// A definite error — code is wrong or will not compile.
    Error,
    /// A likely problem worth fixing.
    Warning,
    /// Informational note.
    Info,
    /// A low-priority hint or style suggestion.
    Hint,
}

/// One finding emitted by a `StaticTool`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDiagnostic {
    /// Tool that produced this diagnostic, e.g. `"clippy"`.
    pub tool: String,
    /// Repo-relative or absolute file path the finding applies to.
    pub file: String,
    /// 1-based line number.
    pub line: u32,
    /// 1-based column number.
    pub col: u32,
    /// Severity bucket.
    pub severity: Severity,
    /// Tool-specific rule code, e.g. `"clippy::needless_return"`.
    pub code: Option<String>,
    /// Human-readable description of the finding.
    pub message: String,
}

/// Plugin interface for an external static-analysis tool.
///
/// Implementations shell out to a binary (clippy, ruff, ...), parse its
/// output, and normalize findings into `ToolDiagnostic`s. Implementations must
/// never panic: a missing binary, non-zero exit, or malformed output should
/// surface as `Ok(vec![])` or an `Err`, never an `unwrap`.
pub trait StaticTool: Send + Sync {
    /// Stable tool identifier, e.g. `"clippy"`.
    fn name(&self) -> &str;

    /// Language tag this tool analyzes, matching `LanguageDetector` tags
    /// (`"rust"`, `"python"`, `"typescript"`, ...).
    fn language(&self) -> &str;

    /// Additional language buckets this tool should also be registered under,
    /// beyond its primary [`language`](Self::language).
    ///
    /// Why: a single binary often lints several `LanguageDetector` tags — e.g.
    /// biome lints both `"typescript"` and `"javascript"`. Without this, files
    /// routed to the secondary tag find an empty bucket and are silently
    /// skipped (the JS half of the #963 class of bug). Defaults to no aliases.
    /// What: each returned tag gets its own registry entry pointing at this
    /// tool, in addition to `language()`.
    /// Test: `aliases_register_tool_under_every_bucket` and
    /// `biome_covers_typescript_and_javascript`.
    fn aliases(&self) -> &[&str] {
        &[]
    }

    /// Cheap availability probe — confirms the backing binary is on `PATH`.
    fn is_available(&self) -> bool;

    /// Run the tool against `file` (whose content is `content`) and return
    /// normalized diagnostics. Returns `Ok(vec![])` when the tool runs cleanly
    /// with no findings; returns `Err` only for unexpected failures.
    fn run(&self, file: &Path, content: &str) -> anyhow::Result<Vec<ToolDiagnostic>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&Severity::Error).expect("serialize"),
            "\"error\""
        );
        assert_eq!(
            serde_json::to_string(&Severity::Hint).expect("serialize"),
            "\"hint\""
        );
    }

    #[test]
    fn diagnostic_round_trips() {
        let d = ToolDiagnostic {
            tool: "clippy".into(),
            file: "src/main.rs".into(),
            line: 12,
            col: 4,
            severity: Severity::Warning,
            code: Some("clippy::needless_return".into()),
            message: "unneeded return statement".into(),
        };
        let json = serde_json::to_string(&d).expect("serialize");
        let back: ToolDiagnostic = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.tool, "clippy");
        assert_eq!(back.line, 12);
        assert_eq!(back.severity, Severity::Warning);
    }
}
