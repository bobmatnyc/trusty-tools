//! `tctl status` — one-line stack summary (verdict + stack version).
//!
//! Why: A quick "is the stack healthy?" command that renders a single verdict
//! line, sugar over `stack health` (DOC-4).
//!
//! What: Phase-0 stub. Phase-1 will probe all members and render a one-liner
//! such as `stack 2026.06-1 — verdict: ready`.
//!
//! Test: `run(false)` does not panic; `run(true)` outputs parseable JSON.

use crate::output;

/// Handle `tctl status`.
///
/// Why: Phase-0 entry point confirming the command is wired.
///
/// What: Prints a structured stub result.
///
/// Test: Call `run(false)` and `run(true)`; neither should panic.
pub fn run(json: bool) {
    output::print_not_yet_implemented("status", json);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn does_not_panic() {
        run(false);
        run(true);
    }
}
