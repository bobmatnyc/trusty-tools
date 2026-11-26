//! Palace-name validation against the shared resolver's project name.
//!
//! Why: Isolating validation from detection and pin-file I/O keeps each file
//! under the 500-SLOC cap and allows the validation logic to be tested without
//! touching the filesystem walker.
//! What: `validate_palace_name` — the single gate that enforces "palace name
//! must be the name readers resolve for the project (or `personal`)".
//! Test: `validate_palace_name_accepts_personal`,
//! `validate_palace_name_accepts_matching_slug`,
//! `validate_palace_name_rejects_mismatch`,
//! `validate_palace_name_rejects_non_personal_without_project`.

use anyhow::Result;
use std::path::Path;

use super::detection::PERSONAL_PALACE;
use super::resolve_palace_for_project;

/// Validate a proposed palace name for the project at `cwd`.
///
/// Why: palace creation in MCP tool calls and HTTP handlers must apply the
/// same enforcement logic. #11823: the rule is the shared resolver's (pin, git
/// `owner/repo`, `parent/dir`), so a creator accepts exactly the name every
/// reader looks for; the old folder-name rule made them disagree.
/// What: returns `Ok(())` when the name is valid; returns `Err` with a
/// human-readable message when it is not. The rules are:
///
/// 1. `personal` is always valid (the escape hatch for non-project contexts).
/// 2. Otherwise the name must equal [`resolve_palace_for_project`] for `cwd`,
///    which ignores this process's `TRUSTY_MEMORY_PALACE`.
/// 3. No project directory, or an untrustworthy pin, is an error — never a
///    fallback name.
///
/// Test: `validate_palace_name_accepts_personal`,
/// `validate_palace_name_accepts_matching_slug`,
/// `validate_palace_name_rejects_mismatch`,
/// `validate_palace_name_rejects_non_personal_without_project`,
/// `validate_palace_name_rejects_a_malformed_pin`.
pub fn validate_palace_name(name: &str, cwd: &Path) -> Result<()> {
    // The `personal` palace is always a valid creation target.
    if name == PERSONAL_PALACE {
        return Ok(());
    }
    let project = resolve_palace_for_project(cwd).map_err(|e| {
        anyhow::anyhow!(
            "cannot validate palace name '{name}': {e}. \
             Use 'personal' for memories not tied to a project."
        )
    })?;
    if name == project.id {
        return Ok(());
    }
    Err(anyhow::anyhow!(
        "palace name '{name}' does not match the project palace '{expected}' \
         (resolved from {cwd}). \
         Either use '{expected}' or use 'personal' for non-project memories.",
        expected = project.id,
        cwd = cwd.display(),
    ))
}
