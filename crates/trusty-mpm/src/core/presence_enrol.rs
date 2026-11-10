//! Local-presence tickets for enrolling and revoking devices (#8878 PR2b-1a).
//!
//! Why: plan-pr2b1.md §1 — `tm presence devices add|revoke` runs Touch ID at
//! the Studio, but the device store must not trust that the CLI ran: a caller
//! speaking to the store directly would skip the prompt. The CLI therefore
//! mints a short-lived ticket after LocalAuthentication answered `Granted`,
//! and the store refuses an enrol or revoke without a valid one.
//! What: [`Ticket`] names one [`TicketPurpose`] and one subject (the device
//! name for an enrol, the credential id for a revoke), a random nonce and an
//! expiry [`TICKET_TTL`] after issue, HMAC-sealed with the presence
//! [`GrantKey`]. [`check_ticket`] refuses a bad seal, another purpose or
//! subject, an expired ticket, or one valid for longer than the TTL.
//! Test: `presence_enrol_tests.rs`.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::core::presence_key::GrantKey;
use crate::core::presence_store::{random_hex, sealed_message};

/// How long a ticket stays valid after it is minted.
pub const TICKET_TTL: Duration = Duration::from_secs(10 * 60);

/// Clock skew tolerated for a ticket's issue time, in seconds.
const SKEW: u64 = 60;

/// The seal's record tag.
const TAG: &str = "tm-presence-ticket-v1";

/// What a ticket authorises.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TicketPurpose {
    /// Enrol a device under the subject name.
    Enrol,
    /// Revoke the device whose credential id is the subject.
    Revoke,
}

impl TicketPurpose {
    fn as_str(self) -> &'static str {
        match self {
            Self::Enrol => "enrol",
            Self::Revoke => "revoke",
        }
    }
}

/// One local-presence ticket, as passed from the CLI to the store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ticket {
    /// What the ticket authorises.
    pub purpose: TicketPurpose,
    /// The device name (enrol) or credential id (revoke).
    pub subject: String,
    /// 128 random bits, hex.
    pub nonce: String,
    /// When the ticket was minted, Unix seconds.
    pub issued_at: u64,
    /// When it stops being valid, Unix seconds.
    pub expires_at: u64,
    /// HMAC-SHA256 over every other field, hex.
    pub mac: String,
}

impl Ticket {
    /// The bytes the seal covers (tests reseal an edited ticket with it).
    pub(crate) fn message(&self) -> Vec<u8> {
        sealed_message(
            TAG,
            &[
                self.purpose.as_str(),
                &self.subject,
                &self.nonce,
                &self.issued_at.to_string(),
                &self.expires_at.to_string(),
            ],
        )
    }
}

/// Why a ticket does not authorise the operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TicketRefusal {
    /// The seal does not verify: the ticket was not minted by tm.
    #[error("the presence ticket's seal does not verify")]
    Forged,
    /// The ticket authorises another operation.
    #[error("the presence ticket is for another operation")]
    WrongPurpose,
    /// The ticket names another device.
    #[error("the presence ticket names another device")]
    WrongSubject,
    /// The ticket's validity window has passed.
    #[error("the presence ticket expired")]
    Expired,
    /// The ticket is dated in the future or valid longer than the TTL.
    #[error("the presence ticket's dates are out of range")]
    BadDates,
    /// #8878: the enrol ticket already enrolled a device.
    #[error("the presence ticket was already used")]
    Spent,
}

/// Where trusty-console serves the browser enrol page (#8878 PR2b-2); the
/// console's `routes::presence::ENROL_PAGE_PATH` must match.
pub const ENROL_PAGE_PATH: &str = "/tools/presence/enrol/";

/// The one-time enrol link for `ticket` on `origin`.
///
/// Why: #8878 PR2b-2, Architect Q3 — the CLI hands the browser a single-use,
/// 10-minute link; the ticket is its only credential.
/// What: `<origin>/tools/presence/enrol/#<base64url(ticket JSON)>`. The
/// ticket rides in the fragment, which a browser never sends to a server, so
/// it stays out of request lines and access logs; the page posts it.
/// Test: `the_enrol_url_carries_the_ticket_in_its_fragment_8878`.
pub fn enrol_url(origin: &str, ticket: &Ticket) -> Result<String, String> {
    use base64::Engine as _;
    let json = serde_json::to_vec(ticket).map_err(|e| e.to_string())?;
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json);
    Ok(format!(
        "{}{ENROL_PAGE_PATH}#{encoded}",
        origin.trim_end_matches('/')
    ))
}

/// Mint a ticket for `purpose` on `subject` at `now`.
///
/// Why: only the CLI calls this, and only after LocalAuthentication answered
/// `Granted` (1b wires the call).
/// What: a fresh nonce, `expires_at = now + TICKET_TTL`, sealed with `key`.
/// Test: `a_minted_ticket_authorises_its_operation_once_checked`.
pub fn mint_ticket(
    key: &GrantKey,
    purpose: TicketPurpose,
    subject: &str,
    now: u64,
) -> Result<Ticket, String> {
    let mut ticket = Ticket {
        purpose,
        subject: subject.to_owned(),
        nonce: random_hex::<16>()?,
        issued_at: now,
        expires_at: now + TICKET_TTL.as_secs(),
        mac: String::new(),
    };
    ticket.mac = key.seal(&ticket.message());
    Ok(ticket)
}

/// Whether `ticket` authorises `purpose` on `subject` at `now`.
///
/// Why/What: see the module doc. The seal is checked first, so no field of
/// an unsealed ticket is trusted.
/// Test: `a_ticket_for_another_name_or_expired_is_refused_8878`,
/// `a_forged_ticket_is_refused_8878`.
pub fn check_ticket(
    key: &GrantKey,
    ticket: &Ticket,
    purpose: TicketPurpose,
    subject: &str,
    now: u64,
) -> Result<(), TicketRefusal> {
    if !key.verify(&ticket.message(), &ticket.mac) {
        return Err(TicketRefusal::Forged);
    }
    if ticket.purpose != purpose {
        return Err(TicketRefusal::WrongPurpose);
    }
    if ticket.subject != subject {
        return Err(TicketRefusal::WrongSubject);
    }
    if ticket.issued_at > now + SKEW
        || ticket.expires_at < ticket.issued_at
        || ticket.expires_at - ticket.issued_at > TICKET_TTL.as_secs()
    {
        return Err(TicketRefusal::BadDates);
    }
    if now > ticket.expires_at {
        return Err(TicketRefusal::Expired);
    }
    Ok(())
}

#[cfg(test)]
#[path = "presence_enrol_tests.rs"]
mod tests;
