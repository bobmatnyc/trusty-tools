//! Turn a device approval into a presence grant (#8878 PR2b-1a).
//!
//! Why: plan-pr2b1.md §1 and §5 — one place decides whether a device's
//! assertion approves one pending request, in a fixed order, and every
//! refusal leaves no grant and the request unconsumed. Architect Q3: the
//! challenge is server-held state per request, not a hash the caller sends.
//! What: [`Ceremonies`] holds, in memory only, one random challenge per
//! request id (a daemon restart drops them all, so a verify after it is
//! refused). [`AssertionVerifier`] is the seam the WebAuthn adapter fills in
//! PR2b-1b; [`StubVerifier`] refuses everything until then.
//! [`verify_and_grant`] runs: key, ceremony, challenge age, request, action
//! hash, live `claude`, device record, verifier, user verification, counter,
//! counter persist, ceremony finish, claim, mint. Pure orchestration: no
//! WebAuthn types.
//! Test: `presence_verify_tests.rs`, `presence_verify_failopen_tests.rs`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use crate::core::presence;
use crate::core::presence_device::{self, Device, DeviceError};
use crate::core::presence_key::GrantKey;
use crate::core::presence_request::{self, Pending, RequestError};
use crate::core::presence_store::random_bytes;
use crate::core::twin_identity::ClaudeProcess;

/// How long a challenge may be answered after it is issued.
pub const CHALLENGE_TTL: Duration = Duration::from_secs(5 * 60);

/// The challenge one request's approval must sign.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Challenge {
    /// The request it was issued for.
    pub request_id: String,
    /// 256 random bits.
    pub bytes: [u8; 32],
    /// When it was issued, Unix seconds.
    pub issued_at: u64,
}

/// The daemon's in-memory challenges, one per request id.
#[derive(Debug, Default)]
pub struct Ceremonies {
    open: Mutex<HashMap<String, Challenge>>,
}

impl Ceremonies {
    /// An empty set: what a freshly started daemon holds.
    pub fn new() -> Self {
        Self::default()
    }

    /// Issue a challenge for the live request `request_id` at `now`.
    ///
    /// Why/What: only a request that [`presence_request::load`] accepts gets
    /// one; a second call replaces the first. Every challenge past
    /// [`CHALLENGE_TTL`] or dated after `now` is dropped first (#8878), so
    /// ceremonies never finished do not pile up.
    /// Test: `a_valid_assertion_mints_a_device_grant_8878`,
    /// `begin_drops_challenges_older_than_the_ttl_8878`.
    pub fn begin(
        &self,
        root: &Path,
        key: &GrantKey,
        request_id: &str,
        now: u64,
    ) -> Result<Challenge, VerifyError> {
        presence_request::load(root, key, request_id, now).map_err(VerifyError::Request)?;
        let challenge = Challenge {
            request_id: request_id.to_owned(),
            bytes: random_bytes::<32>().map_err(VerifyError::NoRandom)?,
            issued_at: now,
        };
        let mut open = self.lock();
        // #8878: an unanswerable challenge is refused at verify; drop it here.
        open.retain(|_, c| c.issued_at <= now && now - c.issued_at <= CHALLENGE_TTL.as_secs());
        open.insert(request_id.to_owned(), challenge.clone());
        Ok(challenge)
    }

    fn current(&self, request_id: &str) -> Option<Challenge> {
        self.lock().get(request_id).cloned()
    }

    /// Remove `challenge` if it is still the open one; `false` if another
    /// caller finished it first.
    fn finish(&self, challenge: &Challenge) -> bool {
        let mut open = self.lock();
        if open.get(&challenge.request_id) == Some(challenge) {
            open.remove(&challenge.request_id);
            true
        } else {
            false
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Challenge>> {
        // A poisoned map still holds valid challenges; a panic elsewhere
        // must not turn into an approval, and recovery keeps refusing.
        self.open
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// An approval as it reaches the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assertion {
    /// The request it claims to approve.
    pub request_id: String,
    /// The credential that signed it.
    pub credential_id: String,
    /// The action hash the approver was shown.
    pub action_hash: String,
    /// The verifier-specific signed payload.
    pub payload: Vec<u8>,
}

/// What a verifier proved about an assertion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Verified {
    /// The authenticator's signature counter.
    pub sign_count: u32,
    /// The user-present flag.
    pub user_present: bool,
    /// The user-verified flag (biometric or passcode).
    pub user_verified: bool,
    /// The backup-state flag.
    pub backup_state: bool,
}

/// Checks one assertion's signature against a device and a challenge.
///
/// Why: the seam between the orchestration here and the WebAuthn library
/// (PR2b-1b); tests drive it with a fake device.
/// What: `Ok` only when `assertion.payload` is a valid signature by `device`
/// over `challenge` (and, in 1b, the right origin and RP id).
pub trait AssertionVerifier {
    /// Verify `assertion` from `device` over `challenge`.
    fn verify(
        &self,
        challenge: &Challenge,
        device: &Device,
        assertion: &Assertion,
    ) -> Result<Verified, String>;
}

/// The verifier until PR2b-1b: refuses every assertion.
#[derive(Debug, Clone, Copy, Default)]
pub struct StubVerifier;

impl AssertionVerifier for StubVerifier {
    fn verify(&self, _: &Challenge, _: &Device, _: &Assertion) -> Result<Verified, String> {
        Err("no assertion verifier is built yet (#8878 PR2b-1b)".to_owned())
    }
}

/// What [`verify_and_grant`] reads from its surroundings.
pub struct VerifyEnv<'a> {
    /// `~/.trusty-mpm`.
    pub root: &'a Path,
    /// The presence key; `None` when it cannot be read.
    pub key: &'a dyn Fn() -> Option<GrantKey>,
    /// The live start time of a PID, `None` when no such process runs.
    pub start_time_of: &'a dyn Fn(u32) -> Option<u64>,
    /// Now, Unix seconds.
    pub now: u64,
}

/// A grant minted from a device approval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Granted {
    /// The grant file.
    pub path: PathBuf,
    /// The `claude` it lets through.
    pub claude: ClaudeProcess,
    /// The grant's method, `device:<credential id>`.
    pub method: String,
}

/// Why an approval did not become a grant.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VerifyError {
    /// The presence key could not be read.
    #[error("the presence key could not be read")]
    NoKey,
    /// No challenge is open for the request (never issued, already used, or
    /// the daemon restarted).
    #[error("no challenge is open for this request")]
    NoCeremony,
    /// The challenge is past [`CHALLENGE_TTL`] or dated after now.
    #[error("the challenge expired or is dated in the future")]
    ChallengeExpired,
    /// The request was refused.
    #[error("the request was refused: {0}")]
    Request(RequestError),
    /// The assertion names another request or action.
    #[error("the approval is for another request or action")]
    WrongRequest,
    /// The requesting `claude` is gone, or its PID was reused.
    #[error("the requesting `claude` is gone or its PID was reused")]
    ProcessGone,
    /// The device was refused.
    #[error("the device was refused: {0}")]
    Device(DeviceError),
    /// The verifier refused the signature.
    #[error("the approval's signature was refused: {0}")]
    Rejected(String),
    /// The user-present or user-verified flag is clear.
    #[error("the device did not verify the user")]
    UserNotVerified,
    /// The signature counter did not move forward.
    #[error("the device's signature counter did not increase (possible clone)")]
    CounterRegressed,
    /// The counter could not be persisted, so the grant is refused.
    #[error("the device's counter could not be saved: {0}")]
    Persist(DeviceError),
    /// No random bytes for a challenge.
    #[error("no random bytes for a challenge: {0}")]
    NoRandom(String),
    /// The grant could not be written; the request is already consumed.
    #[error("the grant could not be written (the request is consumed): {0}")]
    Grant(String),
}

/// Turn `assertion` into a grant for the request it approves.
///
/// Why/What: see the module doc. Every refusal before the claim leaves the
/// request file and the challenge in place and writes no grant; that
/// includes a device revoked, or a higher counter saved by another verify,
/// while the verifier ran (#8878). After the
/// claim the request is gone, so a mint failure is an error with nothing to
/// resubmit.
/// Test: `a_valid_assertion_mints_a_device_grant_8878` and the spoof and
/// fail-open tests in `presence_verify_tests.rs` and
/// `presence_verify_failopen_tests.rs`.
pub fn verify_and_grant(
    env: &VerifyEnv<'_>,
    ceremonies: &Ceremonies,
    verifier: &dyn AssertionVerifier,
    assertion: &Assertion,
) -> Result<Granted, VerifyError> {
    let key = (env.key)().ok_or(VerifyError::NoKey)?;
    let challenge = ceremonies
        .current(&assertion.request_id)
        .ok_or(VerifyError::NoCeremony)?;
    if challenge.issued_at > env.now || env.now - challenge.issued_at > CHALLENGE_TTL.as_secs() {
        return Err(VerifyError::ChallengeExpired);
    }
    let pending: Pending = presence_request::load(env.root, &key, &assertion.request_id, env.now)
        .map_err(VerifyError::Request)?;
    if pending.action_hash != assertion.action_hash {
        return Err(VerifyError::WrongRequest);
    }
    if (env.start_time_of)(pending.pid) != Some(pending.start_time) {
        return Err(VerifyError::ProcessGone);
    }
    let device = presence_device::load(env.root, &key, &assertion.credential_id)
        .map_err(VerifyError::Device)?;
    let verified = verifier
        .verify(&challenge, &device, assertion)
        .map_err(VerifyError::Rejected)?;
    if !verified.user_present || !verified.user_verified {
        return Err(VerifyError::UserNotVerified);
    }
    let both_zero = verified.sign_count == 0 && device.sign_count == 0;
    if verified.sign_count <= device.sign_count && !both_zero {
        return Err(VerifyError::CounterRegressed);
    }
    // #8878: held from the counter save to the mint, so a revoke either
    // sweeps this grant or runs first and leaves no record to save.
    let lock = presence_device::lock_store(env.root).map_err(VerifyError::Persist)?;
    presence_device::record_use(
        env.root,
        &key,
        &lock,
        &device.credential_id,
        verified.sign_count,
        verified.backup_state,
        env.now,
    )
    .map_err(|e| match e {
        DeviceError::Unknown => VerifyError::Device(DeviceError::Unknown),
        DeviceError::CounterRegressed => VerifyError::CounterRegressed,
        other => VerifyError::Persist(other),
    })?;
    if !ceremonies.finish(&challenge) {
        return Err(VerifyError::NoCeremony);
    }
    presence_request::claim(env.root, &key, &pending).map_err(VerifyError::Request)?;
    let claude = ClaudeProcess {
        pid: pending.pid,
        start_time: pending.start_time,
    };
    let method = device.method();
    let path =
        presence::mint(env.root, &claude, &method, &key, env.now).map_err(VerifyError::Grant)?;
    drop(lock);
    Ok(Granted {
        path,
        claude,
        method,
    })
}

#[cfg(test)]
#[path = "presence_verify_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "presence_verify_failopen_tests.rs"]
mod failopen_tests;
