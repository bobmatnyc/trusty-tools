//! The journal halves of the spike's `test_broker.py::test_request_id_retry_is_not_rerun`
//! and `test_call_without_request_id_is_refused`, on the same requests, plus
//! the cap and outcome-unknown cases the plan adds.

mod common;

use common::{FakeDaemon, FixtureState, ndjson_call, read_state, short_tmp};
use serde_json::{Value, json};
use trusty_computer::journal::{JOURNAL_LIMIT, Journal, JournalError, Outcome, request_id};

fn call(tool: &str, rid: &str, args: Value) -> Value {
    json!({"op": "call", "tool": tool, "request_id": rid, "args": args})
}

/// Answer `req` from the journal, or forward it to the fake daemon once.
fn run(journal: &mut Journal, sock: &std::path::Path, req: &Value) -> (Value, bool) {
    let rid = request_id(req).expect("request_id");
    if let Some(entry) = journal.replay(rid) {
        return (entry.reply.clone(), true);
    }
    let wire = json!({"method": "call", "name": req["tool"], "args": req["args"]});
    let reply = ndjson_call(sock, &wire);
    journal
        .record(rid, reply.clone(), Outcome::Known)
        .expect("record");
    (reply, false)
}

#[test]
fn request_id_retry_is_not_rerun() {
    let dir = short_tmp();
    let fixture = FixtureState::new(&dir.path().join("fixture.json"));
    fixture.flush();
    let sock = dir.path().join("d.sock");
    let daemon = FakeDaemon::start(&sock, "0.23.2", Some(fixture.clone()));
    let mut journal = Journal::new();
    let req = call(
        "click",
        "same",
        json!({"pid": 1, "window_id": 1, "element_token": "tok-button"}),
    );
    let (first, replayed_first) = run(&mut journal, &sock, &req);
    let (second, replayed_second) = run(&mut journal, &sock, &req);
    assert!(first["ok"] == true && second["ok"] == true);
    assert_eq!((replayed_first, replayed_second), (false, true));
    assert_eq!(read_state(&fixture.readback)["clicks"], 1);
    assert_eq!(daemon.calls().len(), 1);
}

#[test]
fn call_without_request_id_is_refused() {
    let req = json!({"op": "call", "tool": "list_apps", "args": {}});
    let err = request_id(&req).map(str::to_owned);
    assert_eq!(err, Err(JournalError::RequestIdRequired));
    assert_eq!(
        JournalError::RequestIdRequired.code(),
        "request_id_required"
    );
    for bad in [json!(""), json!(7), json!(null)] {
        let req = json!({"op": "call", "request_id": bad});
        assert!(request_id(&req).is_err(), "{bad} accepted");
    }
}

#[test]
fn journal_refuses_a_new_id_when_full() {
    let mut journal = Journal::new();
    for i in 0..JOURNAL_LIMIT {
        journal
            .record(&format!("r{i}"), json!({"ok": true}), Outcome::Known)
            .expect("record");
    }
    assert_eq!(journal.reserve("new"), Err(JournalError::Full));
    assert_eq!(
        journal.record("new", json!({}), Outcome::Known),
        Err(JournalError::Full)
    );
    assert!(
        journal.reserve("r0").is_ok(),
        "a held id replays, so it is not refused"
    );
    assert_eq!(journal.len(), JOURNAL_LIMIT);
}

#[test]
fn recording_an_id_twice_is_refused_and_keeps_the_first_reply() {
    let mut journal = Journal::new();
    journal
        .record("r1", json!({"ok": true}), Outcome::Known)
        .expect("record");
    let again = journal.record("r1", json!({"ok": false}), Outcome::Known);
    assert_eq!(again.map_err(|e| e.code()), Err("request_id_reused"));
    assert_eq!(
        journal.replay("r1").map(|e| &e.reply),
        Some(&json!({"ok": true}))
    );
}

#[test]
fn outcome_unknown_survives_replay_and_clear_empties() {
    let mut journal = Journal::new();
    let reply = json!({"ok": false, "code": "session_restarted"});
    journal
        .record("m1", reply, Outcome::Unknown)
        .expect("record");
    assert_eq!(
        journal.replay("m1").map(|e| e.outcome),
        Some(Outcome::Unknown)
    );
    journal.clear();
    assert!(journal.is_empty() && journal.replay("m1").is_none());
}
