//! `tctl start`, `tctl stop`, `tctl restart` — daemon lifecycle commands.
//!
//! Why: Lifecycle commands bring daemons up, take them down, or bounce them
//! connection-safely (SIGTERM drain, #534). They are system-scope only and
//! require blast-radius confirmation unless `--yes` is set (DOC-3 §5 / DOC-5
//! §3.3).
//!
//! What: Phase-0 stubs. Phase-1 will dispatch the `start`/`stop`/`restart`
//! DOC-1 contract verb to each selected member's binary, honouring
//! `depends_on` ordering and using `trusty_common::launchd::LaunchdConfig` on
//! macOS (graceful `bootout`/`bootstrap` per CLAUDE.md #534).
//!
//! Test: `run_start(&[], false, false)` etc. do not panic.

/// Handle `tctl start [<members>…]`.
///
/// Why: Phase-0 entry point for the lifecycle `start` verb.
///
/// What: `members` is the named subset; empty = all daemons. `yes` bypasses
/// blast-radius confirmation. `json` selects machine output.
///
/// Test: Call with empty and non-empty `members`; neither should panic.
pub fn run_start(members: &[String], yes: bool, json: bool) {
    run_lifecycle("start", members, yes, json);
}

/// Handle `tctl stop [<members>…]`.
///
/// Why: Phase-0 entry point for the lifecycle `stop` verb.
///
/// What: Same parameter contract as `run_start`.
///
/// Test: Call with various arguments; should not panic.
pub fn run_stop(members: &[String], yes: bool, json: bool) {
    run_lifecycle("stop", members, yes, json);
}

/// Handle `tctl restart [<members>…]`.
///
/// Why: Phase-0 entry point for the lifecycle `restart` verb. `restart` must
/// include the controller's own UI service (DOC-5 §7), bounced last.
///
/// What: Same parameter contract as `run_start`.
///
/// Test: Call with various arguments; should not panic.
pub fn run_restart(members: &[String], yes: bool, json: bool) {
    run_lifecycle("restart", members, yes, json);
}

/// Shared stub implementation for all three lifecycle verbs.
///
/// Why: The three verbs share identical parameter handling and stub behaviour,
/// so a single private helper avoids repetition (DRY principle).
///
/// What: Builds a label from verb + members, then calls `print_not_yet_implemented`.
///
/// Test: Called by `run_start`, `run_stop`, and `run_restart` in their tests.
fn run_lifecycle(verb: &str, members: &[String], yes: bool, json: bool) {
    let mut label = verb.to_owned();
    if yes {
        label.push_str(" --yes");
    }
    if !members.is_empty() {
        label.push(' ');
        label.push_str(&members.join(" "));
    }
    crate::output::print_not_yet_implemented(&label, json);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_does_not_panic() {
        run_start(&[], false, false);
        run_start(&["trusty-search".to_owned()], true, true);
    }

    #[test]
    fn stop_does_not_panic() {
        run_stop(&[], false, false);
    }

    #[test]
    fn restart_does_not_panic() {
        run_restart(&[], true, false);
        run_restart(&[], false, true);
    }
}
