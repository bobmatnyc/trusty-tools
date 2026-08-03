//! `tm doctor` retired-override-file probe (issue #4286).
//!
//! Why: #4286 stopped the prompt composer reading the five `.trusty-mpm/`
//! override files. A hard cut with no signal would delete a project's rules
//! between two releases with no error anywhere — the single worst outcome
//! available, and issue #381 in reverse (there, a file was advertised and never
//! read; here, a file was read and would silently stop being read). The
//! resolver logs an `error!` on every launch, and this probe is the half an
//! operator can run on demand and see in one place.
//!
//! What: [`check_legacy_overrides`] `Fail`s when any retired override file is
//! present in the project, naming every file found and the migration to
//! `CLAUDE.md` named sections. `Fail` rather than `Warn` deliberately: the
//! project's instructions are no longer reaching the PM, which is a broken
//! configuration and not an advisory. Read-only — never deletes or rewrites a
//! project's files.
//! Test: the `tests` module below covers the clean, single-file, all-files and
//! no-project-directory branches.

use std::path::Path;

use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::core::instruction_overrides::{LEGACY_MIGRATION_HINT, detect_legacy_overrides};

/// Probe the project for retired `.trusty-mpm/` instruction override files.
///
/// Why: see the module doc — this is the on-demand half of the #4286 retirement
/// signal. It shares [`detect_legacy_overrides`] with the prompt resolver so the
/// two can never disagree about whether a project is affected; a probe with its
/// own copy of the filename list would drift and under-report.
/// What: `Ok` when the project carries none of the five files. `Fail` naming
/// every file found plus [`LEGACY_MIGRATION_HINT`]. With no `project_dir` the
/// probe cannot be scoped, so it reports `Warn` rather than a false `Ok` —
/// mirroring `check_instructions`, which faces the same unscoped case.
/// Test: `legacy_overrides_ok_when_clean`,
/// `legacy_overrides_fail_names_the_migration`,
/// `legacy_overrides_reports_every_file_found`,
/// `legacy_overrides_warns_without_a_project_dir`.
pub(super) fn check_legacy_overrides(project_dir: Option<&Path>) -> DoctorCheck {
    let Some(project) = project_dir else {
        return DoctorCheck::new(
            "legacy_overrides",
            CheckStatus::Warn,
            "no project directory supplied — cannot check for retired .trusty-mpm/ \
             override files",
        );
    };

    let found = detect_legacy_overrides(project);
    if found.is_empty() {
        return DoctorCheck::new(
            "legacy_overrides",
            CheckStatus::Ok,
            "no retired .trusty-mpm/ instruction override files present",
        );
    }

    let names: Vec<String> = found.iter().map(|p| p.display().to_string()).collect();
    DoctorCheck::new(
        "legacy_overrides",
        CheckStatus::Fail,
        format!(
            "{} retired override file(s) are NO LONGER READ and their contents do not \
             reach the PM prompt: {} — {LEGACY_MIGRATION_HINT}",
            names.len(),
            names.join(", ")
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::instruction_overrides::{
        FILE_INSTRUCTIONS, LEGACY_OVERRIDE_FILES, OVERRIDE_DIR_NAME,
    };

    /// Write `<project>/.trusty-mpm/<name>`, creating parents.
    fn write_legacy(project: &Path, name: &str) {
        let dir = project.join(OVERRIDE_DIR_NAME);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(name), "PROJECT RULES\n").unwrap();
    }

    #[test]
    fn legacy_overrides_ok_when_clean() {
        // A project with no `.trusty-mpm/` directory at all is the common case
        // and must be Ok — the probe may never nag a clean project.
        let tmp = tempfile::tempdir().unwrap();
        let check = check_legacy_overrides(Some(tmp.path()));
        assert_eq!(check.status, CheckStatus::Ok);
    }

    #[test]
    fn legacy_overrides_ok_when_only_unrelated_state_is_present() {
        // `.trusty-mpm/` also holds `last-instructions.md` and session state,
        // which are NOT override files. Their presence must not trip the probe.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(OVERRIDE_DIR_NAME);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("last-instructions.md"), "composed prompt").unwrap();
        std::fs::write(dir.join("config.toml"), "[project]\n").unwrap();

        let check = check_legacy_overrides(Some(tmp.path()));
        assert_eq!(check.status, CheckStatus::Ok);
    }

    #[test]
    fn legacy_overrides_fail_names_the_migration() {
        // The blast-radius case: the ONE file that actually exists in the field
        // (12 instances across 4 real projects at the time of #4286). A leftover
        // file must Fail loudly AND state where the content has to go — a
        // failure that does not name the migration leaves the operator stuck.
        let tmp = tempfile::tempdir().unwrap();
        write_legacy(tmp.path(), FILE_INSTRUCTIONS);

        let check = check_legacy_overrides(Some(tmp.path()));
        assert_eq!(check.status, CheckStatus::Fail);
        assert!(check.message.contains(FILE_INSTRUCTIONS));
        assert!(
            check.message.contains("CLAUDE.md"),
            "the failure must name the destination surface"
        );
        assert!(
            check.message.contains("TRUSTY-MPM:"),
            "the failure must show the marker grammar to migrate into"
        );
        assert!(
            check.message.contains("#4286"),
            "the failure must cite the issue that retired the file"
        );
    }

    #[test]
    fn legacy_overrides_reports_every_file_found() {
        // A project carrying all five must have all five named. Reporting only
        // the first would leave four files to rediscover one `tm doctor` run at
        // a time.
        let tmp = tempfile::tempdir().unwrap();
        for name in LEGACY_OVERRIDE_FILES {
            write_legacy(tmp.path(), name);
        }

        let check = check_legacy_overrides(Some(tmp.path()));
        assert_eq!(check.status, CheckStatus::Fail);
        for name in LEGACY_OVERRIDE_FILES {
            assert!(
                check.message.contains(name),
                "every retired file present must be named; missing {name}"
            );
        }
    }

    #[test]
    fn legacy_overrides_warns_without_a_project_dir() {
        // Unscoped: Warn, never a false Ok. `tm doctor` outside a project must
        // not report "clean" about a project it never looked at.
        let check = check_legacy_overrides(None);
        assert_eq!(check.status, CheckStatus::Warn);
    }
}
