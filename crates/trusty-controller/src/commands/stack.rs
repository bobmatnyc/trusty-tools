//! `tctl stack health` and `tctl stack doctor` — stack-wide rollup commands.
//!
//! Why: These are the primary observability entry points for the whole stack
//! (DOC-4). They fan a liveness or diagnostic probe across every enabled
//! manifest member and render the tools×scope matrix + verdict.
//!
//! What: Phase-0 stubs that return a structured `NotYetImplemented` result.
//! Phase-1 will wire the DOC-4 parallel probe loop
//! (`<binary> health --json --scope all` for each member) and feed the results
//! through the rollup/render pipeline.
//!
//! Test: `run_health(false)` and `run_doctor(None, false)` should not panic
//! and should print the not-yet-implemented message.

use crate::output;

/// Handle `tctl stack health`.
///
/// Why: Phase-0 entry point that confirms the command surface is wired even
/// before the DOC-4 parallel probe loop is implemented.
///
/// What: Prints a structured stub result via `output::print_not_yet_implemented`.
///
/// Test: Call `run_health(false)` and observe it does not panic; call
/// `run_health(true)` and verify the output is valid JSON.
pub fn run_health(json: bool) {
    output::print_not_yet_implemented("stack health", json);
}

/// Handle `tctl stack doctor [<member>]`.
///
/// Why: Phase-0 entry point for the deep diagnostic sweep (DOC-4).
///
/// What: Accepts an optional member to scope the sweep; prints a structured
/// stub result.
///
/// Test: Call `run_doctor(None, false)` and `run_doctor(Some("trusty-search"), false)`;
/// both should not panic.
pub fn run_doctor(member: Option<&str>, json: bool) {
    let cmd = match member {
        Some(m) => format!("stack doctor {m}"),
        None => "stack doctor".to_owned(),
    };
    output::print_not_yet_implemented(&cmd, json);
}

// ── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_does_not_panic() {
        run_health(false);
        run_health(true);
    }

    #[test]
    fn doctor_does_not_panic() {
        run_doctor(None, false);
        run_doctor(Some("trusty-search"), true);
    }
}
