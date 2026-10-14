//! The guard-lift-at-init floor of `tm hook --pm-guard` (#8436 PR C, M1).
//!
//! Test: `guard_lift_init_tests.rs`.

/// The rule name recorded with a guard-lift-at-init deny.
pub(crate) const GUARD_LIFT_INIT_RULE: &str = "guard-lift-init";

/// Stub: red stage.
pub(crate) fn evaluate_guard_lift_init(_command: &str) -> Option<String> {
    None
}

#[cfg(test)]
#[path = "guard_lift_init_tests.rs"]
mod tests;
