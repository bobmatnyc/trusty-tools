//! `tm doctor` row for `CLAUDE.md` files ABOVE the project root (#7673).
//!
//! Why: Claude Code loads every memory file from the session cwd up to the
//! filesystem root, so one file at `$HOME` is prepended to every turn of every
//! agent in every project beneath it. On 2026-09-12 the file found there was
//! tm's own seed template — boilerplate with no project content, paid for on
//! every turn, with nothing in the harness reporting it. No other doctor check
//! looks above the project root at all.
//!
//! What: [`check_ancestor_claude_md`] reports each ancestor memory file with its
//! path, byte size and a token estimate. `Ok` when none loads (nothing found, or
//! every one already in `claudeMdExcludes`); `Warn` when one carries real
//! content; `Fail` when one is a pure tm seed template, which is cost with no
//! content behind it. Read-only — `tm doctor --fix --yes` is the repair.
//! Test: the `tests` module below.

use std::path::Path;

use crate::core::doctor::{CheckStatus, DoctorCheck};

/// The check's name, as `tm doctor` and the generated catalog print it.
///
/// Why: one literal, shared by the probe, the repair and the catalog drift
/// guard.
/// What: `ancestor_claude_md`.
/// Test: `doctor_checks_match_run_doctor_names`.
pub(super) const CHECK_NAME: &str = "ancestor_claude_md";

/// Report every `CLAUDE.md` above the project root, and what it costs.
///
/// Why: see the module doc.
/// What: `Ok` with no project directory (nothing to scan) and `Ok` when nothing
/// above the project loads. Otherwise `Fail` when any finding is a pure seed
/// template — pure cost, no content — and `Warn` when the findings all carry
/// real content, which loads into every session under that directory and may
/// well be wanted. Each message names the path, the byte size, the estimate and
/// the divisor. Read-only: it stats each candidate and opens only the ones small
/// enough to be a seed.
/// Test: `ancestor_claude_md_is_ok_without_a_project`,
/// `ancestor_claude_md_is_ok_with_no_ancestors`,
/// `a_seed_template_ancestor_fails`,
/// `a_content_ancestor_warns`,
/// `an_excluded_ancestor_is_ok`.
pub(super) fn check_ancestor_claude_md(
    project_dir: Option<&Path>,
    home: Option<&Path>,
    managed_config_dir: Option<&Path>,
) -> DoctorCheck {
    let Some(project) = project_dir else {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Ok,
            "no project directory supplied — nothing to scan",
        );
    };

    // #7673 round 3: `project` here is `std::env::current_dir()` from the CLI —
    // an arbitrary process cwd, not necessarily the project's own root. `scan`
    // resolves to the real root (git toplevel, or the registered `.trusty-mpm`
    // marker) before it walks, so running `tm doctor` from a crate subdirectory
    // no longer reports the repo's own root CLAUDE.md as a stray ancestor.
    let found = crate::core::ancestor_claude_md::scan(project, home, managed_config_dir);
    let (excluded, loading): (Vec<_>, Vec<_>) = found.iter().partition(|f| f.excluded);

    if loading.is_empty() {
        let suffix = if excluded.is_empty() {
            String::new()
        } else {
            format!(
                "; {} already kept out by `claudeMdExcludes`",
                excluded.len()
            )
        };
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Ok,
            format!(
                "no CLAUDE.md above {} loads into this project's sessions{suffix}",
                project.display()
            ),
        );
    }

    let total: u64 = loading.iter().map(|f| f.token_estimate()).sum();
    let listed = loading
        .iter()
        .map(|f| f.summary())
        .collect::<Vec<_>>()
        .join("; ");
    let seeds = loading.iter().filter(|f| f.seed_template).count();

    let (status, hint) = if seeds > 0 {
        (
            CheckStatus::Fail,
            format!(
                "{seeds} of them is a tm seed template — pure cost, no project content. \
                 `tm doctor --fix --yes` renames it aside"
            ),
        )
    } else {
        (
            CheckStatus::Warn,
            "each one loads into EVERY session started under that directory. \
             `tm doctor --fix --yes` adds them to `claudeMdExcludes` in this project's \
             .claude/settings.local.json, leaving the files alone"
                .to_string(),
        )
    };

    DoctorCheck::new(
        CHECK_NAME,
        status,
        format!(
            "{} CLAUDE.md file(s) above the project root cost ~{total} tokens per turn \
             (estimated at bytes/{}): {listed}. {hint}",
            loading.len(),
            crate::core::ancestor_claude_md::BYTES_PER_TOKEN,
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::instruction_pipeline::CLAUDE_MD_STUB;
    use tempfile::TempDir;

    /// `<tmp>/ancestor/project`, so the project has exactly one ancestor the
    /// test controls plus the temp root above it.
    fn fixture() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
        let tmp = TempDir::new().unwrap();
        let ancestor = tmp.path().join("ancestor");
        let project = ancestor.join("project");
        std::fs::create_dir_all(&project).unwrap();
        (tmp, ancestor, project)
    }

    #[test]
    fn ancestor_claude_md_is_ok_without_a_project() {
        let check = check_ancestor_claude_md(None, None, None);
        assert_eq!(check.status, CheckStatus::Ok);
    }

    #[test]
    fn ancestor_claude_md_is_ok_with_no_ancestors() {
        let (_tmp, _ancestor, project) = fixture();
        let check = check_ancestor_claude_md(Some(&project), None, None);
        assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
        assert!(check.message.contains("no CLAUDE.md above"));
    }

    /// FAILS BEFORE THIS CHANGE: no check looked above the project root, so the
    /// `$HOME` seed template of 2026-09-12 reported nothing at all.
    #[test]
    fn a_seed_template_ancestor_fails() {
        let (_tmp, ancestor, project) = fixture();
        std::fs::write(ancestor.join("CLAUDE.md"), CLAUDE_MD_STUB).unwrap();

        let check = check_ancestor_claude_md(Some(&project), None, None);

        assert_eq!(check.status, CheckStatus::Fail, "{}", check.message);
        assert!(
            check.message.contains("tm seed template"),
            "{}",
            check.message
        );
        assert!(check.message.contains("bytes/4"), "{}", check.message);
    }

    #[test]
    fn a_content_ancestor_warns() {
        let (_tmp, ancestor, project) = fixture();
        std::fs::write(
            ancestor.join("CLAUDE.md"),
            "# Monorepo\n\nAll packages use pnpm.\n",
        )
        .unwrap();

        let check = check_ancestor_claude_md(Some(&project), None, None);

        assert_eq!(check.status, CheckStatus::Warn, "{}", check.message);
        assert!(
            check.message.contains("claudeMdExcludes"),
            "{}",
            check.message
        );
    }

    /// An ancestor the operator has already excluded is not a finding — the
    /// session does not load it.
    #[test]
    fn an_excluded_ancestor_is_ok() {
        let (_tmp, ancestor, project) = fixture();
        let file = ancestor.join("CLAUDE.md");
        std::fs::write(&file, "# Monorepo\n\nAll packages use pnpm.\n").unwrap();
        // The spelling `--fix` writes is the scan's own (canonical) one, so the
        // fixture uses it too — on macOS a temp dir reaches the same file as
        // both `/var/...` and `/private/var/...`.
        let file = std::fs::canonicalize(&file).unwrap();
        let settings = project.join(".claude").join("settings.local.json");
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        std::fs::write(
            &settings,
            serde_json::json!({ "claudeMdExcludes": [file.display().to_string()] }).to_string(),
        )
        .unwrap();

        let check = check_ancestor_claude_md(Some(&project), None, None);

        assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
        assert!(
            check.message.contains("claudeMdExcludes"),
            "{}",
            check.message
        );
    }
}
