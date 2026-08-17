//! `tm install`'s unmanaged-skill reporter and `--reconcile-skills` path (#4605).
//!
//! Why: a bundled skill absent from a deploy target's manifest is classified
//! project-custom by the tier planner and dropped from `bundled_deploy` before
//! the deployer runs, so it appears in `tm install`'s output as neither
//! deployed, skipped, NOR unchanged — it simply is not mentioned. Measured
//! 2026-08-01: the deploy step printed one line
//! (`= tm-slack-canvas-delivery (unchanged)`) while two `tm-workflow/SKILL.md`
//! copies kept serving text PR #4583 had removed. Silence is the half of the
//! defect an operator can actually act on, so `tm install` now says so, and
//! offers one explicit command that fixes it.
//!
//! What: [`unmanaged_report_lines`] renders one `!` line per unmanaged bundled
//! skill per tier, in `tm install`'s existing glyph vocabulary, and
//! [`reconcile_skills`] is the `--reconcile-skills` handler — it prints the
//! full per-file plan, adopts each skill into its tier's manifest after
//! copying it under a timestamped backup root, then re-runs the ordinary
//! deploy so the adopted skills pick up current bundled content.
//!
//! CONTRACT — a skill whose stem names nothing bundled is never listed and
//! never touched. Scope is decided once, by
//! `trusty_agents_common::skills::unmanaged`, and both the reporter and the
//! reconcile consume that one answer.
//!
//! Test: `install_skills_tests.rs`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use trusty_mpm::core::paths::FrameworkPaths;
use trusty_mpm::core::skill_deploy_tiers::{SkillDeployTier, skill_deploy_tiers};
use trusty_mpm::core::skill_reconcile::adopt_unmanaged_bundled_skills;
use trusty_mpm::core::skill_tiers::{deploy_all_skill_tiers, list_source_stems};
use trusty_mpm::core::skill_unmanaged::unmanaged_bundled_skills;

/// Directory-name prefix for a reconcile run's backups.
///
/// Why: matches the `backup-doctor-remediation-<date>` directories this
/// repository's own remediations already leave under `~/.trusty-mpm/`, so an
/// operator finds backups where they already look for them rather than
/// learning a second convention.
/// What: `backup-reconcile-skills-`; [`backup_root`] appends a UTC timestamp.
/// Test: `backup_root_is_timestamped_under_the_framework_root`.
pub(crate) const RECONCILE_BACKUP_PREFIX: &str = "backup-reconcile-skills-";

/// Where one reconcile run writes its pre-adoption copies.
///
/// Why: one root per run keeps a second reconcile from overwriting the first
/// run's backups, and keeps the whole run recoverable as a unit.
/// What: `<framework root>/backup-reconcile-skills-<YYYYMMDDHHMMSS>` in UTC.
/// Test: `backup_root_is_timestamped_under_the_framework_root`.
pub(crate) fn backup_root(paths: &FrameworkPaths) -> PathBuf {
    let stamp = chrono::Utc::now().format("%Y%m%d%H%M%S");
    paths.root.join(format!("{RECONCILE_BACKUP_PREFIX}{stamp}"))
}

/// The bundled skill roster and every deploy tier to check it against.
///
/// Why: the reporter and the reconcile must agree exactly on scope; resolving
/// it once means they cannot disagree.
/// What: `(bundled stems, tiers)` per `paths.skill_source_dir()` and
/// [`skill_deploy_tiers`]. An empty roster means "cannot classify" — both
/// callers then do nothing rather than treating every deployed skill as
/// unowned.
/// Test: exercised via `report_lines_name_the_tier_and_the_skill`.
fn scope(
    paths: &FrameworkPaths,
    project_dir: Option<&Path>,
) -> (BTreeSet<String>, Vec<SkillDeployTier>) {
    let bundled = list_source_stems(&paths.skill_source_dir()).unwrap_or_default();
    (bundled, skill_deploy_tiers(paths, project_dir))
}

/// Render one status line per unmanaged bundled skill, per tier.
///
/// Why: `tm install`'s deploy summary is the operator's only window onto what
/// the deploy did, and it was structurally unable to mention these files. The
/// line names the tier, so "unmanaged at `~/.claude/skills`" is not confused
/// with "unmanaged at `$CLAUDE_CONFIG_DIR/skills`" — they need separate fixes.
/// What: `! <stem> (untracked at <tier> — not managed, not refreshed; see
/// `tm install --reconcile-skills`)` per finding, ordered tier-then-stem.
/// Empty when nothing is unmanaged, so a clean install prints nothing extra.
/// Test: `report_lines_name_the_tier_and_the_skill`,
/// `report_lines_are_empty_when_every_skill_is_tracked`,
/// `report_lines_ignore_an_operator_skill`.
pub(crate) fn unmanaged_report_lines(
    paths: &FrameworkPaths,
    project_dir: Option<&Path>,
) -> Vec<String> {
    let (bundled, tiers) = scope(paths, project_dir);
    if bundled.is_empty() {
        return Vec::new();
    }
    let mut lines = Vec::new();
    for tier in tiers {
        // #5626: a tier whose ledger could not be read gets a line saying so.
        // On the empty default it produced a line per MANAGED skill instead,
        // telling the operator to reconcile files that were already tracked.
        let found = match unmanaged_bundled_skills(&tier.dir, &bundled) {
            Ok(found) => found,
            Err(e) => {
                lines.push(format!(
                    "? {} \u{2014} ownership ledger unreadable, so nothing here can be \
                     classified: {e}",
                    tier.dir.display()
                ));
                continue;
            }
        };
        for skill in found {
            lines.push(format!(
                "! {} (untracked at {} \u{2014} not managed, not refreshed; \
                 see `tm install --reconcile-skills`)",
                skill.stem,
                tier.dir.display()
            ));
        }
    }
    lines
}

/// `tm install --reconcile-skills` — adopt and refresh unreachable bundled skills.
///
/// Why: the only path that makes an untracked bundled skill deployable again.
/// It is opt-in because adoption writes over a file tm cannot prove it wrote;
/// it backs up every file first because the roster match, while necessary, is
/// not proof the operator did not customize it.
/// What: prints the full plan — every tier, skill, and FILE it will touch —
/// then for each tier adopts the in-scope skills into that tier's manifest
/// (copying each file under [`backup_root`] first) and re-runs
/// [`deploy_all_skill_tiers`] so the adopted skills receive current bundled
/// content. Prints the resulting deploy lines per tier. A tier with nothing in
/// scope is neither adopted nor re-deployed. Errors from one tier abort the
/// run rather than continuing with a partially-reconciled machine.
/// Test: `install_skills_tests.rs` covers the pure helpers; the end-to-end
/// adoption+refresh is pinned by
/// `trusty_agents_common::skills::reconcile::tests::adopt_then_deploy_refreshes_a_stale_skill`.
pub(crate) fn reconcile_skills(
    paths: &FrameworkPaths,
    project_dir: Option<&Path>,
) -> anyhow::Result<()> {
    let (bundled, tiers) = scope(paths, project_dir);
    if bundled.is_empty() {
        println!(
            "Reconcile skipped: no bundled skill source at {} — run `tm install` first.",
            paths.skill_source_dir().display()
        );
        return Ok(());
    }

    let backups = backup_root(paths);
    let mut planned = 0usize;
    println!(
        "Reconciling unmanaged bundled skills (backups: {})",
        backups.display()
    );
    for tier in &tiers {
        // #5626: reconcile WRITES ownership records, so an unreadable ledger
        // aborts the run rather than adopting every managed skill afresh.
        let found = unmanaged_bundled_skills(&tier.dir, &bundled)?;
        if found.is_empty() {
            println!(
                "  {} ({}): nothing unmanaged",
                tier.label,
                tier.dir.display()
            );
            continue;
        }
        println!("  {} ({}):", tier.label, tier.dir.display());
        for skill in &found {
            planned += 1;
            for file in &skill.files {
                println!("    will adopt + refresh {}", file.display());
            }
        }
    }
    if planned == 0 {
        println!("  nothing to reconcile.");
        return Ok(());
    }

    for tier in &tiers {
        let adopted = adopt_unmanaged_bundled_skills(&tier.dir, &bundled, &backups)?;
        if adopted.is_empty() {
            continue;
        }
        for skill in &adopted {
            println!(
                "  \u{2713} adopted {} at {} (backup: {})",
                skill.stem,
                tier.dir.display(),
                skill.backup_dir.display()
            );
        }
        let stats = deploy_all_skill_tiers(
            &paths.skill_source_dir(),
            &paths.user_skill_source_dir(),
            &tier.dir,
            |_| true,
        )?
        .stats;
        for skill in &adopted {
            if stats.deployed.contains(&skill.stem) {
                println!("  \u{2713} {} refreshed from bundled source", skill.stem);
            } else {
                println!("  = {} (already matched bundled source)", skill.stem);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "install_skills_tests.rs"]
mod tests;
