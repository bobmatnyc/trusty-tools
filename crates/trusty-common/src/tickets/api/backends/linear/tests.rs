//! Tests for the Linear backend implementation.
//!
//! Why: Keeps test code out of backend.rs so the production file stays under
//! the 500-SLOC cap while tests get the 1500-SLOC test-file budget.
//! What: Unit tests for priority/state helpers and backend construction.
//! Test: Run with `cargo test -p trusty-common`.

use super::super::types::{int_to_priority, priority_to_int, state_from_name};
use crate::tickets::api::backends::linear::LinearBackend;
use crate::tickets::api::config::LinearConfig;
use crate::tickets::api::models::{IssueState, Priority};

#[test]
fn priority_to_int_mapping() {
    assert_eq!(priority_to_int("critical"), 1);
    assert_eq!(priority_to_int("high"), 2);
    assert_eq!(priority_to_int("medium"), 3);
    assert_eq!(priority_to_int("low"), 4);
    assert_eq!(priority_to_int("unknown"), 0);
}

#[test]
fn int_to_priority_mapping() {
    assert_eq!(int_to_priority(1), Some(Priority::Critical));
    assert_eq!(int_to_priority(4), Some(Priority::Low));
    assert_eq!(int_to_priority(0), None);
}

#[test]
fn state_mapping() {
    assert_eq!(state_from_name("In Progress"), IssueState::InProgress);
    assert_eq!(state_from_name("Done"), IssueState::Done);
    assert_eq!(state_from_name("Backlog"), IssueState::Open);
}

#[test]
fn requires_api_key() {
    let cfg = LinearConfig::default();
    assert!(LinearBackend::new(cfg).is_err());
}
