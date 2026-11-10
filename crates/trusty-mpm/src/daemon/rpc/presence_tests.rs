//! Tests for the `mpm.presence.*` service (#8878 PR2b-1b). The device is
//! SoftToken; the key is the test key, never the Keychain.

use std::path::Path;

use serde_json::{Value, json};
use webauthn_authenticator_rs::prelude::{Url, WebauthnAuthenticator};
use webauthn_authenticator_rs::softtoken::SoftToken;

use super::*;
use crate::core::presence;
use crate::core::presence_enrol::{TicketPurpose, mint_ticket};
use crate::core::presence_test_support::{
    ARCHITECT, NOW, architect_alive, grant_files, key, root, snapshot,
};

const RP: &str = "studio.example.ts.net";
const ORIGIN: &str = "https://studio.example.ts.net";

fn service(root: &Path, verifier: Result<WebauthnVerifier, WebauthnRefusal>) -> PresenceService {
    PresenceService::new(
        Some(root.to_path_buf()),
        Box::new(|| Some(key())),
        Box::new(architect_alive),
        Duration::from_secs(300),
        Box::new(|| NOW),
        verifier,
    )
}

fn live(root: &Path) -> PresenceService {
    service(root, WebauthnVerifier::new(RP, ORIGIN))
}

fn ask(s: &PresenceService) -> Result<Value, Failure> {
    s.call(
        "request",
        json!({"pid": ARCHITECT.pid, "start_time": ARCHITECT.start_time,
            "project": "trusty-tools", "session": "tm-architect", "action": "write config.toml"}),
    )
}

/// Enrol a SoftToken through `register.start` / `register.finish`.
fn enrol(s: &PresenceService) -> (SoftToken, String) {
    let mut token = SoftToken::new(true).expect("softtoken").0;
    let ticket = mint_ticket(&key(), TicketPurpose::Enrol, "iphone", NOW).expect("ticket");
    let started = s
        .call("register.start", json!({ "ticket": ticket }))
        .expect("start");
    let options = serde_json::from_value(started["options"].clone()).expect("options");
    let response = token
        .do_registration(Url::parse(ORIGIN).expect("url"), options)
        .expect("register");
    let done = s
        .call(
            "register.finish",
            json!({ "ticket": ticket, "credential": response }),
        )
        .expect("finish");
    (
        token,
        done["credential_id"].as_str().expect("id").to_owned(),
    )
}

/// Owner ruling D24: a device enrols, but while #10865 has not verified the
/// key ACL neither `approve` nor `verify` runs, and no grant is written.
/// When #10865 makes `acl_verified()` true this test must be revisited.
#[test]
fn an_approval_is_refused_while_the_key_acl_is_unverified_8878() {
    let (_d, root) = root();
    let s = live(&root);
    let (_token, credential_id) = enrol(&s);
    let listed = s.call("devices.list", json!({})).expect("list");
    assert_eq!(
        listed["devices"][0]["credential_id"],
        credential_id.as_str()
    );
    let request = ask(&s).expect("request");
    assert!(
        request["action"]
            .as_str()
            .is_some_and(|a| a.contains("unverified origin"))
    );
    assert_eq!(request["notice"], UNTRUSTED_NOTICE);
    let id = request["id"].as_str().expect("id");
    let before = snapshot(&root);
    let approve = s.call("approve", json!({ "request_id": id }));
    assert_eq!(approve, Err((1, APPROVALS_BLOCKED.to_owned())));
    let verify = s.call(
        "verify",
        json!({ "request_id": id, "credential_id": credential_id,
            "action_hash": "h", "credential": {} }),
    );
    assert_eq!(verify, Err((1, APPROVALS_BLOCKED.to_owned())));
    assert!(grant_files(&root).is_empty());
    assert_eq!(snapshot(&root), before, "the request is not consumed");
    assert!(presence::check(&root, &ARCHITECT, || Some(key()), NOW).is_err());
}

/// A request naming a pid with another start time, or no live process,
/// is refused and writes no pending file.
#[test]
fn a_request_for_a_dead_or_reused_pid_is_refused_8878() {
    let (_d, root) = root();
    let s = live(&root);
    let before = snapshot(&root);
    for (pid, start) in [(ARCHITECT.pid, ARCHITECT.start_time + 1), (9999, 1)] {
        let refused = s.call(
            "request",
            json!({"pid": pid, "start_time": start, "project": "p", "session": "s", "action": "a"}),
        );
        assert!(matches!(refused, Err((1, _))), "{refused:?}");
    }
    assert_eq!(snapshot(&root), before);
}

/// Spoof: a raw-socket caller with no or a forged ticket opens no
/// registration and writes nothing; a verify with no assertion grants nothing.
#[test]
fn register_start_without_a_ticket_writes_nothing_8878() {
    let (_d, root) = root();
    let s = live(&root);
    let before = snapshot(&root);
    let none = s.call("register.start", json!({}));
    assert!(matches!(none, Err((2, _))), "{none:?}");
    let mut forged = mint_ticket(&key(), TicketPurpose::Enrol, "iphone", NOW).expect("ticket");
    forged.subject = "evil".to_owned();
    let refused = s.call("register.start", json!({ "ticket": forged }));
    assert!(matches!(refused, Err((1, _))), "{refused:?}");
    let id = ask(&s).expect("request")["id"].clone();
    let after_request = snapshot(&root);
    let raw = s.call(
        "verify",
        json!({ "request_id": id, "credential_id": "cred", "action_hash": "x", "credential": {} }),
    );
    assert!(raw.is_err(), "{raw:?}");
    assert!(grant_files(&root).is_empty());
    assert_eq!(
        snapshot(&root),
        after_request,
        "the request is not consumed"
    );
    assert_ne!(before, after_request);
}

/// Fail-closed: with no `[presence] rp_id` / `rp_origin` the verifier is
/// unavailable (exit 3) and approve, verify and register write nothing.
#[test]
fn the_service_refuses_when_the_verifier_is_unconfigured_8878() {
    let (_d, root) = root();
    let s = service(
        &root,
        WebauthnVerifier::from_config(&crate::core::presence_config::PresenceConfig::default()),
    );
    let id = ask(&s).expect("a request needs no verifier")["id"].clone();
    let before = snapshot(&root);
    let ticket = mint_ticket(&key(), TicketPurpose::Enrol, "iphone", NOW).expect("ticket");
    for (verb, params) in [
        ("approve", json!({ "request_id": id })),
        ("register.start", json!({ "ticket": ticket })),
        (
            "verify",
            json!({ "request_id": id, "credential_id": "c", "action_hash": "h", "credential": {} }),
        ),
    ] {
        let out = s.call(verb, params);
        assert!(matches!(out, Err((3, _))), "{verb}: {out:?}");
    }
    assert_eq!(snapshot(&root), before);
    assert_eq!(
        s.call("status", json!({})).expect("status")["acl_verified"],
        false
    );
}

#[tokio::test]
async fn the_presence_methods_are_served_8878() {
    let (_d, root) = root();
    let router = register(RpcRouter::new(), Arc::new(live(&root)));
    for method in METHODS {
        assert!(router.method_names().any(|m| m == method), "{method}");
    }
    let frame = json!({"jsonrpc": "2.0", "id": 1, "method": "mpm.presence.request",
        "params": {"pid": 1, "start_time": 1, "project": "p", "session": "s", "action": "a"}});
    let reply = router.dispatch(frame.to_string().as_bytes()).await;
    let error = reply.error.expect("refused");
    assert_eq!(error.data.expect("data")["exit"], 1);
}
