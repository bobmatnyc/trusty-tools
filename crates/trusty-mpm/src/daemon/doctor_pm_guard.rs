//! `tm doctor` pm-guard probe (#9018).
//!
//! Why: owner ruling 307 lets the operator turn the PM guard off with
//! `[pm_guard] enabled = false`. An unguarded fleet is a state the operator
//! must be able to see where they look, not infer from an absent hook entry.
//! What: [`check_pm_guard`] reads `~/.trusty-mpm/config.toml` through
//! [`MpmConfig::load`] — so a missing, unreadable or malformed file reads as
//! enabled, as every launch does — and reports `Ok` when the guard is on and
//! `Warn` when the key turns it off. Read-only.
//! Test: `an_absent_config_reports_the_guard_enabled`,
//! `an_explicit_true_reports_the_guard_enabled`,
//! `a_disabled_guard_warns_naming_the_key`,
//! `a_malformed_config_reports_the_guard_enabled`.

use std::path::Path;

use crate::core::config::MpmConfig;
use crate::core::doctor::{CheckStatus, DoctorCheck};

/// The check's name.
const NAME: &str = "pm_guard";

/// Report whether launches write the `hook --pm-guard` entry.
///
/// Why: see the module doc.
/// What: `Warn` with "pm-guard disabled by [pm_guard] enabled = false" when
/// the key is off, else `Ok`. Both messages say a running session keeps the
/// hooks it loaded at startup, so a change applies at the next launch.
/// Test: `a_disabled_guard_warns_naming_the_key`,
/// `an_absent_config_reports_the_guard_enabled`.
pub(super) fn check_pm_guard(home: &Path) -> DoctorCheck {
    let config = MpmConfig::load(&home.join(".trusty-mpm"));
    if config.pm_guard.enabled {
        return DoctorCheck::new(
            NAME,
            CheckStatus::Ok,
            "pm-guard enabled — launches write the `hook --pm-guard` PreToolUse entry",
        );
    }
    DoctorCheck::new(
        NAME,
        CheckStatus::Warn,
        "pm-guard disabled by [pm_guard] enabled = false in ~/.trusty-mpm/config.toml — \
         launches omit the `hook --pm-guard` entry and strip an existing one. A running \
         Claude Code session keeps the hooks it loaded at startup; the change applies at \
         its next launch.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch home whose `.trusty-mpm/config.toml` holds `body`.
    fn home_with(body: &str) -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join(".trusty-mpm");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("config.toml"), body).unwrap();
        home
    }

    #[test]
    fn an_absent_config_reports_the_guard_enabled() {
        let home = tempfile::tempdir().unwrap();
        let check = check_pm_guard(home.path());
        assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
        assert!(
            check.message.contains("pm-guard enabled"),
            "{}",
            check.message
        );
    }

    #[test]
    fn an_explicit_true_reports_the_guard_enabled() {
        let home = home_with("[pm_guard]\nenabled = true\n");
        let check = check_pm_guard(home.path());
        assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
    }

    #[test]
    fn a_disabled_guard_warns_naming_the_key() {
        let home = home_with("[pm_guard]\nenabled = false\n");
        let check = check_pm_guard(home.path());
        assert_eq!(check.name, NAME);
        assert_eq!(check.status, CheckStatus::Warn, "{}", check.message);
        assert!(
            check
                .message
                .contains("pm-guard disabled by [pm_guard] enabled = false"),
            "{}",
            check.message
        );
        assert!(check.message.contains("next launch"), "{}", check.message);
    }

    /// #9018 Fail-Open Check: a config the loader cannot parse reads as
    /// guarded, so the row must not claim the guard is off.
    #[test]
    fn a_malformed_config_reports_the_guard_enabled() {
        let home = home_with("[pm_guard\nenabled = false\n");
        let check = check_pm_guard(home.path());
        assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
    }
}
