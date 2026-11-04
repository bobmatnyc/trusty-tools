//! Concept IDs and foreign paths (SPEC-OKF-01 §2.11, §2.2.1).
//!
//! Why: a concept ID is its bundle-relative path, so the ID rule is the first
//! line of path confinement. A `.md` file whose path breaks the rule can still
//! be a conformant OKF concept; trusty-okf reads it as foreign and never
//! writes it.
//! What: [`ConceptId::parse`] applies the eight lexical rules of §2.11;
//! [`concept_ref_for_path`] maps a bundle-relative `.md` path to a
//! [`ConceptRef`] plus the info finding a foreign file earns.
//! Test: `concept_id_rules_refuse_each_case`,
//! `foreign_paths_classify_with_one_info_finding`.

use std::fmt;
use std::path::PathBuf;

use serde::Serialize;

use crate::OkfError;
use crate::format::validate::{Finding, FindingKind, Severity};

/// A validated concept ID: a bundle-relative path without the `.md` suffix.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ConceptId(String);

impl ConceptId {
    /// Parse `s` under the §2.11 lexical rules.
    ///
    /// Why: every write target and every link trusty-okf builds comes from a
    /// parsed ID, so an ID that could leave the bundle never exists.
    /// What: refuses with [`OkfError::InvalidId`] naming the first rule `s`
    /// breaks: 1 empty; 2 absolute or holds `\`; 3 a segment is empty, `.` or
    /// `..`; 4 a segment holds `:`; 5 NUL or a control character; 6 the last
    /// segment is `index` or `log` in any case; 7 the first segment starts
    /// with `_` or `.`; 8 it ends in `.md`.
    /// Test: `concept_id_rules_refuse_each_case`.
    pub fn parse(s: &str) -> Result<ConceptId, OkfError> {
        let refuse = |rule: u8| OkfError::InvalidId {
            input: s.to_string(),
            rule,
        };
        if s.is_empty() {
            return Err(refuse(1));
        }
        if s.starts_with('/') || s.contains('\\') {
            return Err(refuse(2));
        }
        let segments: Vec<&str> = s.split('/').collect();
        if segments
            .iter()
            .any(|seg| seg.is_empty() || *seg == "." || *seg == "..")
        {
            return Err(refuse(3));
        }
        if segments.iter().any(|seg| seg.contains(':')) {
            return Err(refuse(4));
        }
        if s.chars().any(char::is_control) {
            return Err(refuse(5));
        }
        let last = segments[segments.len() - 1];
        if last.eq_ignore_ascii_case("index") || last.eq_ignore_ascii_case("log") {
            return Err(refuse(6));
        }
        if segments[0].starts_with('_') || segments[0].starts_with('.') {
            return Err(refuse(7));
        }
        // `get` and not an index: a multi-byte char at the end would panic.
        let tail = s.len().checked_sub(3).and_then(|i| s.get(i..));
        if tail.is_some_and(|t| t.eq_ignore_ascii_case(".md")) {
            return Err(refuse(8));
        }
        Ok(ConceptId(s.to_string()))
    }

    /// The ID as a string, without the `.md` suffix.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The bundle-relative file path of the concept: the ID plus `.md`.
    pub fn file_name(&self) -> String {
        format!("{}.md", self.0)
    }
}

impl fmt::Display for ConceptId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a bundle-relative `.md` path names (SPEC-OKF-01 §2.2.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConceptRef {
    /// A concept whose path passes §2.11.
    Id(ConceptId),
    /// A `.md` file whose path fails §2.11. Read-only: no operation writes,
    /// moves or deletes it.
    Foreign(PathBuf),
}

impl ConceptRef {
    /// The bundle-relative path, `.md` included, as a display string.
    pub fn display_path(&self) -> String {
        match self {
            ConceptRef::Id(id) => id.file_name(),
            ConceptRef::Foreign(p) => p.to_string_lossy().into_owned(),
        }
    }
}

/// Classify a bundle-relative `.md` path.
///
/// Why: `Bundle::open` (S3) reads every `.md` file; a file that fails the ID
/// rule is still read, as foreign, with an info finding (§2.2.1).
/// What: strips one `.md` suffix and parses the rest as a [`ConceptId`]. On
/// success returns `(ConceptRef::Id, None)`; on failure returns
/// `(ConceptRef::Foreign(rel), Some(info finding))`. The caller excludes the
/// reserved `index.md` and `log.md` files before calling.
/// Test: `foreign_paths_classify_with_one_info_finding`.
pub fn concept_ref_for_path(rel: &str) -> (ConceptRef, Option<Finding>) {
    let stem = rel.strip_suffix(".md").unwrap_or(rel);
    match ConceptId::parse(stem) {
        Ok(id) => (ConceptRef::Id(id), None),
        Err(err) => {
            let finding = Finding {
                severity: Severity::Info,
                kind: FindingKind::ForeignPath,
                path: rel.to_string(),
                concept: None,
                message: format!("read-only foreign file: {err}"),
                spec_ref: "SPEC-OKF-01 §2.2.1",
            };
            (ConceptRef::Foreign(PathBuf::from(rel)), Some(finding))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule_of(s: &str) -> u8 {
        match ConceptId::parse(s) {
            Err(OkfError::InvalidId { rule, .. }) => rule,
            other => panic!("expected InvalidId for {s:?}, got {other:?}"),
        }
    }

    /// Why: each §2.11 rule closes one escape or collision route.
    /// What: one refused input per rule, plus accepted nested IDs.
    /// Test: self-contained.
    #[test]
    fn concept_id_rules_refuse_each_case() {
        assert_eq!(rule_of(""), 1);
        assert_eq!(rule_of("/abs"), 2);
        assert_eq!(rule_of("a\\b"), 2);
        assert_eq!(rule_of("a//b"), 3);
        assert_eq!(rule_of("a/../b"), 3);
        assert_eq!(rule_of("./a"), 3);
        assert_eq!(rule_of("C:foo"), 4);
        assert_eq!(rule_of("notes/a:b"), 4);
        assert_eq!(rule_of("a\u{0}b"), 5);
        assert_eq!(rule_of("a\nb"), 5);
        assert_eq!(rule_of("Index"), 6);
        assert_eq!(rule_of("notes/LOG"), 6);
        assert_eq!(rule_of("_drafts/x"), 7);
        assert_eq!(rule_of(".git/x"), 7);
        assert_eq!(rule_of("notes/a.md"), 8);
        assert_eq!(ConceptId::parse("tables/orders").unwrap().as_str(), "tables/orders");
        assert_eq!(ConceptId::parse("a/_b/.c").unwrap().file_name(), "a/_b/.c.md");
        // A multi-byte tail must not panic the rule 8 check.
        assert!(ConceptId::parse("notes/éé").is_ok());
    }

    /// Why: criterion 115 (S2 part) — the three §2.2.1 example paths read as
    /// foreign with exactly one info finding each.
    /// What: classifies each path and a normal one.
    /// Test: self-contained.
    #[test]
    fn foreign_paths_classify_with_one_info_finding() {
        for rel in ["notes/a:b.md", "Index.md", "_drafts/x.md"] {
            let (r, finding) = concept_ref_for_path(rel);
            assert_eq!(r, ConceptRef::Foreign(PathBuf::from(rel)));
            let finding = finding.expect("a foreign file earns an info finding");
            assert_eq!(finding.severity, Severity::Info);
            assert_eq!(finding.kind, FindingKind::ForeignPath);
        }
        let (r, finding) = concept_ref_for_path("notes/a.md");
        assert!(matches!(r, ConceptRef::Id(_)));
        assert!(finding.is_none());
    }
}
