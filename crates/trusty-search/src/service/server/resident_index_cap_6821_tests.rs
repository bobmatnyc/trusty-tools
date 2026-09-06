//! `/health`'s resident-index-cap reporting (#6821).
//!
//! Why: `TRUSTY_MAX_RESIDENT_INDEXES` used to ship disabled, so the number that
//! decides whether an index gets cold-parked was both absent and unreported.
//! #6821 gave it a machine-tier default, which makes the acting value something
//! an operator has to be able to read back — a shrinking `indexes` count and an
//! `index_not_resident` refusal are otherwise unexplained.
//! What: drives `health_handler` on a state pinned to a known tier across the
//! three resolutions — env unset, an explicit number, and `off`.
//!
//! Isolation: `#[serial_test::serial]` because `TRUSTY_MAX_RESIDENT_INDEXES` is
//! shared process env state (`residency_sweep_tests` mutates the same var).
//!
//! Test: `cargo test -p trusty-search -- resident_index_cap`

use super::*;
use crate::core::registry::IndexRegistry;
use axum::extract::State;
use axum::Json;
use trusty_common::machine_tier::MemoryTier;

fn clear_cap_env() {
    unsafe { std::env::remove_var("TRUSTY_MAX_RESIDENT_INDEXES") };
}

/// `/health` reports the acting resident-index cap and where it came from.
///
/// Test: this IS the test.
#[tokio::test]
#[serial_test::serial]
async fn health_reports_the_resident_index_cap_and_its_source() {
    let mut state = SearchAppState::new(IndexRegistry::new());
    // Pinned so the assertion does not depend on the RAM of the machine
    // running the suite.
    state.machine_tier = MemoryTier::Medium;
    let state = Arc::new(state);

    clear_cap_env();
    let Json(resp) = health_handler(State(state.clone())).await;
    assert_eq!(
        resp.resident_index_cap,
        Some(crate::service::lazy_loader::default_max_resident_indexes(
            MemoryTier::Medium
        )),
        "an unset env var must report the Medium tier default, not `null` — \
         before #6821 there was no cap to report at all"
    );
    assert_eq!(resp.resident_index_cap_source, "tier default");

    unsafe { std::env::set_var("TRUSTY_MAX_RESIDENT_INDEXES", "7") };
    let Json(resp) = health_handler(State(state.clone())).await;
    assert_eq!(resp.resident_index_cap, Some(7));
    assert_eq!(resp.resident_index_cap_source, "env");

    unsafe { std::env::set_var("TRUSTY_MAX_RESIDENT_INDEXES", "off") };
    let Json(resp) = health_handler(State(state.clone())).await;
    assert_eq!(
        resp.resident_index_cap, None,
        "`off` must report as null — the disabled signal"
    );
    assert_eq!(resp.resident_index_cap_source, "env (off)");

    clear_cap_env();
}
