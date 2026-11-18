//! The consent flow fails closed: no TTY, wrong code, expired code, denied
//! or timed-out dialog, and the lockout (issue #11460, Phase 4).
//!
//! Every case runs over the real `operator.sock` on the fake daemon with the
//! fake dialog (`common::dialog`); no test shows a real dialog.

mod common;

use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use common::dialog::Answer;
use common::rig::{Conn, Rig, grant_req, pid};
use serde_json::{Value, json};
use trusty_computer::consent::DialogOutcome;
use trusty_computer::grants::Grants;
use trusty_computer::operator_cli::{CodeSource, OperatorClient, grant_flow};

struct Typed(String);

impl CodeSource for Typed {
    fn read_code(&mut self, _prompt: &str) -> io::Result<String> {
        Ok(self.0.clone())
    }
}

fn grant_count(rig: &Rig) -> usize {
    Grants::load(&rig.state().join("grants.json"), 0.0)
        .expect("grants")
        .iter()
        .count()
}

fn wrong(code: &str) -> String {
    if code == "000000" { "000001" } else { "000000" }.to_owned()
}

/// The `trusty-computer` bin in a new session: no controlling terminal.
fn cli(rig: &Rig, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_trusty-computer"));
    cmd.arg("--state").arg(rig.state()).args(args);
    cmd.stdin(Stdio::null());
    // SAFETY: setsid is async-signal-safe and touches no parent state.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd.output().expect("run trusty-computer")
}

#[test]
fn no_tty_grant_never_reaches_the_broker() {
    let rig = Rig::new();
    let (shown, lines) = (rig.dialog.shown(), rig.audit().len());
    let window = pid().to_string();
    let out = cli(
        &rig,
        &[
            "grant",
            common::rig::BUNDLE,
            "--pid",
            &window,
            "--window",
            "1",
        ],
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(3), "{stderr}");
    assert!(stderr.contains("no_tty"), "{stderr}");
    assert_eq!(rig.dialog.shown(), shown, "no dialog without a TTY");
    assert_eq!(rig.audit().len(), lines, "nothing reached the broker");
    assert_eq!(grant_count(&rig), 1);
}

#[test]
fn wrong_code_refuses_and_locks_out() {
    let rig = Rig::new();
    let mut client = OperatorClient::connect(&rig.state()).expect("operator");
    let req = grant_req(&[], 1);
    // The CLI flow mints with the code the operator read from the dialog.
    let mut right = Typed(String::new());
    let reply = client.send(&req).expect("request");
    right.0 = rig.dialog.last_code();
    let challenge = reply["result"]["challenge"].clone();
    let ok = client
        .send(&json!({"op": "grant_confirm", "challenge": challenge, "code": right.0}))
        .expect("confirm");
    assert!(
        ok["result"]["code"].as_str().is_some_and(|c| c.len() == 33),
        "{ok}"
    );
    assert_eq!(grant_count(&rig), 2);
    // A wrong code mints nothing; five kill the challenge and lock minting out.
    let reply = client.send(&req).expect("request");
    let challenge = reply["result"]["challenge"].clone();
    let bad = wrong(&rig.dialog.last_code());
    for _ in 0..5 {
        let r = client
            .send(&json!({"op": "grant_confirm", "challenge": challenge, "code": bad}))
            .expect("confirm");
        assert_eq!(r["code"], "bad_code", "{r}");
    }
    let good = rig.dialog.last_code();
    let r = client
        .send(&json!({"op": "grant_confirm", "challenge": challenge, "code": good}))
        .expect("confirm");
    assert_eq!(r["code"], "locked_out", "{r}");
    let shown = rig.dialog.shown();
    assert_eq!(client.send(&req).expect("request")["code"], "locked_out");
    assert_eq!(rig.dialog.shown(), shown, "no dialog while locked out");
    assert_eq!(grant_count(&rig), 2);
    rig.advance(61.0);
    let r = grant_flow(&mut client, &req, &mut Typed(String::new())).expect("flow");
    assert_eq!(r["code"], "bad_code", "an empty code is wrong: {r}");
}

#[test]
fn expired_code_refuses() {
    let rig = Rig::new();
    let mut op = Conn::open(&rig.server.operator);
    let reply = op.send(&grant_req(&[], 1));
    rig.advance(121.0);
    let r = op.send(
        &json!({"op": "grant_confirm", "challenge": reply["result"]["challenge"],
        "code": rig.dialog.last_code()}),
    );
    assert_eq!(r["code"], "code_expired", "{r}");
    assert_eq!(grant_count(&rig), 1);
}

fn refused_by_dialog(rig: &Rig, answer: Answer) -> Value {
    rig.dialog.set(answer);
    let mut op = Conn::open(&rig.server.operator);
    let reply = op.send(&grant_req(&[], 1));
    assert!(reply["result"]["challenge"].is_null(), "{reply}");
    let r = op.send(&json!({"op": "grant_confirm", "challenge": "ch-any",
        "code": rig.dialog.last_code()}));
    assert_eq!(r["code"], "bad_challenge");
    assert_eq!(grant_count(rig), 1);
    reply
}

#[test]
fn dialog_denied_refuses() {
    let rig = Rig::new();
    let r = refused_by_dialog(&rig, Answer::Say(DialogOutcome::Denied));
    assert_eq!(r["code"], "dialog_denied");
    let r = refused_by_dialog(&rig, Answer::Say(DialogOutcome::Failed("no gui".into())));
    assert_eq!(r["code"], "dialog_failed");
}

#[test]
fn dialog_timeout_refuses() {
    let rig = Rig::new();
    let r = refused_by_dialog(&rig, Answer::Say(DialogOutcome::TimedOut));
    assert_eq!(r["code"], "dialog_timeout");
    // A dialog that ignores its timeout still times out in the broker (500 ms in the rig).
    let started = Instant::now();
    let r = refused_by_dialog(&rig, Answer::Hang(Duration::from_secs(3)));
    assert_eq!(r["code"], "dialog_timeout", "{r}");
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn grant_code_is_one_use_and_redeem_locks_out() {
    let mut rig = Rig::new();
    let other = rig.mint(2);
    let code = rig.code(&other.id);
    let mut two = Conn::open(&rig.server.agent);
    let ok = two.send(&json!({"op": "connect", "code": code}));
    assert_eq!(ok["result"]["lease"], "queued", "{ok}");
    let mut three = Conn::open(&rig.server.agent);
    let again = three.send(&json!({"op": "connect", "code": code}));
    assert_eq!(again["code"], "bad_code", "a code is one-use");
    let fresh = rig.mint(2);
    let fresh_code = rig.code(&fresh.id);
    for _ in 0..4 {
        three.send(&json!({"op": "connect", "code": "tc-guess"}));
    }
    let locked = three.send(&json!({"op": "connect", "code": fresh_code}));
    assert_eq!(locked["code"], "locked_out", "{locked}");
    assert_eq!(rig.call("list_apps", "r1", json!({}))["ok"], true);
}

#[test]
fn lockout_revokes_everything_and_stops() {
    let mut rig = Rig::new();
    rig.snap("s1");
    let spare = rig.mint(2);
    let spare_code = rig.code(&spare.id);
    let r = rig.op(&json!({"op": "lockout"}));
    assert_eq!(r["result"]["daemon_stopped"], true, "{r}");
    assert!(rig.broker.shutdown_requested());
    let grants = Grants::load(&rig.state().join("grants.json"), 0.0).expect("grants");
    assert!(
        grants.iter().all(|g| g.revoked),
        "every grant revoked on disk"
    );
    {
        let inner = rig.inner();
        assert!(inner.supervisor.pid().is_none(), "daemon stopped");
    }
    assert_eq!(rig.call("list_apps", "r2", json!({}))["code"], "locked_out");
    let mut agent = Conn::open(&rig.server.agent);
    let redeem = agent.send(&json!({"op": "connect", "code": spare_code}));
    assert_eq!(redeem["code"], "locked_out");
    let shown = rig.dialog.shown();
    assert_eq!(rig.op(&grant_req(&[], 1))["code"], "locked_out");
    assert_eq!(rig.dialog.shown(), shown);
    let lease = rig.op(&json!({"op": "lease"}));
    assert_eq!(lease["code"], "locked_out");
    let status = rig.conn.send(&json!({"op": "status"}));
    assert_eq!(status["result"]["lease"]["lease"], "none", "{status}");
    assert!(rig.audit().iter().any(|l| l["op"] == "lockout"));
}

#[test]
fn cli_verbs_drive_the_operator_socket() {
    let rig = Rig::new();
    let out = cli(&rig, &["grants"]);
    assert_eq!(out.status.code(), Some(0));
    let listed: Value = serde_json::from_slice(&out.stdout).expect("grants json");
    assert_eq!(listed["grants"][0]["id"], json!(rig.grant.id));
    let lease: Value = serde_json::from_slice(&cli(&rig, &["lease"]).stdout).expect("lease");
    assert_eq!(lease["holder"]["grant"], json!(rig.grant.id));
    assert_eq!(cli(&rig, &["status"]).status.code(), Some(0));
    assert_eq!(cli(&rig, &["revoke", &rig.grant.id]).status.code(), Some(0));
    assert_eq!(cli(&rig, &["revoke", "g-none"]).status.code(), Some(1));
    assert_eq!(cli(&rig, &["bogus"]).status.code(), Some(2));
    assert_eq!(cli(&rig, &["lockout"]).status.code(), Some(0));
    assert!(rig.broker.shutdown_requested());
}

#[test]
fn agent_grants_wait_and_refresh() {
    let mut rig = Rig::new();
    let _other = rig.mint(2);
    let g = rig.grant.id.clone();
    let mine = rig.conn.send(&json!({"op": "grants"}));
    let rows = mine["result"]["grants"].as_array().expect("rows");
    assert_eq!(rows.len(), 1, "only this session's grant: {mine}");
    assert_eq!(rows[0]["id"], json!(g));
    let waited = rig
        .conn
        .send(&json!({"op": "wait", "grant_id": g, "seconds": 0.1}));
    assert_eq!(waited["ok"], true, "{waited}");
    let long = rig
        .conn
        .send(&json!({"op": "wait", "grant_id": g, "seconds": 9}));
    assert_eq!(long["code"], "bad_args");
    let mut other = Conn::open(&rig.server.agent);
    let foreign = other.send(&json!({"op": "wait", "grant_id": g, "seconds": 0}));
    assert_eq!(foreign["code"], "lease_not_held", "{foreign}");
    rig.snap("s1");
    let refreshed = rig.conn.send(&json!({"op": "refresh", "grant_id": g}));
    assert_eq!(refreshed["ok"], true, "{refreshed}");
    assert!(rig.inner().supervisor.launcher().starts >= 2);
    let bye = rig.conn.send(&json!({"op": "disconnect"}));
    assert_eq!(bye["ok"], true);
    let gone = rig.conn.send(&json!({"op": "connect", "grant_id": g}));
    assert_eq!(gone["code"], "grant_not_bound");
}
