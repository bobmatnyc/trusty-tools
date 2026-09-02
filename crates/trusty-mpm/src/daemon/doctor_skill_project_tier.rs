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
//! do. So the probe REPORTS and names the exact command, and the operator
//! decides — the same contract `skill_unmanaged` keeps.
//! [`crate::core::project_tier_strays`] is the action half, previewed by
//! `tm doctor --fix-skills` and applied by `tm doctor --fix-skills --yes`.
//!
//! What: [`check_skill_project_tier`] reads the project's `.claude/skills/`
//! FROM DISK and reports [`CheckStatus::Warn`] for every directory whose stem
//! the bundled roster also carries. `Warn`, not `Unknown`: unlike #4605 this
//! probe knows exactly what it found — a bundled-named skill at a tier that must
//! no longer hold one — and unlike `Fail` it does not break a session, because
//! for skills Claude Code resolves `personal > project`, so the current
//! user-tier copy is the one that loads. READ-ONLY.
//!
//! DISK, NOT THE LEDGER (#6586, live verification). This probe first intersected
//! the roster with `list_project_custom_stems`, which by design drops every stem
//! the tier's `.trusty-mpm-skills-manifest.json` marks MANAGED. That is the
//! right rule for the deploy planner — a managed stem is tm's to refresh, not a
//! project-custom name to honour — and the exact wrong rule here, because a copy
//! the pre-#6602 deploy left behind is manifest-managed BY CONSTRUCTION. So the
//! one shape this check exists to catch was the one shape it could not see: a
//! project holding 51 bundled copies reported `✅ … holds no bundled skill`.
//! The ledger now decides only what the REPAIR may remove, never what the probe
//! may report.
//!
//! Test: `doctor_skill_project_tier_tests.rs`.

use std::collections::BTreeSet;
use std::path::Path;

use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::core::paths::FrameworkPaths;
use crate::core::project_tier_strays::unclassifiable_bundled_entries;
use crate::core::skill_deploy_tiers::project_skill_tier;
use crate::core::skill_tiers::list_source_stems;
use crate::core::skill_unmanaged::bundled_skill_dirs;

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
/// skill directories [`bundled_skill_dirs`] finds ON DISK under the project's
/// `.claude/skills/` — managed or not, since a pre-#6602 stray is always
/// managed. `Ok` when nothing matches; [`CheckStatus::Unknown`] when there is no
/// project directory to scan, no bundled roster to compare against, or a project
/// tier that exists and cannot be read — none is a clean bill of health;
/// otherwise [`CheckStatus::Warn`] naming the count, up to [`MAX_NAMED`] stems,
/// and the `tm doctor --fix-skills` remediation (which previews; `--yes`
/// applies). Never removes a file.
/// Test: `project_tier_bundled_copy_warns`,
/// `project_tier_manifest_managed_copy_is_still_flagged`,
/// `project_tier_without_bundled_names_is_ok`, `a_user_custom_skill_is_not_flagged`,
/// `project_custom_only_tier_is_ok`, `unverifiable_states_are_unknown_not_ok`,
/// `an_unclassifiable_bundled_entry_is_counted_by_the_check`,
/// `an_unreadable_project_tier_is_unknown_not_ok`,
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

    let dir = project_skill_tier(project_dir);
    // #6586: `bundled_skill_dirs` treats an unreadable directory as empty, and
    // "empty" here renders as a clean bill of health — the #4605 fail-open shape.
    // An ABSENT tier genuinely holds no stray; anything else is undetermined.
    match std::fs::read_dir(&dir) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
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
    }

    // #6586: read the tier from DISK. The manifest decides what the repair may
    // remove, never what this probe may report — see the module doc.
    let mut found: Vec<String> = bundled_skill_dirs(&dir, &bundled)
        .into_iter()
        .map(|skill| skill.stem)
        .collect();
    // #6586 critic: the sweep reports a refusal for a bundled-named entry that
    // is not a skill directory, so a check that counted only the classified set
    // said "holds no bundled skill" about a tier `--fix-skills` then listed. The
    // shared finder is what keeps the two counts equal.
    let unclassifiable: Vec<String> = {
        let classified: BTreeSet<&str> = found.iter().map(String::as_str).collect();
        unclassifiable_bundled_entries(&dir, &bundled, &classified)
            .iter()
            .filter_map(|path| Some(path.file_name()?.to_string_lossy().into_owned()))
            .collect()
    };
    found.extend(unclassifiable);
    found.sort();
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
             Claude Code loads (for skills, personal outranks project). Preview the removal with \
             `tm doctor --fix-skills` and apply it with `tm doctor --fix-skills --yes`, or \
             keep one deliberately by renaming it so it no longer shadows a bundled name",
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
