//! The session goal `tm issue next --goal` ranks first.
//!
//! Why: each factory session maps to one goal in the Architect's `goals.md`;
//! intake ranks the issues that goal names ahead of the rest.
//! What: [`Goal`] and [`load_goal`], which finds the `## ... (<id>)` section
//! of the goals file and collects the `#N` issue references in it. An issue
//! also serves a goal when it carries a `goal:<id>` label.
//! Test: `a_goal_section_collects_its_issue_references`,
//! `an_unknown_goal_is_an_error`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::NextError;

/// Environment override of the goals file path.
pub(crate) const GOALS_FILE_ENV: &str = "TM_GOALS_FILE";

/// One goal and the issues it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Goal {
    /// Goal id as given (`G01`).
    pub(crate) id: String,
    /// Issue numbers the goal's section references.
    pub(crate) issues: BTreeSet<u64>,
}

impl Goal {
    /// Whether an issue with `number` and `labels` serves this goal.
    pub(crate) fn serves(&self, number: u64, labels: &[String]) -> bool {
        let label = format!("goal:{}", self.id);
        self.issues.contains(&number) || labels.iter().any(|l| l.eq_ignore_ascii_case(&label))
    }
}

/// The goals file: `$TM_GOALS_FILE`, else the Architect's
/// `~/trusty-mpm-projects/architect/local/goals.md`.
pub(crate) fn goals_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(GOALS_FILE_ENV).filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    dirs::home_dir().map(|h| h.join("trusty-mpm-projects/architect/local/goals.md"))
}

/// Read goal `id` from the goals file at `path`.
///
/// What: an `ar-` prefix on `id` is dropped (`ar-G01` is `G01`). A missing
/// file or a goal with no section is an error, never an unranked run.
pub(crate) fn load_goal(path: &Path, id: &str) -> Result<Goal, NextError> {
    let text = std::fs::read_to_string(path).map_err(|e| NextError::Goals {
        detail: format!("cannot read {}: {e}", path.display()),
    })?;
    parse_goal(&text, id).ok_or_else(|| NextError::Goals {
        detail: format!("no `## ... ({id})` section in {}", path.display()),
    })
}

/// Find goal `id`'s section in `text` and collect its `#N` references.
pub(crate) fn parse_goal(text: &str, id: &str) -> Option<Goal> {
    let id = id.strip_prefix("ar-").unwrap_or(id).trim();
    let marker = format!("({id})");
    let mut issues = BTreeSet::new();
    let mut found = false;
    let mut inside = false;
    for line in text.lines() {
        if line.starts_with("## ") {
            inside = line.contains(&marker);
            found |= inside;
            continue;
        }
        if inside {
            issues.extend(issue_refs(line));
        }
    }
    found.then(|| Goal {
        id: id.to_string(),
        issues,
    })
}

/// Every `#N` in `line`.
fn issue_refs(line: &str) -> Vec<u64> {
    line.split('#')
        .skip(1)
        .filter_map(|rest| {
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().ok()
        })
        .collect()
}
