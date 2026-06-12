//! `tctl install` — zero-knowledge system install (UUC2, DOC-8).
//!
//! Why: The one-time entry point that brings a vanilla machine to a fully
//! installed and verified stack. Idempotent: already-installed members are
//! reported no-ops (DOC-3 §4 / DOC-8 §1.5).
//!
//! What: Phase-0 stub. Phase-1 will implement the DOC-8 §1 flow:
//! preflight → manifest → topological install → supervise daemons →
//! contract verify → rollup. Each cargo member uses
//! `trusty_common::update::perform_upgrade` (= `cargo install <crate> --locked`
//! with `SKIP_UI_BUILD=1` for UI-embedding members). The orchestrator uses
//! `uv tool install claude-mpm`.
//!
//! Test: `run(&[], false, false)` does not panic.

use crate::output;

/// Handle `tctl install [<members>…]`.
///
/// Why: Phase-0 entry point for the system install flow.
///
/// What: `members` names specific members to install; empty = all enabled
/// members. `yes` bypasses the blast-radius confirmation (DOC-3 §5). `json`
/// selects machine output.
///
/// Test: Call with an empty slice and both `json` values; neither should panic.
pub fn run(members: &[String], yes: bool, json: bool) {
    let mut label = "install".to_owned();
    if yes {
        label.push_str(" --yes");
    }
    if !members.is_empty() {
        label.push(' ');
        label.push_str(&members.join(" "));
    }
    output::print_not_yet_implemented(&label, json);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn does_not_panic() {
        run(&[], false, false);
        run(&["trusty-search".to_owned()], true, true);
    }
}
