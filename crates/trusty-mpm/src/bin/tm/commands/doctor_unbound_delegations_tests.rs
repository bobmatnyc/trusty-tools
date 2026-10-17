//! Tests for the #9007 unbound-delegations doctor row.
//! Test: this file IS the test module.

use super::*;

fn snapshot(field: serde_json::Value) -> HealthSnapshot {
    let mut body = serde_json::json!({ "status": "ok" });
    if !field.is_null() {
        body["unbound_delegations"] = field;
    }
    serde_json::from_value(body).expect("snapshot parses")
}

#[test]
fn zero_is_ok_9007() {
    let row = unbound_delegations_row(Some(&snapshot(serde_json::json!({ "Ok": 0 }))));
    assert_eq!(row.name, CHECK_NAME);
    assert_eq!(row.status, CheckStatus::Ok, "{}", row.message);
}

#[test]
fn a_positive_count_warns_and_names_the_remedy_9007() {
    let row = unbound_delegations_row(Some(&snapshot(serde_json::json!({ "Ok": 3 }))));
    assert_eq!(row.status, CheckStatus::Warn, "{}", row.message);
    assert!(row.message.contains("3 live delegation"), "{}", row.message);
    assert!(
        row.message.contains("tm repair delegation"),
        "{}",
        row.message
    );
}

/// #9007 Fail-Open Check: a count the daemon could not take surfaces its
/// error and is never a pass.
#[test]
fn a_read_error_is_reported_never_a_pass_9007() {
    let row = unbound_delegations_row(Some(&snapshot(
        serde_json::json!({ "Err": "the session-claude registry is sealed: x" }),
    )));
    assert_eq!(row.status, CheckStatus::Unknown, "{}", row.message);
    assert!(
        row.message.contains("registry is sealed: x"),
        "{}",
        row.message
    );
}

#[test]
fn a_daemon_without_the_field_is_unknown_9007() {
    let row = unbound_delegations_row(Some(&snapshot(serde_json::Value::Null)));
    assert_eq!(row.status, CheckStatus::Unknown, "{}", row.message);
}

#[test]
fn no_daemon_is_a_warning_not_a_pass_9007() {
    let row = unbound_delegations_row(None);
    assert_eq!(row.status, CheckStatus::Warn, "{}", row.message);
    assert!(
        !row.message.contains("trusty-mpm daemon:"),
        "{}",
        row.message
    );
}
