//! Doctor probe: can the harness actually REACH every bundled skill? (#4947)
//!
//! Why: the agent side has [`super::doctor_agent_reachability`], which fails
//! when the roster deploys into a tier the managed spawn never loads. The skill
//! side had no equivalent. Every existing skill probe assumes the tiers it is
//! handed are the live ones and then audits something else inside them —
//! `skill_staleness` compares checksums, `skill_unmanaged` compares against a
//! deploy ledger, `skill_project_tier` reports a retired duplicate. None of them
//! can fail when a rostered skill reaches no tier at all, which is exactly what
//! `deploy_skills_filtered` did to directory-shaped skills for weeks: two real
//! skills were dropped from every deploy while every presence-only probe stayed
//! green (#4949).
//!
//! What this probe asserts is the one invariant those cannot: for EVERY skill
//! the framework's own source roster declares deployable, a copy exists in a
//! tier the harness reads, that copy's frontmatter parses, and its declared
//! `name` matches the name the harness resolves it by. Both sides come from
//! production code — the roster from [`FrameworkPaths::skill_source_dir`], the
//! tiers from [`skill_deploy_tiers`], the layout rule from the deployer's own
//! [`scan_skill_sources`] — so this is not a restatement of a constant: drop a
//! skill from the deploy, deploy it somewhere the harness does not read, or
//! break its frontmatter, and this check fails.
//!
//! It also reports the SHADOWING half. A bundled skill present in two tiers is
//! a defect under the 2026-09-01 owner ruling (bundled skills are user-tier
//! only), and only the higher-precedence copy ever loads — so the lower one
//! freezes at whatever text shipped the day it landed while the operator edits
//! a file that never runs. That is a `Warn`, not a `Fail`: a skill still loads.
//!
//! READ-ONLY. Like every sibling skill probe, it removes nothing it reports.
//!
//! Test: `crates/trusty-mpm/src/daemon/doctor_skill_reachability_tests.rs`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use trusty_agents_common::skills::source_scan::scan_skill_sources;

use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::core::frontmatter::{parse_kv_line, validate_frontmatter};
use crate::core::paths::FrameworkPaths;
use crate::core::skill_deploy_tiers::skill_deploy_tiers;

/// Name of this check as it appears in `tm doctor` output.
const CHECK_NAME: &str = "skill_reachability";

/// How many findings the message names before summarising the rest.
const MAX_NAMED: usize = 5;

/// One deployed copy of a rostered skill.
///
/// Why: a finding is only actionable when it carries the tier LABEL an operator
/// recognises ("project", "operator home") next to the path they must act on,
/// and shadow reporting needs both sides of the pair.
/// What: the tier's short label and the entry point (`SKILL.md`, or a flat
/// `<stem>.md`) the harness would load.
/// Test: `a_project_copy_shadowing_the_user_copy_warns_and_names_both`.
struct DeployedCopy {
    /// Short tier label from [`skill_deploy_tiers`].
    label: &'static str,
    /// The entry-point file the harness loads for this copy.
    entry: PathBuf,
}

/// Probe whether every rostered bundled skill is reachable by the harness.
///
/// Why: see the module doc — this is the resolvability half for skills, the
/// gate #4947 filed as missing. `Fail` rather than `Warn` for an unreachable
/// skill: the capability is simply absent from every session, and an agent that
/// was told to load it silently proceeds without it.
/// What: reads the framework's deployable roster from
/// [`FrameworkPaths::skill_source_dir`] with the deployer's own
/// [`scan_skill_sources`] (so both supported layouts count), then scans every
/// tier [`skill_deploy_tiers`] enumerates. A rostered stem present in no tier,
/// or whose winning copy has unparseable frontmatter, no `name`, or a `name`
/// that disagrees with the name it is deployed under, is a `Fail` finding
/// naming the path and the reason. A stem present in more than one tier is a
/// `Warn` finding naming every path and the copy to delete. `Ok` with the count
/// when the roster is clean; [`CheckStatus::Unknown`] when there is no roster to
/// compare against or a tier that cannot be read — neither is a clean bill of
/// health (the #4605 fail-open shape). Never writes.
/// Test: `a_fully_deployed_roster_is_ok`,
/// `a_rostered_skill_deployed_nowhere_fails_and_names_it`,
/// `malformed_frontmatter_fails_and_names_the_path`,
/// `a_frontmatter_name_that_disagrees_with_the_directory_fails`,
/// `a_project_copy_shadowing_the_user_copy_warns_and_names_both`,
/// `an_empty_roster_is_unknown_not_ok`, `an_unreadable_tier_is_unknown_not_ok`,
/// `the_probe_removes_nothing_it_reports`.
pub(super) fn check_skill_reachability(
    paths: &FrameworkPaths,
    project_dir: Option<&Path>,
) -> DoctorCheck {
    let source = paths.skill_source_dir();
    let roster: Vec<String> = match scan_skill_sources(&source) {
        Ok(scan) => scan.skills.into_iter().map(|skill| skill.stem).collect(),
        Err(e) => {
            return unknown(format!(
                "the bundled skill roster at {} could not be read, so which skills should be \
                 reachable is undetermined: {e}",
                source.display()
            ));
        }
    };
    if roster.is_empty() {
        // An empty roster declares nothing, and "nothing was missing" would
        // render as healthy — the fail-open shape #4605 was filed for.
        return unknown(format!(
            "no bundled skill source found at {} — nothing declares which skills should be \
             deployed, so reachability is undetermined (run `tm install` to populate it)",
            source.display()
        ));
    }

    let tiers = skill_deploy_tiers(paths, project_dir);
    let mut deployed: BTreeMap<String, Vec<DeployedCopy>> = BTreeMap::new();
    for tier in &tiers {
        // `scan_skill_sources` treats an ABSENT directory as an empty tier,
        // which is correct — a tier nobody deployed into holds nothing. An
        // EXISTING directory that cannot be listed is a different fact, and
        // counting it as empty would report every skill in it unreachable.
        let scan = match scan_skill_sources(&tier.dir) {
            Ok(scan) => scan,
            Err(e) => {
                return unknown(format!(
                    "the {} skill tier at {} could not be read, so whether the bundled roster \
                     is reachable is undetermined: {e}",
                    tier.label,
                    tier.dir.display()
                ));
            }
        };
        for skill in scan.skills {
            deployed.entry(skill.stem).or_default().push(DeployedCopy {
                label: tier.label,
                entry: skill.entry,
            });
        }
    }

    let searched = tiers
        .iter()
        .map(|tier| format!("{} ({})", tier.label, tier.dir.display()))
        .collect::<Vec<_>>()
        .join(", ");

    let mut unreachable: Vec<String> = Vec::new();
    let mut shadowed: Vec<String> = Vec::new();
    for stem in &roster {
        let Some(copies) = deployed.get(stem) else {
            unreachable.push(format!(
                "`{stem}` is in the bundled roster but present in no tier the harness reads \
                 (searched {searched})"
            ));
            continue;
        };
        // `skill_deploy_tiers` is ordered highest-reach first, so index 0 is the
        // copy that actually loads and the only one worth parsing.
        let winner = &copies[0];
        if let Some(reason) = frontmatter_defect(&winner.entry, stem) {
            unreachable.push(format!("`{stem}` at {}: {reason}", winner.entry.display()));
        }
        if let Some(finding) = shadow_finding(stem, copies) {
            shadowed.push(finding);
        }
    }

    verdict(roster.len(), unreachable, shadowed)
}

/// Why the harness could not resolve `entry` as the skill named `stem`.
///
/// Why: presence is not reachability. A deployed file whose frontmatter does
/// not parse, or which declares a different `name` than the directory it sits
/// in, is on disk and still unusable — and every presence-only probe reports it
/// as deployed.
/// What: `None` when the file reads, its frontmatter strict-parses (the same
/// [`validate_frontmatter`] the agent deploy path applies), and its `name` field
/// equals `stem`. Otherwise a reason phrase naming what went wrong.
/// Test: `malformed_frontmatter_fails_and_names_the_path`,
/// `a_frontmatter_name_that_disagrees_with_the_directory_fails`,
/// `frontmatter_without_a_name_fails`.
fn frontmatter_defect(entry: &Path, stem: &str) -> Option<String> {
    let content = match std::fs::read_to_string(entry) {
        Ok(content) => content,
        Err(e) => return Some(format!("its entry point could not be read: {e}")),
    };
    if let Err(detail) = validate_frontmatter(&content) {
        return Some(format!(
            "its frontmatter does not parse ({detail}), so the harness cannot load it"
        ));
    }
    let Some(name) = declared_name(&content) else {
        return Some(
            "its frontmatter declares no `name`, so the harness has nothing to resolve it by"
                .to_string(),
        );
    };
    if name != stem {
        return Some(format!(
            "its frontmatter declares `name: {name}` but it is deployed as `{stem}` — the \
             harness resolves the deployed name, so the skill is unreachable under the name it \
             claims"
        ));
    }
    None
}

/// The `name:` field of a document's frontmatter block.
///
/// Why: `serde_yaml` would deserialize the whole block for one scalar, and this
/// repo already has one shared line parser every frontmatter reader routes
/// through.
/// What: scans lines between the opening fence and the first closing `---`,
/// returning the first `name` value [`parse_kv_line`] yields.
/// Test: `a_frontmatter_name_that_disagrees_with_the_directory_fails`,
/// `frontmatter_without_a_name_fails`.
fn declared_name(content: &str) -> Option<String> {
    let mut lines = content.lines();
    // Skip the opening `---` fence; `validate_frontmatter` already proved it is
    // there and terminated.
    lines.next()?;
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        if let Some((key, value)) = parse_kv_line(line)
            && key == "name"
        {
            return Some(value);
        }
    }
    None
}

/// The shadowing finding for a stem deployed at more than one tier.
///
/// Why: bundled skills are user-tier only (owner ruling 2026-09-01, #6586).
/// Two copies means only the higher-precedence one ever loads, so the other
/// freezes at the text that shipped when it landed and any edit to it is
/// invisible. The operator can only act on that if the finding names BOTH paths
/// and says which one to delete.
/// What: `None` for a single copy; otherwise a phrase naming the winning tier
/// and path, every shadowed tier and path, and the deletion.
/// Test: `a_project_copy_shadowing_the_user_copy_warns_and_names_both`.
fn shadow_finding(stem: &str, copies: &[DeployedCopy]) -> Option<String> {
    let (winner, losers) = copies.split_first()?;
    if losers.is_empty() {
        return None;
    }
    let shadowed = losers
        .iter()
        .map(|copy| format!("{} ({})", copy.label, copy.entry.display()))
        .collect::<Vec<_>>()
        .join(", ");
    let delete = losers
        .iter()
        .map(|copy| copy.entry.display().to_string())
        .collect::<Vec<_>>()
        .join(" and ");
    Some(format!(
        "`{stem}` is deployed at {} tiers — the {} copy at {} is the one the harness loads and \
         it SHADOWS {shadowed}. Bundled skills are user-tier only (owner ruling, #6586), so \
         delete the shadowed copy at {delete}",
        copies.len(),
        winner.label,
        winner.entry.display()
    ))
}

/// Fold the findings into one [`DoctorCheck`].
///
/// Why: an unreachable skill is absent from every session and a shadowed one
/// still loads, so the two cannot share a severity — but they do share a row,
/// because both answer "can the harness reach the roster?".
/// What: `Ok` naming the roster size when both lists are empty; `Fail` when any
/// skill is unreachable; `Warn` when only shadowing was found. The message names
/// up to [`MAX_NAMED`] findings and summarises the rest.
/// Test: `a_fully_deployed_roster_is_ok`,
/// `a_rostered_skill_deployed_nowhere_fails_and_names_it`,
/// `a_project_copy_shadowing_the_user_copy_warns_and_names_both`,
/// `an_unreachable_skill_outranks_a_shadow`.
fn verdict(rostered: usize, unreachable: Vec<String>, shadowed: Vec<String>) -> DoctorCheck {
    if unreachable.is_empty() && shadowed.is_empty() {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Ok,
            format!(
                "all {rostered} bundled skill(s) are reachable — each is deployed in a tier the \
                 harness reads, parses, and resolves under its own name, with no tier shadowing \
                 another (#4947)"
            ),
        );
    }
    let status = if unreachable.is_empty() {
        CheckStatus::Warn
    } else {
        CheckStatus::Fail
    };
    let mut findings = unreachable;
    findings.extend(shadowed);
    let shown: Vec<&str> = findings
        .iter()
        .take(MAX_NAMED)
        .map(String::as_str)
        .collect();
    let suffix = if findings.len() > shown.len() {
        format!(" … (+{} more)", findings.len() - shown.len())
    } else {
        String::new()
    };
    DoctorCheck::new(
        CHECK_NAME,
        status,
        format!(
            "{} reachability finding(s) across the {rostered} bundled skill(s) the framework \
             declares deployable: {}{suffix} (issue #4947)",
            findings.len(),
            shown.join("; ")
        ),
    )
}

/// An undetermined outcome — never `Ok`.
///
/// Why: reporting a state this probe could not verify as healthy is the exact
/// fail-open shape #4605 was filed for.
/// What: an [`CheckStatus::Unknown`] check carrying `message`.
/// Test: `an_empty_roster_is_unknown_not_ok`, `an_unreadable_tier_is_unknown_not_ok`.
fn unknown(message: String) -> DoctorCheck {
    DoctorCheck::new(CHECK_NAME, CheckStatus::Unknown, message)
}

#[cfg(test)]
#[path = "doctor_skill_reachability_tests.rs"]
mod tests;
