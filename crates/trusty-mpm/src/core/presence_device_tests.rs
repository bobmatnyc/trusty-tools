//! Tests for the sealed device store (#8878 PR2b-1a).

use super::*;
use crate::core::presence_enrol::{TicketPurpose, mint_ticket};
use crate::core::presence_test_support::{FakeDevice, NOW, root, snapshot};

fn key() -> GrantKey {
    crate::core::presence_test_support::key()
}

#[test]
fn an_enrolled_device_loads_back_sealed_8878() {
    let (_d, root) = root();
    let made = FakeDevice::new("cred-A", 1).enrol(&root, 7);
    assert_eq!(load(&root, &key(), "cred-A"), Ok(made.clone()));
    assert_eq!(made.method(), "device:cred-A");
    let listed = list(&root, &key()).expect("list");
    assert_eq!(listed, vec![("cred-A".to_owned(), Ok(made))]);
    assert_eq!(
        load(&root, &key(), "../escape"),
        Err(DeviceError::InvalidId)
    );
}

/// Spoof: enrolment without a valid ticket for the name writes nothing.
#[test]
fn register_start_with_no_ticket_is_refused_8878() {
    let (_d, root) = root();
    let phone = FakeDevice::new("cred-A", 1);
    let before = snapshot(&root);
    let other_name = mint_ticket(&key(), TicketPurpose::Enrol, "ipad", NOW).expect("ticket");
    let refused = enrol(
        &root,
        &key(),
        &other_name,
        phone.new_device("iphone", 0),
        NOW,
    );
    assert!(
        matches!(refused, Err(DeviceError::Ticket(_))),
        "{refused:?}"
    );
    let revoke_ticket = mint_ticket(&key(), TicketPurpose::Revoke, "iphone", NOW).expect("ticket");
    let refused = enrol(
        &root,
        &key(),
        &revoke_ticket,
        phone.new_device("iphone", 0),
        NOW,
    );
    assert!(
        matches!(refused, Err(DeviceError::Ticket(_))),
        "{refused:?}"
    );
    let mut forged = mint_ticket(&key(), TicketPurpose::Enrol, "iphone", NOW).expect("ticket");
    forged.mac = "00".repeat(32);
    let refused = enrol(&root, &key(), &forged, phone.new_device("iphone", 0), NOW);
    assert!(
        matches!(refused, Err(DeviceError::Ticket(_))),
        "{refused:?}"
    );
    assert_eq!(snapshot(&root), before);
}

/// Spoof: revoke without a valid ticket for the credential removes nothing.
#[test]
fn revoke_with_no_ticket_is_refused_8878() {
    let (_d, root) = root();
    FakeDevice::new("cred-A", 1).enrol(&root, 0);
    crate::core::presence::mint(
        &root,
        &crate::core::presence_test_support::ARCHITECT,
        "device:cred-A",
        &key(),
        NOW,
    )
    .expect("mint");
    let before = snapshot(&root);
    let other = mint_ticket(&key(), TicketPurpose::Revoke, "cred-B", NOW).expect("ticket");
    let refused = revoke(&root, &key(), &other, "cred-A", NOW);
    assert!(
        matches!(refused, Err(DeviceError::Ticket(_))),
        "{refused:?}"
    );
    let enrol_ticket = mint_ticket(&key(), TicketPurpose::Enrol, "cred-A", NOW).expect("ticket");
    let refused = revoke(&root, &key(), &enrol_ticket, "cred-A", NOW);
    assert!(
        matches!(refused, Err(DeviceError::Ticket(_))),
        "{refused:?}"
    );
    assert_eq!(snapshot(&root), before);
}

/// Spoof: a hand-written record, or a real one edited, carries no valid seal.
#[test]
fn a_forged_device_record_with_a_bad_seal_is_refused_8878() {
    let (_d, root) = root();
    let made = FakeDevice::new("cred-A", 1).enrol(&root, 0);
    let path = device_path(&root, "cred-A").expect("path");
    // An attacker swaps in their own public key.
    let edited = Device {
        public_key: hex::encode([9u8; 32]),
        ..made.clone()
    };
    crate::core::architect_session::write_owner_only(
        &path,
        &serde_json::to_vec(&edited).expect("json"),
    )
    .expect("write");
    assert_eq!(load(&root, &key(), "cred-A"), Err(DeviceError::Forged));
    // A record sealed with another key.
    let theirs = Device {
        mac: GrantKey::from_bytes([9; 32]).seal(&made.message()),
        ..made.clone()
    };
    crate::core::architect_session::write_owner_only(
        &path,
        &serde_json::to_vec(&theirs).expect("json"),
    )
    .expect("write");
    assert_eq!(load(&root, &key(), "cred-A"), Err(DeviceError::Forged));
    // A real record copied under another credential's file name.
    let copy = device_path(&root, "cred-B").expect("path");
    crate::core::architect_session::write_owner_only(
        &copy,
        &serde_json::to_vec(&made).expect("json"),
    )
    .expect("write");
    assert_eq!(load(&root, &key(), "cred-B"), Err(DeviceError::Malformed));
}

#[test]
fn enrol_refuses_a_duplicate_or_a_bad_name_8878() {
    let (_d, root) = root();
    let phone = FakeDevice::new("cred-A", 1);
    phone.enrol(&root, 0);
    let ticket = mint_ticket(&key(), TicketPurpose::Enrol, "phone-cred-A", NOW).expect("ticket");
    let again = enrol(
        &root,
        &key(),
        &ticket,
        phone.new_device("phone-cred-A", 0),
        NOW,
    );
    assert_eq!(again, Err(DeviceError::Exists));
    let bad = "bad\u{7}name";
    let ticket = mint_ticket(&key(), TicketPurpose::Enrol, bad, NOW).expect("ticket");
    let refused = enrol(
        &root,
        &key(),
        &ticket,
        FakeDevice::new("cred-B", 2).new_device(bad, 0),
        NOW,
    );
    assert_eq!(refused, Err(DeviceError::InvalidName));
}
