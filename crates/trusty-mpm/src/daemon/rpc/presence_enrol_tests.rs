//! Fail-open tests for the enrolment finish (#8878 PR2b-2). The device is
//! SoftToken and the key is the test key, never the Keychain. Every refusal
//! asserts that no device record was written; each test names the broken
//! variant it was run red against (the Fail-Open Check in the PR body).

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};
use webauthn_authenticator_rs::prelude::{CreationChallengeResponse, Url, WebauthnAuthenticator};
use webauthn_authenticator_rs::softtoken::SoftToken;

use super::*;
use crate::core::presence_device::{DeviceError, NewDevice};
use crate::core::presence_webauthn::ES256_ALG;
use crate::core::presence_enrol::{TICKET_TTL, TicketPurpose, mint_ticket};
use crate::core::presence_test_support::{NOW, architect_alive, key, root};

const RP: &str = "studio.example.ts.net";
const ORIGIN: &str = "https://studio.example.ts.net";

/// A service over `root` whose clock reads `clock`.
fn service_at(root: &Path, clock: &Arc<AtomicU64>) -> PresenceService {
    let clock = Arc::clone(clock);
    PresenceService::new(
        Some(root.to_path_buf()),
        Box::new(|| Some(key())),
        Box::new(architect_alive),
        Duration::from_secs(300),
        Box::new(move || clock.load(Ordering::SeqCst)),
        WebauthnVerifier::new(RP, ORIGIN),
    )
}

fn clock(at: u64) -> Arc<AtomicU64> {
    Arc::new(AtomicU64::new(at))
}

fn ticket_at(name: &str, at: u64) -> Ticket {
    mint_ticket(&key(), TicketPurpose::Enrol, name, at).expect("ticket")
}

/// Reseal `ticket` after an edit, as only a holder of the key could.
fn resealed(mut ticket: Ticket) -> Ticket {
    ticket.mac = key().seal(&ticket.message());
    ticket
}

/// `register.start` for `ticket`, as options SoftToken can answer.
fn start(s: &PresenceService, ticket: &Ticket) -> Result<CreationChallengeResponse, Failure> {
    let reply = s.call("register.start", json!({ "ticket": ticket }))?;
    Ok(serde_json::from_value(reply["options"].clone()).expect("options"))
}

/// SoftToken's answer to `options`, as a browser at `origin` would post it.
fn answer(options: CreationChallengeResponse, origin: &str) -> Value {
    let mut token = SoftToken::new(true).expect("softtoken").0;
    let response = token
        .do_registration(Url::parse(origin).expect("url"), options)
        .expect("register");
    serde_json::to_value(response).expect("json")
}

fn finish(s: &PresenceService, ticket: &Ticket, credential: &Value) -> Result<Value, Failure> {
    s.call(
        "register.finish",
        json!({ "ticket": ticket, "credential": credential }),
    )
}

fn devices(root: &Path) -> usize {
    presence_device::list(root, &key()).expect("list").len()
}

fn assert_refused(out: &Result<Value, Failure>, needle: &str) {
    match out {
        Err((1, message)) => assert!(message.contains(needle), "{message}"),
        other => panic!("expected a refusal naming `{needle}`, got {other:?}"),
    }
}

/// Q3: the link is single use. After one enrolment the same ticket opens no
/// second registration and finishes nothing. Red against: no spent check in
/// `register.start`.
#[test]
fn a_spent_ticket_is_refused_at_start_and_finish_8878() {
    let (_d, root) = root();
    let s = service_at(&root, &clock(NOW));
    let ticket = ticket_at("iphone", NOW);
    let credential = answer(start(&s, &ticket).expect("start"), ORIGIN);
    finish(&s, &ticket, &credential).expect("first finish enrols");
    assert_eq!(devices(&root), 1);
    assert!(presence_device::ticket_spent(&root, &ticket));
    assert_refused(&s.call("register.start", json!({ "ticket": ticket })), "already used");
    assert!(finish(&s, &ticket, &credential).is_err());
    assert_eq!(devices(&root), 1, "no second record");
}

/// An expired link opens nothing, and a ticket that expires between start
/// and finish enrols nothing. Red against: no `now > expires_at` check.
#[test]
fn an_expired_ticket_is_refused_at_start_and_finish_8878() {
    let (_d, root) = root();
    let ttl = TICKET_TTL.as_secs();
    let now = clock(NOW);
    let s = service_at(&root, &now);
    let stale = ticket_at("iphone", NOW - ttl - 1);
    assert_refused(&s.call("register.start", json!({ "ticket": stale })), "expired");
    let ending = ticket_at("iphone", NOW - ttl + 10);
    let credential = answer(start(&s, &ending).expect("start"), ORIGIN);
    now.store(NOW + 11, Ordering::SeqCst);
    assert_refused(&finish(&s, &ending, &credential), "expired");
    assert_eq!(devices(&root), 0);
    assert!(!presence_device::ticket_spent(&root, &ending));
}

/// The ceremony is consumed before the library check, so a failed finish
/// cannot be retried against the same challenge; the ticket stays unspent,
/// so a fresh start still works. Red against: `finish_registration` reading
/// the open state instead of removing it.
#[test]
fn a_failed_finish_consumes_the_ceremony_and_spends_nothing_8878() {
    let (_d, root) = root();
    let s = service_at(&root, &clock(NOW));
    let ticket = ticket_at("iphone", NOW);
    let options = start(&s, &ticket).expect("start");
    let good = answer(options, ORIGIN);
    let mut broken = good.clone();
    // `{}` as base64url: client data with no challenge or origin.
    broken["response"]["clientDataJSON"] = json!("e30");
    assert_refused(&finish(&s, &ticket, &broken), "refused");
    assert_refused(&finish(&s, &ticket, &good), "no ceremony");
    assert_eq!(devices(&root), 0);
    assert!(!presence_device::ticket_spent(&root, &ticket));
    let retry = answer(start(&s, &ticket).expect("restart"), ORIGIN);
    finish(&s, &ticket, &retry).expect("a fresh start enrols");
    assert_eq!(devices(&root), 1);
}

/// The ticket is spent before the record is written, so a failed write
/// leaves a spent ticket and no device, never a device with a live ticket.
/// The record path is too long for the file system, which fails the write
/// after the spend. Red against: writing the record before the spend.
#[test]
fn a_record_write_failure_leaves_the_ticket_spent_and_no_device_8878() {
    let (_d, root) = root();
    let ticket = ticket_at("iphone", NOW);
    let new = NewDevice {
        credential_id: "A".repeat(300),
        name: "iphone".to_owned(),
        public_key: "{}".to_owned(),
        sign_count: 0,
        backup_eligible: false,
        backup_state: false,
    };
    let out = presence_device::enrol(&root, &key(), &ticket, new.clone(), NOW);
    assert!(matches!(out, Err(DeviceError::Write(_))), "{out:?}");
    assert_eq!(devices(&root), 0);
    assert!(presence_device::ticket_spent(&root, &ticket));
    let again = presence_device::enrol(&root, &key(), &ticket, new, NOW);
    assert!(
        matches!(again, Err(DeviceError::Ticket(TicketRefusal::Spent))),
        "{again:?}"
    );
}

/// A ticket for another name cannot finish the ceremony opened for this
/// one: not one with its own nonce, and not one resealed onto this nonce.
/// Red against: dropping the `name == subject` check in
/// `finish_registration`.
#[test]
fn a_ticket_for_another_name_cannot_finish_this_ceremony_8878() {
    let (_d, root) = root();
    let s = service_at(&root, &clock(NOW));
    let iphone = ticket_at("iphone", NOW);
    let credential = answer(start(&s, &iphone).expect("start"), ORIGIN);
    let ipad = ticket_at("ipad", NOW);
    assert_refused(&finish(&s, &ipad, &credential), "no ceremony");
    let renamed = resealed(Ticket {
        nonce: iphone.nonce.clone(),
        ..ipad
    });
    assert!(finish(&s, &renamed, &credential).is_err());
    assert_eq!(devices(&root), 0);
}

/// Clock skew: a ticket dated more than a minute ahead, or valid longer
/// than the TTL, opens nothing. Red against: dropping either date clause
/// of `check_ticket`.
#[test]
fn a_ticket_dated_ahead_or_valid_too_long_is_refused_8878() {
    let (_d, root) = root();
    let s = service_at(&root, &clock(NOW));
    let ahead = ticket_at("iphone", NOW + 61);
    assert_refused(&s.call("register.start", json!({ "ticket": ahead })), "out of range");
    let long = resealed(Ticket {
        expires_at: NOW + TICKET_TTL.as_secs() + 1,
        ..ticket_at("iphone", NOW)
    });
    assert_refused(&s.call("register.start", json!({ "ticket": long })), "out of range");
    assert_eq!(devices(&root), 0);
}

/// A daemon restart between start and finish drops the ceremony: the new
/// daemon refuses the finish and enrols nothing, and the ticket stays
/// usable. Red against: ceremony state that outlives the service.
#[test]
fn a_daemon_restart_between_start_and_finish_enrols_nothing_8878() {
    let (_d, root) = root();
    let now = clock(NOW);
    let before = service_at(&root, &now);
    let ticket = ticket_at("iphone", NOW);
    let credential = answer(start(&before, &ticket).expect("start"), ORIGIN);
    drop(before);
    let after = service_at(&root, &now);
    assert_refused(&finish(&after, &ticket, &credential), "no ceremony");
    assert_eq!(devices(&root), 0);
    let retry = answer(start(&after, &ticket).expect("restart"), ORIGIN);
    finish(&after, &ticket, &retry).expect("enrols after a fresh start");
}

/// D72: the options offer ES256 only. Red against: dropping the `retain`.
#[test]
fn register_start_offers_es256_only_8878() {
    let (_d, root) = root();
    let s = service_at(&root, &clock(NOW));
    let reply = s
        .call("register.start", json!({ "ticket": ticket_at("iphone", NOW) }))
        .expect("start");
    let params = reply["options"]["publicKey"]["pubKeyCredParams"]
        .as_array()
        .expect("params");
    assert!(!params.is_empty());
    assert!(params.iter().all(|p| p["alg"] == ES256_ALG), "{params:?}");
    assert!(reply.get("incomplete").is_none());
}

/// Q4: `status` names the enrol origin only when the switch put one there.
#[test]
fn status_names_the_enrol_origin_only_when_switched_on_8878() {
    let (_d, root) = root();
    let off = service_at(&root, &clock(NOW));
    assert_eq!(off.call("status", json!({})).expect("status")["enrol_origin"], Value::Null);
    let on = service_at(&root, &clock(NOW)).with_enrol_origin(Some(ORIGIN.to_owned()));
    assert_eq!(on.call("status", json!({})).expect("status")["enrol_origin"], ORIGIN);
}

/// No software authenticator or test clock reaches a production build:
/// SoftToken is a dev-dependency only, the test support module is
/// `cfg(test)`, and the live service reads the system clock.
#[test]
fn softtoken_and_test_clocks_stay_out_of_production_8878() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest: toml::Value =
        toml::from_str(&std::fs::read_to_string(dir.join("Cargo.toml")).expect("manifest"))
            .expect("toml");
    let soft = "webauthn-authenticator-rs";
    assert!(manifest["dependencies"].get(soft).is_none(), "{soft} in [dependencies]");
    assert!(manifest["dev-dependencies"].get(soft).is_some());
    let core = std::fs::read_to_string(dir.join("src/core/mod.rs")).expect("core/mod.rs");
    assert!(core.contains("#[cfg(test)]\npub(crate) mod presence_test_support;"));
    let rpc = std::fs::read_to_string(dir.join("src/daemon/rpc/presence.rs")).expect("rpc");
    let live = rpc.split("pub fn live()").nth(1).expect("live()");
    let live = live.split("\n    }\n").next().expect("body");
    assert!(live.contains("std::time::SystemTime::now()"), "{live}");
}
