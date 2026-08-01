//! TEMPORARY — mutation probe for #4614. Deleted in the next commit.
//!
//! Why: the defect #4614 fixes is that the `Test` check is ALWAYS green, so a
//! green run proves nothing about the fix. The only acceptance evidence is a
//! demonstrated RED. This test fails on purpose so the check has something real
//! to fail on; the commit that removes it must then go green, proving the gate
//! reports both states rather than being stuck.
//! What: panics.
//! Test: itself.

#[test]
fn deliberate_failure_proving_the_test_gate_can_go_red_4614() {
    panic!("#4614 mutation probe — this failure MUST turn the Test check red");
}
