//! launchd label resolution for stable-set daemons (macOS).
//!
//! Why: `tctl start|stop|restart` and `tctl stack doctor` need each launchd
//! daemon's *actual* `~/Library/LaunchAgents/<label>.plist` label to drive
//! `launchctl bootstrap`/`bootout` and to check the plist file's presence. The
//! convention is `com.trusty.<binary>`, but reality deviates: trusty-memory and
//! trusty-analyze register their agents as `com.trusty.memory` /
//! `com.trusty.analyze` (binary stem stripped of the `trusty-` prefix), whereas
//! trusty-search uses the full `com.trusty.trusty-search`. Encoding the
//! deviations in ONE mapping function means every lifecycle/health path agrees
//! on exactly which launchd job to target rather than re-deriving (and getting
//! it wrong) at each call site.
//!
//! What: [`plist_label_for`] returns the launchd label for a binary name —
//! `com.trusty.<binary>` by default, with explicit overrides for the daemons
//! whose real label differs (verified by grepping each daemon crate's
//! `LAUNCHD_LABEL` constant). [`plist_path_for`] joins that label into the
//! `~/Library/LaunchAgents/<label>.plist` path.
//!
//! Test: `tests` pins the override table (memory/analyze) and the default
//! derivation (search/review/console), and asserts the plist path layout.

use std::path::PathBuf;

/// Resolve the launchd agent label for a member binary.
///
/// Why: The `com.trusty.<binary>` convention does not hold for every daemon —
/// trusty-memory and trusty-analyze strip the `trusty-` prefix in their real
/// `LAUNCHD_LABEL` (`com.trusty.memory`, `com.trusty.analyze`), while
/// trusty-search keeps the full name (`com.trusty.trusty-search`). Centralising
/// the override table keeps `bootout`/`bootstrap` targeting the job that
/// actually exists on disk.
///
/// What: Returns the verified launchd label for known-deviating binaries,
/// otherwise derives `com.trusty.<binary>` by convention. The override table
/// mirrors the `LAUNCHD_LABEL` constants in the respective daemon crates —
/// `trusty-memory` → `com.trusty.memory` and `trusty-analyze` →
/// `com.trusty.analyze` (both in `commands/service.rs`). `trusty-search` is
/// intentionally NOT overridden: its real label is the derived
/// `com.trusty.trusty-search`.
///
/// Test: `tests::overrides_match_reality`, `tests::default_derivation`.
pub fn plist_label_for(binary: &str) -> String {
    match binary {
        "trusty-memory" => "com.trusty.memory".to_owned(),
        "trusty-analyze" => "com.trusty.analyze".to_owned(),
        // trusty-search registers as the full name; the convention is correct.
        other => format!("com.trusty.{other}"),
    }
}

/// Resolve the on-disk plist path for a member binary's launchd agent.
///
/// Why: `tctl stack doctor` reports whether each daemon's LaunchAgent plist is
/// installed; the path is `~/Library/LaunchAgents/<label>.plist` keyed by the
/// resolved label (so an overridden member checks the right file).
///
/// What: Joins the user's home directory with
/// `Library/LaunchAgents/<plist_label_for(binary)>.plist`. Returns `None` when
/// the home directory cannot be resolved.
///
/// Test: `tests::plist_path_layout`.
pub fn plist_path_for(binary: &str) -> Option<PathBuf> {
    let label = plist_label_for(binary);
    dirs::home_dir().map(|h| {
        h.join("Library")
            .join("LaunchAgents")
            .join(format!("{label}.plist"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: The override table is load-bearing — a wrong label means
    /// `bootout`/`bootstrap` silently targets a non-existent job. Pin the two
    /// verified deviations against the daemon crates' `LAUNCHD_LABEL` constants.
    /// What: Asserts memory/analyze resolve to their prefix-stripped labels.
    /// Test: This is the test.
    #[test]
    fn overrides_match_reality() {
        assert_eq!(plist_label_for("trusty-memory"), "com.trusty.memory");
        assert_eq!(plist_label_for("trusty-analyze"), "com.trusty.analyze");
    }

    /// Why: Members without a deviation must use the `com.trusty.<binary>`
    /// convention; trusty-search's real label IS the derived full-name form.
    /// What: Asserts search/review/console derive by convention.
    /// Test: This is the test.
    #[test]
    fn default_derivation() {
        assert_eq!(plist_label_for("trusty-search"), "com.trusty.trusty-search");
        assert_eq!(plist_label_for("trusty-review"), "com.trusty.trusty-review");
        assert_eq!(
            plist_label_for("trusty-console"),
            "com.trusty.trusty-console"
        );
    }

    /// Why: The plist path must key off the *resolved* label so an overridden
    /// member checks the correct file.
    /// What: Asserts the path ends with `LaunchAgents/com.trusty.memory.plist`.
    /// Test: This is the test.
    #[test]
    fn plist_path_layout() {
        if let Some(p) = plist_path_for("trusty-memory") {
            assert!(p.ends_with(
                std::path::Path::new("Library")
                    .join("LaunchAgents")
                    .join("com.trusty.memory.plist")
            ));
        }
    }
}
