//! Doctor probe: deployed skills versus the running binary's own assets
//! (issues #2876, #4604).
//!
//! Why: this check used to compare `FrameworkPaths::skill_source_dir()` — the
//! `~/.trusty-mpm/framework/skills/` EXTRACTION CACHE — against the checksum in
//! each deploy manifest, so a cache that lagged the installed binary made every
//! skill it covered report clean regardless of what actually shipped. That is
//! the #4604 defect: `tm-workflow` drifted in all three deploy tiers and the
//! check named only two other skills, which reads as an all-clear for the one it
//! missed. The reference point is now the binary's own embedded asset and the
//! comparison reads the deployed FILE — see [`crate::core::skill_drift`] for the
//! full reasoning.
//!
//! What: [`check_skill_staleness`] audits every skill deploy tier
//! ([`skill_deploy_tiers`] — the managed `$CLAUDE_CONFIG_DIR/skills`, the
//! operator's `~/.claude/skills`, and the project's `.claude/skills`), folds the
//! per-skill states into one severity, and prescribes the remedy that matches
//! the state. It distinguishes 'drifted — `tm install` will fix it' from
//! 'drifted and FROZEN — `tm install` will deliberately skip it', because the
//! remediation differs and a frozen skill can otherwise stay stale forever with
//! no signal. Since #5865 it also distinguishes drift BY TIER: `tm install`
//! writes only the operator-home tier, so claiming it repairs managed-config or
//! project-tier drift sent operators re-running a command that could not move
//! the count. Anything it cannot verify reports [`CheckStatus::Unknown`], never
//! `Ok`. READ-ONLY: the repair lives behind `tm doctor --fix-skills`.
//!
//! Test: `doctor_skill_drift_tests.rs`.

use std::path::Path;

use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::core::paths::FrameworkPaths;
use crate::core::skill_deploy_tiers::skill_deploy_tiers;
use crate::core::skill_drift::{
    ManifestState, SkillDrift, SkillReference, audit_deployed_skills, key_stem, skill_reference,
};

/// Name of this check as it appears in `tm doctor` output.
const CHECK_NAME: &str = "skill_staleness";

/// How many `<tier>/<stem>` pairs each severity bucket names before summarising.
const MAX_NAMED: usize = 5;

/// Bundled skills whose drift can silently drop a framework-guaranteed rule.
///
/// Why: most bundled skills are good-practice guidance where staleness is
/// low-stakes, so ordinary drift is advisory. But a small subset are the sole
/// place a *framework convention* — a rule other code or process depends on
/// being followed exactly, like the commit/PR attribution footer — is spelled
/// out. Drift in one of THESE reproduces the PR #2825 attribution bypass this
/// probe exists to catch, so it escalates to `Fail`. A skill belongs here only
/// if it states a framework-guaranteed rule as executable instruction AND a
/// stale copy could make that rule lapse with no other signal. Kept
/// intentionally short: sweeping in ordinary skills would make `Fail` noise that
/// trains operators to ignore it.
/// What: the list, with each entry's rule:
/// - `documentation-style` — the mandatory Why/What/Test doc-comment triple.
/// - `tm-workflow` — the attribution-footer text, the
///   `--assignee @me --label trusty-mpm` defaults, and squash-merge-only. It
///   inherited all three from the retired `tm-pr-workflow` (#5202).
/// - `tm-git-file-tracking` — CB#4's blocking file-tracking gate, and the other
///   skill reproducing the attribution footer inline.
///
/// Test: `staleness_escalates_when_conventions_skill_drifts`,
/// `staleness_warns_when_ordinary_skill_drifts`.
const CONVENTIONS_BEARING_SKILLS: &[&str] =
    &["documentation-style", "tm-workflow", "tm-git-file-tracking"];

/// Audit every deploy tier's skills against the running binary's assets.
///
/// Why: see the module doc. The pre-#4604 version compared a cache against a
/// manifest and could report clean while both were stale.
/// What: builds the reference with [`skill_reference`] (the `agents/skills`
/// submodule when one is checked out, else the compiled-in bundle — NEVER the
/// extraction cache), audits each [`skill_deploy_tiers`] entry, and folds the
/// findings:
/// - `Fail` when a [`CONVENTIONS_BEARING_SKILLS`] entry drifted, is frozen, or
///   is missing;
/// - `Unknown` when anything could not be verified;
/// - `Warn` for ordinary drift, a frozen ordinary skill, or a missing file;
/// - `Ok` only when every managed skill at every tier matches byte-for-byte.
///
/// Drift is reported one fragment per tier via [`drift_parts`], each carrying
/// that tier's own remedy (#5865). Severity is unchanged by that split — the
/// [`CONVENTIONS_BEARING_SKILLS`] escalation is stated per SKILL and fires at
/// every tier, so managed-config drift in `tm-workflow` is still `Fail`.
///
/// The message reports the frozen set separately, since `tm install` will not
/// repair it. An empty reference is itself `Unknown` — with nothing to compare
/// against, "clean" would be a claim the probe cannot support.
/// Test: `doctor_skill_drift_tests.rs`.
pub(super) fn check_skill_staleness(
    paths: &FrameworkPaths,
    project_dir: Option<&Path>,
) -> DoctorCheck {
    // `skill_source_dir()` returns the git submodule when one is checked out and
    // `paths.skills` (the extraction cache) otherwise. Only the former is a
    // legitimate reference; the latter is precisely what #4604 proved untrustworthy.
    let source = paths.skill_source_dir();
    let submodule = (source != paths.skills).then_some(source);
    let reference = skill_reference(submodule.as_deref());
    report(&reference, paths, project_dir)
}

/// Pure-ish core of [`check_skill_staleness`]: audit tiers and fold findings.
///
/// Why: taking the reference as a parameter lets the tests drive every severity
/// branch with literal asset content instead of the real embedded bundle.
/// What: see [`check_skill_staleness`].
/// Test: `doctor_skill_drift_tests.rs`.
fn report(
    reference: &SkillReference,
    paths: &FrameworkPaths,
    project_dir: Option<&Path>,
) -> DoctorCheck {
    if reference.assets.is_empty() {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Unknown,
            "no bundled skill assets are available to compare against — deployed skill \
             content cannot be verified (issue #4604)",
        );
    }

    // #5865: drift is grouped by TIER, because the remedy differs per tier and
    // only one of them is the tier `tm install` writes.
    let mut drifted: Vec<(&'static str, String)> = Vec::new();
    let mut frozen: Vec<String> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    let mut unverifiable: Vec<String> = Vec::new();
    let mut conventions: Vec<String> = Vec::new();

    for tier in skill_deploy_tiers(paths, project_dir) {
        let audit = audit_deployed_skills(reference, &tier.dir);
        // #4622 review (MEDIUM): a tier whose ownership ledger is missing over
        // deployed content, or present but unparseable, has NOT been shown to be
        // clean — `SkillManifest::load` returns an empty manifest for both, which
        // previously rendered as `Ok — every deployed skill matches`.
        match &audit.manifest {
            ManifestState::Present | ManifestState::AbsentAndEmpty => {}
            ManifestState::AbsentButPopulated => unverifiable.push(format!(
                "{} (skills are deployed at {} but its `{}` ownership ledger is absent —                  nothing there can be attributed to tm or to the operator)",
                tier.label,
                tier.dir.display(),
                crate::core::skill_manifest::SKILL_MANIFEST_FILE,
            )),
            ManifestState::Unreadable(why) => {
                unverifiable.push(format!("{} ({why})", tier.label))
            }
        }

        for finding in audit.findings {
            let where_ = format!("{}/{}", tier.label, finding.stem);
            // Severity is stated per SKILL, but a manifest key may name one of
            // that skill's `references/*.md` files — match on the stem so a
            // drifted reference of a conventions-bearing skill still escalates.
            let conventions_bearing = CONVENTIONS_BEARING_SKILLS.contains(&key_stem(&finding.stem));
            match finding.state {
                SkillDrift::Fresh => continue,
                SkillDrift::Drifted => drifted.push((tier.label, where_.clone())),
                SkillDrift::DriftedFrozen => frozen.push(where_.clone()),
                SkillDrift::Missing => missing.push(where_.clone()),
                SkillDrift::Unverifiable(why) => {
                    unverifiable.push(format!("{where_} ({why})"));
                    // An unverifiable skill is not evidence of a lapsed
                    // convention — it is an absence of evidence — so it never
                    // joins the escalation list.
                    continue;
                }
            }
            if conventions_bearing {
                conventions.push(where_);
            }
        }
    }

    if drifted.is_empty() && frozen.is_empty() && missing.is_empty() && unverifiable.is_empty() {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Ok,
            format!(
                "every deployed skill matches {} at every deploy tier",
                reference.origin
            ),
        );
    }

    let mut parts: Vec<String> = Vec::new();
    parts.extend(drift_parts(&drifted));
    if !frozen.is_empty() {
        parts.push(format!(
            "{} drifted and FROZEN — hand-edited after deployment, so `tm install` will \
             deliberately skip them and they stay stale ({}); re-deploy with \
             `tm doctor --fix-skills --include-frozen` (backs each file up first)",
            frozen.len(),
            summarise(&frozen)
        ));
    }
    if !missing.is_empty() {
        parts.push(format!(
            "{} recorded as deployed but ABSENT from disk ({})",
            missing.len(),
            summarise(&missing)
        ));
    }
    if !unverifiable.is_empty() {
        parts.push(format!(
            "{} could NOT be verified ({})",
            unverifiable.len(),
            summarise(&unverifiable)
        ));
    }

    let body = format!(
        "compared against {}: {} (issue #4604)",
        reference.origin,
        parts.join("; ")
    );

    let status = if !conventions.is_empty() {
        CheckStatus::Fail
    } else if !unverifiable.is_empty() {
        CheckStatus::Unknown
    } else {
        CheckStatus::Warn
    };

    let message = if conventions.is_empty() {
        body
    } else {
        format!(
            "conventions-bearing skill(s) have drifted ({}) — a framework-guaranteed rule \
             (the commit/PR attribution footer, the Why/What/Test doc pattern) may have \
             silently lapsed in the deployed copy. {body}",
            summarise(&conventions)
        )
    };

    DoctorCheck::new(CHECK_NAME, status, message)
}

/// One message fragment per DRIFTED tier, each naming the remedy that tier
/// actually has (#5865).
///
/// Why: the single fragment this replaces said "N drifted and REPAIRABLE by
/// `tm install`" for every finding at every tier, but `tm install` writes only
/// `paths.claude_skills_dir()` — the operator-home tier. An operator reading
/// that line re-ran `tm install` against managed-config and project-tier drift
/// and watched the count refuse to move, which is the whole of #5865. Grouping
/// by tier is what makes the sentence checkable against what the code does.
/// What: preserves the tier order [`skill_deploy_tiers`] produced (findings are
/// appended tier by tier), emits one fragment per tier that has drift, and
/// tags each with [`drift_remedy`]. Returns an empty vector when nothing
/// drifted, so a frozen-only or missing-only report gains no drift clause.
/// Test: `drift_names_the_tm_install_remedy_only_for_the_operator_home_tier`,
/// `managed_config_drift_does_not_claim_tm_install_repairs_it`.
fn drift_parts(drifted: &[(&'static str, String)]) -> Vec<String> {
    let mut order: Vec<&'static str> = Vec::new();
    for (label, _) in drifted {
        if !order.contains(label) {
            order.push(label);
        }
    }
    order
        .into_iter()
        .map(|label| {
            let entries: Vec<String> = drifted
                .iter()
                .filter(|(l, _)| *l == label)
                .map(|(_, where_)| where_.clone())
                .collect();
            format!(
                "{} drifted at the {label} tier — {} ({})",
                entries.len(),
                drift_remedy(label),
                summarise(&entries)
            )
        })
        .collect()
}

/// What actually repairs drift at one deploy tier (#5865).
///
/// Why: three tiers, three writers. `tm install` deploys into
/// `paths.claude_skills_dir()` and nowhere else; the managed
/// `$CLAUDE_CONFIG_DIR/skills` tier is written by
/// [`crate::core::standalone::global_config::ensure_global_config_dir`] on
/// managed-session bootstrap; a project's `.claude/skills` is written when a
/// managed session is prepared for that project. Naming one command for all
/// three is the defect. `tm doctor --fix-skills` is the one remedy that covers
/// every tier — [`crate::core::skill_repair::repair_skills_in_mode`] iterates
/// [`skill_deploy_tiers`] — so it is offered wherever the tier's own writer is
/// not something the operator can invoke directly.
/// What: a phrase per known tier label. An unrecognised label (a tier added
/// without updating this table) falls back to the tier-agnostic remedy rather
/// than to a claim about `tm install` that may be false.
/// Test: `drift_names_the_tm_install_remedy_only_for_the_operator_home_tier`,
/// `project_tier_drift_names_a_remedy_that_writes_the_project_tier`,
/// `an_unknown_tier_label_never_claims_tm_install_repairs_it`.
fn drift_remedy(label: &str) -> &'static str {
    match label {
        "operator home" => "REPAIRABLE by `tm install`",
        "managed config" => {
            "`tm install` does NOT write this tier — it is refreshed on managed-session \
             bootstrap, or repair it now with `tm doctor --fix-skills`"
        }
        "project" => {
            "`tm install` does NOT write this tier — it is refreshed when a managed session \
             is next prepared for this project, or repair it now with `tm doctor --fix-skills`"
        }
        _ => "repairable with `tm doctor --fix-skills`",
    }
}

/// Render up to [`MAX_NAMED`] entries, then a count of the remainder.
///
/// Why: the message must stay bounded no matter how many skills drifted, while
/// still naming enough to act on.
/// What: comma-joined first entries plus `, … (+N more)`.
/// Test: `staleness_message_is_bounded`.
fn summarise(items: &[String]) -> String {
    let shown: Vec<&str> = items.iter().take(MAX_NAMED).map(String::as_str).collect();
    if items.len() > shown.len() {
        format!(
            "{}, … (+{} more)",
            shown.join(", "),
            items.len() - shown.len()
        )
    } else {
        shown.join(", ")
    }
}

#[cfg(test)]
#[path = "doctor_skill_drift_tests.rs"]
mod tests;
