//! Refactor suggestions derived from complexity metrics and code smells.
//!
//! Why: complexity + smell detection tells callers *what* is wrong; the refactor
//! analyzer tells them *what to do about it*. Maps detected issues to concrete,
//! human-readable refactoring actions (extract method, reduce nesting, …) so
//! Claude Code, dashboards, and CLIs can surface actionable guidance without
//! re-implementing the rule engine.
//!
//! What: a pure rule engine — `analyze()` takes a chunk's identity + complexity
//! metrics + smells and returns zero or more `RefactorSuggestion`s. Severity
//! mirrors the chunk's `ComplexityGrade` (A → skip, F → Critical) with a one-
//! level bump when 3+ smells are present.
//!
//! Test: see `mod tests` below — covers grade A (no suggestion), grade B (Low),
//! grade F + LongFunction (Critical + ExtractMethod), and severity bump
//! (grade C + 3 smells → High).

use crate::core::region_kind::RegionKind;
use crate::types::{CodeSmell, ComplexityGrade, ComplexityMetrics};
use serde::{Deserialize, Serialize};

/// One actionable refactoring suggestion for a chunk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefactorSuggestion {
    pub chunk_id: String,
    pub file: String,
    pub line_start: u32,
    pub line_end: u32,
    pub function_name: Option<String>,
    /// What kind of region was measured, when the analyzer can tell (#6177).
    ///
    /// Why: a hotspot's remediation only makes sense once the reader knows what
    /// was measured. A Python `class Foo:` body and a nameless function body
    /// were indistinguishable downstream — both arrived with `function_name:
    /// None` — so a class body got "extract the body of this function", which is
    /// not an action anyone can take against one.
    /// What: `Some` only for Python today; `None` for every other language,
    /// which leaves their payloads byte-identical and their consumers unchanged.
    /// Test: `crate::core::region_kind::tests`, and
    /// `service::handlers::analysis::tests::refactor_suggestions_carry_the_python_region_kind`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region_kind: Option<RegionKind>,
    pub refactor_type: RefactorType,
    pub severity: Severity,
    pub rationale: String,
    pub suggested_action: String,
    pub complexity_before: Option<u32>,
    pub complexity_after: Option<u32>,
    pub smells: Vec<String>,
}

/// Concrete refactoring action. Names mirror Fowler's canonical catalog so
/// callers familiar with refactoring literature recognise them on sight.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RefactorType {
    ExtractMethod,
    ExtractVariable,
    InlineFunction,
    SplitConditional,
    ReplaceConditionalWithPolymorphism,
    IntroduceParameterObject,
    RemoveDuplication,
    ReduceNesting,
    SimplifyBoolean,
    Other(String),
}

/// Severity tier. Derived from `ComplexityGrade` and optionally bumped by
/// smell count (see `analyze`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Low,
    Medium,
    High,
    Critical,
}

impl Severity {
    /// Map a `ComplexityGrade` to a severity. Returns `None` for grade A,
    /// where no suggestion should be emitted.
    fn from_grade(grade: ComplexityGrade) -> Option<Self> {
        match grade {
            ComplexityGrade::A => None,
            ComplexityGrade::B => Some(Self::Low),
            ComplexityGrade::C => Some(Self::Medium),
            ComplexityGrade::D => Some(Self::High),
            ComplexityGrade::F => Some(Self::Critical),
        }
    }

    /// Bump one tier (Critical saturates).
    fn bumped(self) -> Self {
        match self {
            Self::Low => Self::Medium,
            Self::Medium => Self::High,
            Self::High => Self::Critical,
            Self::Critical => Self::Critical,
        }
    }

    /// Parse from a case-insensitive string (`"low"`, `"medium"`, …).
    /// Used by HTTP/MCP query parameters.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            "critical" => Some(Self::Critical),
            _ => None,
        }
    }
}

/// Short human-readable label for a `CodeSmell`. Used in `smells` field.
fn smell_label(s: &CodeSmell) -> String {
    match s {
        CodeSmell::LongFunction { lines } => format!("long_function({lines} lines)"),
        CodeSmell::DeepNesting { max_depth } => format!("deep_nesting(depth {max_depth})"),
        CodeSmell::TooManyParams { count } => format!("too_many_params({count})"),
        CodeSmell::MissingDocstring => "missing_docstring".to_string(),
    }
}

/// Decide the dominant refactor type for a chunk. Order matters: more-specific
/// smells win over generic complexity-driven `ExtractMethod`.
fn pick_refactor_type(metrics: &ComplexityMetrics, smells: &[CodeSmell]) -> RefactorType {
    // Specific smells first, then fall back to high-cyclomatic ExtractMethod.
    for smell in smells {
        match smell {
            CodeSmell::DeepNesting { .. } => return RefactorType::ReduceNesting,
            CodeSmell::TooManyParams { .. } => return RefactorType::IntroduceParameterObject,
            CodeSmell::LongFunction { .. } => return RefactorType::ExtractMethod,
            CodeSmell::MissingDocstring => {} // not a structural refactor; skip
        }
    }
    if metrics.cyclomatic >= 10 {
        RefactorType::ExtractMethod
    } else {
        RefactorType::Other("review_for_clarity".to_string())
    }
}

/// Human-readable action sentence for a `RefactorType`.
fn action_for(
    rt: &RefactorType,
    function_name: Option<&str>,
    line_start: u32,
    line_end: u32,
) -> String {
    let fn_label = function_name.unwrap_or("this function");
    match rt {
        RefactorType::ExtractMethod => format!(
            "Extract the body of '{fn_label}' (lines {line_start}–{line_end}) into 2–3 smaller functions"
        ),
        RefactorType::ReduceNesting => format!(
            "Reduce nesting depth in '{fn_label}' using early returns or guard clauses"
        ),
        RefactorType::IntroduceParameterObject => {
            format!("Group parameters of '{fn_label}' into a dedicated struct")
        }
        RefactorType::SimplifyBoolean => {
            format!("Simplify boolean expressions in '{fn_label}' by extracting predicates")
        }
        RefactorType::RemoveDuplication => {
            format!("Extract duplicated code in '{fn_label}' into a shared helper")
        }
        RefactorType::SplitConditional => {
            format!("Split conditional logic in '{fn_label}' into separate branches/functions")
        }
        RefactorType::ReplaceConditionalWithPolymorphism => format!(
            "Replace conditional dispatch in '{fn_label}' with polymorphism / trait objects"
        ),
        RefactorType::ExtractVariable => {
            format!("Extract complex expressions in '{fn_label}' into named variables")
        }
        RefactorType::InlineFunction => {
            format!("Inline '{fn_label}' if it adds indirection without clarity")
        }
        RefactorType::Other(label) => format!(
            "Review '{fn_label}' for clarity ({label})"
        ),
    }
}

/// Rationale string explaining *why* the suggestion fired.
fn rationale_for(metrics: &ComplexityMetrics, smells: &[CodeSmell]) -> String {
    let mut parts: Vec<String> = Vec::new();
    parts.push(format!(
        "cyclomatic complexity {} (grade {})",
        metrics.cyclomatic, metrics.grade
    ));
    if !smells.is_empty() {
        let labels: Vec<String> = smells.iter().map(smell_label).collect();
        parts.push(format!("smells: {}", labels.join(", ")));
    }
    parts.join("; ")
}

/// Run the rule engine on a single chunk.
///
/// Why: callers (HTTP handler, MCP tool, CLI) all want the same mapping from
/// "metrics + smells" to "actionable advice". Centralising avoids divergence.
/// What: returns one suggestion when the grade is B–F, with severity bumped if
/// 3+ smells are present. Returns an empty vector for grade A (clean code).
/// `region_kind` (#6177) is carried through verbatim — this engine does not
/// classify, it only records what the caller measured — so a `None` produces a
/// payload byte-identical to the pre-#6177 one.
/// Test: see `mod tests` — covers grade A, grade B, grade F + smell, and
/// severity bump.
#[allow(clippy::too_many_arguments)]
pub fn analyze(
    chunk_id: &str,
    file: &str,
    line_start: u32,
    line_end: u32,
    function_name: Option<&str>,
    region_kind: Option<RegionKind>,
    metrics: &ComplexityMetrics,
    smells: &[CodeSmell],
) -> Vec<RefactorSuggestion> {
    let Some(mut severity) = Severity::from_grade(metrics.grade) else {
        return Vec::new();
    };
    if smells.len() >= 3 {
        severity = severity.bumped();
    }

    let refactor_type = pick_refactor_type(metrics, smells);
    let suggested_action = action_for(&refactor_type, function_name, line_start, line_end);
    let rationale = rationale_for(metrics, smells);

    let complexity_before = Some(metrics.cyclomatic);
    let complexity_after = match refactor_type {
        RefactorType::ExtractMethod => Some(metrics.cyclomatic / 2),
        _ => None,
    };

    vec![RefactorSuggestion {
        chunk_id: chunk_id.to_string(),
        file: file.to_string(),
        line_start,
        line_end,
        function_name: function_name.map(str::to_string),
        region_kind,
        refactor_type,
        severity,
        rationale,
        suggested_action,
        complexity_before,
        complexity_after,
        smells: smells.iter().map(smell_label).collect(),
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics(cyclomatic: u32) -> ComplexityMetrics {
        ComplexityMetrics {
            cyclomatic,
            cognitive: cyclomatic,
            grade: ComplexityGrade::from_cyclomatic(cyclomatic),
            smells: vec![],
        }
    }

    #[test]
    fn grade_a_emits_no_suggestion() {
        let m = metrics(2); // grade A
        let s = analyze("c1", "f.rs", 1, 10, Some("clean"), None, &m, &[]);
        assert!(s.is_empty(), "grade A should not produce suggestions");
    }

    #[test]
    fn grade_b_no_smells_emits_low_suggestion() {
        let m = metrics(7); // grade B
        let s = analyze("c2", "f.rs", 1, 20, Some("ok_fn"), None, &m, &[]);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].severity, Severity::Low);
        assert_eq!(s[0].complexity_before, Some(7));
    }

    #[test]
    fn grade_f_with_long_function_produces_critical_extract_method() {
        let mut m = metrics(30); // grade F
        m.smells = vec![CodeSmell::LongFunction { lines: 120 }];
        let smells = m.smells.clone();
        let s = analyze("c3", "f.rs", 1, 200, Some("god_fn"), None, &m, &smells);
        assert_eq!(s.len(), 1);
        let sug = &s[0];
        assert_eq!(sug.severity, Severity::Critical);
        assert_eq!(sug.refactor_type, RefactorType::ExtractMethod);
        assert_eq!(sug.complexity_before, Some(30));
        assert_eq!(sug.complexity_after, Some(15));
        assert!(sug.suggested_action.contains("Extract"));
        assert!(sug.suggested_action.contains("god_fn"));
    }

    #[test]
    fn severity_bumps_when_three_or_more_smells() {
        // grade C base = Medium; with 3 smells → High.
        let mut m = metrics(13); // grade C
        m.smells = vec![
            CodeSmell::DeepNesting { max_depth: 6 },
            CodeSmell::TooManyParams { count: 8 },
            CodeSmell::MissingDocstring,
        ];
        let smells = m.smells.clone();
        let s = analyze("c4", "f.rs", 1, 50, Some("messy_fn"), None, &m, &smells);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].severity, Severity::High);
        // First smell wins: DeepNesting → ReduceNesting.
        assert_eq!(s[0].refactor_type, RefactorType::ReduceNesting);
    }

    #[test]
    fn too_many_params_picks_introduce_parameter_object() {
        let mut m = metrics(11); // grade C
        m.smells = vec![CodeSmell::TooManyParams { count: 9 }];
        let smells = m.smells.clone();
        let s = analyze("c5", "f.rs", 1, 30, Some("wide_fn"), None, &m, &smells);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].refactor_type, RefactorType::IntroduceParameterObject);
        // complexity_after is None for non-ExtractMethod refactors.
        assert_eq!(s[0].complexity_after, None);
    }

    #[test]
    fn severity_parses_case_insensitive() {
        assert_eq!(Severity::parse("low"), Some(Severity::Low));
        assert_eq!(Severity::parse("HIGH"), Some(Severity::High));
        assert_eq!(Severity::parse("Critical"), Some(Severity::Critical));
        assert_eq!(Severity::parse("bogus"), None);
    }

    #[test]
    fn severity_critical_does_not_overflow_when_bumped() {
        let mut m = metrics(50); // grade F → Critical
        m.smells = vec![
            CodeSmell::LongFunction { lines: 200 },
            CodeSmell::DeepNesting { max_depth: 7 },
            CodeSmell::MissingDocstring,
        ];
        let smells = m.smells.clone();
        let s = analyze("c6", "f.rs", 1, 200, Some("worst"), None, &m, &smells);
        assert_eq!(s[0].severity, Severity::Critical);
    }
}
