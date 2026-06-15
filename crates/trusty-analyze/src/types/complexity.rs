//! Complexity metrics passed between trusty-search and trusty-analyzer.
//!
//! Wire-compatible with `trusty_search_core::complexity::ComplexityMetrics`.
//! The variant names on `ComplexityGrade` and `CodeSmell` match exactly so
//! serde round-trips JSON produced by the search daemon without translation.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Runtime-configurable thresholds for code-smell detection.
///
/// Why: The README advertises configurable thresholds, but the original
/// implementation used compile-time constants. This struct lets callers
/// override thresholds at startup (or per-request) without recompilation,
/// keeping the defaults identical to the former constants so existing
/// deployments see no behaviour change unless they explicitly set values.
///
/// What: Holds the three numeric thresholds used by both the text-heuristic
/// and tree-sitter-backed smell detectors. Created with `Default::default()`
/// to get the original constant values.
///
/// Test: `custom_thresholds_change_detection_results` in `types/complexity`
/// tests proves that lowering a threshold triggers the smell on a chunk that
/// the default threshold would not flag.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SmellThresholds {
    /// Lines-of-code threshold above which a function is flagged as
    /// `LongFunction`. Default: 50.
    pub long_function_lines: usize,
    /// Maximum nesting depth above which `DeepNesting` is reported. Default: 4.
    pub deep_nesting_depth: u8,
    /// Parameter count above which `TooManyParams` is reported. Default: 5.
    pub too_many_params: usize,
}

impl Default for SmellThresholds {
    /// Why: matches the former compile-time constants so existing callers that
    /// construct `SmellThresholds::default()` observe no behaviour change.
    /// What: returns `long_function_lines=50`, `deep_nesting_depth=4`,
    /// `too_many_params=5`.
    /// Test: `default_thresholds_match_legacy_constants` below.
    fn default() -> Self {
        Self {
            long_function_lines: 50,
            deep_nesting_depth: 4,
            too_many_params: 5,
        }
    }
}

/// Bundle of per-chunk complexity numbers and detected smells.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ComplexityMetrics {
    #[serde(default)]
    pub cyclomatic: u32,
    #[serde(default)]
    pub cognitive: u32,
    #[serde(default)]
    pub grade: ComplexityGrade,
    #[serde(default)]
    pub smells: Vec<CodeSmell>,
}

/// Letter grade derived from cyclomatic complexity. A is best, F is worst.
///
/// Note: `E` is intentionally absent. The grading bands match trusty-search's
/// thresholds exactly (A/B/C/D/F), and the variant set is part of the wire
/// format — adding `E` would break JSON compatibility with existing payloads
/// produced by the search daemon.
///
/// Variants are declared in natural order (A < B < C < D < F), so the derived
/// `PartialOrd`/`Ord` impls let callers compare grades directly (e.g. to flag
/// any chunk worse than `B`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ComplexityGrade {
    #[default]
    A,
    B,
    C,
    D,
    F,
}

impl fmt::Display for ComplexityGrade {
    /// Why: Lets callers render the grade as a single letter for HTTP/MCP
    /// responses and CLI output without each call site re-implementing the
    /// match.
    /// What: Writes one of `"A"`, `"B"`, `"C"`, `"D"`, `"F"`.
    /// Test: `assert_eq!(ComplexityGrade::B.to_string(), "B")`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            ComplexityGrade::A => "A",
            ComplexityGrade::B => "B",
            ComplexityGrade::C => "C",
            ComplexityGrade::D => "D",
            ComplexityGrade::F => "F",
        };
        f.write_str(s)
    }
}

impl ComplexityGrade {
    /// Map a cyclomatic complexity number to a letter grade. Bands match
    /// trusty-search exactly so the same chunk receives the same grade in
    /// both projects.
    pub fn from_cyclomatic(v: u32) -> Self {
        match v {
            0..=5 => Self::A,
            6..=10 => Self::B,
            11..=15 => Self::C,
            16..=20 => Self::D,
            _ => Self::F,
        }
    }
}

/// A single detected code smell within a chunk.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum CodeSmell {
    LongFunction { lines: usize },
    DeepNesting { max_depth: u8 },
    TooManyParams { count: usize },
    MissingDocstring,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grade_from_cyclomatic_thresholds() {
        assert_eq!(ComplexityGrade::from_cyclomatic(0), ComplexityGrade::A);
        assert_eq!(ComplexityGrade::from_cyclomatic(5), ComplexityGrade::A);
        assert_eq!(ComplexityGrade::from_cyclomatic(6), ComplexityGrade::B);
        assert_eq!(ComplexityGrade::from_cyclomatic(11), ComplexityGrade::C);
        assert_eq!(ComplexityGrade::from_cyclomatic(16), ComplexityGrade::D);
        assert_eq!(ComplexityGrade::from_cyclomatic(50), ComplexityGrade::F);
    }

    #[test]
    fn grade_display_is_single_letter() {
        assert_eq!(ComplexityGrade::A.to_string(), "A");
        assert_eq!(ComplexityGrade::B.to_string(), "B");
        assert_eq!(ComplexityGrade::C.to_string(), "C");
        assert_eq!(ComplexityGrade::D.to_string(), "D");
        assert_eq!(ComplexityGrade::F.to_string(), "F");
    }

    #[test]
    fn grade_orders_a_through_f() {
        assert!(ComplexityGrade::A < ComplexityGrade::B);
        assert!(ComplexityGrade::B < ComplexityGrade::C);
        assert!(ComplexityGrade::C < ComplexityGrade::D);
        assert!(ComplexityGrade::D < ComplexityGrade::F);
    }

    #[test]
    fn metrics_round_trip_json() {
        let m = ComplexityMetrics {
            cyclomatic: 7,
            cognitive: 12,
            grade: ComplexityGrade::B,
            smells: vec![CodeSmell::LongFunction { lines: 80 }],
        };
        let s = serde_json::to_string(&m).unwrap();
        let back: ComplexityMetrics = serde_json::from_str(&s).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn metrics_default_when_field_missing() {
        // Forward-compat: trusty-search may emit chunks without a
        // `complexity` field. Default deserialization must not fail.
        let s = r#"{}"#;
        let m: ComplexityMetrics = serde_json::from_str(s).unwrap();
        assert_eq!(m, ComplexityMetrics::default());
    }

    #[test]
    fn default_thresholds_match_legacy_constants() {
        // Why: verifies backward compatibility — default thresholds must equal
        // the original compile-time constants so no behaviour change occurs
        // when callers construct `SmellThresholds::default()`.
        let t = SmellThresholds::default();
        assert_eq!(t.long_function_lines, 50);
        assert_eq!(t.deep_nesting_depth, 4);
        assert_eq!(t.too_many_params, 5);
    }

    #[test]
    fn smell_thresholds_round_trip_json() {
        let t = SmellThresholds {
            long_function_lines: 30,
            deep_nesting_depth: 2,
            too_many_params: 3,
        };
        let s = serde_json::to_string(&t).unwrap();
        let back: SmellThresholds = serde_json::from_str(&s).unwrap();
        assert_eq!(t, back);
    }
}
