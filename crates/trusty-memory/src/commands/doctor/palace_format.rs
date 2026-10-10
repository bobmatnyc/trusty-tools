//! Doctor check: palaces the daemon has yet to migrate to this format (#9274).
//!
//! Why: ADR-0067 D3 rule 2 — a format-0 palace is refused to every read-only
//! open (CLI, stdio MCP) until the daemon's writer-intent open migrates it.
//! The operator needs that state by name, apart from a palace the binary
//! refuses outright (newer, split, or a failed migration).
//! What: one pass over the palace root, reading each palace's format state
//! through `palace_format_status`, which writes nothing. Needs migration →
//! `Warn`; a refusal → `Fail`; otherwise `Pass`.
//! Test: palace_format_tests.rs —
//! `palace_format_check_names_a_palace_that_needs_migration`.

use std::path::Path;

use trusty_common::memory_core::store::palace_format::{
    palace_format_status, PalaceFormatStatus, PALACE_FORMAT_SUPPORTED,
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
/// Test: `palace_format_check_names_a_palace_that_needs_migration`.
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
    let (mut needs, mut refused) = (Vec::new(), Vec::new());
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
            Ok(_) => {}
            Err(e) => refused.push(format!("{name}: {e}")),
        }
    }
    let needs_line = format!(
        "needs migration by the daemon, at its next open of the palace: {}",
        needs.join(", ")
    );
    if !refused.is_empty() {
        let mut detail = format!("refused: {}", refused.join("; "));
        if !needs.is_empty() {
            detail.push_str("; ");
            detail.push_str(&needs_line);
        }
        CheckResult::fail(LABEL, detail)
    } else if !needs.is_empty() {
        CheckResult::warn(LABEL, needs_line)
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
