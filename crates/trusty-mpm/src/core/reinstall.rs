//! Redeploy the bundled agents and skills to EVERY destination that serves
//! them — the engine behind `tm reinstall`.
//!
//! Why: asset deployment is two hops, and no command covered both across all
//! the destinations. Hop 1 materializes the compiled-in bundle into
//! `~/.trusty-mpm/framework/{agents,skills}`
//! ([`crate::core::agent_source`] #4840, [`crate::core::skill_source`] #1917).
//! Hop 2 copies that source into a deploy target — and there are several,
//! written by different commands: `tm install` writes the managed
//! `claude-config/agents` plus `~/.claude/skills`, a daemon-managed session
//! additionally writes `<project>/.claude/skills`, and the standalone driver
//! (`tm run` / `tm load` / `tm login`) writes `~/.trusty-mpm/claude-config/`
//! and never runs hop 1 at all (#4849). So an operator who redeploys through
//! one command still runs stale assets under another, with nothing saying so.
//!
//! What: [`reinstall_assets`] runs hop 1 once, then hop 2 into every target
//! [`reinstall_targets`] enumerates, and returns a per-destination
//! [`TierReport`] counting what was deployed, repaired, preserved, unchanged,
//! and failed. Ownership is the deployers' existing model, unchanged: a
//! framework-owned file that drifted is repaired (#4408), a user-customized
//! one is preserved. `force` is the explicit clobber — agents through
//! [`crate::core::agent_reset::reset_agents`], skills through
//! [`trusty_agents_common::skills::reconcile::force_adopt_bundled_skills`] —
//! and both back every file up before touching it.
//!
//! CONCURRENCY. Every write here goes through a deployer that takes the target
//! directory's exclusive cross-process ledger lock (#4409 for agents, #4881 for
//! skills), so a reinstall running while the daemon deploys for a session spawn
//! serialises against it rather than racing its manifest. What is NOT atomic is
//! the SEQUENCE: each target's agent deploy, skill deploy, and force pass take
//! and release the lock separately, so a concurrent writer can interleave
//! between them. That is a visibility window, not a lost update — every
//! individual load-modify-save cycle is protected, and `save_merging` folds in
//! anything a lock-bypassing writer published.
//!
//! Test: `reinstall_tests.rs`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::core::paths::FrameworkPaths;
use crate::core::skill_deploy_tiers::skill_deploy_tiers;

/// What one deploy did to one asset class at one destination.
///
/// Why: "a reinstall that silently no-ops on one of three tiers is the exact
/// failure this command exists to end" — so the outcome is counted per
/// destination and per asset class rather than summed into one number.
/// What: five disjoint counts. `repaired` is a SUBSET of `deployed` for agents
/// (the #4408 corruption branch) and is always 0 for skills, which carry no
/// framework-owned/user-owned origin distinction.
/// Test: `reinstall_reports_every_target`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AssetCounts {
    /// Files (re)written from the bundle this run.
    pub deployed: usize,
    /// Files rewritten because their managed copy had been corrupted (#4408).
    pub repaired: usize,
    /// Files left alone because the operator owns or edited them.
    pub preserved: usize,
    /// Files already byte-identical to the bundle.
    pub unchanged: usize,
    /// Files that could not be composed or written at all.
    pub failed: usize,
}

/// One deploy destination and what the reinstall did to it.
///
/// Why: the three destinations are literally different directories, and an
/// operator reading the output needs to see which one each count belongs to.
/// What: the label and paths from [`ReinstallTarget`], the per-class counts,
/// and any human-readable notes (backups written, deploys that failed
/// outright). `agents` is all-zero for a skills-only destination.
/// Test: `reinstall_reports_every_target`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TierReport {
    /// Short operator-facing label (`managed config`, `standalone`, …).
    pub label: String,
    /// The agents directory, when this destination serves agents.
    pub agents_dir: Option<PathBuf>,
    /// The skills directory.
    pub skills_dir: PathBuf,
    /// Agent outcome at this destination.
    pub agents: AssetCounts,
    /// Skill outcome at this destination.
    pub skills: AssetCounts,
    /// Bounded human-readable notes — warnings and hard errors.
    pub notes: Vec<String>,
    /// Whether any step at this destination failed outright (as opposed to a
    /// per-file failure, which rides `AssetCounts::failed`).
    pub errored: bool,
}

/// One deploy destination, before anything has been written to it.
///
/// Why: enumerating the destinations separately from acting on them is what
/// makes the target set assertable in a test without a real home directory.
/// What: a stable label, an optional agents directory (bundled agents deploy
/// into exactly two of the destinations; #4409 keeps them out of the rest), and
/// a skills directory.
/// Test: `targets_cover_the_standalone_tier`, `targets_deduplicate`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReinstallTarget {
    /// Short operator-facing label.
    pub label: String,
    /// Where composed agents go, when this destination serves them.
    pub agents_dir: Option<PathBuf>,
    /// Where deployed skills go.
    pub skills_dir: PathBuf,
}

/// The whole run: hop-1 outcome plus one report per destination.
///
/// Why: the operator needs to know whether the source was even refreshed
/// before reading the per-destination counts — a stale source deploying
/// cleanly everywhere is the #4840 failure with a green report.
/// What: whether each source directory was re-materialized from the running
/// binary, the per-destination reports, and any run-level notes.
/// Test: `reinstall_refreshes_the_source_first`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReinstallReport {
    /// The agent source was re-materialized from the compiled-in bundle.
    pub agent_source_refreshed: bool,
    /// The skill source was re-materialized from the compiled-in bundle.
    pub skill_source_refreshed: bool,
    /// One entry per deploy destination, in [`reinstall_targets`] order.
    pub tiers: Vec<TierReport>,
    /// Run-level notes (a hop-1 refresh that failed, for instance).
    pub notes: Vec<String>,
}

impl ReinstallReport {
    /// Did anything fail — a per-file compose/write failure, or a whole step?
    ///
    /// Why: the CLI exits non-zero on a partial reinstall rather than printing
    /// failures under a success banner.
    /// Test: `reinstall_reports_a_failed_destination`.
    pub fn has_failures(&self) -> bool {
        self.tiers
            .iter()
            .any(|t| t.errored || t.agents.failed > 0 || t.skills.failed > 0)
    }
}

/// Enumerate every destination a bundled asset must reach, deduplicated.
///
/// Why: the destinations are owned by three different commands and no single
/// list existed. Reusing [`skill_deploy_tiers`] for the skill half means this
/// command cannot cover a different set than `tm install`'s reporter and
/// `tm doctor` do; the standalone tier appended after it is precisely the one
/// none of them reach (#4849).
/// What: [`skill_deploy_tiers`]'s tiers (managed config, the operator's
/// `~/.claude/skills`, and the project when one is supplied) followed by the
/// standalone driver's `<standalone_config_dir>/skills`. Agents attach to the
/// two destinations that serve them: [`FrameworkPaths::agent_deploy_dir`] on
/// the managed-config tier and `<standalone_config_dir>/agents` on the
/// standalone tier — #4409 keeps framework-owned agents out of every other
/// directory, including the operator's `~/.claude/agents` and any project. A
/// destination whose skills directory duplicates an earlier one is dropped.
/// Pure path arithmetic — nothing is read or created.
/// Test: `targets_cover_the_standalone_tier`, `targets_deduplicate`,
/// `targets_attach_agents_to_two_destinations_only`.
pub fn reinstall_targets(
    paths: &FrameworkPaths,
    standalone_config_dir: &Path,
    project_dir: Option<&Path>,
) -> Vec<ReinstallTarget> {
    let agent_deploy = paths.agent_deploy_dir();
    let mut targets: Vec<ReinstallTarget> = skill_deploy_tiers(paths, project_dir)
        .into_iter()
        .map(|tier| ReinstallTarget {
            agents_dir: (tier.label == "managed config").then(|| agent_deploy.clone()),
            label: tier.label.to_string(),
            skills_dir: tier.dir,
        })
        .collect();

    let standalone = ReinstallTarget {
        label: "standalone".to_string(),
        agents_dir: Some(standalone_config_dir.join("agents")),
        skills_dir: standalone_config_dir.join("skills"),
    };
    if !targets
        .iter()
        .any(|t| t.skills_dir == standalone.skills_dir)
    {
        targets.push(standalone);
    }
    targets
}

/// Refresh the compiled-in bundle onto disk, then deploy it everywhere.
///
/// Why: this is the whole command. Hop 1 removes the dependency on someone
/// having run `tm install` under the current binary; hop 2 removes the
/// dependency on which launcher the operator happens to use next.
/// What: refreshes both source directories once (the sha256 stamp makes a
/// no-change run cheap), then for each [`reinstall_targets`] destination
/// deploys agents (when that destination serves them) and skills, folding each
/// outcome into a [`TierReport`]. With `force`, each destination additionally
/// gets the clobber pass described on this module before its deploy, so the
/// deploy itself does the writing and the counts stay honest. Backups land
/// under `backup_root`. Never panics and never propagates: a destination that
/// cannot be written is reported as errored and the remaining destinations are
/// still served — a reinstall that abandons two tiers because the third is
/// read-only is the failure this command exists to end.
/// Test: `reinstall_reports_every_target`, `reinstall_refreshes_the_source_first`,
/// `reinstall_creates_a_missing_destination`,
/// `reinstall_preserves_a_customized_agent_without_force`,
/// `reinstall_replaces_a_customized_agent_with_force`,
/// `reinstall_repairs_a_corrupt_agent_without_force`,
/// `reinstall_preserves_a_customized_skill_without_force`,
/// `reinstall_replaces_a_customized_skill_with_force`.
pub fn reinstall_assets(
    paths: &FrameworkPaths,
    standalone_config_dir: &Path,
    project_dir: Option<&Path>,
    force: bool,
    backup_root: &Path,
) -> ReinstallReport {
    let mut report = ReinstallReport::default();

    match crate::core::skill_source::ensure_skill_source_fresh(paths) {
        Ok(refreshed) => report.skill_source_refreshed = refreshed,
        Err(err) => report.notes.push(format!(
            "warning: could not refresh the bundled skill source at {} ({err}) — \
             continuing with whatever is already on disk",
            paths.skills.display()
        )),
    }

    let agent_source = paths.agent_source_dir();
    // A populated `agents/agents` git submodule is authoritative on its own;
    // materializing the compiled-in bundle over it would be destructive. Same
    // guard `agent_source::autodeploy_agents_for` applies.
    if agent_source == paths.agents || !crate::core::agent_source::has_agent_markdown(&agent_source)
    {
        match crate::core::agent_source::ensure_agent_source_fresh(&paths.agents) {
            Ok(refreshed) => report.agent_source_refreshed = refreshed,
            Err(err) => report.notes.push(format!(
                "warning: could not refresh the bundled agent source at {} ({err}) — \
                 continuing with whatever is already on disk",
                paths.agents.display()
            )),
        }
    }
    // Re-resolve: the refresh above may have created the framework source dir.
    let agent_source = paths.agent_source_dir();
    let bundled_stems = bundled_skill_stems(paths);

    for target in reinstall_targets(paths, standalone_config_dir, project_dir) {
        let mut tier = TierReport {
            label: target.label,
            agents_dir: target.agents_dir.clone(),
            skills_dir: target.skills_dir.clone(),
            ..TierReport::default()
        };
        if let Some(dir) = &target.agents_dir {
            deploy_agents_into(&agent_source, dir, force, &mut tier);
        }
        deploy_skills_into(
            paths,
            &target.skills_dir,
            &bundled_stems,
            force,
            backup_root,
            &mut tier,
        );
        report.tiers.push(tier);
    }

    report
}

/// The set of skill names this binary ships, for the force pass's roster test.
///
/// Why: `force_adopt_bundled_skills` uses the bundled roster as its ONLY
/// admission test — a skill whose name matches nothing bundled is the
/// operator's own and is never touched, however `--force` is spelled.
/// What: the source stems under the resolved skill source directory; an
/// unreadable directory yields an empty set, which the reconcile pass correctly
/// treats as "cannot classify by name" and therefore touches nothing.
/// Test: `force_leaves_an_operator_skill_alone`.
fn bundled_skill_stems(paths: &FrameworkPaths) -> BTreeSet<String> {
    crate::core::skill_tiers::list_source_stems(&paths.skill_source_dir()).unwrap_or_default()
}

/// Deploy composed agents into one destination, optionally clobbering first.
///
/// Why: split out so [`reinstall_assets`] reads as "for each destination, do
/// the two asset classes" and each class's error handling lives in one place.
/// What: with `force`, runs [`crate::core::agent_reset::reset_agents`] first —
/// it backs up every diverged file before recomposing it — then always runs the
/// ordinary [`crate::core::agent_deployer::deploy_agents`], whose result
/// supplies the counts. A failure in either step marks the destination errored
/// and leaves the rest of the run going.
/// Test: `reinstall_preserves_a_customized_agent_without_force`,
/// `reinstall_replaces_a_customized_agent_with_force`,
/// `reinstall_reports_a_failed_destination`.
fn deploy_agents_into(source: &Path, dest: &Path, force: bool, tier: &mut TierReport) {
    if force {
        // `reset_agents` backs a diverged file up in place as a
        // `<name>.md.bak-<nanos>` sibling, so it needs no `backup_root`.
        match crate::core::agent_reset::reset_agents(source, dest, None) {
            Ok(reset) if !reset.recomposed.is_empty() => tier.notes.push(format!(
                "forced {} agent file(s) back to the bundled composition ({} backed up in place)",
                reset.recomposed.len(),
                reset.backed_up.len()
            )),
            Ok(_) => {}
            Err(err) => {
                tier.errored = true;
                tier.notes.push(format!(
                    "agent force-reset failed at {}: {err}",
                    dest.display()
                ));
            }
        }
    }

    match crate::core::agent_deployer::deploy_agents(source, dest) {
        Ok(result) => {
            tier.agents = AssetCounts {
                deployed: result.deployed.len(),
                repaired: result.repaired.len(),
                preserved: result.skipped.len(),
                // An adopted file is byte-identical to the fresh composition —
                // registered into the manifest, never rewritten.
                unchanged: result.unchanged.len() + result.adopted.len(),
                failed: result.failed.len(),
            };
            tier.notes.extend(preserved_note(
                "agent",
                &result.skipped,
                &crate::core::agent_source::preview(&result.skipped, 5),
            ));
            if !result.failed.is_empty() {
                tier.notes.push(format!(
                    "{} agent(s) failed to compose and did NOT deploy at all: {}.",
                    result.failed.len(),
                    crate::core::agent_source::preview(&result.failed, 3)
                ));
            }
        }
        Err(err) => {
            tier.errored = true;
            tier.notes
                .push(format!("agent deploy failed at {}: {err}", dest.display()));
        }
    }
}

/// One bounded line naming what a destination preserved, or nothing.
///
/// Why: the preserved set can be dozens wide, and one line per file would bury
/// the counts this command exists to make legible — the same count-plus-preview
/// policy `agents::deployer` settled on in #2504. The pointer names
/// `tm reinstall --force` rather than `tm install --reset-agents` because that
/// is the flag on the command the operator just ran.
/// What: `None` when nothing was preserved; otherwise a count, a five-entry
/// preview, and the force pointer. Pure.
/// Test: `preserved_note_is_empty_when_nothing_was_preserved`,
/// `preserved_note_names_the_force_flag`.
fn preserved_note(kind: &str, preserved: &[String], preview: &str) -> Option<String> {
    if preserved.is_empty() {
        return None;
    }
    Some(format!(
        "{} {kind} file(s) preserved — you own or edited them, so the bundled version was \
         NOT written: {preview}. Run `tm reinstall --force` to overwrite them (each is \
         backed up first).",
        preserved.len()
    ))
}

/// Deploy skills into one destination, optionally clobbering first.
///
/// Why: the mirror of [`deploy_agents_into`] for the skill half, whose force
/// path differs — skills carry no framework-owned/user-owned origin, so the
/// clobber is a backed-up manifest re-stamp that hands the actual rewrite to
/// the deploy immediately after it.
/// What: with `force`, runs
/// [`trusty_agents_common::skills::reconcile::force_adopt_bundled_skills`],
/// then always runs [`crate::core::skill_tiers::deploy_all_skill_tiers`] over
/// the bundled and user tiers. The tier orchestrator's counts supply the
/// report.
/// Test: `reinstall_preserves_a_customized_skill_without_force`,
/// `reinstall_replaces_a_customized_skill_with_force`,
/// `reinstall_creates_a_missing_destination`.
fn deploy_skills_into(
    paths: &FrameworkPaths,
    dest: &Path,
    bundled_stems: &BTreeSet<String>,
    force: bool,
    backup_root: &Path,
    tier: &mut TierReport,
) {
    if force {
        match trusty_agents_common::skills::reconcile::force_adopt_bundled_skills(
            dest,
            bundled_stems,
            backup_root,
        ) {
            Ok(adopted) if !adopted.is_empty() => tier.notes.push(format!(
                "forced {} skill(s) back under management at {} (originals backed up under {})",
                adopted.len(),
                dest.display(),
                backup_root.display()
            )),
            Ok(_) => {}
            Err(err) => {
                tier.errored = true;
                tier.notes.push(format!(
                    "skill force-adopt failed at {}: {err}",
                    dest.display()
                ));
            }
        }
    }

    match crate::core::skill_tiers::deploy_all_skill_tiers(
        &paths.skill_source_dir(),
        &paths.user_skill_source_dir(),
        dest,
        |_| true,
    ) {
        Ok(deploy) => {
            tier.skills = AssetCounts {
                deployed: deploy.stats.deployed.len(),
                repaired: 0,
                preserved: deploy.stats.skipped.len(),
                unchanged: deploy.stats.unchanged.len(),
                failed: 0,
            };
            tier.notes.extend(preserved_note(
                "skill",
                &deploy.stats.skipped,
                &crate::core::agent_source::preview(&deploy.stats.skipped, 5),
            ));
        }
        Err(err) => {
            tier.errored = true;
            tier.notes
                .push(format!("skill deploy failed at {}: {err}", dest.display()));
        }
    }
}

#[cfg(test)]
#[path = "reinstall_tests.rs"]
mod tests;
