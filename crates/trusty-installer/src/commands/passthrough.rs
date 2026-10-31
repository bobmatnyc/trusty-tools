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
//! Test: `every_stub_path_returns_not_implemented`;
//! `health_passthrough_stub_exits_non_zero` (tests/passthrough_stub_exit.rs).

use crate::output;

/// Exit code for a passthrough that is not implemented yet.
// #10585: a stub that exits 0 reads as a passing check to install validation.
pub const EXIT_NOT_IMPLEMENTED: i32 = 2;

/// Handle the generic passthrough `tctl <tool> <verb> [args]`.
///
/// Why: Phase-0 entry point that confirms the passthrough slot is wired in the
/// dispatcher, without letting a caller mistake the stub for success (#10585).
///
/// What: `args[0]` is the member id; `args[1]` is the verb; remaining tokens
/// are forwarded as-is. In Phase 0 every call prints the stub result and
/// returns [`EXIT_NOT_IMPLEMENTED`] for the process exit code.
///
/// Test: `every_stub_path_returns_not_implemented`.
pub fn run(args: &[String], json: bool) -> i32 {
    if args.len() < 2 {
        output::print_not_yet_implemented("passthrough (usage: tctl <tool> <verb> [args])", json);
        return EXIT_NOT_IMPLEMENTED;
    }
    let tool = &args[0];
    let verb = &args[1];
    let label = format!("{tool} {verb} (passthrough)");
    output::print_not_yet_implemented(&label, json);
    EXIT_NOT_IMPLEMENTED
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_stub_path_returns_not_implemented() {
        let two = ["trusty-search".to_owned(), "doctor".to_owned()];
        assert_eq!(run(&two, false), EXIT_NOT_IMPLEMENTED);
        assert_eq!(run(&two, true), EXIT_NOT_IMPLEMENTED);
        assert_eq!(run(&[], false), EXIT_NOT_IMPLEMENTED);
        assert_ne!(EXIT_NOT_IMPLEMENTED, 0);
    }
}
