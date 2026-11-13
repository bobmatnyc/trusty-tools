//! Tests for the passkey adapter (#8878 PR2b-1b). The device is SoftToken, a
//! software authenticator; the key is the test key, never the Keychain.
//! Every refusal asserts no grant and an unchanged anchor.

use serde_json::Value;
use webauthn_authenticator_rs::prelude::{RequestChallengeResponse, Url, WebauthnAuthenticator};
use webauthn_authenticator_rs::softtoken::SoftToken;

use super::*;
use crate::core::presence;
use crate::core::presence_enrol::mint_ticket;
use crate::core::presence_request::Pending;
use crate::core::presence_test_support::{
    ARCHITECT, NOW, grant_files, key, request, root, snapshot, verify_at,
};
use crate::core::presence_verify::{Ceremonies, Granted, VerifyError};

const RP: &str = "studio.example.ts.net";
const ORIGIN: &str = "https://studio.example.ts.net";

fn url(s: &str) -> Url {
    Url::parse(s).expect("url")
}

/// A SoftToken that claims user verification, as a phone's Face ID does.
fn token() -> SoftToken {
    SoftToken::new(true).expect("softtoken").0
}

/// Enrol a SoftToken passkey through a fresh ticket.
fn enrolled(root: &Path) -> (WebauthnVerifier, SoftToken, Device) {
    let v = WebauthnVerifier::new(RP, ORIGIN).expect("verifier");
    let mut t = token();
    let ticket = mint_ticket(&key(), TicketPurpose::Enrol, "iphone", NOW).expect("ticket");
    let options = v.start_registration(&key(), &ticket, NOW).expect("start");
    let response = t.do_registration(url(ORIGIN), options).expect("register");
    let device = v
        .finish_registration(root, &key(), &ticket, &response, NOW)
        .expect("finish");
    (v, t, device)
}

/// Ask `t` to approve `pending`, `edit` changing the options first and
/// `origin` being where the browser says it runs.
fn approve(
    root: &Path,
    v: &WebauthnVerifier,
    t: &mut SoftToken,
    device: &Device,
    pending: &Pending,
    origin: &str,
    edit: impl FnOnce(&mut Value),
) -> Result<Granted, VerifyError> {
    let ceremonies = Ceremonies::new();
    let challenge = ceremonies.begin(root, &key(), &pending.id, NOW)?;
    let options = v
        .start_authentication(&challenge, std::slice::from_ref(device), NOW)
        .expect("start auth");
    let mut raw = serde_json::to_value(options).expect("json");
    edit(&mut raw);
    let options: RequestChallengeResponse = serde_json::from_value(raw).expect("options");
    let Ok(credential) = t.do_authentication(url(origin), options) else {
        return Err(VerifyError::Rejected(
            "the authenticator refused".to_owned(),
        ));
    };
    let assertion = Assertion {
        request_id: pending.id.clone(),
        credential_id: device.credential_id.clone(),
        action_hash: pending.action_hash.clone(),
        payload: serde_json::to_vec(&credential).expect("payload"),
    };
    verify_at(root, &ceremonies, v, &assertion, NOW + 1)
}

/// Every `"ES256"` in `v` becomes `"RS256"`.
fn as_rs256(v: &mut Value) {
    match v {
        Value::String(s) if s == "ES256" => *s = "RS256".to_owned(),
        Value::Array(a) => a.iter_mut().for_each(as_rs256),
        Value::Object(o) => o.values_mut().for_each(as_rs256),
        _ => {}
    }
}

#[test]
fn the_verifier_needs_an_rp_id_and_origin_8878() {
    let none = WebauthnVerifier::from_config(&PresenceConfig::default());
    assert!(matches!(none, Err(WebauthnRefusal::Unavailable(_))));
    let mismatched = WebauthnVerifier::new(RP, "https://evil.example");
    assert!(matches!(mismatched, Err(WebauthnRefusal::Unavailable(_))));
}

#[test]
fn registration_round_trip_stores_a_sealed_device_8878() {
    let (_d, root) = root();
    let (_, _, device) = enrolled(&root);
    assert_eq!(
        presence_device::load(&root, &key(), &device.credential_id),
        Ok(device.clone())
    );
    assert_eq!(device.name, "iphone");
    let passkey: Passkey = serde_json::from_str(&device.public_key).expect("passkey");
    assert!(is_es256(&passkey));
}

/// Q4 / plan §5: registration options need a valid enrol ticket.
#[test]
fn register_start_needs_an_enrol_ticket_8878() {
    let v = WebauthnVerifier::new(RP, ORIGIN).expect("verifier");
    let revoke = mint_ticket(&key(), TicketPurpose::Revoke, "iphone", NOW).expect("ticket");
    assert!(matches!(
        v.start_registration(&key(), &revoke, NOW),
        Err(WebauthnRefusal::Ticket(_))
    ));
    let mut forged = mint_ticket(&key(), TicketPurpose::Enrol, "iphone", NOW).expect("ticket");
    forged.subject = "ipad".to_owned();
    assert!(matches!(
        v.start_registration(&key(), &forged, NOW),
        Err(WebauthnRefusal::Ticket(_))
    ));
}

/// D72: a non-ES256 credential never becomes a device record.
#[test]
fn a_non_es256_passkey_is_refused_at_registration_8878() {
    let (_d, root) = root();
    let (_, _, device) = enrolled(&root);
    let mut raw: Value = serde_json::from_str(&device.public_key).expect("json");
    as_rs256(&mut raw);
    let rs256: Passkey = serde_json::from_value(raw).expect("passkey");
    let (_d2, root2) = crate::core::presence_test_support::root();
    let before = snapshot(&root2);
    let ticket = mint_ticket(&key(), TicketPurpose::Enrol, "ipad", NOW).expect("ticket");
    let refused = enrol_passkey(&root2, &key(), &ticket, &rs256, NOW);
    assert_eq!(refused, Err(WebauthnRefusal::NotEs256));
    assert_eq!(snapshot(&root2), before, "no device record is written");
}

/// D72: a sealed record holding a non-ES256 key approves nothing.
#[test]
fn a_non_es256_device_record_is_refused_at_verify_8878() {
    let (_d, root) = root();
    let (v, mut t, device) = enrolled(&root);
    let mut raw: Value = serde_json::from_str(&device.public_key).expect("json");
    as_rs256(&mut raw);
    // Re-seal the record with the RS256 key under the test key.
    std::fs::remove_file(presence_device::device_path(&root, &device.credential_id).expect("p"))
        .expect("rm");
    let ticket = mint_ticket(&key(), TicketPurpose::Enrol, "iphone2", NOW).expect("ticket");
    let new = NewDevice {
        credential_id: device.credential_id.clone(),
        name: "iphone2".to_owned(),
        public_key: raw.to_string(),
        sign_count: 0,
        backup_eligible: false,
        backup_state: false,
    };
    let rs_device = presence_device::enrol(&root, &key(), &ticket, new, NOW).expect("enrol");
    let pending = request(&root, "write config.toml");
    let before = snapshot(&root);
    let refused = approve(&root, &v, &mut t, &device, &pending, ORIGIN, |_| {});
    assert!(rs_device.public_key.contains("RS256"));
    assert!(
        matches!(&refused, Err(VerifyError::Rejected(m)) if m.contains("ES256")),
        "{refused:?}"
    );
    assert_eq!(snapshot(&root), before, "no grant, the request stays");
}

#[test]
fn authentication_round_trip_mints_a_grant_8878() {
    let (_d, root) = root();
    let (v, mut t, device) = enrolled(&root);
    let pending = request(&root, "write config.toml");
    let granted = approve(&root, &v, &mut t, &device, &pending, ORIGIN, |_| {}).expect("grant");
    assert_eq!(granted.claude, ARCHITECT);
    assert_eq!(granted.method, device.method());
    let check = presence::check(&root, &ARCHITECT, || Some(key()), NOW + 2);
    assert!(check.is_ok(), "{check:?}");
}

/// Assert `outcome` is a refusal that wrote no grant and kept the request.
fn refused(
    root: &Path,
    before: &std::collections::BTreeMap<std::path::PathBuf, Vec<u8>>,
    outcome: &Result<Granted, VerifyError>,
) {
    assert!(outcome.is_err(), "the spoof was granted: {outcome:?}");
    assert!(grant_files(root).is_empty());
    assert_eq!(
        &snapshot(root),
        before,
        "a refusal writes and consumes nothing"
    );
}

#[test]
fn webauthn_user_verification_clear_is_refused_8878() {
    let (_d, root) = root();
    let (v, mut t, device) = enrolled(&root);
    let pending = request(&root, "write config.toml");
    let before = snapshot(&root);
    let outcome = approve(&root, &v, &mut t, &device, &pending, ORIGIN, |o| {
        o["publicKey"]["userVerification"] = Value::from("preferred");
    });
    refused(&root, &before, &outcome);
}

#[test]
fn wrong_origin_is_refused_8878() {
    let (_d, root) = root();
    let (v, mut t, device) = enrolled(&root);
    let pending = request(&root, "write config.toml");
    let before = snapshot(&root);
    let outcome = approve(
        &root,
        &v,
        &mut t,
        &device,
        &pending,
        "https://evil.studio.example.ts.net",
        |_| {},
    );
    refused(&root, &before, &outcome);
}

#[test]
fn wrong_rp_id_is_refused_8878() {
    let (_d, root) = root();
    let (v, mut t, device) = enrolled(&root);
    let pending = request(&root, "write config.toml");
    let before = snapshot(&root);
    let outcome = approve(
        &root,
        &v,
        &mut t,
        &device,
        &pending,
        "https://evil.example",
        |o| {
            o["publicKey"]["rpId"] = Value::from("evil.example");
        },
    );
    refused(&root, &before, &outcome);
}

/// Architect Q3: an assertion over request A's challenge does not answer
/// request B, even from the same device.
#[test]
fn a_webauthn_assertion_for_request_a_is_refused_for_b_8878() {
    let (_d, root) = root();
    let (v, mut t, device) = enrolled(&root);
    // Both requests are the Architect's, B made past the rate window.
    let b = crate::core::presence_test_support::request_for(
        &root,
        ARCHITECT,
        "write settings.json",
        NOW - 130,
    );
    let a = request(&root, "write config.toml");
    let ceremonies = Ceremonies::new();
    let ca = ceremonies.begin(&root, &key(), &a.id, NOW).expect("a");
    let cb = ceremonies.begin(&root, &key(), &b.id, NOW).expect("b");
    v.start_authentication(&cb, std::slice::from_ref(&device), NOW)
        .expect("auth b");
    let options = v
        .start_authentication(&ca, std::slice::from_ref(&device), NOW)
        .expect("auth");
    let credential = t.do_authentication(url(ORIGIN), options).expect("sign");
    let before = snapshot(&root);
    let assertion = Assertion {
        request_id: b.id.clone(),
        credential_id: device.credential_id.clone(),
        action_hash: b.action_hash.clone(),
        payload: serde_json::to_vec(&credential).expect("payload"),
    };
    let outcome = verify_at(&root, &ceremonies, &v, &assertion, NOW + 1);
    assert!(
        matches!(&outcome, Err(VerifyError::Rejected(_))),
        "the verifier refuses it: {outcome:?}"
    );
    refused(&root, &before, &outcome);
}

/// Replay: the same signed assertion posted twice grants once.
#[test]
fn a_webauthn_assertion_replayed_is_refused_8878() {
    let (_d, root) = root();
    let (v, mut t, device) = enrolled(&root);
    let pending = request(&root, "write config.toml");
    let ceremonies = Ceremonies::new();
    let challenge = ceremonies
        .begin(&root, &key(), &pending.id, NOW)
        .expect("begin");
    let options = v
        .start_authentication(&challenge, std::slice::from_ref(&device), NOW)
        .expect("auth");
    let credential = t.do_authentication(url(ORIGIN), options).expect("sign");
    let assertion = Assertion {
        request_id: pending.id.clone(),
        credential_id: device.credential_id.clone(),
        action_hash: pending.action_hash.clone(),
        payload: serde_json::to_vec(&credential).expect("payload"),
    };
    verify_at(&root, &ceremonies, &v, &assertion, NOW + 1).expect("first");
    let before = snapshot(&root);
    let again = verify_at(&root, &ceremonies, &v, &assertion, NOW + 2);
    assert!(again.is_err(), "{again:?}");
    assert_eq!(snapshot(&root), before);
}
