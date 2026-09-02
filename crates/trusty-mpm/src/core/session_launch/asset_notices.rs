//! One operator-visible line per asset kind at session launch (#6649).
//!
//! Why: every asset-hygiene finding tm already computes at launch went to
//! `tracing` and nowhere else. The #4448 quarantine MOVES files in the
//! operator's working tree and said so at `warn` level, which a terminal
//! session does not show; the #6586 project-tier skill strays were computed
//! only by `tm doctor`, which nobody runs at launch; the #6649 same-stem
//! duplicate was computed nowhere at all. Owner ruling 2026-09-02: startup
//! checks agents and skills, and the operator sees the result.
//!
//! What: [`launch_asset_notices`] returns at most three lines — agents
//! quarantined, project-tier skill strays, and same-stem duplicates — each
//! naming its count and the names behind it. It returns an EMPTY vector when
//! all three are zero, because this runs on every launch and a clean project
//! must add nothing to the terminal. The caller stashes them on
//! [`super::PrepReport::asset_notices`], which every launch path prints.
//!
//! FAIL-OPEN IS A FINDING, NOT SILENCE (#6649). A tier that cannot be listed
//! and a roster that cannot be built both produce a line saying so. The
//! alternative — an empty result — is the #4605/#5626 shape: a check that
//! renders "nothing to report" for a question it never answered.
//!
//! It reports and stops. Nothing here removes, moves, or rewrites a file; the
//! quarantine it summarises already ran, and the strays and duplicates are the
//! operator's to resolve (`tm doctor --fix-skills`, `tm doctor --fix-agents`,
//! or a rename).
//!
//! Test: `asset_notices_tests.rs`.

use std::collections::BTreeSet;
use std::path::Path;

use trusty_agents_common::agents::quarantine_receipt::QuarantineReport;

use crate::core::asset_duplicates::{DuplicateStem, name_duplicates, scan_duplicate_stems};
use crate::core::paths::FrameworkPaths;
use crate::core::project_tier_agent_strays::project_agent_tier;
use crate::core::skill_deploy_tiers::project_skill_tier;
use crate::core::skill_tiers::list_source_stems;
use crate::core::skill_unmanaged::bundled_skill_dirs;

/// How many names one line lists before summarising the remainder.
const MAX_NAMED: usize = 5;

/// The launch lines for one project, or none when everything is clean.
///
/// Why: see the module doc — one call site, one contract, so a fourth asset
/// kind is added here rather than as a fourth thing a launch path must
/// remember to print.
/// What: the agent line (from `quarantine`, which the caller has already run),
/// the skill-stray line, and the duplicate line, in that order, omitting any
/// that has nothing to say. `quarantine` is `None` when the sweep itself
/// refused to run — the caller reports that through `roster_errors`, so this
/// adds no second line for it.
/// Test: `a_clean_project_produces_no_notice`,
/// `a_quarantined_agent_produces_one_line`,
/// `a_project_tier_skill_stray_produces_one_line`,
/// `a_same_stem_duplicate_produces_one_line`.
pub fn launch_asset_notices(
    fw: &FrameworkPaths,
    project_dir: &Path,
    quarantine: Option<&QuarantineReport>,
) -> Vec<String> {
    [
        quarantine.and_then(agent_notice),
        skill_notice(fw, project_dir),
        duplicate_notice(project_dir),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// The agents line: what the #4448 sweep moved this launch.
///
/// Why: the sweep changed the operator's working tree without being asked, so
/// the names go on the terminal, not only into a log nobody is tailing.
/// What: `None` when the sweep moved nothing and failed at nothing. Otherwise
/// the count, the names it moved, and the count it could not move.
/// Test: `a_quarantined_agent_produces_one_line`,
/// `a_clean_sweep_produces_no_agent_line`.
fn agent_notice(report: &QuarantineReport) -> Option<String> {
    if !report.wrote_anything() {
        return None;
    }
    let mut names: Vec<&str> = report.moved.iter().map(|m| m.name.as_str()).collect();
    names.sort_unstable();
    let shown = names.iter().take(MAX_NAMED).copied().collect::<Vec<_>>();
    let rest = names.len().saturating_sub(shown.len());
    let listed = if rest == 0 {
        shown.join(", ")
    } else {
        format!("{} (+{rest} more)", shown.join(", "))
    };
    Some(format!(
        "agents quarantined {} ({listed}), {} could not be moved — each has a backup and an \
         inert `.md.disabled` sibling under `.trusty-mpm/agent-quarantine/`",
        report.moved.len(),
        report.failed.len()
    ))
}

/// The skills line: bundled copies still sitting at the project tier.
///
/// Why: #6586 made bundled skills user-tier only, so a copy here is refreshed
/// by nothing and freezes at the text that shipped when it landed. The doctor
/// row says so; nobody runs doctor at launch.
/// What: `None` when the tier holds no bundled-named entry. A line naming the
/// count, the stems, and `tm doctor --fix-skills` otherwise. A tier that cannot
/// be listed, and an empty roster over a tier that HOLDS something, both report
/// the undetermined state rather than reading as clean — an absent tier and an
/// empty roster over an empty tier are genuinely clean and stay silent.
/// Test: `a_project_tier_skill_stray_produces_one_line`,
/// `an_unreadable_skill_tier_reports_the_failure`,
/// `an_empty_roster_over_a_populated_tier_reports_undetermined`.
fn skill_notice(fw: &FrameworkPaths, project_dir: &Path) -> Option<String> {
    let dir = project_skill_tier(project_dir);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries.flatten().count(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            return Some(format!(
                "skills stray UNDETERMINED — the project skill tier {} could not be listed: {e}",
                dir.display()
            ));
        }
    };

    let bundled: BTreeSet<String> = list_source_stems(&fw.skill_source_dir()).unwrap_or_default();
    if bundled.is_empty() {
        if entries == 0 {
            return None;
        }
        return Some(format!(
            "skills stray UNDETERMINED — no bundled skill roster could be built, so the {entries} \
             entr(y/ies) in {} could not be classified (run `tm install`)",
            dir.display()
        ));
    }

    let found: Vec<String> = bundled_skill_dirs(&dir, &bundled)
        .into_iter()
        .map(|skill| skill.stem)
        .collect();
    if found.is_empty() {
        return None;
    }
    let shown: Vec<&str> = found.iter().take(MAX_NAMED).map(String::as_str).collect();
    let rest = found.len().saturating_sub(shown.len());
    let listed = if rest == 0 {
        shown.join(", ")
    } else {
        format!("{} (+{rest} more)", shown.join(", "))
    };
    Some(format!(
        "skills stray {} ({listed}) at the project tier {} — bundled skills are user-tier only \
         since #6586, so nothing refreshes these; preview the removal with `tm doctor \
         --fix-skills`",
        found.len(),
        dir.display()
    ))
}

/// The duplicates line: one name claimed twice in one project tier.
///
/// Why: `qa.md` beside `qa/`, or `QA.md` beside `qa.md`, means only one of the
/// two ever loads and which one is loader order. tm does not repair it — it
/// cannot know which entry the operator meant — so the launch line is the whole
/// remedy path.
/// What: `None` when neither project tier holds a duplicate. One line naming
/// the total and the stems otherwise, with an unlistable tier reported rather
/// than passed over.
/// Test: `a_same_stem_duplicate_produces_one_line`,
/// `an_unreadable_agent_tier_reports_the_failure`.
fn duplicate_notice(project_dir: &Path) -> Option<String> {
    let mut found: Vec<DuplicateStem> = Vec::new();
    let mut undetermined: Vec<String> = Vec::new();
    for dir in [
        project_agent_tier(project_dir),
        project_skill_tier(project_dir),
    ] {
        match scan_duplicate_stems(&dir) {
            Ok(dups) => found.extend(dups),
            Err(e) => undetermined.push(e.to_string()),
        }
    }
    if found.is_empty() {
        if undetermined.is_empty() {
            return None;
        }
        return Some(format!(
            "duplicates UNDETERMINED — {}",
            undetermined.join("; ")
        ));
    }
    found.sort_by(|a, b| a.stem.cmp(&b.stem));
    let suffix = if undetermined.is_empty() {
        String::new()
    } else {
        format!("; UNDETERMINED: {}", undetermined.join("; "))
    };
    Some(format!(
        "duplicates {} ({}) — one name, two entries in the same tier, so only one of them \
         loads; delete or rename one (tm does not repair this){suffix}",
        found.len(),
        name_duplicates(&found, MAX_NAMED)
    ))
}

/// Log one prepared session's non-fatal findings, for a caller with no terminal.
///
/// Why: five spawn paths — the daemon provisioner, the in-project spawn, the
/// client connect, the standalone load, and `tm launch` — each wrote the same
/// two loops over `roster_errors` and `asset_notices`, and #6649 was about to
/// make it seven. One helper is what keeps a sixth path from surfacing only
/// half of them; it is also what kept `provisioner/workspace.rs` under the
/// 500-SLOC cap when the second loop landed.
/// What: one `error!` per roster error — those are provisioning gaps — and one
/// `warn!` per asset notice, since the launch itself succeeded and what is
/// reported is the state of the operator's working tree. `scope` names what the
/// findings belong to (a worktree path, a session id). Silent for a clean
/// report. The CLI paths print to stdout instead and do NOT call this.
/// Test: `findings_are_logged_by_severity`.
pub fn log_prep_findings(
    roster_errors: &[String],
    asset_notices: &[String],
    scope: impl std::fmt::Display,
) {
    for err in roster_errors {
        tracing::error!(
            "{scope}: roster provisioning gap (session still launches with its trusty-mpm \
             identity): {err}"
        );
    }
    for notice in asset_notices {
        tracing::warn!("{scope}: assets: {notice}");
    }
}

#[cfg(test)]
#[path = "asset_notices_tests.rs"]
mod tests;
