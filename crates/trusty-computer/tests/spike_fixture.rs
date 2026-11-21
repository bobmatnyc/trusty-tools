//! Port of the spike's `tests/test_fixture.py`, plus a fake-daemon self-test.

mod common;

use common::{FakeDaemon, FixtureState, ndjson_call, read_state, short_tmp};
use serde_json::json;

#[test]
fn readback_tracks_text_and_clicks() {
    let dir = short_tmp();
    let mut state = FixtureState::new(&dir.path().join("s.json"));
    state.set_text("ab");
    state.press();
    state.press();
    let got = read_state(&state.readback);
    assert_eq!(
        (&got["text"], &got["clicks"], &got["keystrokes"]),
        (&json!("ab"), &json!(2), &json!(2))
    );
}

#[test]
fn fake_daemon_speaks_ndjson_and_drives_fixture() {
    let dir = short_tmp();
    let fixture = FixtureState::new(&dir.path().join("fixture.json"));
    fixture.flush();
    let sock = dir.path().join("d.sock");
    let daemon = FakeDaemon::start(&sock, "0.23.2", Some(fixture.clone()));
    let meta = ndjson_call(&sock, &json!({"method": "metadata"}));
    assert_eq!(meta["result"]["driver_version"], "0.23.2");
    let call = |name: &str, args: serde_json::Value| {
        ndjson_call(
            &sock,
            &json!({"method": "call", "name": name, "args": args}),
        )
    };
    assert_eq!(
        call("click", json!({"element_token": "tok-button"}))["ok"],
        true
    );
    call(
        "type_text",
        json!({"element_token": "tok-field", "text": "hi"}),
    );
    let snap = call("get_window_state", json!({}));
    assert!(
        snap["result"]["structuredContent"]["tree_markdown"]
            .as_str()
            .is_some_and(|t| t.contains("clicks=1"))
    );
    let state = read_state(&fixture.readback);
    assert_eq!(
        (&state["clicks"], &state["text"]),
        (&json!(1), &json!("hi"))
    );
    let names: Vec<String> = daemon.calls().into_iter().map(|(n, _)| n).collect();
    assert_eq!(names, ["click", "type_text", "get_window_state"]);
    let bad = ndjson_call(&sock, &json!({"method": "nope"}));
    assert_eq!(bad["exit_code"], 64);
}
