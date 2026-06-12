//! Generic passthrough: `tctl <tool> <verb> [args]`.
//!
//! Why: The passthrough is the substrate of the entire DOC-5 dispatch engine
//! (§2). It allows any member verb to be forwarded without a controller
//! release, satisfying the "zero tool-specific logic" requirement (spec §83 /
//! DOC-5 §2.2). First-class commands are sugar over this passthrough.
//!
//! What: Phase-0 stub that validates the `args` slice has at least two tokens
//! (member id + verb) and returns a structured `NotYetImplemented` result.
//! Phase-1 will load the manifest, validate the member id and verb against
//! `verbs[]` (from `<binary> version --json`), then spawn the binary with the
//! forwarded args and DOC-5 §4.2 `--json` flag.
//!
//! Test: `run(&["trusty-search", "doctor"], false)` does not panic;
//! `run(&[], false)` prints the usage-error message (exit 3 in Phase 1).

use crate::output;

/// Handle the generic passthrough `tctl <tool> <verb> [args]`.
///
/// Why: Phase-0 entry point that confirms the passthrough slot is wired in the
/// dispatcher.
///
/// What: `args[0]` is the member id; `args[1]` is the verb; remaining tokens
/// are forwarded as-is. In Phase 0, any non-empty call returns a stub result.
/// An empty `args` slice indicates a usage error.
///
/// Test: Call with `args = ["trusty-search", "doctor"]`; should not panic.
/// Call with `args = []`; should print a usage-error message.
pub fn run(args: &[String], json: bool) {
    if args.len() < 2 {
        output::print_not_yet_implemented("passthrough (usage: tctl <tool> <verb> [args])", json);
        return;
    }
    let tool = &args[0];
    let verb = &args[1];
    let label = format!("{tool} {verb} (passthrough)");
    output::print_not_yet_implemented(&label, json);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_args_does_not_panic() {
        run(&["trusty-search".to_owned(), "doctor".to_owned()], false);
    }

    #[test]
    fn empty_args_does_not_panic() {
        run(&[], false);
    }

    #[test]
    fn json_mode_does_not_panic() {
        run(&["trusty-memory".to_owned(), "health".to_owned()], true);
    }
}
