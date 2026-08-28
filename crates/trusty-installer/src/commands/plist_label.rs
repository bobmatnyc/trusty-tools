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
/// What: looks `binary` up in [`trusty_common::launchd_labels::SERVICES`] and
/// returns that service's label, falling back to the `com.trusty.<stem>`
/// convention for a member the registry does not list.
///
/// #4868 review: delegating straight to `canonical_label` skipped `SERVICES`
/// entirely, so a member whose only unit is a SUB-unit resolved wrongly —
/// `trusty-agents` returned `com.trusty.agents`, not the
/// `com.trusty.agents.slack` that is actually loaded. The registry is consulted
/// first; the convention is the fallback, not the answer.
///
/// Test: `tests::labels_match_the_daemon_crates`, `tests::default_derivation`,
/// `tests::sub_unit_members_resolve_to_their_registered_label`.
pub fn plist_label_for(binary: &str) -> String {
    // #4868: delegate rather than restate — the local override table this
    // replaces is what drifted from the daemons it claimed to mirror.
    let for_member = || {
        launchd_labels::SERVICES
            .iter()
            .filter(|s| s.member == binary)
    };
    // A member's MAIN daemon wins when it has one; otherwise its sole sub-unit
    // is what `tctl` must target. Selecting explicitly rather than by table
    // order keeps the answer stable if `SERVICES` is ever reordered.
    for_member()
        .find(|s| s.sub_unit.is_none())
        .or_else(|| for_member().next())
        .map_or_else(
            || launchd_labels::canonical_label(binary),
            |s| s.label.to_owned(),
        )
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
    plist_path_for_label(&plist_label_for(binary))
}

/// Resolve the on-disk plist path for a launchd LABEL.
///
/// Why (#6350): a retired member's eviction walks
/// `trusty_common::launchd_labels::retired_labels_for_member`, which yields
/// LABELS — the canonical one plus every legacy alias — and each has its own
/// file. [`plist_path_for`] cannot answer that: it resolves ONE label from a
/// binary name, so an alias would resolve to the canonical file and the alias's
/// own plist would be left on disk.
///
/// What: `~/Library/LaunchAgents/<label>.plist`. `None` when the home directory
/// cannot be resolved.
/// Test: `tests::plist_path_layout` covers the shared join.
pub fn plist_path_for_label(label: &str) -> Option<PathBuf> {
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

    /// Why (#4868 review): delegating straight to the convention could not
    /// express a member whose only unit is a sub-unit — `trusty-agents`
    /// resolved to `com.trusty.agents` while the loaded unit is
    /// `com.trusty.agents.slack`, so `tctl` would have targeted a job that does
    /// not exist. That is the same failure this issue is about, one member over.
    /// What: a sub-unit-only member resolves to its registered label; a member
    /// with both a main daemon and a sub-unit resolves to the main daemon.
    /// Test: This is the test.
    #[test]
    fn sub_unit_members_resolve_to_their_registered_label() {
        assert_eq!(
            plist_label_for("trusty-agents"),
            launchd_labels::AGENTS_SLACK,
            "a member whose only registered unit is a sub-unit must resolve to \
             that sub-unit's label"
        );
        assert_eq!(
            plist_label_for("trusty-search"),
            launchd_labels::SEARCH,
            "a member with both a daemon and a sub-unit must resolve to the \
             daemon, not the logrotate agent"
        );
        assert_eq!(plist_label_for("trusty-mpm"), launchd_labels::MPM);
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
