//! Tests for the `by_*` convention checker (#10906).

use serde_json::json;

use super::by_convention::{ByConventionViolation, check_by_convention};

/// The trusty-events payload shape and the console's (#10907) both pass.
#[test]
fn a_conforming_payload_passes() {
    let events = json!({
        "uptime_secs": 4,
        "events": 3,
        "by_source": {"mpm": {"events": 3, "by_domain": {"message": 2, "hook": 1}}},
        "messages": {"sent": 1, "received": 1},
        "by_project": {
            "unattributed": {
                "events": 3,
                "by_source": {"mpm": {"events": 3, "by_domain": {"message": 2, "hook": 1}}},
                "messages": {"sent": 1, "received": 1},
            },
        },
    });
    assert_eq!(check_by_convention(&events), Ok(()));
    let console = json!({
        "uptime_secs": 1,
        "totals": {"requests": 1, "by_status": {"200": 1}, "by_method": {"GET": 1}},
        "by_route": {"/health": {"requests": 1, "by_status": {"200": 1}}},
        "by_upstream": {},
        "by_source": {},
    });
    assert_eq!(check_by_convention(&console), Ok(()));
}

/// Each rule's violation is reported with the dotted path to the value,
/// including one nested under a plain field and one inside a body.
#[test]
fn each_rule_violation_is_reported_with_its_path() {
    let cases = [
        (json!([]), "metrics", "metrics is not a JSON object"),
        (
            json!({"by_": {}}),
            "metrics.by_",
            "a breakdown key names no dimension",
        ),
        (
            json!({"by_source": [1, 2]}),
            "metrics.by_source",
            "a breakdown is not a JSON object",
        ),
        (
            json!({"by_source": {"mpm": "many"}}),
            "metrics.by_source",
            "a breakdown entry is neither a count nor an object",
        ),
        (
            json!({"by_source": {"mpm": -1}}),
            "metrics.by_source",
            "a breakdown entry is neither a count nor an object",
        ),
        (
            json!({"by_source": {"mpm": 1, "code": {}}}),
            "metrics.by_source",
            "a breakdown mixes counts and objects",
        ),
        (
            json!({"by_source": {"mpm": {"by_domain": null}}}),
            "metrics.by_source.mpm.by_domain",
            "a breakdown is not a JSON object",
        ),
        (
            json!({"totals": {"by_status": [200]}}),
            "metrics.totals.by_status",
            "a breakdown is not a JSON object",
        ),
    ];
    for (metrics, path, reason) in cases {
        assert_eq!(
            check_by_convention(&metrics),
            Err(ByConventionViolation {
                path: path.to_string(),
                reason,
            }),
            "{metrics}"
        );
    }
}
