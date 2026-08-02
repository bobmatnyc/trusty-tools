//! `tm doctor --fix-skills` — the local repair action (issue #4604).
//!
//! Why: split out of `commands/misc.rs`, which the 500-SLOC production cap
//! would otherwise reject. The behaviour and its constraints live in
//! [`trusty_mpm::core::skill_repair`]; this file is CLI plumbing and printing.
//! What: [`fix_skills_locally`] — resolve the deploy tiers and the skill
//! reference, run the repair, print one line per outcome.
//! Test: `core::skill_repair`'s own tests cover every outcome.

/// `--fix-skills` action — redeploy drifted skills and VERIFY from disk.
///
/// Why (#4604): the `skill_staleness` check reports which deployed skills no
/// longer match this binary's bundled assets, but until now the only remedy was
/// `tm install`, which deliberately SKIPS a hand-edited (frozen) file — so a
/// frozen skill could stay stale forever with no way to repair it short of
/// hand-copying. This is that remedy, with the owner's three constraints
/// enforced in [`trusty_mpm::core::skill_repair`]: a frozen skill is never
/// silently overwritten, every overwrite is backed up first, and the fix
/// re-reads each file from disk rather than reporting success from its own
/// intent.
///
/// It never deletes anything and never touches worktrees — `tm doctor`'s
/// worktree checks are report-only and stay that way.
/// What: resolves the deploy tiers from [`FrameworkPaths::default`] and the
/// current directory, builds the skill reference from the running binary (or a
/// checked-out `agents/skills` submodule), runs the repair, and prints one line
/// per outcome plus a summary. Local-filesystem only — independent of where the
/// daemon that produced the report above happens to be reachable.
/// Test: `core::skill_repair`'s own tests cover every outcome; this function is
/// path resolution + printing.
pub(crate) fn fix_skills_locally(include_frozen: bool) {
    use trusty_mpm::core::paths::FrameworkPaths;
    use trusty_mpm::core::skill_drift::skill_reference;
    use trusty_mpm::core::skill_repair::{RepairAction, backup_root_for, repair_skills};

    let paths = FrameworkPaths::default();
    let project_dir = std::env::current_dir().ok();
    // NEVER the `~/.trusty-mpm/framework/skills` extraction cache — that is the
    // reference point #4604 proved untrustworthy.
    let source = paths.skill_source_dir();
    let submodule = (source != paths.skills).then_some(source);
    let reference = skill_reference(submodule.as_deref());

    let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    let backup_root = backup_root_for(&home, chrono::Utc::now());

    println!("\nfix-skills (compared against {})", reference.origin);
    let outcomes = repair_skills(
        &reference,
        &paths,
        project_dir.as_deref(),
        include_frozen,
        &backup_root,
    );
    if outcomes.is_empty() {
        println!("  nothing to repair — every deployed skill already matches");
        return;
    }

    let mut repaired = 0usize;
    let mut failed = 0usize;
    for outcome in &outcomes {
        let line = match &outcome.action {
            RepairAction::Repaired { backup } => {
                repaired += 1;
                match backup {
                    Some(p) => format!("repaired and verified from disk (backup: {})", p.display()),
                    None => "written and verified from disk (was absent)".to_string(),
                }
            }
            RepairAction::SkippedFrozen => {
                "FROZEN — hand-edited after deployment; left untouched (pass \
                 `--include-frozen` to overwrite it, backing it up first)"
                    .to_string()
            }
            RepairAction::SkippedUnverifiable(why) => {
                format!("skipped — {why}")
            }
            RepairAction::Failed(why) => {
                failed += 1;
                format!("FAILED — {why}")
            }
        };
        println!("  {}/{}: {line}", outcome.tier, outcome.stem);
    }
    println!(
        "  {repaired} repaired, {} skipped, {failed} failed",
        outcomes.len() - repaired - failed
    );
}
