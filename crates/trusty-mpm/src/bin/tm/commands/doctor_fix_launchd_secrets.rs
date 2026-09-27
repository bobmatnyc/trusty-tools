//! `tm doctor --fix-launchd-secrets` — the LaunchAgent credential strip alone (#8236).
//!
//! Why: `--fix` runs every repair class machine-wide; an operator fixing one
//! credential exposure needs the `launchd_secrets` repair and nothing else. A
//! dedicated flag follows the `--fix-skills` / `--fix-agents` precedent rather
//! than a generic `--only <check>`, which would need a check-name plumbing
//! across all ~15 repair classes for one caller. The behaviour lives in
//! [`trusty_mpm::daemon::doctor_launchd_secrets_scoped`]; this file is CLI
//! plumbing and printing.
//! What: [`fix_launchd_secrets_locally`] — resolve `$HOME`, run the scoped
//! repair in the mode `--yes` selects, print the steps in the shared format.
//! Test: the library module's tests cover every outcome; `tests` below pins
//! the apply hint and the gate.

use trusty_mpm::core::doctor_repair::RepairMode;
use trusty_mpm::daemon::doctor_launchd_secrets_scoped::repair_launchd_secrets_only;

/// The command that applies a `--fix-launchd-secrets` preview.
const FIX_LAUNCHD_SECRETS_APPLY_HINT: &str = "tm doctor --fix-launchd-secrets --yes";

/// `--fix-launchd-secrets` action — preview, or apply, the credential strip.
///
/// Why: see the module doc. A preview is the default; `--yes` is the second
/// deliberate act that migrates, strips and tightens.
/// What: resolves `$HOME`, runs [`repair_launchd_secrets_only`], and prints one
/// line per plist. With no home directory it prints why and writes nothing.
/// Test: `a_bare_fix_launchd_secrets_writes_nothing`.
pub(crate) fn fix_launchd_secrets_locally(yes: bool) {
    let mode = RepairMode::from_apply_flag(yes);
    println!(
        "\nfix-launchd-secrets: credentials in trusty LaunchAgent plists ({}) (#8236)",
        if yes {
            "applying"
        } else {
            "dry run — nothing will be written"
        }
    );
    let Some(home) = dirs::home_dir() else {
        eprintln!("  could not resolve the home directory — nothing was checked");
        return;
    };
    let steps = repair_launchd_secrets_only(&home, mode);
    super::doctor_repair::print_steps(&steps, yes, FIX_LAUNCHD_SECRETS_APPLY_HINT);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_apply_hint_names_the_fix_launchd_secrets_command() {
        assert_eq!(
            FIX_LAUNCHD_SECRETS_APPLY_HINT,
            "tm doctor --fix-launchd-secrets --yes"
        );
    }

    #[test]
    fn a_bare_fix_launchd_secrets_writes_nothing() {
        // The mode a bare flag selects IS the gate; the repair's own tests
        // prove `DryRun` writes and chmods nothing.
        assert_eq!(RepairMode::from_apply_flag(false), RepairMode::DryRun);
    }
}
