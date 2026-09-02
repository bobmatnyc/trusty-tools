//! `tm doctor --fix-agents` — the project-tier agent sweep (#6649).
//!
//! Why: split out of `commands/doctor_repair.rs`, which the 500-SLOC
//! production cap would otherwise reject, and to mirror the file layout
//! `--fix-skills` already has. The behaviour and every refusal live in
//! [`trusty_mpm::core::project_tier_agent_strays`]; this file is CLI plumbing
//! and printing.
//! What: [`fix_agents_locally`] — resolve this machine's paths, run the sweep
//! in the mode `--yes` selects, and print the steps in the shared `--fix`
//! format.
//! Test: `core::project_tier_agent_strays`' own tests cover every outcome; the
//! `tests` module below pins the gate.

use trusty_mpm::core::doctor_repair::RepairMode;
use trusty_mpm::core::paths::FrameworkPaths;
use trusty_mpm::core::project_tier_agent_strays::remove_project_tier_agent_strays;

/// The command that applies a `--fix-agents` preview.
const FIX_AGENTS_APPLY_HINT: &str = "tm doctor --fix-agents --yes";

/// `--fix-agents` action — preview, or apply, the project-tier agent sweep.
///
/// Why: see the module doc. A preview is the default because this repair
/// DELETES; `--yes` is the second deliberate act that lets one removal happen.
/// What: resolves `FrameworkPaths::default()` and the current directory, runs
/// [`remove_project_tier_agent_strays`] in one mode, and prints one line per
/// step. A bare `--fix-agents` writes nothing at all — no file, no ledger
/// entry, no backup directory.
/// Test: `a_bare_fix_agents_writes_nothing`.
pub(crate) fn fix_agents_locally(yes: bool) {
    let mode = RepairMode::from_apply_flag(yes);
    let paths = FrameworkPaths::default();
    let project_dir = std::env::current_dir().ok();
    let backup_root = super::doctor_fix_skills::default_backup_root();

    let steps =
        remove_project_tier_agent_strays(&paths, project_dir.as_deref(), &backup_root, mode);

    println!(
        "\nfix-agents: stray bundled copies at the project tier ({}) (#6649)",
        if yes {
            "applying"
        } else {
            "dry run — nothing will be written"
        }
    );
    super::doctor_repair::print_steps(&steps, yes, FIX_AGENTS_APPLY_HINT);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_apply_hint_names_the_fix_agents_command() {
        // The shared printer takes the hint as a parameter precisely so a
        // preview cannot tell the operator to run a command that would not
        // touch their agents. Pinned so the string cannot silently collapse
        // back onto `--fix --yes`.
        assert_eq!(FIX_AGENTS_APPLY_HINT, "tm doctor --fix-agents --yes");
    }

    #[test]
    fn a_bare_fix_agents_writes_nothing() {
        // The mode a bare flag selects IS the whole gate; the sweep's own tests
        // prove `DryRun` writes nothing.
        assert_eq!(RepairMode::from_apply_flag(false), RepairMode::DryRun);
        assert_eq!(RepairMode::from_apply_flag(true), RepairMode::Apply);
    }
}
