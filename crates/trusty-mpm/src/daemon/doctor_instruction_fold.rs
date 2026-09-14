//! `tm doctor` instruction-fold probe (issue #7616, renamed #7867).
//!
//! Why: when the fold produces no reduction the producer writes no ledger row,
//! and nothing an operator reads said so — #7245 gave the decline a one-time
//! `warn!` in the daemon log, which is not where anyone asks "is the
//! instruction fold doing anything on this project?" Silence and "working" were
//! indistinguishable. This check is that answer.
//!
//! The name says "instruction fold", not "compression": the owner's 2026-09-14
//! ruling (#7867) reserves "compression" for tool-output compression — what
//! rtk- and shunt-style interception saves on a tool call, which is what the
//! `💸` segment and the `tool_output_compression` row beside this one report.
//! The two measurements share no clock and no input, and one word for both is
//! what made a launch-time prompt comparison read as a per-call saving.
//!
//! What: [`check_instruction_fold`] reports the measured fold for the project's
//! most recently compiled prompt — `Ok` with both byte counts and the
//! percentage when the prompt came out smaller than the sources that fed it,
//! `Warn` naming INACTIVE and both counts when it did not. Never `Fail`: a
//! project that folds nothing away is overriding no bundled section, not broken.
//! Read-only.
//! Test: the `tests` module below.

use std::path::Path;

use crate::core::doctor::{CheckStatus, DoctorCheck};

/// The check's name, asserted on by the tests and by anything parsing
/// `tm doctor --json`.
const NAME: &str = "instruction_fold";

/// Probe how much the instruction fold saved this project.
///
/// Why: see the module doc — this is the surface that states the fold is
/// inactive, rather than leaving an operator to infer it from a daemon-log line
/// they never read.
/// What: `Warn` with no project directory (the probe cannot be scoped, and a
/// false `Ok` is worse than an admission); `Warn` when no session has compiled a
/// prompt yet; `Ok` naming both byte counts and the percentage when the compiled
/// prompt is smaller than its sources; `Warn` naming INACTIVE and both counts
/// when it is not.
/// Test: `instruction_fold_warns_without_a_project_dir`,
/// `instruction_fold_warns_before_any_prompt_is_compiled`,
/// `instruction_fold_reports_inactive_when_the_prompt_did_not_shrink`,
/// `instruction_fold_reports_the_measured_percentage_when_it_did`.
pub(super) fn check_instruction_fold(project_dir: Option<&Path>) -> DoctorCheck {
    let Some(project) = project_dir else {
        return DoctorCheck::new(
            NAME,
            CheckStatus::Warn,
            "no project directory supplied — cannot measure the instruction fold",
        );
    };

    let Some((source_bytes, compiled_bytes)) =
        crate::core::savings_instructions::measure_project_fold(project)
    else {
        return DoctorCheck::new(
            NAME,
            CheckStatus::Warn,
            "no session has compiled a PM prompt for this project yet, so the \
             instruction fold cannot be measured — start a session and re-run",
        );
    };

    DoctorCheck::new(NAME, status_for(source_bytes, compiled_bytes), {
        describe(source_bytes, compiled_bytes)
    })
}

/// `Ok` iff the compiled prompt is strictly smaller than its sources.
///
/// Why: the same comparison the producer makes before writing a row, so the
/// check and the ledger can never disagree about whether the fold happened.
/// What: `Ok` below, `Warn` at or above.
/// Test: `instruction_fold_reports_inactive_when_the_prompt_did_not_shrink`.
fn status_for(source_bytes: usize, compiled_bytes: usize) -> CheckStatus {
    if compiled_bytes < source_bytes {
        CheckStatus::Ok
    } else {
        CheckStatus::Warn
    }
}

/// The finding text, carrying both measured byte counts either way.
///
/// Why: a percentage with no counts behind it cannot be checked against the
/// ledger's `basis` field, and the counts are what tell an operator whether the
/// corpus or the roster moved.
/// What: an active line with the reduction and its percentage, or an INACTIVE
/// line naming the reason no savings row is written for this project.
/// Test: `instruction_fold_reports_the_measured_percentage_when_it_did`,
/// `instruction_fold_reports_inactive_when_the_prompt_did_not_shrink`.
fn describe(source_bytes: usize, compiled_bytes: usize) -> String {
    if compiled_bytes < source_bytes {
        let saved = source_bytes - compiled_bytes;
        let percent = (saved as f64 / source_bytes as f64) * 100.0;
        return format!(
            "instruction fold ACTIVE: sources {source_bytes} B - compiled \
             {compiled_bytes} B = {saved} B ({percent:.1}% smaller)"
        );
    }
    // #7867: the fold's own measurement and nothing else — the 💸 segment
    // reports tool-output compression, which this state does not touch.
    format!(
        "instruction fold INACTIVE for this project: sources {source_bytes} B, \
         compiled {compiled_bytes} B — the compiled prompt is not smaller than the \
         instruction bodies it was built from, so no instruction-fold savings row \
         is written for it. See issue #7616."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::harness_root::{HARNESS_DIR, SESSIONS_DIR};
    use crate::core::instruction_pipeline::COMPILED_PROMPT_FILE;

    /// Write a compiled prompt of `bytes` length for `project`.
    fn compile_prompt(project: &Path, bytes: usize) {
        let dir = project.join(HARNESS_DIR).join(SESSIONS_DIR).join("local");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(COMPILED_PROMPT_FILE), "x".repeat(bytes)).unwrap();
    }

    /// The bundled source floor every measurement sits above, so a test can
    /// place a prompt deliberately on either side of it without hard-coding a
    /// byte count that moves whenever a section is edited.
    fn bundled_bytes() -> usize {
        crate::core::instruction_pipeline::SECTION_SOURCES
            .iter()
            .map(|(_, body)| body.len())
            .sum()
    }

    #[test]
    fn instruction_fold_warns_without_a_project_dir() {
        let check = check_instruction_fold(None);
        assert_eq!(check.status, CheckStatus::Warn);
    }

    #[test]
    fn instruction_fold_warns_before_any_prompt_is_compiled() {
        let tmp = tempfile::tempdir().unwrap();
        let check = check_instruction_fold(Some(tmp.path()));
        assert_eq!(check.status, CheckStatus::Warn);
        assert!(
            check.message.contains("no session has compiled"),
            "the finding must say why it could not measure: {}",
            check.message
        );
    }

    /// #7616: the whole point of the check. A project whose fold yields nothing
    /// must SAY SO rather than leaving a daemon-log line as the only evidence.
    /// #7867 adds the second half: the finding names the instruction fold and
    /// claims nothing about the `💸` segment, which measures something else.
    #[test]
    fn instruction_fold_reports_inactive_when_the_prompt_did_not_shrink() {
        let tmp = tempfile::tempdir().unwrap();
        compile_prompt(tmp.path(), bundled_bytes() * 2);

        let check = check_instruction_fold(Some(tmp.path()));

        assert_eq!(check.status, CheckStatus::Warn);
        assert!(
            check.message.contains("INACTIVE"),
            "the finding must name the state: {}",
            check.message
        );
        assert!(
            check.message.contains("instruction fold"),
            "the finding must name the instruction fold, not compression: {}",
            check.message
        );
        for banned in ["\u{1f4b8}", "statusline", "CLAUDE.md", "override"] {
            assert!(
                !check.message.contains(banned),
                "the finding must claim nothing about {banned:?}: {}",
                check.message
            );
        }
        assert!(
            check.message.contains("#7616"),
            "the finding must cite the issue: {}",
            check.message
        );
        assert!(
            check
                .message
                .contains(&format!("{} B", bundled_bytes() * 2)),
            "the finding must carry the measured compiled bytes: {}",
            check.message
        );
    }

    #[test]
    fn instruction_fold_reports_the_measured_percentage_when_it_did() {
        let tmp = tempfile::tempdir().unwrap();
        let compiled = bundled_bytes() / 2;
        compile_prompt(tmp.path(), compiled);

        let check = check_instruction_fold(Some(tmp.path()));

        assert_eq!(check.status, CheckStatus::Ok);
        assert!(
            check.message.contains("ACTIVE") && check.message.contains('%'),
            "the finding must carry the percentage: {}",
            check.message
        );
        assert!(
            check.message.contains(&format!("compiled {compiled} B")),
            "the finding must carry the measured compiled bytes: {}",
            check.message
        );
    }

    #[test]
    fn the_status_mirrors_the_producers_own_comparison() {
        // Equal bytes is the boundary the producer declines on, so the check
        // must decline there too — not one byte either side of it.
        assert_eq!(status_for(100, 100), CheckStatus::Warn);
        assert_eq!(status_for(100, 99), CheckStatus::Ok);
        assert_eq!(status_for(100, 101), CheckStatus::Warn);
    }
}
