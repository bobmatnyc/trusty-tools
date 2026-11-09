//! Tests for local-presence tickets (#8878 PR2b-1a).

use super::*;
use crate::core::presence_test_support::{NOW, key};

#[test]
fn a_minted_ticket_authorises_its_operation_once_checked() {
    let ticket = mint_ticket(&key(), TicketPurpose::Enrol, "iphone", NOW).expect("mint");
    assert_eq!(ticket.expires_at, NOW + TICKET_TTL.as_secs());
    assert_eq!(
        check_ticket(&key(), &ticket, TicketPurpose::Enrol, "iphone", NOW + 599),
        Ok(())
    );
}

/// Spoof: a ticket names one operation on one device for ten minutes.
#[test]
fn a_ticket_for_another_name_or_expired_is_refused_8878() {
    let ticket = mint_ticket(&key(), TicketPurpose::Enrol, "iphone", NOW).expect("mint");
    let at = |t: &Ticket, p, s, now| check_ticket(&key(), t, p, s, now);
    assert_eq!(
        at(&ticket, TicketPurpose::Enrol, "ipad", NOW),
        Err(TicketRefusal::WrongSubject)
    );
    assert_eq!(
        at(&ticket, TicketPurpose::Revoke, "iphone", NOW),
        Err(TicketRefusal::WrongPurpose)
    );
    assert_eq!(
        at(
            &ticket,
            TicketPurpose::Enrol,
            "iphone",
            NOW + TICKET_TTL.as_secs() + 1
        ),
        Err(TicketRefusal::Expired)
    );
    // A ticket minted in the future is out of range.
    let future = mint_ticket(&key(), TicketPurpose::Enrol, "iphone", NOW + 3600).expect("mint");
    assert_eq!(
        at(&future, TicketPurpose::Enrol, "iphone", NOW),
        Err(TicketRefusal::BadDates)
    );
}

/// Spoof: a ticket edited after sealing, or sealed with another key.
#[test]
fn a_forged_ticket_is_refused_8878() {
    let mut ticket = mint_ticket(&key(), TicketPurpose::Revoke, "cred-A", NOW).expect("mint");
    ticket.expires_at += 86_400;
    assert_eq!(
        check_ticket(&key(), &ticket, TicketPurpose::Revoke, "cred-A", NOW),
        Err(TicketRefusal::Forged)
    );
    let other = GrantKey::from_bytes([9; 32]);
    let theirs = mint_ticket(&other, TicketPurpose::Revoke, "cred-A", NOW).expect("mint");
    assert_eq!(
        check_ticket(&key(), &theirs, TicketPurpose::Revoke, "cred-A", NOW),
        Err(TicketRefusal::Forged)
    );
}

#[test]
fn the_enrol_url_carries_the_ticket_in_its_fragment_8878() {
    use base64::Engine as _;
    let ticket = mint_ticket(&key(), TicketPurpose::Enrol, "iphone", NOW).expect("ticket");
    let url = enrol_url("https://s.ts.net/", &ticket).expect("url");
    let (page, fragment) = url.split_once('#').expect("a fragment");
    assert_eq!(page, "https://s.ts.net/tools/presence/enrol/");
    assert!(
        !page.contains(&ticket.nonce),
        "the ticket stays out of the path"
    );
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(fragment)
        .expect("base64url");
    let back: Ticket = serde_json::from_slice(&raw).expect("ticket json");
    assert_eq!(back, ticket);
}
