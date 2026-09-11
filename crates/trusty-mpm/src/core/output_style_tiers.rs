//! Every directory a bundled output style is deployed into (issue #7423).
//!
//! Why: output styles, like skills before #4605, have more than one
//! destination, and the probe and the repair each knew about only one of them.
//! `daemon::doctor_output_style::check_output_style_staleness` scanned
//! `<home>/.claude/output-styles/` and
//! `core::doctor_repair::repair_output_style` wrote `home.join(".claude")` —
//! but a tm-launched session reads `$CLAUDE_CONFIG_DIR` as its `user` tier
//! (`core::model_inject`'s `SETTING_SOURCES_FLAG_RELOCATED`), so the copy that
//! actually governs a managed session lives under the managed config dir.
//! Measured on 2026-09-11 after `tm doctor --fix --yes` on trusty-mpm 1.5.29:
//! `~/.claude/output-styles/trusty-mpm.md` was redeployed while
//! `$CLAUDE_CONFIG_DIR/output-styles/trusty-mpm.md` sat at the old 12,012
//! bytes, reported clean, and kept governing every managed session.
//!
//! What: [`output_style_tiers`] enumerates those directories, labelled and
//! deduplicated. Pure path arithmetic apart from one `is_dir` probe on the
//! managed tier — see the function doc for why that probe is there.
//!
//! Test: the `tests` module below, plus
//! `daemon::doctor_output_style`'s `staleness_warns_on_managed_config_drift`
//! and `core::doctor_repair`'s `output_style_repair_rewrites_the_managed_tier`.

use std::path::{Path, PathBuf};

/// One directory bundled output styles are deployed into.
///
/// Why: every finding has to name WHICH copy drifted — "trusty-mpm.md drifted"
/// is not actionable when two tiers hold that file and only one of them is what
/// a managed session reads. The label travels with the path for that reason.
/// What: a short stable label plus the `CLAUDE_CONFIG_DIR`-shaped directory
/// that `output-styles/` sits under — i.e. what
/// [`crate::core::output_style_deployer::deploy_output_styles`] takes.
/// Test: `tiers_cover_home_and_managed_config`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputStyleTier {
    /// Short label for operator-facing output (`operator home`, `managed
    /// config`) — the same vocabulary
    /// [`crate::core::skill_deploy_tiers::SkillDeployTier`] uses.
    pub label: &'static str,
    /// The directory `output-styles/` lives under.
    pub claude_dir: PathBuf,
}

impl OutputStyleTier {
    /// The `output-styles/` directory itself.
    ///
    /// Why: the drift scan takes the styles directory and the deploy takes its
    /// parent; spelling the join once keeps the two from disagreeing.
    /// What: `<claude_dir>/output-styles`.
    /// Test: `tiers_cover_home_and_managed_config`.
    pub fn styles_dir(&self) -> PathBuf {
        self.claude_dir.join("output-styles")
    }
}

/// Enumerate every output-style deploy target, highest-reach first.
///
/// Why: see the module doc — one enumeration is what keeps the doctor probe and
/// `tm doctor --fix` acting on the same set of files.
/// What: `<home>/.claude` always, then `managed_config` when it is supplied AND
/// already exists on disk. The existence probe is deliberate: a redeploy
/// materialising a managed `CLAUDE_CONFIG_DIR` that nothing has provisioned
/// would have `tm doctor --fix` create config state rather than repair it, and
/// an absent directory is `standalone::global_config`'s to create. A managed
/// path equal to `<home>/.claude` yields one tier, not two.
/// Test: `tiers_cover_home_and_managed_config`,
/// `an_absent_managed_dir_contributes_no_tier`,
/// `a_managed_dir_equal_to_home_is_not_listed_twice`.
pub fn output_style_tiers(home: &Path, managed_config: Option<&Path>) -> Vec<OutputStyleTier> {
    let mut tiers = vec![OutputStyleTier {
        label: "operator home",
        claude_dir: home.join(".claude"),
    }];

    if let Some(managed) = managed_config.filter(|dir| dir.is_dir())
        && !tiers.iter().any(|t| t.claude_dir == managed)
    {
        tiers.push(OutputStyleTier {
            label: "managed config",
            claude_dir: managed.to_path_buf(),
        });
    }
    tiers
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn tiers_cover_home_and_managed_config() {
        let home = TempDir::new().unwrap();
        let managed = TempDir::new().unwrap();
        let tiers = output_style_tiers(home.path(), Some(managed.path()));

        assert_eq!(tiers.len(), 2, "tiers: {tiers:?}");
        assert_eq!(tiers[0].label, "operator home");
        assert_eq!(
            tiers[0].styles_dir(),
            home.path().join(".claude").join("output-styles")
        );
        assert_eq!(tiers[1].label, "managed config");
        assert_eq!(tiers[1].styles_dir(), managed.path().join("output-styles"));
    }

    #[test]
    fn an_absent_managed_dir_contributes_no_tier() {
        // `tm doctor --fix` repairs what exists; it does not provision a
        // managed CLAUDE_CONFIG_DIR nothing has bootstrapped yet.
        let home = TempDir::new().unwrap();
        let tiers = output_style_tiers(home.path(), Some(&home.path().join("never-made")));

        assert_eq!(tiers.len(), 1, "tiers: {tiers:?}");
        assert_eq!(tiers[0].label, "operator home");
    }

    #[test]
    fn a_managed_dir_equal_to_home_is_not_listed_twice() {
        // Listing it twice would double every finding reported against it.
        let home = TempDir::new().unwrap();
        let claude = home.path().join(".claude");
        std::fs::create_dir_all(&claude).unwrap();
        let tiers = output_style_tiers(home.path(), Some(&claude));

        assert_eq!(tiers.len(), 1, "tiers: {tiers:?}");
    }
}
