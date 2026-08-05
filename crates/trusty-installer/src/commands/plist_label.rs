//! launchd label resolution for stable-set daemons (macOS).
//!
//! Why: `tctl start|stop|restart` and `tctl stack doctor` need each launchd
//! daemon's *actual* `~/Library/LaunchAgents/<label>.plist` label to drive
//! `launchctl bootstrap`/`bootout` and to check the plist file's presence.
//!
//! #4868: this module used to answer that question from its OWN table —
//! `com.trusty.<binary>` with hand-added overrides, kept in step with the
//! daemon crates by grepping their `LAUNCHD_LABEL` constants. A mirror
//! maintained by grep is a mirror that drifts, and it did: the table said
//! trusty-search's label was `com.trusty.trusty-search` and stated the
//! convention "is correct" for it, while the unit launchd actually had loaded
//! was `com.trusty.search`. Every `tctl` bootout and every doctor plist-presence
//! check therefore targeted a job that does not exist. The table is gone; both
//! sides now read [`trusty_common::launchd_labels`].
//!
//! What: [`plist_label_for`] delegates to the canonical registry, and
//! [`plist_path_for`] joins that label into the
//! `~/Library/LaunchAgents/<label>.plist` path.
//!
//! Test: `tests` pins the delegation against the daemon crates' constants and
//! asserts the plist path layout.

use std::path::PathBuf;

use trusty_common::launchd_labels;

/// Resolve the launchd agent label for a member binary.
///
/// Why: `bootout`/`bootstrap` must target the job that actually exists on disk.
/// Deriving that here independently is what let it diverge (#4868) — the
/// registry is the one definition, and the daemon crates' own `LAUNCHD_LABEL`
/// constants read from it too, so the two cannot disagree.
///
/// What: returns the canonical label for `binary` from
/// [`trusty_common::launchd_labels`].
///
/// Test: `tests::labels_match_the_daemon_crates`, `tests::default_derivation`.
pub fn plist_label_for(binary: &str) -> String {
    // #4868: delegate rather than restate — the local override table this
    // replaces is what drifted from the daemons it claimed to mirror.
    launchd_labels::canonical_label(binary)
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

    /// Why (#4868): a wrong label means `bootout`/`bootstrap` silently targets
    /// a non-existent job — which is what happened for trusty-search. Asserting
    /// against the registry constants (not re-typed literals) is what makes
    /// this test unable to agree with a lie.
    /// What: every member resolves to the same constant its daemon crate uses.
    /// Test: This is the test.
    #[test]
    fn labels_match_the_daemon_crates() {
        assert_eq!(plist_label_for("trusty-memory"), launchd_labels::MEMORY);
        assert_eq!(plist_label_for("trusty-analyze"), launchd_labels::ANALYZE);
        assert_eq!(plist_label_for("trusty-search"), launchd_labels::SEARCH);
        assert_eq!(plist_label_for("trusty-review"), launchd_labels::REVIEW);
        assert_eq!(plist_label_for("trusty-console"), launchd_labels::CONSOLE);
        assert_eq!(plist_label_for("trusty-mpm"), launchd_labels::MPM);
    }

    /// Why: the pre-#4868 table returned `com.trusty.trusty-search` for the
    /// daemon whose loaded unit is `com.trusty.search`, so `tctl stop` unloaded
    /// nothing. Naming the wrong answer explicitly stops a "restore the
    /// convention" refactor from quietly reinstating it.
    /// What: asserts the resolved label is NOT the drifted full-name form.
    /// Test: This is the test.
    #[test]
    fn default_derivation() {
        assert_ne!(
            plist_label_for("trusty-search"),
            "com.trusty.trusty-search",
            "the full-name form is a legacy alias, not the label launchd has \
             loaded (#4868)"
        );
        assert_eq!(plist_label_for("trusty-search"), "com.trusty.search");
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
