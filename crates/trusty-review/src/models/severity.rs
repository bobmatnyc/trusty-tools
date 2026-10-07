//! How serious a finding is, as the review reports it (#9310).
//!
//! Why: the parser folded the reviewer's `severity` into `effort` and dropped
//! it, so a "critical" finding read the same as a "high" one, and nothing the
//! review returned ranked its findings for a reader.
//! What: [`Severity`], a closed, ordered set serialized as a lowercase string.
//! Test: `severity_serializes_lowercase_and_orders_low_to_critical`,
//! `severity_reads_only_the_four_reviewer_strings`.

use serde::{Deserialize, Serialize};

/// How serious a finding is (#9310).
///
/// Why: `effort` drives the verdict floor; severity is what a reader ranks
/// findings by. Keeping the two apart lets severity carry "critical" without
/// moving any verdict or grade.
/// What: `Low < Medium < High < Critical`, serialized `"low"` to
/// `"critical"`. A finalized finding's severity never exceeds what its final
/// `effort` allows (`pipeline::severity`).
/// Test: `severity_serializes_lowercase_and_orders_low_to_critical`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum Severity {
    /// A minor issue.
    Low,
    /// A real issue that does not block on its own.
    Medium,
    /// A serious defect.
    High,
    /// A defect the reviewer marked critical.
    Critical,
}

impl Severity {
    /// Every severity, lowest first.
    const ALL: [Severity; 4] = [Self::Low, Self::Medium, Self::High, Self::Critical];

    /// The serialized string, e.g. `"critical"`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }

    /// Read a reviewer's `severity` string (#9310).
    ///
    /// What: trims it and matches the four names ignoring ASCII case; an
    /// empty or unknown string is `None`, so the pipeline derives a severity.
    /// Test: `severity_reads_only_the_four_reviewer_strings`.
    pub fn from_reviewer(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        Self::ALL
            .into_iter()
            .find(|s| raw.eq_ignore_ascii_case(s.as_str()))
    }
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::Severity;

    /// Each variant serializes to its `as_str` name and back, in ascending order.
    #[test]
    fn severity_serializes_lowercase_and_orders_low_to_critical() {
        for s in Severity::ALL {
            let json = serde_json::to_value(s).expect("serialize");
            assert_eq!(json, serde_json::Value::String(s.to_string()));
            let back: Severity = serde_json::from_value(json).expect("deserialize");
            assert_eq!(back, s);
        }
        assert!(Severity::Low < Severity::Medium);
        assert!(Severity::Medium < Severity::High);
        assert!(Severity::High < Severity::Critical);
    }

    /// The four names parse in any case and padding; anything else is `None`.
    #[test]
    fn severity_reads_only_the_four_reviewer_strings() {
        for (raw, want) in [
            ("critical", Some(Severity::Critical)),
            (" HIGH ", Some(Severity::High)),
            ("Medium", Some(Severity::Medium)),
            ("low", Some(Severity::Low)),
            ("", None),
            ("   ", None),
            ("urgent", None),
            ("blocker", None),
        ] {
            assert_eq!(Severity::from_reviewer(raw), want, "raw: {raw:?}");
        }
    }
}
