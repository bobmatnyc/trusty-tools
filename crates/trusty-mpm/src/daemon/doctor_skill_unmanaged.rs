//! Doctor probe: bundled skills a deploy tier does not manage (issue #4605).
//!
//! Why: `check_skill_staleness` compares the bundled source against the deploy
//! MANIFEST, so a skill absent from that manifest is outside everything it can
//! see — the probe reports a clean `Ok` while the file on disk carries text
//! the current binary removed. That is not drift and must not be reported as
//! drift: drift means "tm knows what it wrote here and the source has moved
//! on", and the fix is a deploy. This means "tm has no record of this file at
//! all", the deploy is structurally unable to reach it, and the only fix is
//! the explicit `tm install --reconcile-skills` adoption.
//!
//! Measured 2026-08-01: two `tm-workflow/SKILL.md` copies (mtimes 07-03 and
//! 07-17) still served text PR #4583 removed, while `tm install` printed only
//! `= tm-slack-canvas-delivery (unchanged)` and every gate stayed green.
//!
//! What: [`check_skill_unmanaged`] scans EVERY skill deploy tier
//! ([`skill_deploy_tiers`] — the managed `$CLAUDE_CONFIG_DIR/skills`, the
//! operator's `~/.claude/skills`, and the project's `.claude/skills`) for
//! bundled-named skills their manifests do not track, and reports
//! [`CheckStatus::Unknown`] when any exist. `Unknown`, never `Warn` and never
//! `Fail`: the probe genuinely cannot tell whether the file is an orphaned tm
//! deployment or a deliberate customization — content alone does not
//! distinguish them — and it must never render as healthy. READ-ONLY.
//!
//! Test: `doctor_skill_unmanaged_tests.rs`.

use std::collections::BTreeSet;

use std::path::Path;

use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::core::paths::FrameworkPaths;
use crate::core::skill_deploy_tiers::skill_deploy_tiers;
use crate::core::skill_tiers::list_source_stems;
use crate::core::skill_unmanaged::unmanaged_bundled_skills;

/// Name of this check as it appears in `tm doctor` output.
const CHECK_NAME: &str = "skill_unmanaged";

/// How many `<tier>/<stem>` pairs the message names before summarising.
const MAX_NAMED: usize = 5;

/// Probe for bundled skills that no deploy tier manages.
///
/// Why: see the module doc — this is the reachability half of the skill deploy
/// story, structurally invisible to every manifest-based probe because the
/// manifest is exactly what these files are missing from.
/// What: builds the bundled roster from `paths.skill_source_dir()`, then scans
/// each [`skill_deploy_tiers`] entry with
/// [`unmanaged_bundled_skills`]. `Ok` when the roster is empty (nothing to
/// classify by — reported honestly as "no bundled skill source", not as a
/// clean bill of health) or when every tier is clean; otherwise
/// [`CheckStatus::Unknown`] naming the offending `<tier>/<stem>` pairs and
/// pointing at `tm install --reconcile-skills`.
/// Test: `unmanaged_bundled_skill_reports_unknown`,
/// `managed_tier_is_ok`, `operator_skill_does_not_fire`,
/// `empty_roster_is_not_a_clean_bill`.
pub(super) fn check_skill_unmanaged(
    paths: &FrameworkPaths,
    project_dir: Option<&Path>,
) -> DoctorCheck {
    let bundled: BTreeSet<String> =
        list_source_stems(&paths.skill_source_dir()).unwrap_or_default();
    if bundled.is_empty() {
        // An empty roster cannot classify anything. Saying "clean" here would
        // be the #4605 failure mode in miniature — an unverifiable state
        // rendered as healthy.
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Unknown,
            "no bundled skill source found — cannot tell which deployed skills are tm's \
             (run `tm install` to populate it)",
        );
    }

    let mut findings: Vec<String> = Vec::new();
    for tier in skill_deploy_tiers(paths, project_dir) {
        for skill in unmanaged_bundled_skills(&tier.dir, &bundled) {
            findings.push(format!("{}/{}", tier.label, skill.stem));
        }
    }

    if findings.is_empty() {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Ok,
            "every deployed bundled skill is tracked by its tier's manifest",
        );
    }

    let shown: Vec<&str> = findings
        .iter()
        .take(MAX_NAMED)
        .map(String::as_str)
        .collect();
    let suffix = if findings.len() > shown.len() {
        format!(", … (+{} more)", findings.len() - shown.len())
    } else {
        String::new()
    };
    DoctorCheck::new(
        CHECK_NAME,
        CheckStatus::Unknown,
        format!(
            "{} bundled skill(s) are deployed but UNMANAGED ({}{}) — untracked at that tier, \
             so no deploy can refresh them and their contents cannot be verified; \
             run `tm install --reconcile-skills` to adopt and refresh them",
            findings.len(),
            shown.join(", "),
            suffix
        ),
    )
}

#[cfg(test)]
#[path = "doctor_skill_unmanaged_tests.rs"]
mod tests;
