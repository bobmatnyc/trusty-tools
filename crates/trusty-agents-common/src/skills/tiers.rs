//! Multi-tier skill precedence — project-custom > user-custom > bundled.
//!
//! Why: skills are deployed per-project into `<project>/.claude/skills/`, and
//! historically only ONE source fed that deploy (the bundled framework skills,
//! see [`crate::skills::deployer`]). Two custom tiers were missing as
//! first-class, documented concepts (issue #2816). The `project-custom` tier is
//! a skill a user hand-places directly in `<project>/.claude/skills/<name>/`;
//! Claude Code already discovers it and the deployer's ownership manifest
//! already preserves it (an unmanaged file is never overwritten), but nothing
//! NAMED it a tier or let it deliberately shadow a same-named bundled skill. The
//! `user-custom` tier is a skill authored once in `~/.trusty-mpm/skills/` and
//! deployed into EVERY project, ranking above the shipped portfolio. This
//! mirrors the user-level agent source (`~/.trusty-mpm/agents/`) and the
//! agent-precedence work tracked in #387 so the two land consistently. Moved
//! to trusty-agents-common (#2892, #2818) from `trusty-mpm::core::skill_tiers`
//! so a second harness (trusty-code) can reuse the same precedence resolver;
//! `trusty-mpm` re-exports every item here from `core::skill_tiers` for source
//! compatibility.
//!
//! What: [`plan_skill_tiers`] is the PURE precedence resolver — given the skill
//! stems present in each tier it decides which stems each *deployable* tier
//! (user, bundled) should actually write and records the shadow relationships
//! for logging. [`deploy_all_skill_tiers`] is the thin IO orchestrator that
//! enumerates each tier's stems, runs the planner, and drives the existing
//! [`crate::skills::deployer::deploy_skills_filtered`] once per deployable
//! tier so the manifest-based ownership/atomic-write machinery is reused
//! unchanged.
//! Test: see `plan_disjoint_all_deploy` and the sibling `plan_*` unit tests for
//! the pure precedence table, and `deploy_all_user_overrides_bundled` and its
//! `deploy_all_*` siblings for the end-to-end tier merge over real directories.

use std::collections::BTreeSet;
use std::path::Path;

use crate::agents::manifest::Result;
use crate::skills::deployer::{DeployStats, deploy_skills_filtered};
use crate::skills::manifest::SkillManifest;

/// The three skill source tiers, ordered highest precedence first.
///
/// Why: shadow logging and the planner need a stable, comparable identity for
/// each tier so a collision can name its winner and loser.
/// What: `Project` (hand-placed in the project's `.claude/skills/`) outranks
/// `User` (`~/.trusty-mpm/skills/`), which outranks `Bundled` (shipped).
/// Test: `plan_project_shadows_user_and_bundled`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillTier {
    /// Hand-placed in `<project>/.claude/skills/` — highest precedence.
    Project,
    /// Authored in `~/.trusty-mpm/skills/` — deployed into every project.
    User,
    /// Shipped with trusty-mpm (bundled framework skills) — lowest precedence.
    Bundled,
}

impl SkillTier {
    /// Human-readable label for log lines.
    ///
    /// Why: shadow warnings identify tiers by name to operators.
    /// What: a short lowercase slug per variant.
    /// Test: exercised via `deploy_all_skill_tiers` log assertions indirectly.
    pub fn label(self) -> &'static str {
        match self {
            SkillTier::Project => "project-custom",
            SkillTier::User => "user-custom",
            SkillTier::Bundled => "bundled",
        }
    }
}

/// One name-collision record: `winner` shadows `loser` for skill `stem`.
///
/// Why: callers log every collision so an operator understands why a
/// lower-precedence skill of a given name was not deployed.
/// What: the colliding stem plus the winning and losing tiers.
/// Test: `plan_user_shadows_bundled`, `plan_project_shadows_user_and_bundled`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shadow {
    /// The skill name (stem) that appears in more than one tier.
    pub stem: String,
    /// The tier whose copy is deployed.
    pub winner: SkillTier,
    /// The tier whose copy is suppressed.
    pub loser: SkillTier,
}

/// The deploy decision produced by [`plan_skill_tiers`].
///
/// Why: the orchestrator needs, per deployable tier, exactly which stems to
/// write (so no two tiers fight over the same target and each skill is written
/// at most once), plus the shadow records for logging.
/// What: the bundled and user stems that survive precedence, and every
/// collision as a [`Shadow`]. Project-custom stems are never "deployed" — they
/// already exist on disk and are preserved by the manifest ownership model — so
/// they appear only as shadow winners.
/// Test: every `plan_*` test asserts on these fields.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TierPlan {
    /// Bundled stems to deploy (not shadowed by a higher tier).
    pub bundled_deploy: BTreeSet<String>,
    /// User-custom stems to deploy (not shadowed by the project tier).
    pub user_deploy: BTreeSet<String>,
    /// Every name collision, deterministically ordered.
    pub shadowed: Vec<Shadow>,
}

/// Resolve a single skill stem's tier via the 3-tier precedence, or `None` if
/// absent from every tier.
///
/// Why (DOC-42, issue #2889): agent-bundled skills need the SAME precedence
/// rule [`plan_skill_tiers`] encodes, but for one declared skill name at a
/// time — co-deployment logging, `tm doctor`'s dangling-reference check, and
/// `tm agent show`'s per-skill tier column all ask "which tier (if any) does
/// THIS ONE stem resolve to?" rather than planning a full multi-stem deploy.
/// What: `Project` if `project` contains `stem`, else `User` if `user`
/// contains it, else `Bundled` if `bundled` contains it, else `None` (the
/// skill does not exist in any tier — a dangling reference).
/// Test: `resolve_tier_project_wins`, `resolve_tier_user_wins_over_bundled`,
/// `resolve_tier_bundled_only`, `resolve_tier_absent_is_none`.
pub fn resolve_skill_tier(
    stem: &str,
    project: &BTreeSet<String>,
    user: &BTreeSet<String>,
    bundled: &BTreeSet<String>,
) -> Option<SkillTier> {
    if project.contains(stem) {
        Some(SkillTier::Project)
    } else if user.contains(stem) {
        Some(SkillTier::User)
    } else if bundled.contains(stem) {
        Some(SkillTier::Bundled)
    } else {
        None
    }
}

/// Resolve tier precedence for a set of skill stems.
///
/// Why: precedence must be decided in ONE pure, exhaustively-tested place so
/// the IO orchestrator stays trivial and the rule (`project > user > bundled`)
/// cannot drift between call sites.
/// What: for each user stem, it is suppressed iff the project tier also has it
/// (project shadows user); otherwise it deploys. For each bundled stem, it is
/// suppressed iff EITHER higher tier has it (the nearer tier — project, else
/// user — is recorded as the winner); otherwise it deploys. Collisions are
/// returned sorted by `(stem, winner-precedence)` for stable output.
/// Test: `plan_disjoint_all_deploy`, `plan_user_shadows_bundled`,
/// `plan_project_shadows_user_and_bundled`, `plan_project_shadows_user_only`.
pub fn plan_skill_tiers(
    project: &BTreeSet<String>,
    user: &BTreeSet<String>,
    bundled: &BTreeSet<String>,
) -> TierPlan {
    let mut plan = TierPlan::default();

    for stem in user {
        if project.contains(stem) {
            plan.shadowed.push(Shadow {
                stem: stem.clone(),
                winner: SkillTier::Project,
                loser: SkillTier::User,
            });
        } else {
            plan.user_deploy.insert(stem.clone());
        }
    }

    for stem in bundled {
        if project.contains(stem) {
            plan.shadowed.push(Shadow {
                stem: stem.clone(),
                winner: SkillTier::Project,
                loser: SkillTier::Bundled,
            });
        } else if user.contains(stem) {
            plan.shadowed.push(Shadow {
                stem: stem.clone(),
                winner: SkillTier::User,
                loser: SkillTier::Bundled,
            });
        } else {
            plan.bundled_deploy.insert(stem.clone());
        }
    }

    // Deterministic order: by stem, then by loser tier (bundled after user for
    // the same stem — e.g. a project skill shadowing both user and bundled).
    plan.shadowed.sort_by(|a, b| {
        a.stem
            .cmp(&b.stem)
            .then((a.loser as u8).cmp(&(b.loser as u8)))
    });
    plan
}

/// List the deployable skill stems in a source directory.
///
/// Why: the planner works over stem sets; the orchestrator must translate a
/// source directory into that set using the EXACT SAME file filter and stem
/// derivation `deploy_skills_filtered` uses. A second, independently-written
/// rule here previously used a repeated `trim_end_matches(".md")` strip
/// against the deployer's single `strip_suffix(".md")` (PR #2818 review,
/// MEDIUM): for a pathological `foo.md.md` source file the planner would
/// derive stem `foo` while the deployer's own `select` predicate — driven by
/// [`crate::skills::deployer::skill_stem`] — expects `foo.md`, so the
/// planner would greenlight a stem the deployer then silently rejected and the
/// skill never deployed. Reusing the deployer's own [`is_skill_file`] and
/// [`skill_stem`] helpers makes that drift structurally impossible.
/// What: returns the stem (per [`skill_stem`]) of every file directly under
/// `dir` that [`is_skill_file`] accepts. A missing or non-directory path
/// yields an empty set (an absent tier is not an error). Errors reading an
/// existing directory propagate.
/// Test: `list_source_stems_reads_md_files`, `list_source_stems_missing_empty`.
pub fn list_source_stems(dir: &Path) -> Result<BTreeSet<String>> {
    use crate::skills::deployer::{is_skill_file, skill_stem};

    let mut stems = BTreeSet::new();
    if !dir.is_dir() {
        return Ok(stems);
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if is_skill_file(&name) && entry.file_type()?.is_file() {
            stems.insert(skill_stem(&name).to_string());
        }
    }
    Ok(stems)
}

/// List the project-custom skill stems already present in the deploy target.
///
/// Why: a project-custom skill is one the user hand-placed in the project's
/// `.claude/skills/` — it is on disk but absent from the deploy manifest. The
/// planner needs those names to let them deliberately shadow same-named user or
/// bundled skills.
/// What: scans `<dest>/<stem>/SKILL.md` directories and returns each `<stem>`
/// that the deploy manifest does NOT manage. A missing target yields an empty
/// set. Errors reading an existing directory propagate.
/// Test: `project_custom_stems_finds_unmanaged`.
pub fn list_project_custom_stems(dest: &Path) -> Result<BTreeSet<String>> {
    let mut stems = BTreeSet::new();
    if !dest.is_dir() {
        return Ok(stems);
    }
    let manifest = SkillManifest::load(dest);
    for entry in std::fs::read_dir(dest)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if entry.path().join("SKILL.md").is_file() && !manifest.is_managed(&name) {
            stems.insert(name);
        }
    }
    Ok(stems)
}

/// Result of a multi-tier deploy: the merged stats plus every shadow.
///
/// Why: callers want the same [`DeployStats`] shape they had with the single
/// source, plus the collision records to surface in logs / diagnostics.
/// What: the union of both deploy runs' stats and the planner's shadow list.
/// Test: `deploy_all_user_overrides_bundled`, `deploy_all_preserves_project`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TierDeploy {
    /// Combined stats across the bundled and user deploy passes.
    pub stats: DeployStats,
    /// Name collisions resolved by precedence this run.
    pub shadowed: Vec<Shadow>,
}

/// Deploy the bundled and user-custom skill tiers into `dest`, honoring
/// precedence project-custom > user-custom > bundled.
///
/// Why: this is the single entry point session provisioning calls in place of
/// the old single-source deploy — it adds the user tier and makes project
/// precedence explicit while reusing the existing per-file ownership machinery.
/// What: builds each tier's stem set (bundled filtered by `select`; user-custom
/// deployed in full — an explicit user opt-in is not subject to the harness
/// manifest's bundled include/exclude), plans precedence, then runs
/// [`deploy_skills_filtered`] for the bundled-then-user survivors. Project-custom
/// files already on disk are never rewritten (the deployer skips unmanaged
/// targets). Deploying the two tiers over DISJOINT stem sets means each skill is
/// written at most once and the winner is never clobbered by a loser.
/// Test: `deploy_all_user_overrides_bundled`, `deploy_all_preserves_project`,
/// `deploy_all_no_user_tier_matches_bundled_only`.
pub fn deploy_all_skill_tiers(
    bundled_source: &Path,
    user_source: &Path,
    dest: &Path,
    select: impl Fn(&str) -> bool,
) -> Result<TierDeploy> {
    let bundled: BTreeSet<String> = list_source_stems(bundled_source)?
        .into_iter()
        .filter(|s| select(s))
        .collect();
    let user = list_source_stems(user_source)?;
    let project = list_project_custom_stems(dest)?;

    let plan = plan_skill_tiers(&project, &user, &bundled);

    for shadow in &plan.shadowed {
        tracing::info!(
            skill = %shadow.stem,
            winner = shadow.winner.label(),
            loser = shadow.loser.label(),
            "skill tier collision — deploying the higher-precedence copy"
        );
    }

    let mut deploy = TierDeploy {
        shadowed: plan.shadowed.clone(),
        ..TierDeploy::default()
    };

    // Bundled tier first, then the user tier — over disjoint stem sets.
    let bundled_stats = deploy_skills_filtered(bundled_source, dest, |name| {
        plan.bundled_deploy.contains(name)
    })?;
    merge_stats(&mut deploy.stats, bundled_stats);

    if user_source.is_dir() {
        let user_stats =
            deploy_skills_filtered(user_source, dest, |name| plan.user_deploy.contains(name))?;
        merge_stats(&mut deploy.stats, user_stats);
    }

    Ok(deploy)
}

/// Fold one deploy pass's stats into the running total.
///
/// Why: two deploy passes each return their own [`DeployStats`]; callers want a
/// single merged view.
/// What: appends each vector. The passes deploy disjoint stem sets, so no
/// de-duplication is required.
/// Test: covered via `deploy_all_*` end-to-end assertions.
fn merge_stats(into: &mut DeployStats, from: DeployStats) {
    into.deployed.extend(from.deployed);
    into.skipped.extend(from.skipped);
    into.unchanged.extend(from.unchanged);
}

#[cfg(test)]
#[path = "tiers_tests.rs"]
mod tests;
