//! The code-critic fix round of issue #11460: replay binding, the pid's real
//! bundle, snapshot redaction that fails closed, and a lockout that survives
//! a broker restart. Each case failed against the pre-fix code.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::FakeLauncher;
use common::dialog::{Answer, FakeDialog};
use common::rig::{BUNDLE, Conn, Rig, T0, WINDOW, call, grant_req, pid};
use serde_json::{Value, json};
use trusty_computer::app_identity::parse_lsappinfo;
use trusty_computer::broker::{Broker, Clock, Ctx};
use trusty_computer::consent::DialogOutcome;
use trusty_computer::supervisor::Supervisor;

fn snap_args() -> Value {
    json!({"pid": pid(), "window_id": WINDOW})
}

#[test]
fn replay_by_another_session_is_refused() {
    let mut rig = Rig::new();
    let first = rig.snap("s1");
    let mut unbound = Conn::open(&rig.server.agent);
    let stolen = unbound.send(&call(&rig.grant.id, "get_window_state", "s1", snap_args()));
    assert_eq!(stolen["code"], "grant_not_bound", "{stolen}");
    assert!(stolen.get("result").is_none(), "{stolen}");
    // A session bound to its own grant cannot replay this one either.
    let other = rig.mint(2);
    let code = rig.code(&other.id);
    let mut queued = Conn::open(&rig.server.agent);
    assert_eq!(
        queued.send(&json!({"op": "connect", "code": code}))["ok"],
        true
    );
    let stolen = queued.send(&call(&rig.grant.id, "get_window_state", "s1", snap_args()));
    assert_eq!(stolen["code"], "grant_not_bound", "{stolen}");
    // The owner still gets its reply back, not a re-run.
    let again = rig.call("get_window_state", "s1", snap_args());
    assert_eq!(again["result"], first);
}

#[test]
fn grant_request_refuses_a_pid_of_another_app() {
    let rig = Rig::new();
    let shown = rig.dialog.shown();
    rig.resolver.set(pid(), "com.googlecode.iterm2", "iTerm");
    let reply = rig.op(&grant_req(&[], WINDOW));
    assert_eq!(reply["code"], "bundle_mismatch", "{reply}");
    let line = rig.audit().last().cloned().expect("audit");
    assert_eq!(line["resolved_bundle_id"], "com.googlecode.iterm2");
    rig.resolver
        .apps
        .lock()
        .expect("apps")
        .insert(pid(), Err("no such app".into()));
    let reply = rig.op(&grant_req(&[], WINDOW));
    assert_eq!(reply["code"], "app_unresolved", "{reply}");
    assert_eq!(rig.dialog.shown(), shown, "no dialog for a refused pid");
}

#[test]
fn sensitive_check_reads_the_resolved_bundle() {
    let rig = Rig::new();
    let text = rig.dialog.prompts.lock().expect("prompts")[0].text();
    assert!(
        text.contains(&format!("Fixture ({BUNDLE}, pid {})", pid())),
        "{text}"
    );
    let shown = rig.dialog.shown();
    rig.resolver.set(pid(), "COM.APPLE.TERMINAL", "Terminal");
    let req = json!({"op": "grant_request", "bundle_id": "COM.APPLE.TERMINAL", "pid": pid(),
        "window_id": WINDOW});
    let reply = rig.op(&req);
    assert_eq!(reply["code"], "sensitive_app", "{reply}");
    assert_eq!(rig.dialog.shown(), shown);
}

#[test]
fn lsappinfo_output_parses() {
    let app = parse_lsappinfo(
        "\"CFBundleIdentifier\"=\"com.apple.finder\"\n\"LSDisplayName\"=\"Finder\"\n",
    )
    .expect("parsed");
    assert_eq!(
        (app.bundle_id.as_str(), app.name.as_str()),
        ("com.apple.finder", "Finder")
    );
    assert_eq!(parse_lsappinfo("\"CFBundleIdentifier\"=[ NULL ] \n"), None);
    assert_eq!(parse_lsappinfo(""), None);
}

#[test]
fn snapshot_without_a_structured_part_is_refused() {
    let mut rig = Rig::new();
    rig.daemon(|d| d.set_snapshot_mode("text_only"));
    let reply = rig.call("get_window_state", "r1", snap_args());
    assert_eq!(reply["code"], "redaction_failed", "{reply}");
    assert!(!reply.to_string().contains("Shut Down"), "{reply}");
    let click = rig.click("r2", "tok-button");
    assert_eq!(click["code"], "token_not_exposed");
}

#[test]
fn driver_refused_result_is_redacted() {
    let mut rig = Rig::new();
    rig.daemon(|d| d.set_snapshot_mode("refused"));
    let reply = rig.call("get_window_state", "r1", snap_args());
    assert_eq!(reply["code"], "driver_refused", "{reply}");
    let text = reply.to_string();
    assert!(
        !text.contains("Shut Down") && !text.contains("AXMenuBar"),
        "{reply}"
    );
    assert!(text.contains("Press Me"), "{reply}");
}

/// A second broker on the rig's state dir, as launchd's restart would start it.
fn restarted(rig: &Rig, dialog: Arc<FakeDialog>) -> Broker<FakeLauncher> {
    let sup = Supervisor::new(FakeLauncher::new("0.23.2", None), &rig.state())
        .expect("supervisor")
        .with_ready_timeout(Duration::from_secs(2));
    let clock: Clock = Arc::new(|| T0);
    Broker::new(sup, &rig.state(), clock, dialog)
        .expect("broker")
        .with_dialog_timeout(Duration::from_millis(500))
        .with_app_resolver(rig.resolver.clone())
}

fn ctx(operator: bool) -> Ctx {
    Ctx {
        session: if operator { "op" } else { "s-9" }.into(),
        uid: trusty_computer::private::uid(),
        operator,
    }
}

#[test]
fn lockout_survives_a_broker_restart() {
    let rig = Rig::new();
    assert_eq!(
        rig.op(&json!({"op": "lockout"}))["result"]["marker_persisted"],
        true
    );
    assert!(rig.state().join("lockout").is_file());
    let dialog = FakeDialog::confirming();
    let broker = restarted(&rig, dialog.clone());
    let reply = broker.handle(Some(&grant_req(&[], WINDOW)), &ctx(true));
    assert_eq!(reply["code"], "locked_out", "{reply}");
    assert_eq!(dialog.shown(), 0);
    let connect = broker.handle(Some(&json!({"op": "connect", "code": "tc-x"})), &ctx(false));
    assert_eq!(connect["code"], "locked_out");
    let status = broker.handle(Some(&json!({"op": "status"})), &ctx(true));
    assert_eq!(status["ok"], true, "{status}");
}

#[test]
fn unlock_needs_the_dialog_and_its_code() {
    let rig = Rig::new();
    rig.op(&json!({"op": "lockout"}));
    let dialog = FakeDialog::answering(Answer::Say(DialogOutcome::Denied));
    let broker = restarted(&rig, dialog.clone());
    let op = |req: Value| broker.handle(Some(&req), &ctx(true));
    let agent = broker.handle(Some(&json!({"op": "unlock_request"})), &ctx(false));
    assert_eq!(
        agent["code"], "locked_out",
        "the agent socket cannot unlock"
    );
    assert_eq!(op(json!({"op": "unlock_request"}))["code"], "dialog_denied");
    let blind = op(json!({"op": "unlock_confirm", "challenge": "ch-x", "code": "000000"}));
    assert_eq!(blind["code"], "bad_challenge");
    dialog.set(Answer::Say(DialogOutcome::Confirmed));
    let asked = op(json!({"op": "unlock_request"}));
    let challenge = asked["result"]["challenge"].as_str().expect("challenge");
    let right = dialog.last_code();
    let wrong = if right == "000000" {
        "000001"
    } else {
        "000000"
    };
    let bad = op(json!({"op": "unlock_confirm", "challenge": challenge, "code": wrong}));
    assert_eq!(bad["code"], "bad_code");
    assert!(
        rig.state().join("lockout").is_file(),
        "a bad code keeps the marker"
    );
    assert_eq!(op(grant_req(&[], WINDOW))["code"], "locked_out");
    let ok = op(json!({"op": "unlock_confirm", "challenge": challenge, "code": right}));
    assert_eq!(ok["result"]["unlocked"], true, "{ok}");
    assert!(!rig.state().join("lockout").exists());
    let shown = dialog.shown();
    op(grant_req(&[], WINDOW));
    assert_eq!(
        dialog.shown(),
        shown + 1,
        "grant_request reaches the dialog again"
    );
}

#[test]
fn lockout_marker_is_written_when_revoke_all_fails() {
    let rig = Rig::new();
    // A directory where the grants temp file goes makes `revoke_all` fail.
    std::fs::create_dir(rig.state().join("grants.json.tmp")).expect("block tmp");
    let reply = rig.op(&json!({"op": "lockout"}));
    assert_eq!(reply["result"]["grants_persisted"], false, "{reply}");
    assert_eq!(reply["result"]["marker_persisted"], true, "{reply}");
    assert!(rig.state().join("lockout").is_file());
}
