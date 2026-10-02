//! The shutdown legacy-registry pass, which kills nothing (#8942, #9101).
//!
//! Why: boot discovery fills the legacy registry from ANY claude pane, the
//! Architect's included, and an entry records only a tmux name. The shutdown
//! reap used to kill every such name, so it reached whichever session carried
//! the name at shutdown: the Architect (#8942), an operator's own claude, or
//! after a tmux server restart an unrelated session (#9101). A name proves no
//! ownership, and the daemon kills only sessions it proves it owns.
//! What: [`skip_legacy_names`] logs each name as skipped and kills none.
//! Test: `the_shutdown_legacy_pass_kills_no_legacy_session`.

use tracing::warn;

/// Log every tmux session in `names` as left running; returns how many.
///
/// Why: see the module doc. The managed sessions the daemon can prove it
/// owns are stopped by `SessionManager::shutdown` before this runs.
/// What: one `warn` per name; no tmux call.
/// Test: `the_shutdown_legacy_pass_kills_no_legacy_session`.
pub(crate) fn skip_legacy_names(names: impl IntoIterator<Item = String>) -> usize {
    let mut skipped = 0usize;
    for name in names {
        warn!(
            "graceful shutdown: legacy session '{name}' left running — a legacy registry \
             entry carries no pane or server identity to prove it is the daemon's (#9101)"
        );
        skipped += 1;
    }
    skipped
}

#[cfg(test)]
#[path = "legacy_reap_tests.rs"]
mod tests;
