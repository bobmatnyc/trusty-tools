//! Unit tests for the prohibition-redirect helper (the SP guard text).
//!
//! Why: the redirect is the operator-facing proof that the SM refuses direct work
//! and points to delegation (§3.2). The wording must name the prohibition and the
//! corrective action so a driver/operator understands WHY the SM did not just do
//! it.
//! What: drives `redirect_direct_work` with present/empty summaries.
//! Test: this is the test module.

use super::{DIRECT_WORK_REDIRECT, redirect_direct_work};

/// Why: the redirect must name the prohibition (SP1-SP5) and the corrective action
/// (launch a session) so the refusal is self-explanatory.
/// What: asserts the constant mentions both.
/// Test: this is the test.
#[test]
fn redirect_mentions_prohibition_and_launch() {
    assert!(DIRECT_WORK_REDIRECT.contains("SP1-SP5"));
    assert!(
        DIRECT_WORK_REDIRECT
            .to_ascii_lowercase()
            .contains("launch a session")
    );
}

/// Why: surfacing the attempted work makes the redirect actionable + auditable.
/// What: asserts the summary text rides back in the redirect.
/// Test: this is the test.
#[test]
fn redirect_includes_attempted_work() {
    let msg = redirect_direct_work("edit main.rs to add a flag");
    assert!(msg.contains("edit main.rs to add a flag"));
    assert!(msg.starts_with(DIRECT_WORK_REDIRECT));
}

/// Why: a blank summary must still produce a clean, grammatical redirect (no
/// dangling colon).
/// What: asserts the empty-summary redirect ends with a period and no trailing colon.
/// Test: this is the test.
#[test]
fn redirect_handles_empty_summary() {
    let msg = redirect_direct_work("   ");
    assert!(msg.ends_with('.'));
    assert!(!msg.contains(": ."));
}
