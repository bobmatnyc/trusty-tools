//! Palace-name validation against project-slug enforcement rules.
//!
//! Why: Isolating validation from detection and pin-file I/O keeps each file
//! under the 500-SLOC cap and allows the validation logic to be tested without
//! touching the filesystem walker.
//! What: `validate_palace_name` — the single gate that enforces "palace name
//! must match the derived slug (or be `personal`)".
//! Test: `validate_palace_name_accepts_personal`,
//! `validate_palace_name_accepts_matching_slug`,
//! `validate_palace_name_rejects_mismatch`,
//! `validate_palace_name_rejects_non_personal_without_project`.

use anyhow::Result;
use std::path::Path;

use super::detection::PERSONAL_PALACE;
use super::pin_file::project_slug_at;

/// Validate a proposed palace name against project-slug enforcement rules.
///
/// Why: palace creation in MCP tool calls and HTTP handlers must apply the
/// same enforcement logic. Centralising the check here keeps the rule in one
/// place and makes it easy to write exhaustive unit tests.
/// What: returns `Ok(())` when the name is valid; returns `Err` with a
/// human-readable message when it is not. The rules are:
///   1. `personal` is always valid (the escape hatch for non-project
///      contexts).
///   2. When a project root is detectable from `cwd`, the name must equal
///      the derived slug.
///   3. When no project root is detectable, only `personal` is allowed.
///
/// Existing palaces are **not** affected by this check; it applies only to
/// *new* palace creation requests.
/// Test: `validate_palace_name_accepts_personal`,
/// `validate_palace_name_accepts_matching_slug`,
/// `validate_palace_name_rejects_mismatch`,
/// `validate_palace_name_rejects_non_personal_without_project`.
pub fn validate_palace_name(name: &str, cwd: &Path) -> Result<()> {
    // The `personal` palace is always a valid creation target.
    if name == PERSONAL_PALACE {
        return Ok(());
    }

    match project_slug_at(cwd) {
        Some(expected) => {
            if name == expected {
                Ok(())
            } else {
                Err(anyhow::anyhow!(
                    "palace name '{name}' does not match the project slug '{expected}' \
                     (derived from {cwd}). \
                     Either use '{expected}' or use 'personal' for non-project memories.",
                    cwd = cwd.display(),
                ))
            }
        }
        None => Err(anyhow::anyhow!(
            "no project root found at or above '{cwd}'. \
             Use 'personal' for memories not tied to a project, \
             or run from inside a project directory that contains \
             a .git file, Cargo.toml, pyproject.toml, or package.json.",
            cwd = cwd.display(),
        )),
    }
}
