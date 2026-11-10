//! #8878 PR2b-1a: device approvals become grants only for the request they
//! sign (plan-pr2b1.md §5, R and S tests). Spoofs run through `spoof!`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::*;
use crate::core::presence::{
    DeviceGrantPolicy, PresenceRefusal, check, check_with_policy, grant_path,
};
use crate::core::presence_enrol::{TicketPurpose, mint_ticket};
use crate::core::presence_test_support::{
    ARCHITECT, FakeDevice, FakeVerifier, NOW, key, request, request_for, root, snapshot, spoof,
    verify_at,
};

type Outcome = (BTreeMap<PathBuf, Vec<u8>>, Result<Granted, VerifyError>);

fn honour(root: &Path, now: u64) -> Result<crate::core::presence::Grant, PresenceRefusal> {
    check_with_policy(
        root,
        &ARCHITECT,
        || Some(key()),
        now,
        DeviceGrantPolicy::Honour,
    )
}

/// R: a valid assertion over the open challenge mints `device:<id>`.
#[test]
fn a_valid_assertion_mints_a_device_grant_8878() {
    let (_d, root) = root();
    let phone = FakeDevice::new("cred-A", 1);
    phone.enrol(&root, 0);
    let pending = request(&root, "write .claude/settings.json");
    let ceremonies = Ceremonies::new();
    let challenge = ceremonies
        .begin(&root, &key(), &pending.id, NOW)
        .expect("begin");
    let assertion = phone.sign(&challenge, &pending, 1, true);
    let granted = verify_at(&root, &ceremonies, &FakeVerifier, &assertion, NOW + 5).expect("grant");
    assert_eq!(granted.path, grant_path(&root, ARCHITECT.pid));
    assert_eq!(granted.method, "device:cred-A");
    assert_eq!(granted.claude, ARCHITECT);
    assert!(
        presence_request::load(&root, &key(), &pending.id, NOW + 5).is_err(),
        "the request is consumed"
    );
    let device = presence_device::load(&root, &key(), "cred-A").expect("device");
    assert_eq!(device.sign_count, 1, "the counter is persisted");
    assert_eq!(
        honour(&root, NOW + 6).expect("valid").method,
        "device:cred-A"
    );
}

/// R + D67: a device grant passes under Honour and is refused while Inert;
/// `check` applies the one switch.
#[test]
fn a_device_grant_passes_presence_check_8878() {
    let (_d, root) = root();
    FakeDevice::new("cred-A", 1).enrol(&root, 0);
    crate::core::presence::mint(&root, &ARCHITECT, "device:cred-A", &key(), NOW).expect("mint");
    assert!(honour(&root, NOW).is_ok());
    // A grant naming a device that was never enrolled is refused.
    crate::core::presence::mint(&root, &ARCHITECT, "device:cred-Z", &key(), NOW).expect("mint");
    assert_eq!(honour(&root, NOW), Err(PresenceRefusal::DeviceRevoked));
}

#[test]
fn device_grants_are_refused_while_inert_8878() {
    let (_d, root) = root();
    FakeDevice::new("cred-A", 1).enrol(&root, 0);
    crate::core::presence::mint(&root, &ARCHITECT, "device:cred-A", &key(), NOW).expect("mint");
    let inert = check_with_policy(
        &root,
        &ARCHITECT,
        || Some(key()),
        NOW,
        DeviceGrantPolicy::Inert,
    );
    assert_eq!(inert, Err(PresenceRefusal::DeviceGrantsInert));
    // `local` is unaffected by the switch.
    crate::core::presence::mint(&root, &ARCHITECT, "local", &key(), NOW).expect("mint");
    let local = check_with_policy(
        &root,
        &ARCHITECT,
        || Some(key()),
        NOW,
        DeviceGrantPolicy::Inert,
    );
    assert!(local.is_ok());
}

/// D67: `check` is `check_with_policy` under `DEVICE_GRANTS`, which ships
/// `Honour` (#8878: owner ruling "Honour device grants now").
#[test]
fn check_applies_the_d67_switch_8878() {
    let (_d, root) = root();
    FakeDevice::new("cred-A", 1).enrol(&root, 0);
    crate::core::presence::mint(&root, &ARCHITECT, "device:cred-A", &key(), NOW).expect("mint");
    assert_eq!(
        crate::core::presence::DEVICE_GRANTS,
        DeviceGrantPolicy::Honour
    );
    let via_switch = check_with_policy(
        &root,
        &ARCHITECT,
        || Some(key()),
        NOW,
        crate::core::presence::DEVICE_GRANTS,
    );
    let shipped = check(&root, &ARCHITECT, || Some(key()), NOW);
    assert_eq!(shipped, via_switch);
    assert_eq!(shipped.map(|g| g.method), Ok("device:cred-A".to_owned()));
}

/// R: revoking device X kills X's live grant at once.
#[test]
fn revoke_kills_a_live_grant_8878() {
    let (_d, root) = root();
    FakeDevice::new("cred-A", 1).enrol(&root, 0);
    crate::core::presence::mint(&root, &ARCHITECT, "device:cred-A", &key(), NOW).expect("mint");
    assert!(honour(&root, NOW).is_ok());
    let ticket = mint_ticket(&key(), TicketPurpose::Revoke, "cred-A", NOW).expect("ticket");
    let revoked = presence_device::revoke(&root, &key(), &ticket, "cred-A", NOW).expect("revoke");
    assert_eq!(
        revoked.grants_removed,
        vec![grant_path(&root, ARCHITECT.pid)]
    );
    assert_eq!(honour(&root, NOW), Err(PresenceRefusal::NoGrant));
}

/// The usual setup: device A enrolled, one request, its challenge open.
fn setup(root: &Path) -> (FakeDevice, presence_request::Pending, Ceremonies, Challenge) {
    let phone = FakeDevice::new("cred-A", 1);
    phone.enrol(root, 3);
    let pending = request(root, "write .claude/settings.json");
    let ceremonies = Ceremonies::new();
    let challenge = ceremonies
        .begin(root, &key(), &pending.id, NOW)
        .expect("begin");
    (phone, pending, ceremonies, challenge)
}

spoof!(
    the_same_assertion_posted_twice_is_refused_the_second_time_8878,
    orchestrator,
    |root: &Path, v: &dyn AssertionVerifier| -> Outcome {
        let (phone, pending, ceremonies, challenge) = setup(root);
        let assertion = phone.sign(&challenge, &pending, 4, true);
        verify_at(root, &ceremonies, v, &assertion, NOW).expect("first");
        let before = snapshot(root);
        (before, verify_at(root, &ceremonies, v, &assertion, NOW + 1))
    }
);

spoof!(
    an_assertion_for_request_a_is_refused_for_b_8878,
    verifier,
    |root: &Path, v: &dyn AssertionVerifier| -> Outcome {
        let (phone, a, ceremonies, challenge_a) = setup(root);
        let other = crate::core::twin_identity::ClaudeProcess {
            start_time: ARCHITECT.start_time,
            ..ARCHITECT
        };
        // B: the same claude, a later request (past the rate window).
        presence_request::claim(root, &key(), &a).expect("retire A");
        let b = request_for(root, other, "write .claude/settings.json", NOW + 200);
        ceremonies
            .begin(root, &key(), &b.id, NOW + 200)
            .expect("begin B");
        let mut assertion = phone.sign(&challenge_a, &a, 4, true);
        assertion.request_id = b.id.clone();
        let before = snapshot(root);
        (
            before,
            verify_at(root, &ceremonies, v, &assertion, NOW + 201),
        )
    }
);

spoof!(
    a_challenge_not_minted_by_the_daemon_is_refused_8878,
    verifier,
    |root: &Path, v: &dyn AssertionVerifier| -> Outcome {
        let (phone, pending, ceremonies, challenge) = setup(root);
        let forged = Challenge {
            bytes: [0xAB; 32],
            ..challenge
        };
        let assertion = phone.sign(&forged, &pending, 4, true);
        let before = snapshot(root);
        (before, verify_at(root, &ceremonies, v, &assertion, NOW))
    }
);

spoof!(
    a_revoked_device_is_refused_8878,
    orchestrator,
    |root: &Path, v: &dyn AssertionVerifier| -> Outcome {
        let (phone, pending, ceremonies, challenge) = setup(root);
        let ticket = mint_ticket(&key(), TicketPurpose::Revoke, "cred-A", NOW).expect("ticket");
        presence_device::revoke(root, &key(), &ticket, "cred-A", NOW).expect("revoke");
        let assertion = phone.sign(&challenge, &pending, 4, true);
        let before = snapshot(root);
        (before, verify_at(root, &ceremonies, v, &assertion, NOW))
    }
);

spoof!(
    an_unknown_credential_is_refused_8878,
    orchestrator,
    |root: &Path, v: &dyn AssertionVerifier| -> Outcome {
        let (_, pending, ceremonies, challenge) = setup(root);
        let stranger = FakeDevice::new("cred-Q", 9);
        let assertion = stranger.sign(&challenge, &pending, 4, true);
        let before = snapshot(root);
        (before, verify_at(root, &ceremonies, v, &assertion, NOW))
    }
);

spoof!(
    an_expired_challenge_is_refused_8878,
    orchestrator,
    |root: &Path, v: &dyn AssertionVerifier| -> Outcome {
        let (phone, pending, ceremonies, challenge) = setup(root);
        let assertion = phone.sign(&challenge, &pending, 4, true);
        let late = NOW + CHALLENGE_TTL.as_secs() + 1;
        let before = snapshot(root);
        (before, verify_at(root, &ceremonies, v, &assertion, late))
    }
);

spoof!(
    an_expired_request_is_refused_8878,
    orchestrator,
    |root: &Path, v: &dyn AssertionVerifier| -> Outcome {
        let (phone, pending, ceremonies, _) = setup(root);
        // A fresh challenge near the end of the request's life.
        let late = pending.expires;
        let challenge = Challenge {
            request_id: pending.id.clone(),
            bytes: [1; 32],
            issued_at: late,
        };
        ceremonies
            .lock()
            .insert(pending.id.clone(), challenge.clone());
        let assertion = phone.sign(&challenge, &pending, 4, true);
        let before = snapshot(root);
        (before, verify_at(root, &ceremonies, v, &assertion, late))
    }
);

spoof!(
    a_sign_count_going_backwards_is_refused_8878,
    verifier,
    |root: &Path, v: &dyn AssertionVerifier| -> Outcome {
        let (phone, pending, ceremonies, challenge) = setup(root);
        // Stored 3; a cloned authenticator reports 2.
        let assertion = phone.sign(&challenge, &pending, 2, true);
        let before = snapshot(root);
        (before, verify_at(root, &ceremonies, v, &assertion, NOW))
    }
);

#[test]
fn a_sign_count_of_zero_on_zero_is_accepted_8878() {
    // Synced passkeys report 0 forever.
    let (_d, root) = root();
    let phone = FakeDevice::new("cred-S", 2);
    phone.enrol(&root, 0);
    let pending = request(&root, "write .claude/settings.json");
    let ceremonies = Ceremonies::new();
    let challenge = ceremonies
        .begin(&root, &key(), &pending.id, NOW)
        .expect("begin");
    let assertion = phone.sign(&challenge, &pending, 0, true);
    verify_at(&root, &ceremonies, &FakeVerifier, &assertion, NOW).expect("0 on 0 passes");
}

spoof!(
    user_verification_clear_is_refused_8878,
    verifier,
    |root: &Path, v: &dyn AssertionVerifier| -> Outcome {
        let (phone, pending, ceremonies, challenge) = setup(root);
        let assertion = phone.sign(&challenge, &pending, 4, false);
        let before = snapshot(root);
        (before, verify_at(root, &ceremonies, v, &assertion, NOW))
    }
);

spoof!(
    a_request_naming_another_pid_cannot_grant_it_8878,
    orchestrator,
    |root: &Path, v: &dyn AssertionVerifier| -> Outcome {
        // A request for a PID that is not a live `claude` at that start time.
        let phone = FakeDevice::new("cred-A", 1);
        phone.enrol(root, 3);
        let ghost = crate::core::twin_identity::ClaudeProcess {
            pid: 999,
            start_time: 5,
        };
        let pending = request_for(root, ghost, "write .claude/settings.json", NOW);
        let ceremonies = Ceremonies::new();
        let challenge = ceremonies
            .begin(root, &key(), &pending.id, NOW)
            .expect("begin");
        let assertion = phone.sign(&challenge, &pending, 4, true);
        let before = snapshot(root);
        (before, verify_at(root, &ceremonies, v, &assertion, NOW))
    }
);

spoof!(
    an_assertion_shown_another_action_is_refused_8878,
    orchestrator,
    |root: &Path, v: &dyn AssertionVerifier| -> Outcome {
        let (phone, pending, ceremonies, challenge) = setup(root);
        let mut assertion = phone.sign(&challenge, &pending, 4, true);
        assertion.action_hash = presence_request::action_hash("something harmless");
        let before = snapshot(root);
        (before, verify_at(root, &ceremonies, v, &assertion, NOW))
    }
);

spoof!(
    a_raw_uds_verify_with_no_assertion_grants_nothing_8878,
    verifier,
    |root: &Path, v: &dyn AssertionVerifier| -> Outcome {
        let (_, pending, ceremonies, _) = setup(root);
        let empty = Assertion {
            request_id: pending.id.clone(),
            credential_id: "cred-A".to_owned(),
            action_hash: pending.action_hash.clone(),
            payload: Vec::new(),
        };
        let before = snapshot(root);
        (before, verify_at(root, &ceremonies, v, &empty, NOW))
    }
);

/// Spoof: a device grant copied to another PID's file is not that PID's.
#[test]
fn a_device_grant_copied_across_pids_is_refused_8878() {
    let (_d, root) = root();
    FakeDevice::new("cred-A", 1).enrol(&root, 0);
    crate::core::presence::mint(&root, &ARCHITECT, "device:cred-A", &key(), NOW).expect("mint");
    let other = crate::core::twin_identity::ClaudeProcess {
        pid: 777,
        ..ARCHITECT
    };
    std::fs::copy(
        grant_path(&root, ARCHITECT.pid),
        grant_path(&root, other.pid),
    )
    .expect("copy");
    let refused = check_with_policy(
        &root,
        &other,
        || Some(key()),
        NOW,
        DeviceGrantPolicy::Honour,
    );
    assert_eq!(refused, Err(PresenceRefusal::OtherProcess));
}

/// The StubVerifier PR2b-1a ships refuses every assertion.
#[test]
fn the_shipped_stub_verifier_grants_nothing_8878() {
    let (_d, root) = root();
    let (phone, pending, ceremonies, challenge) = setup(&root);
    let assertion = phone.sign(&challenge, &pending, 4, true);
    let before = snapshot(&root);
    let refused = verify_at(&root, &ceremonies, &StubVerifier, &assertion, NOW);
    assert!(
        matches!(refused, Err(VerifyError::Rejected(_))),
        "{refused:?}"
    );
    assert_eq!(snapshot(&root), before);
}

/// #8878: challenges never finished do not pile up; a later `begin` drops
/// every one past the TTL.
#[test]
fn begin_drops_challenges_older_than_the_ttl_8878() {
    let (_d, root) = root();
    let old = request(&root, "a");
    let ceremonies = Ceremonies::new();
    ceremonies
        .begin(&root, &key(), &old.id, NOW)
        .expect("begin");
    let later = NOW + CHALLENGE_TTL.as_secs() + 1;
    let fresh = request_for(&root, ARCHITECT, "b", later);
    ceremonies
        .begin(&root, &key(), &fresh.id, later)
        .expect("begin");
    let open = ceremonies.lock();
    assert!(!open.contains_key(&old.id), "the stale challenge is gone");
    assert!(open.contains_key(&fresh.id));
    assert_eq!(open.len(), 1);
}
