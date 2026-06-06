//! Shared metric data structures for the code-analysis tool surface (#373).
//!
//! Why: Centralizes the JSON-serializable shapes returned by every analysis
//! tool so callers (LLM, test code) see one consistent vocabulary across
//! `analyze_file`, `analyze_project`, `find_smells`, etc.
//! What: `FunctionMetrics`, `FileMetrics`, `Smell`, `SmellType`, `Severity`,
//! plus helpers that classify complexity grades and severities.
//! Test: Unit tests exercise `complexity_grade` and `default_severity`.

use serde::{Deserialize, Serialize};

/// Per-function structural metrics emitted by the AST walker.
///
/// Why: One row per function gives the LLM a uniform shape it can sort and
/// filter on.
/// What: All counts are pre-computed; `complexity_grade` is derived from
/// `cyclomatic_complexity` via `complexity_grade()`.
/// Test: Indirectly via `analyze_file_returns_function_metrics`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionMetrics {
    pub name: String,
    pub start_line: usize,
    pub end_line: usize,
    pub cyclomatic_complexity: u32,
    pub cognitive_complexity: u32,
    pub max_nesting_depth: u32,
    pub parameter_count: u32,
    pub lines_of_code: usize,
    pub complexity_grade: String,
    pub smells: Vec<String>,
}

/// File-level rollup metrics.
///
/// Why: Project-wide aggregations are built by summing these.
/// What: Plain serde struct; `instability` is `Ce / (Ca + Ce)` clamped to
/// [0.0, 1.0].
/// Test: Covered by `analyze_file_returns_file_metrics`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileMetrics {
    pub total_functions: usize,
    pub avg_cyclomatic: f64,
    pub max_cyclomatic: u32,
    pub coupling_afferent: usize,
    pub coupling_efferent: usize,
    pub instability: f64,
    pub god_class: bool,
}

/// One detected code smell.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Smell {
    pub file: String,
    pub function: String,
    pub start_line: usize,
    pub smell_type: String,
    pub severity: String,
    pub details: String,
}

/// Closed enum of smell categories the analyzer recognises.
///
/// Why: Keeps smell-name strings consistent across detection sites and
/// downstream filtering (`find_smells` accepts a `smell_type` filter).
/// What: A small enum with `as_str()` for emission.
/// Test: `smelltype_as_str_round_trip`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmellType {
    LongMethod,
    DeepNesting,
    LongParameterList,
    ComplexMethod,
    GodClass,
}

impl SmellType {
    pub fn as_str(self) -> &'static str {
        match self {
            SmellType::LongMethod => "LongMethod",
            SmellType::DeepNesting => "DeepNesting",
            SmellType::LongParameterList => "LongParameterList",
            SmellType::ComplexMethod => "ComplexMethod",
            SmellType::GodClass => "GodClass",
        }
    }

    // Why: We deliberately return `Option<Self>` here rather than implementing
    // `std::str::FromStr` (which forces a `Result<Self, Err>` API and an error
    // type that callers would have to import). Internal call sites chain this
    // through `and_then`, so `Option` matches the consumer shape exactly.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "LongMethod" => Some(Self::LongMethod),
            "DeepNesting" => Some(Self::DeepNesting),
            "LongParameterList" => Some(Self::LongParameterList),
            "ComplexMethod" => Some(Self::ComplexMethod),
            "GodClass" => Some(Self::GodClass),
            _ => None,
        }
    }

    pub fn default_severity(self) -> Severity {
        match self {
            SmellType::LongMethod | SmellType::LongParameterList => Severity::Info,
            SmellType::DeepNesting | SmellType::ComplexMethod => Severity::Warning,
            SmellType::GodClass => Severity::Error,
        }
    }
}

/// Smell severity used for filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Warning => "warning",
            Severity::Error => "error",
        }
    }

    // Why: Option-returning by design — see SmellType::from_str.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "info" => Some(Self::Info),
            "warning" | "warn" => Some(Self::Warning),
            "error" => Some(Self::Error),
            _ => None,
        }
    }
}

/// Convert cyclomatic complexity to a letter grade.
///
/// Why: A grade is friendlier than a raw number for the LLM to summarise.
/// What: A=0–5, B=6–10, C=11–15, D=16–20, F=21+.
/// Test: `complexity_grade_buckets`.
pub fn complexity_grade(cyclomatic: u32) -> &'static str {
    match cyclomatic {
        0..=5 => "A",
        6..=10 => "B",
        11..=15 => "C",
        16..=20 => "D",
        _ => "F",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complexity_grade_buckets() {
        assert_eq!(complexity_grade(0), "A");
        assert_eq!(complexity_grade(5), "A");
        assert_eq!(complexity_grade(6), "B");
        assert_eq!(complexity_grade(10), "B");
        assert_eq!(complexity_grade(11), "C");
        assert_eq!(complexity_grade(16), "D");
        assert_eq!(complexity_grade(21), "F");
        assert_eq!(complexity_grade(99), "F");
    }

    #[test]
    fn smelltype_as_str_round_trip() {
        for v in [
            SmellType::LongMethod,
            SmellType::DeepNesting,
            SmellType::LongParameterList,
            SmellType::ComplexMethod,
            SmellType::GodClass,
        ] {
            assert_eq!(SmellType::from_str(v.as_str()), Some(v));
        }
    }

    #[test]
    fn severity_round_trip() {
        for s in [Severity::Info, Severity::Warning, Severity::Error] {
            assert_eq!(Severity::from_str(s.as_str()), Some(s));
        }
        assert_eq!(Severity::from_str("warn"), Some(Severity::Warning));
    }
}
