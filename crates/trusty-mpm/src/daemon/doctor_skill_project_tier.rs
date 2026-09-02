//! Doctor probe: bundled skills left behind in a project's tier (#6586).
//!
//! Why: owner ruling 2026-09-01 makes bundled `tm-*` skills user-tier only, and
//! the deploy sites now honour it
//! (`project_skill_tier::bundled_excluded_from_project_tier`). That stops NEW
//! copies; it removes nothing. Every project provisioned by an earlier binary
//! still holds a full byte-identical set under `.claude/skills/`, and those
//! copies are now unreachable by any deploy — no `tm install` writes them, so
//! they freeze at whatever text shipped the day they landed while the user-tier
//! copy moves on. A stale copy that nothing refreshes and nothing reports is the
//! shape #4604 and #4605 were both filed for.
//!
//! WHY THIS DOES NOT DELETE. Removing a file from the operator's project on a
//! read-only diagnostic's own initiative is the one thing this probe must never
//! do: `list_project_custom_stems` cannot tell a leftover tm deployment from a
//! project-custom skill the operator wrote under a bundled name, and the second
//! is real work. So the probe REPORTS and names the exact command, and the
//! operator decides — the same contract `skill_unmanaged` keeps.
//!
//! What: [`check_skill_project_tier`] scans the project's `.claude/skills/` for
//! stems the bundled roster also carries and reports [`CheckStatus::Warn`] with
//! the remediation. `Warn`, not `Unknown`: unlike #4605 this probe knows exactly
//! what it found — a bundled-named skill at a tier that must no longer hold one —
//! and unlike `Fail` it does not break a session, because for skills Claude Code
//! resolves `personal > project`, so the current user-tier copy is the one that
//! loads. READ-ONLY.
//!
//! Test: `doctor_skill_project_tier_tests.rs`.

use std::collections::BTreeSet;
use std::path::Path;

use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::core::paths::FrameworkPaths;
use crate::core::skill_tiers::{list_project_custom_stems, list_source_stems};

/// Name of this check as it appears in `tm doctor` output.
const CHECK_NAME: &str = "skill_project_tier";

/// How many stems the message names before summarising.
const MAX_NAMED: usize = 5;

/// Probe for bundled skills still deployed at the PROJECT tier (#6586).
///
/// Why: see the module doc — the deploy-side fix cannot reach a copy an earlier
/// binary already wrote, so without this probe the duplication is invisible from
/// the day it stops being created.
/// What: intersects the bundled roster (`paths.skill_source_dir()`) with the
/// project's own `.claude/skills/` stems. `Ok` when the intersection is empty;
/// [`CheckStatus::Unknown`] when there is no project directory to scan or no
/// bundled roster to compare against — neither is a clean bill of health;
/// otherwise [`CheckStatus::Warn`] naming the stems and the
/// `tm doctor --fix-skills` remediation. Never removes a file.
/// Test: `project_tier_bundled_copy_warns`,
/// `project_tier_without_bundled_names_is_ok`,
/// `project_custom_only_tier_is_ok`, `unverifiable_states_are_unknown_not_ok`,
/// `the_probe_removes_nothing_it_reports`.
pub(super) fn check_skill_project_tier(
    paths: &FrameworkPaths,
    project_dir: Option<&Path>,
) -> DoctorCheck {
    let Some(project_dir) = project_dir else {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Unknown,
            "no project directory in scope, so whether a project tier holds bundled skills \
             could not be determined (run `tm doctor` from inside a project)",
        );
    };
    let bundled: BTreeSet<String> =
        list_source_stems(&paths.skill_source_dir()).unwrap_or_default();
    if bundled.is_empty() {
        // An empty roster cannot classify anything, and reporting `Ok` for an
        // unverifiable state is the #4605 failure mode in miniature.
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Unknown,
            "no bundled skill source found — cannot tell which project-tier skills are \
             bundled duplicates (run `tm install` to populate it)",
        );
    }

    let dir = project_dir.join(".claude").join("skills");
    let project = match list_project_custom_stems(&dir) {
        Ok(stems) => stems,
        Err(e) => {
            return DoctorCheck::new(
                CHECK_NAME,
                CheckStatus::Unknown,
                format!(
                    "the project skill tier at {} could not be read, so whether it holds \
                     bundled duplicates is undetermined: {e}",
                    dir.display()
                ),
            );
        }
    };

    let found: Vec<&String> = project.intersection(&bundled).collect();
    if found.is_empty() {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Ok,
            "the project skill tier holds no bundled skill — bundled skills are user-tier \
             only (#6586)",
        );
    }

    let shown: Vec<&str> = found.iter().take(MAX_NAMED).map(|s| s.as_str()).collect();
    let suffix = if found.len() > shown.len() {
        format!(", … (+{} more)", found.len() - shown.len())
    } else {
        String::new()
    };
    DoctorCheck::new(
        CHECK_NAME,
        CheckStatus::Warn,
        format!(
            "{} bundled skill(s) are still deployed at the PROJECT tier {} ({}{}) — bundled \
             skills are user-tier only since #6586, so no deploy refreshes these and they \
             freeze at the text that shipped when they landed. The user-tier copy is the one \
             Claude Code loads (for skills, personal outranks project). Remove them with \
             `tm doctor --fix-skills`, or keep one deliberately by renaming it so it no \
             longer shadows a bundled name",
            found.len(),
            dir.display(),
            shown.join(", "),
            suffix
        ),
    )
}

#[cfg(test)]
#[path = "doctor_skill_project_tier_tests.rs"]
mod tests;
