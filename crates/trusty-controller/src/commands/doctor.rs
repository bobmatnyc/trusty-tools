//! `tctl doctor --self-check <member>` — conformance self-check of a member.
//!
//! Why: The controller-side conformance audit that validates a member speaks
//! the DOC-1 contract: correct envelope shape, `verbs[]` advertisement, and
//! secret redaction (DOC-6 §8).
//!
//! What: Phase-0 stub. Phase-1 will invoke `<binary> version --json` to probe
//! `contract_version` and `verbs[]`, then call each advertised verb and
//! validate the returned envelope against the DOC-1 schemas.
//!
//! Note: Per-member `doctor` (raw envelope) is reached via the generic
//! passthrough (`tctl <tool> doctor`), not this command. This command is
//! exclusively the **controller's own conformance audit** (DOC-5 §1.4).
//!
//! Test: `run(true, Some("trusty-search"), false)` does not panic.

use crate::output;

/// Handle `tctl doctor [--self-check <member>]`.
///
/// Why: Phase-0 entry point for the conformance self-check (DOC-6 §8).
///
/// What: `self_check` enables the conformance mode; `member` names the target.
/// Without `--self-check` this is a no-op placeholder (the main self-check
/// path is the primary use case for Phase 0).
///
/// Test: Call with `self_check = true` and both `member = None` and
/// `member = Some("trusty-search")`; neither should panic.
pub fn run(self_check: bool, member: Option<&str>, json: bool) {
    let label = match (self_check, member) {
        (true, Some(m)) => format!("doctor --self-check {m}"),
        (true, None) => "doctor --self-check (member required)".to_owned(),
        (false, _) => "doctor".to_owned(),
    };
    output::print_not_yet_implemented(&label, json);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn does_not_panic() {
        run(true, Some("trusty-search"), false);
        run(true, None, true);
        run(false, None, false);
    }
}
