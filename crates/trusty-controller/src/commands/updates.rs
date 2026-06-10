//! `tctl updates` — read-only listing of available updates + changelog headlines.
//!
//! Why: The read-only companion to `tctl upgrade` (DOC-5 §1.3 / DOC-9 §1).
//! It surfaces which members have a newer pinned or published version without
//! performing any mutation.
//!
//! What: Phase-0 stub. Phase-1 will walk the manifest, call
//! `trusty_common::update::check_crates_io` for each cargo member (or the uv
//! probe for the orchestrator), and render the per-member installed→available
//! diff table plus changelog headlines (DOC-9 §2).
//!
//! Test: `run(false, false)` and `run(true, false)` do not panic.

use crate::output;

/// Handle `tctl updates [--latest]`.
///
/// Why: Phase-0 entry point confirming the read-only update listing is wired.
///
/// What: `latest` selects crates.io HEAD over the BOM pin (DOC-9 §1.2). Both
/// paths are stubs in Phase 0.
///
/// Test: Call with `latest = false` and `latest = true`; neither should panic.
pub fn run(latest: bool, json: bool) {
    let cmd = if latest {
        "updates --latest"
    } else {
        "updates"
    };
    output::print_not_yet_implemented(cmd, json);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn does_not_panic() {
        run(false, false);
        run(true, false);
        run(false, true);
    }
}
