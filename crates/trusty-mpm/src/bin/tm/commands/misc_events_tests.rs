//! `tm events` rendering of daemon-emitted events (#12049).
//!
//! Why: kept out of `bin/tm/tests.rs`, which sits at the 3000-SLOC test cap.
//! Test: the test below.

/// #12049: a daemon-emitted `ci_transition` prints as one `tm events` line
/// naming the kind and the dedupe key.
#[test]
fn daemon_event_line_names_kind_and_key_12049() {
    let event: trusty_common::control_bus::HarnessEvent =
        serde_json::from_value(serde_json::json!({
            "source": "mpm", "seq": 3, "at": "2026-11-27T13:16:44Z",
            "payload": {"domain": "hook", "event": {"kind": "ci_transition",
                "data": {"key": "o/r#7@h1:green#1", "pr": 7}}}
        }))
        .expect("the bus wire shape");
    assert_eq!(
        super::daemon_event_line(&event),
        "2026-11-27T13:16:44+00:00 daemon ci_transition o/r#7@h1:green#1"
    );
}
