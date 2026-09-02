//! Doctor probe: one asset name claimed twice inside one tier (#6649).
//!
//! Why: `asset_tier` (#4442) and `skill_project_tier` (#6586) both compare one
//! directory against another — is this agent in the wrong tier, is this skill
//! at the project tier. Neither can see a collision that never leaves a single
//! directory: `qa.md` beside `qa/`, or `QA.md` beside `qa.md`. On macOS the
//! filesystem is case-insensitive, so the second pair is ONE file to the
//! loader; on a case-sensitive volume it is two, and which one loads is loader
//! order. Either way the operator edits one and the harness may run the other.
//!
//! WHY A NEW CHECK RATHER THAN A FOLD INTO EITHER EXISTING ONE. The subject is
//! different, and folding would make one row's verdict answer two unrelated
//! questions. `asset_tier` asks "is this file tm's, and is it in the wrong
//! place" — its whole predicate is OWNERSHIP, and a duplicate needs no
//! ownership at all (both entries may be the operator's own). `skill_project_
//! tier` asks the same about one kind, and this probe spans BOTH kinds, so
//! folding it there would leave agents uncovered. A separate row also keeps the
//! remediation honest: those two name a `tm doctor --fix-*` command, and this
//! one deliberately names none.
//!
//! What: [`check_asset_duplicates`] scans the agent and skill tiers this
//! machine actually resolves — the canonical deploy dirs and, when doctor is
//! scoped to a project, that project's own `.claude/agents` and
//! `.claude/skills` — and reports every stem more than one entry claims.
//! READ-ONLY, and unlike its two neighbours it stays that way: tm cannot know
//! which of two entries the operator meant to keep, so there is no `--fix`
//! (owner ruling 2026-09-02).
//!
//! Test: `doctor_asset_duplicates_tests.rs`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::core::asset_duplicates::{DuplicateStem, name_duplicates, scan_duplicate_stems};
use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::core::paths::FrameworkPaths;

/// Name of this check as it appears in `tm doctor` output.
const CHECK_NAME: &str = "asset_duplicates";

/// How many stems one tier's message names before summarising.
const MAX_NAMED: usize = 5;

/// One tier and what the scan established about it.
struct TierScan {
    /// Human label (`project agents`, `user skills`, …).
    label: &'static str,
    /// The directory scanned.
    dir: PathBuf,
    /// The colliding stems, or the reason none could be established.
    outcome: Result<Vec<DuplicateStem>, String>,
}

/// Probe every resolved asset tier for a name claimed by two entries.
///
/// Why: see the module doc — the one collision shape no tier-vs-tier probe can
/// reach.
/// What: scans the canonical agent and skill deploy dirs plus, when
/// `project_dir` is in scope, that project's own `.claude/agents` and
/// `.claude/skills`, deduplicating any path two of those resolve to.
/// [`CheckStatus::Warn`] when any tier holds a duplicate — it is a real,
/// identified ambiguity, but it breaks no session on its own, and the operator
/// is the only party who can say which entry to keep.
/// [`CheckStatus::Unknown`] when nothing collided but a tier could not be
/// listed: that is a question left open, not a clean bill of health (ADR-0045).
/// Otherwise [`CheckStatus::Ok`]. Removes and renames nothing.
/// Test: `a_file_beside_a_directory_warns`, `a_clean_machine_is_ok`,
/// `an_unscannable_tier_is_unknown_not_ok`,
/// `a_real_duplicate_outranks_an_unscannable_tier`,
/// `a_project_custom_asset_does_not_fire`, `the_probe_removes_nothing`.
pub(super) fn check_asset_duplicates(
    paths: &FrameworkPaths,
    project_dir: Option<&Path>,
) -> DoctorCheck {
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    let mut scans: Vec<TierScan> = Vec::new();
    for (label, dir) in [
        ("user agents", Some(paths.agent_deploy_dir())),
        ("user skills", Some(paths.skill_deploy_dir())),
        (
            "project agents",
            project_dir.map(|p| p.join(".claude").join("agents")),
        ),
        (
            "project skills",
            project_dir.map(|p| p.join(".claude").join("skills")),
        ),
    ] {
        let Some(dir) = dir else { continue };
        if !seen.insert(dir.clone()) {
            continue;
        }
        let outcome = scan_duplicate_stems(&dir).map_err(|e| e.to_string());
        scans.push(TierScan {
            label,
            dir,
            outcome,
        });
    }

    verdict(&scans)
}

/// Pure verdict over the scanned tiers.
///
/// Why: split out so every branch is directly testable without provisioning a
/// machine, and so the severity rule is stated once.
/// What: see [`check_asset_duplicates`]. A confirmed duplicate outranks an
/// unscannable tier — a found ambiguity is the more actionable finding, and the
/// message still names the tier it could not read.
/// Test: `a_file_beside_a_directory_warns`, `a_clean_machine_is_ok`,
/// `an_unscannable_tier_is_unknown_not_ok`,
/// `a_real_duplicate_outranks_an_unscannable_tier`.
fn verdict(scans: &[TierScan]) -> DoctorCheck {
    let unscannable: Vec<String> = scans
        .iter()
        .filter_map(|s| s.outcome.as_ref().err())
        .cloned()
        .collect();
    let hits: Vec<&TierScan> = scans
        .iter()
        .filter(|s| s.outcome.as_ref().is_ok_and(|found| !found.is_empty()))
        .collect();

    if hits.is_empty() {
        if !unscannable.is_empty() {
            return DoctorCheck::new(
                CHECK_NAME,
                CheckStatus::Unknown,
                format!(
                    "an asset tier could not be listed, so whether one name is claimed twice \
                     inside it is UNDETERMINED: {}. Make the directory readable and re-run.",
                    unscannable.join("; ")
                ),
            );
        }
        let scanned: Vec<String> = scans
            .iter()
            .filter(|s| s.outcome.is_ok())
            .map(|s| s.dir.display().to_string())
            .collect();
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Ok,
            format!(
                "no agent or skill name is claimed by two entries in the same tier (scanned: {})",
                if scanned.is_empty() {
                    "none".to_owned()
                } else {
                    scanned.join(", ")
                }
            ),
        );
    }

    let mut detail: Vec<String> = hits
        .iter()
        .filter_map(|s| {
            let found = s.outcome.as_ref().ok()?;
            Some(format!(
                "{} tier {} — {} ({})",
                s.label,
                s.dir.display(),
                found.len(),
                name_duplicates(found, MAX_NAMED)
            ))
        })
        .collect();
    detail.extend(unscannable.iter().map(|u| format!("{u} (UNDETERMINED)")));

    DoctorCheck::new(
        CHECK_NAME,
        CheckStatus::Warn,
        format!(
            "one asset name is claimed by two entries in the same tier: {}. Only one of them \
             ever loads — which one is loader order, and on a case-insensitive filesystem the \
             two names are ONE file — so the copy you edit may not be the copy that runs. tm \
             does not repair this: it cannot know which entry you meant to keep, and both may \
             be yours. Delete or rename one.",
            detail.join("; ")
        ),
    )
}

#[cfg(test)]
#[path = "doctor_asset_duplicates_tests.rs"]
mod tests;
