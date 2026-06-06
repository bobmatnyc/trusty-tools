//! Unit tests for workflow config parsing, validation, and wave repair.
//!
//! Why: Split into focused files (#359) to keep each under the 500-line cap —
//! `workflow` covers the `WorkflowDef`/`PhaseDef` serde shapes, semantic config
//! validation, and the `safe_join` guard; `assignments` covers the
//! `Assignments` path-safety + structural checks and the topological-repair
//! logic.
//! What: A `#[cfg(test)] mod tests` parent whose submodules `use super::super::*`
//! to reach the parent `config` module (which re-exports the assignment types).
//! Test: The submodules below ARE the test bodies.

mod assignments;
mod wave_repair;
mod workflow;

/// Build a `FileAssignment` from raw parts for the assignment tests (#114).
///
/// Why: Shared across the `assignments` test file's structural-validation and
/// wave-repair cases.
/// What: Constructs a `FileAssignment` with `None` stub/max_lines and a
/// derived purpose; `depends_on` is the supplied dependency list.
/// Test: Used by the `assignments` submodule's tests.
pub(super) fn fa(path: &str, depends_on: Vec<&str>) -> super::FileAssignment {
    super::FileAssignment {
        path: path.to_string(),
        stub: None,
        purpose: format!("test {path}"),
        depends_on: depends_on.into_iter().map(String::from).collect(),
        max_lines: None,
    }
}
