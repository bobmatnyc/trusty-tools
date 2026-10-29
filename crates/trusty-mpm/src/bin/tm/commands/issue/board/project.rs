//! The `project:` block of `issue-state.yaml` (#10275).
//!
//! Why: a repo names its own board in the file `tm issue` already reads, so
//! the attach target is reviewed with the lifecycle (epic #10274, D8). The
//! same schema serves #9400 (D1).
//! What: [`ProjectBlock`] — owner, project number, Status options the repo
//! drops, and the repo's extra fields — parsed as part of the state model with
//! unknown keys refused, and [`validate_project`], its check against the board
//! spec.
//! Test: `board/tests.rs`.

use serde::{Deserialize, Serialize};

use super::spec::BoardSpec;
use super::validate::{BoardSpecError, STATUS_FIELD};

/// The repo's board, as `issue-state.yaml` names it.
///
/// Why: Phase 2's `tm issue board add` and `check` need one attach target and
/// the repo's departures from the standard.
/// What: the board `owner` (user or organisation login), the project `number`,
/// the Status options this repo's board omits (`drop_options`), and the
/// non-standard fields it carries (`extra_fields`, reported by `check` as repo
/// extras, not drift). An unknown key fails the parse, naming the key.
/// Test: `the_project_block_parses_and_refuses_an_unknown_key_10275`,
/// `the_repo_block_names_project_45_with_crate_and_agents_10275`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProjectBlock {
    /// The login that owns the board.
    pub(crate) owner: String,
    /// The GitHub project number.
    pub(crate) number: u64,
    /// Status options this repo's board does not carry.
    #[serde(default)]
    pub(crate) drop_options: Vec<String>,
    /// Fields this repo's board carries beyond the standard set.
    #[serde(default)]
    pub(crate) extra_fields: Vec<String>,
}

/// Check a `project:` block against the board spec.
///
/// Why: a dropped option the spec does not know, or an "extra" that is a
/// standard field, is a typo Phase 2 would act on.
/// What: refuses an empty owner, project number 0, a `drop_options` name that
/// is not a Status option of `spec`, and an `extra_fields` name that is a
/// field of `spec`.
/// Test: `the_project_block_is_checked_against_the_spec_10275`.
pub(crate) fn validate_project(
    block: &ProjectBlock,
    spec: &BoardSpec,
) -> Result<(), BoardSpecError> {
    if block.owner.trim().is_empty() {
        return Err(BoardSpecError::EmptyOwner);
    }
    if block.number == 0 {
        return Err(BoardSpecError::ZeroProjectNumber);
    }
    validate_drop_options(&block.drop_options, spec)?;
    if let Some(extra) = block.extra_fields.iter().find(|e| spec.field(e).is_some()) {
        return Err(BoardSpecError::ExtraIsStandard(extra.clone()));
    }
    Ok(())
}

/// Refuse a `drop_options` name that is not a Status option of `spec`.
///
/// Why: #10276 — `init` must refuse a mistyped `--drop-option` before it
/// creates anything, by the same rule [`validate_project`] later applies to
/// the block it prints.
/// Test: `init_refuses_a_mistyped_drop_option_before_any_call_10276`,
/// `the_project_block_is_checked_against_the_spec_10275`.
pub(crate) fn validate_drop_options(
    drop_options: &[String],
    spec: &BoardSpec,
) -> Result<(), BoardSpecError> {
    let status = spec.field(STATUS_FIELD);
    for dropped in drop_options {
        let known = status.is_some_and(|f| f.options.iter().any(|o| &o.name == dropped));
        if !known {
            return Err(BoardSpecError::UnknownDropOption(dropped.clone()));
        }
    }
    Ok(())
}
