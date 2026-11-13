//! The WebAuthn (passkey) adapter for presence devices (#8878 PR2b-1b).
//!
//! Why: plan-pr2b1.md §1 — `verify_and_grant` needs a real signature check
//! behind its [`AssertionVerifier`] seam, and enrolment needs a registration
//! ceremony. Owner ruling D72 accepts webauthn-rs and allows ES256
//! credentials only: the rsa crate it pulls (RUSTSEC-2023-0071) must never
//! run, so registration and verify both refuse any other algorithm.
//! What: [`WebauthnVerifier`] holds the relying party (RP id and the one
//! allowed origin from `[presence]`) and, in memory only, one registration
//! state per enrol ticket and one authentication state per request
//! (Architect Q3: server-held challenge state; a restart drops them all).
//! The authentication state is keyed by the request id and tied to the
//! `Ceremonies` challenge it was started for, so an assertion for request A
//! cannot satisfy request B. This is the only file that imports webauthn-rs.
//! Test: `presence_webauthn_tests.rs`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use webauthn_rs::prelude::{
    COSEAlgorithm, COSEKeyType, CreationChallengeResponse, ECDSACurve, Passkey,
    PasskeyAuthentication, PasskeyRegistration, PublicKeyCredential, RegisterPublicKeyCredential,
    RequestChallengeResponse, Url, Uuid,
};
use webauthn_rs::{Webauthn, WebauthnBuilder};

use crate::core::presence_config::PresenceConfig;
use crate::core::presence_device::{self, Device, DeviceError, NewDevice};
use crate::core::presence_enrol::{Ticket, TicketPurpose, TicketRefusal, check_ticket};
use crate::core::presence_key::GrantKey;
use crate::core::presence_store::random_bytes;
use crate::core::presence_verify::{
    Assertion, AssertionVerifier, CHALLENGE_TTL, Challenge, Verified,
};

/// The COSE id of ES256 (ECDSA P-256 with SHA-256), the only one accepted.
pub const ES256_ALG: i64 = -7;

/// Why a WebAuthn step was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WebauthnRefusal {
    /// `[presence] rp_id` / `rp_origin` is missing or invalid.
    #[error("the device verifier is unavailable: {0}")]
    Unavailable(String),
    /// The enrol ticket was refused.
    #[error("the local-presence ticket was refused: {0}")]
    Ticket(TicketRefusal),
    /// No registration or authentication is open for this ticket or request.
    #[error("no ceremony is open for this ticket or request")]
    NoCeremony,
    /// D72: the credential is not ES256.
    #[error("only ES256 passkeys are accepted (owner ruling D72)")]
    NotEs256,
    /// No enrolled ES256 device can approve.
    #[error("no enrolled device can approve")]
    NoDevices,
    /// The library refused the response.
    #[error("the passkey response was refused: {0}")]
    Rejected(String),
    /// The device store refused the record.
    #[error("the device store refused: {0}")]
    Device(DeviceError),
}

/// One open registration: the name it enrols and the library state.
struct OpenReg {
    name: String,
    issued_at: u64,
    state: PasskeyRegistration,
}

/// One open authentication: the challenge it was started for.
struct OpenAuth {
    challenge: [u8; 32],
    issued_at: u64,
    state: PasskeyAuthentication,
}

/// The passkey verifier; see the module doc.
pub struct WebauthnVerifier {
    webauthn: Webauthn,
    reg: Mutex<HashMap<String, OpenReg>>,
    auth: Mutex<HashMap<String, OpenAuth>>,
}

impl std::fmt::Debug for WebauthnVerifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WebauthnVerifier")
    }
}

/// Lock a ceremony map; a poisoned map still only holds valid states.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether `issued_at` is still inside [`CHALLENGE_TTL`] at `now`.
fn fresh(issued_at: u64, now: u64) -> bool {
    issued_at <= now && now - issued_at <= CHALLENGE_TTL.as_secs()
}

/// Whether `passkey` is ES256 on P-256 (owner ruling D72).
///
/// Test: `a_non_es256_passkey_is_refused_at_registration_8878`,
/// `a_non_es256_device_record_is_refused_at_verify_8878`.
pub fn is_es256(passkey: &Passkey) -> bool {
    let key = passkey.get_public_key();
    *passkey.cred_algorithm() == COSEAlgorithm::ES256
        && key.type_ == COSEAlgorithm::ES256
        && matches!(&key.key, COSEKeyType::EC_EC2(ec) if ec.curve == ECDSACurve::SECP256R1)
}

/// The base64url credential id of `passkey`.
pub fn credential_id(passkey: &Passkey) -> String {
    URL_SAFE_NO_PAD.encode(passkey.cred_id())
}

/// Store `passkey` as device `name`, authorised by `ticket`.
///
/// Why: D72 — the ES256 check sits here, between the library and the store,
/// so a non-ES256 credential never becomes a record.
/// What: refuses a non-ES256 passkey before the store is touched, then
/// [`presence_device::enrol`] (ticket, single use, seal) with the passkey
/// JSON as the record's public key material.
/// Test: `registration_round_trip_stores_a_sealed_device_8878`,
/// `a_non_es256_passkey_is_refused_at_registration_8878`.
pub fn enrol_passkey(
    root: &Path,
    key: &GrantKey,
    ticket: &Ticket,
    passkey: &Passkey,
    now: u64,
) -> Result<Device, WebauthnRefusal> {
    if !is_es256(passkey) {
        return Err(WebauthnRefusal::NotEs256);
    }
    let json =
        serde_json::to_value(passkey).map_err(|e| WebauthnRefusal::Rejected(e.to_string()))?;
    let flag = |name: &str| json["cred"][name].as_bool().unwrap_or(false);
    let sign_count = json["cred"]["counter"]
        .as_u64()
        .and_then(|c| u32::try_from(c).ok())
        .unwrap_or(0);
    let new = NewDevice {
        credential_id: credential_id(passkey),
        name: ticket.subject.clone(),
        public_key: json.to_string(),
        sign_count,
        backup_eligible: flag("backup_eligible"),
        backup_state: flag("backup_state"),
    };
    presence_device::enrol(root, key, ticket, new, now).map_err(WebauthnRefusal::Device)
}

impl WebauthnVerifier {
    /// A verifier for RP `rp_id` accepting exactly `rp_origin`.
    ///
    /// Test: `the_verifier_needs_an_rp_id_and_origin_8878`.
    pub fn new(rp_id: &str, rp_origin: &str) -> Result<Self, WebauthnRefusal> {
        let origin = Url::parse(rp_origin)
            .map_err(|e| WebauthnRefusal::Unavailable(format!("rp_origin: {e}")))?;
        let webauthn = WebauthnBuilder::new(rp_id, &origin)
            .and_then(|b| b.rp_name("trusty-mpm presence").build())
            .map_err(|e| WebauthnRefusal::Unavailable(format!("rp_id / rp_origin: {e}")))?;
        Ok(Self {
            webauthn,
            reg: Mutex::default(),
            auth: Mutex::default(),
        })
    }

    /// The verifier `[presence]` configures; missing keys are
    /// [`WebauthnRefusal::Unavailable`] (no default guess).
    ///
    /// Test: `the_verifier_needs_an_rp_id_and_origin_8878`.
    pub fn from_config(config: &PresenceConfig) -> Result<Self, WebauthnRefusal> {
        match (config.rp_id.as_deref(), config.rp_origin.as_deref()) {
            (Some(id), Some(origin)) => Self::new(id, origin),
            _ => Err(WebauthnRefusal::Unavailable(
                "[presence] rp_id and rp_origin are not set".to_owned(),
            )),
        }
    }

    /// Open a registration for the device `ticket` names.
    ///
    /// Why: plan-pr2b1.md §4 — no registration options without a fresh
    /// Studio presence ticket.
    /// What: checks the ticket, opens the library ceremony (user
    /// verification required, attestation none), keeps its state under the
    /// ticket nonce, and returns the options with ES256 as the only
    /// algorithm offered.
    /// Test: `registration_round_trip_stores_a_sealed_device_8878`,
    /// `register_start_needs_an_enrol_ticket_8878`.
    pub fn start_registration(
        &self,
        key: &GrantKey,
        ticket: &Ticket,
        now: u64,
    ) -> Result<CreationChallengeResponse, WebauthnRefusal> {
        check_ticket(key, ticket, TicketPurpose::Enrol, &ticket.subject, now)
            .map_err(WebauthnRefusal::Ticket)?;
        let user = Uuid::from_bytes(random_bytes::<16>().map_err(WebauthnRefusal::Rejected)?);
        let (mut options, state) = self
            .webauthn
            .start_passkey_registration(user, &ticket.subject, &ticket.subject, None)
            .map_err(|e| WebauthnRefusal::Rejected(e.to_string()))?;
        // D72: offer ES256 only; `enrol_passkey` enforces it on the result.
        options
            .public_key
            .pub_key_cred_params
            .retain(|p| p.alg == ES256_ALG);
        let mut open = lock(&self.reg);
        open.retain(|_, o| fresh(o.issued_at, now));
        open.insert(
            ticket.nonce.clone(),
            OpenReg {
                name: ticket.subject.clone(),
                issued_at: now,
                state,
            },
        );
        Ok(options)
    }

    /// Finish the registration `ticket` opened and store the device.
    ///
    /// What: the open state is removed first (one finish per start); the
    /// library checks the response; [`enrol_passkey`] refuses non-ES256 and
    /// spends the ticket.
    /// Test: `registration_round_trip_stores_a_sealed_device_8878`.
    pub fn finish_registration(
        &self,
        root: &Path,
        key: &GrantKey,
        ticket: &Ticket,
        response: &RegisterPublicKeyCredential,
        now: u64,
    ) -> Result<Device, WebauthnRefusal> {
        let open = lock(&self.reg)
            .remove(&ticket.nonce)
            .filter(|o| o.name == ticket.subject && fresh(o.issued_at, now))
            .ok_or(WebauthnRefusal::NoCeremony)?;
        let passkey = self
            .webauthn
            .finish_passkey_registration(response, &open.state)
            .map_err(|e| WebauthnRefusal::Rejected(e.to_string()))?;
        enrol_passkey(root, key, ticket, &passkey, now)
    }

    /// Open the authentication that answers `challenge`, allowing every
    /// enrolled ES256 device in `devices`.
    ///
    /// Test: `authentication_round_trip_mints_a_grant_8878`.
    pub fn start_authentication(
        &self,
        challenge: &Challenge,
        devices: &[Device],
        now: u64,
    ) -> Result<RequestChallengeResponse, WebauthnRefusal> {
        let passkeys: Vec<Passkey> = devices
            .iter()
            .filter_map(|d| serde_json::from_str::<Passkey>(&d.public_key).ok())
            .filter(is_es256)
            .collect();
        if passkeys.is_empty() {
            return Err(WebauthnRefusal::NoDevices);
        }
        let (options, state) = self
            .webauthn
            .start_passkey_authentication(&passkeys)
            .map_err(|e| WebauthnRefusal::Rejected(e.to_string()))?;
        let mut open = lock(&self.auth);
        open.retain(|_, o| fresh(o.issued_at, now));
        open.insert(
            challenge.request_id.clone(),
            OpenAuth {
                challenge: challenge.bytes,
                issued_at: challenge.issued_at,
                state,
            },
        );
        Ok(options)
    }
}

impl AssertionVerifier for WebauthnVerifier {
    /// Why/What: the payload is the browser's `PublicKeyCredential` JSON. It
    /// must answer the authentication opened for exactly `challenge`, come
    /// from `device` (an ES256 passkey, D72), and pass the library's origin,
    /// RP id, user-verification and signature checks.
    /// Test: `authentication_round_trip_mints_a_grant_8878` and the refusals
    /// in `presence_webauthn_tests.rs`.
    fn verify(
        &self,
        challenge: &Challenge,
        device: &Device,
        assertion: &Assertion,
    ) -> Result<Verified, String> {
        let passkey: Passkey = serde_json::from_str(&device.public_key)
            .map_err(|_| "the device record holds no passkey".to_owned())?;
        if !is_es256(&passkey) {
            return Err(WebauthnRefusal::NotEs256.to_string());
        }
        let credential: PublicKeyCredential = serde_json::from_slice(&assertion.payload)
            .map_err(|_| "the approval is not a passkey assertion".to_owned())?;
        let signer = URL_SAFE_NO_PAD.encode(&credential.raw_id);
        if signer != device.credential_id
            || signer != assertion.credential_id
            || credential_id(&passkey) != device.credential_id
        {
            return Err("the assertion names another credential".to_owned());
        }
        let open = lock(&self.auth);
        let state = open
            .get(&challenge.request_id)
            .filter(|o| o.challenge == challenge.bytes && o.issued_at == challenge.issued_at)
            .ok_or_else(|| WebauthnRefusal::NoCeremony.to_string())?;
        let result = self
            .webauthn
            .finish_passkey_authentication(&credential, &state.state)
            .map_err(|e| WebauthnRefusal::Rejected(e.to_string()).to_string())?;
        if URL_SAFE_NO_PAD.encode(result.cred_id()) != device.credential_id {
            return Err("the assertion names another credential".to_owned());
        }
        Ok(Verified {
            sign_count: result.counter(),
            user_present: true,
            user_verified: result.user_verified(),
            backup_state: result.backup_state(),
        })
    }
}

#[cfg(test)]
#[path = "presence_webauthn_tests.rs"]
mod tests;
