//! Doctor check: palaces the daemon has yet to migrate to this format (#9274).
//!
//! Why: ADR-0067 D3 rule 2 — a palace one format behind (format 1 since
//! #9275) is refused to every read-only open (CLI, stdio MCP) until the
//! daemon's writer-intent open migrates it; format 0 is refused outright.
//! The operator needs that state by name, apart from a palace the binary
//! refuses outright (newer, split, or a failed migration).
//! What: one pass over the palace root, reading each palace's format state
//! through `palace_format_status`, which writes nothing. Needs migration, or a
//! format that could not be verified → `Warn`; a refusal → `Fail`; otherwise
//! `Pass`.
//! Test: palace_format_tests.rs —
//! `palace_format_check_names_a_palace_that_needs_migration`,
//! `palace_format_check_warns_on_a_palace_it_cannot_verify`.

use std::path::Path;

use crate::engine::store::palace_format::{
    PALACE_FORMAT_SUPPORTED, PalaceFormatStatus, palace_format_status,
};

use super::CheckResult;

const LABEL: &str = "palace format";

/// `trusty-memory doctor`'s palace-format check, on this machine's data dir.
pub fn check_palace_formats() -> CheckResult {
    let data_dir = match trusty_common::resolve_data_dir("trusty-memory") {
        Ok(d) => d,
        Err(e) => return CheckResult::fail(LABEL, format!("could not resolve data dir: {e}")),
    };
    palace_format_verdict(&crate::resolve_palace_registry_dir(data_dir))
}

/// The verdict for every palace one level under `root`.
///
/// Test: `palace_format_check_names_a_palace_that_needs_migration`,
/// `palace_format_check_warns_on_a_palace_it_cannot_verify`.
pub(super) fn palace_format_verdict(root: &Path) -> CheckResult {
    let Ok(entries) = std::fs::read_dir(root) else {
        return CheckResult::pass(LABEL, format!("no palace root at {}", root.display()));
    };
    let mut names: Vec<_> = entries
        .flatten()
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .map(|e| e.path())
        .filter(|p| p.join("palace.json").is_file())
        .collect();
    names.sort();
    let (mut needs, mut unverified, mut refused) = (Vec::new(), Vec::new(), Vec::new());
    for dir in names {
        let name = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        match palace_format_status(&dir) {
            Ok(PalaceFormatStatus::NeedsMigration { found, interrupted }) => {
                needs.push(if interrupted {
                    format!("{name} (interrupted migration from format {found})")
                } else {
                    format!("{name} (format {found})")
                })
            }
            // #9274: a held or crash-repair kg.redb is not known to be current.
            Ok(PalaceFormatStatus::Unverified) => unverified.push(name),
            Ok(_) => {}
            Err(e) => refused.push(format!("{name}: {e}")),
        }
    }
    let mut warnings = Vec::new();
    if !needs.is_empty() {
        warnings.push(format!(
            "needs migration by the daemon, at its next open of the palace: {}",
            needs.join(", ")
        ));
    }
    if !unverified.is_empty() {
        warnings.push(format!(
            "format could not be verified (kg.redb is held by a running daemon or needs crash \
             repair; stop the daemon and re-run doctor to read it): {}",
            unverified.join(", ")
        ));
    }
    if !refused.is_empty() {
        let mut detail = format!("refused: {}", refused.join("; "));
        for w in &warnings {
            detail.push_str("; ");
            detail.push_str(w);
        }
        CheckResult::fail(LABEL, detail)
    } else if !warnings.is_empty() {
        CheckResult::warn(LABEL, warnings.join("; "))
    } else {
        CheckResult::pass(
            LABEL,
            format!("every palace reads at format {PALACE_FORMAT_SUPPORTED}"),
        )
    }
}

#[cfg(test)]
#[path = "palace_format_tests.rs"]
mod tests;
