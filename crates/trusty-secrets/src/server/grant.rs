//! The exec-grant registry (DOC-74 §15.8 tier 3, S8 slice 1, #9070).
//!
//! Why: `secrets.resolve` is the only method that returns a value, and it
//! answers only a process `tm secrets exec` granted. A grant names the keys,
//! the child process and an expiry; a random token proves the caller holds
//! it. Architect ruling 2026-10-10: grants live in memory only, and the
//! server defers its idle exit while an unexpired grant exists
//! ([`GrantRegistry::has_unexpired`]).
//! What: [`GrantRegistry`] mints, authorizes and revokes [`GrantToken`]s.
//! A token is 256 bits from the OS CSPRNG; the registry keeps only its
//! SHA-256 and compares hashes in constant time. Expiry is checked on every
//! use, the TTL is capped at mint, and a one-shot grant is removed by its
//! first successful use. Every failure denies: a wrong, unknown or expired
//! token, a key outside the grant and a caller outside the child's tree all
//! answer the one [`GrantError::Refused`]; a poisoned lock, an unreadable
//! process table and a clock error answer their own variant.
//! Test: `grant_tests.rs` beside this module.

use std::collections::BTreeSet;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::ancestry::{
    OsProcessTable, ProcessError, ProcessTable, StartTime, is_self_or_descendant,
};
use crate::api::SecretKey;

/// The TTL cap [`GrantRegistry::os_default`] applies at mint: one hour.
pub const DEFAULT_MAX_TTL: Duration = Duration::from_secs(60 * 60);

/// The most unexpired grants the registry holds at once.
pub const MAX_LIVE_GRANTS: usize = 1024;

/// Token length in bytes: 256 bits.
const TOKEN_BYTES: usize = 32;

type TokenHash = [u8; 32];

/// Why a grant operation failed. Every variant denies.
///
/// Why: AC 1 and 6 of #9070. A token that does not match, has expired, or
/// is used outside its keys or its process tree gets the one `Refused`, so
/// a caller learns nothing about which check failed. Registry faults get a
/// typed variant so the server can audit them.
/// Test: `wrong_expired_and_unknown_tokens_return_the_same_error`,
/// `registry_poisoned_lock_denies`, `unreadable_process_table_denies`,
/// `unreadable_start_time_denies`, `clock_error_denies`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum GrantError {
    /// No grant authorizes this use.
    #[error("grant refused")]
    Refused,
    /// The registry lock is poisoned; no grant can be read.
    #[error("grant registry unavailable")]
    RegistryPoisoned,
    /// The process table could not be read.
    #[error("process check failed: {0}")]
    Process(#[from] ProcessError),
    /// The clock failed, or moved back past a grant's mint time.
    #[error("clock unavailable")]
    Clock,
    /// The OS random source failed.
    #[error("random source unavailable")]
    Random,
    /// [`MAX_LIVE_GRANTS`] unexpired grants already exist.
    #[error("too many live grants")]
    CapacityReached,
    /// The request itself is unusable.
    #[error("invalid grant request: {0}")]
    InvalidRequest(&'static str),
}

/// The clock failed. See [`Clock`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("clock unavailable")]
pub struct ClockError;

/// Time since the Unix epoch, behind a trait so a test can move it.
///
/// Why: expiry must count wall time, including time the host slept.
/// What: [`SystemClock`] fails when the system time is before the epoch.
/// Test: `clock_error_denies`, `grant_use_after_expiry_is_refused`.
pub trait Clock: Send + Sync {
    /// Now, as a duration since the Unix epoch.
    fn now(&self) -> Result<Duration, ClockError>;
}

/// The system wall clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Result<Duration, ClockError> {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| ClockError)
    }
}

/// A grant token. `Debug` redacts it; it has no `Display` and no `==`.
///
/// Why: the token is a bearer credential for `secrets.resolve`.
/// What: 64 lowercase hex characters when minted; any string when read
/// from a request. Compare through the registry only, which hashes it.
/// Test: `grant_token_is_256_bit_and_stored_only_as_a_hash`.
#[derive(Clone)]
pub struct GrantToken(String);

impl GrantToken {
    /// Wrap a token a caller presented.
    pub fn from_wire(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    /// The token text, for the one place that hands it to the child.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for GrantToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GrantToken(<redacted>)")
    }
}

/// What a grant allows, as the spawner asks for it.
///
/// Test: `mint_rejects_invalid_requests`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct GrantRequest {
    /// The keys the child may resolve. Must not be empty.
    pub keys: BTreeSet<SecretKey>,
    /// The child pid. Must be above 1.
    pub child_pid: u32,
    /// The requested lifetime, capped at the registry's max TTL. Not zero.
    pub ttl: Duration,
    /// Remove the grant on its first successful use.
    pub one_shot: bool,
}

impl GrantRequest {
    /// A reusable grant for `keys` and `child_pid`, living `ttl`.
    pub fn new(keys: BTreeSet<SecretKey>, child_pid: u32, ttl: Duration) -> Self {
        Self {
            keys,
            child_pid,
            ttl,
            one_shot: false,
        }
    }

    /// The same request, refused after its first successful use.
    pub fn one_shot(mut self) -> Self {
        self.one_shot = true;
        self
    }
}

/// A minted grant: the token to hand the child, and when it expires.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct MintedGrant {
    /// The token. The registry keeps only its hash.
    pub token: GrantToken,
    /// Expiry, as a duration since the Unix epoch.
    pub expires_at: Duration,
    /// The lifetime granted, after the max-TTL cap.
    pub ttl: Duration,
}

/// One live grant. Holds the token's hash, never the token.
struct Grant {
    token_hash: TokenHash,
    keys: BTreeSet<SecretKey>,
    child_pid: u32,
    child_start: StartTime,
    minted_at: Duration,
    expires_at: Duration,
    one_shot: bool,
}

/// The in-memory registry of exec grants.
///
/// Why: see the module docs.
/// What: one mutex over the live grants. [`GrantRegistry::authorize`] holds
/// it across the ancestry walk, so a one-shot grant cannot be used twice by
/// two racing callers.
/// Test: `grant_tests.rs`.
pub struct GrantRegistry {
    grants: Mutex<Vec<Grant>>,
    procs: Arc<dyn ProcessTable>,
    clock: Arc<dyn Clock>,
    max_ttl: Duration,
}

impl fmt::Debug for GrantRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GrantRegistry")
            .field("max_ttl", &self.max_ttl)
            .finish_non_exhaustive()
    }
}

impl GrantRegistry {
    /// A registry over `procs` and `clock`, capping every TTL at `max_ttl`.
    pub fn new(procs: Arc<dyn ProcessTable>, clock: Arc<dyn Clock>, max_ttl: Duration) -> Self {
        Self {
            grants: Mutex::new(Vec::new()),
            procs,
            clock,
            max_ttl,
        }
    }

    /// The production registry: host process table, system clock,
    /// [`DEFAULT_MAX_TTL`].
    pub fn os_default() -> Self {
        Self::new(
            Arc::new(OsProcessTable),
            Arc::new(SystemClock),
            DEFAULT_MAX_TTL,
        )
    }

    // #9070: a poisoned lock denies; it is never recovered with `into_inner`.
    fn lock(&self) -> Result<MutexGuard<'_, Vec<Grant>>, GrantError> {
        self.grants.lock().map_err(|_| GrantError::RegistryPoisoned)
    }

    fn now(&self) -> Result<Duration, GrantError> {
        self.clock.now().map_err(|_| GrantError::Clock)
    }

    /// Mint a grant and return its token.
    ///
    /// Why: `secrets.grant` (slice 2) registers the spawner's child here.
    /// What: validates the request, records the child's start time, caps
    /// the TTL at the registry's max, drops expired grants, and refuses at
    /// [`MAX_LIVE_GRANTS`].
    /// Test: `grant_ttl_is_capped_at_max_ttl`, `mint_rejects_invalid_requests`,
    /// `unreadable_start_time_denies`, `live_grant_count_is_capped`.
    pub fn mint(&self, request: GrantRequest) -> Result<MintedGrant, GrantError> {
        if request.keys.is_empty() {
            return Err(GrantError::InvalidRequest("no keys"));
        }
        if request.child_pid <= 1 {
            return Err(GrantError::InvalidRequest("child pid"));
        }
        if request.ttl.is_zero() {
            return Err(GrantError::InvalidRequest("zero ttl"));
        }
        let mut grants = self.lock()?;
        let now = self.now()?;
        let child_start = self.procs.start_time(request.child_pid)?;
        let ttl = request.ttl.min(self.max_ttl);
        grants.retain(|g| g.expires_at > now);
        if grants.len() >= MAX_LIVE_GRANTS {
            return Err(GrantError::CapacityReached);
        }
        let expires_at = now.checked_add(ttl).ok_or(GrantError::Clock)?;
        let (token, token_hash) = new_token()?;
        grants.push(Grant {
            token_hash,
            keys: request.keys,
            child_pid: request.child_pid,
            child_start,
            minted_at: now,
            expires_at,
            one_shot: request.one_shot,
        });
        Ok(MintedGrant {
            token,
            expires_at,
            ttl,
        })
    }

    /// Allow `peer_pid` to resolve `keys` under `token`, or deny.
    ///
    /// Why: DOC-74 §15.8 — all three conditions, checked on every use.
    /// What: in order — token match (constant time), clock not behind the
    /// mint time, unexpired (an expired grant is removed), every key in the
    /// grant, `peer_pid` the child or a descendant with the child's recorded
    /// start time. A one-shot grant is removed on success.
    /// Test: `grant_use_after_expiry_is_refused`,
    /// `pid_reuse_with_new_start_time_is_refused`,
    /// `one_shot_grant_refuses_second_use`, `key_outside_grant_is_refused`,
    /// `grant_allows_child_and_grandchild_and_refuses_sibling`.
    pub fn authorize(
        &self,
        token: &GrantToken,
        keys: &[SecretKey],
        peer_pid: u32,
    ) -> Result<(), GrantError> {
        if keys.is_empty() {
            return Err(GrantError::InvalidRequest("no keys"));
        }
        let presented = hash_token(token);
        let mut grants = self.lock()?;
        let now = self.now()?;
        let index = find(&grants, &presented).ok_or(GrantError::Refused)?;
        let grant = &grants[index];
        if now < grant.minted_at {
            return Err(GrantError::Clock);
        }
        // #9070: expiry is checked on every use, not only at mint.
        if now >= grant.expires_at {
            grants.swap_remove(index);
            return Err(GrantError::Refused);
        }
        if !keys.iter().all(|key| grant.keys.contains(key)) {
            return Err(GrantError::Refused);
        }
        // #9070: the child's recorded start time defeats pid reuse.
        if !is_self_or_descendant(
            self.procs.as_ref(),
            peer_pid,
            grant.child_pid,
            grant.child_start,
        )? {
            return Err(GrantError::Refused);
        }
        if grant.one_shot {
            grants.swap_remove(index);
        }
        Ok(())
    }

    /// Remove the grant `token` names. `Ok(false)` when none matched.
    ///
    /// Test: `revoke_removes_the_grant`.
    pub fn revoke(&self, token: &GrantToken) -> Result<bool, GrantError> {
        let presented = hash_token(token);
        let mut grants = self.lock()?;
        match find(&grants, &presented) {
            Some(index) => {
                grants.swap_remove(index);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Whether any unexpired grant exists. Drops expired grants.
    ///
    /// Why: the server defers its 60 s idle exit while this is `true`
    /// (Architect ruling 2026-10-10, slice 2).
    /// Test: `has_unexpired_tracks_expiry_and_revoke`.
    pub fn has_unexpired(&self) -> Result<bool, GrantError> {
        let mut grants = self.lock()?;
        let now = self.now()?;
        grants.retain(|g| g.expires_at > now);
        Ok(!grants.is_empty())
    }
}

/// A fresh token from the OS CSPRNG, and its hash.
fn new_token() -> Result<(GrantToken, TokenHash), GrantError> {
    let mut bytes = [0u8; TOKEN_BYTES];
    OsRng
        .try_fill_bytes(&mut bytes)
        .map_err(|_| GrantError::Random)?;
    let token = GrantToken(hex(&bytes));
    let hash = hash_token(&token);
    Ok((token, hash))
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}

fn hash_token(token: &GrantToken) -> TokenHash {
    Sha256::digest(token.0.as_bytes()).into()
}

/// The index of the grant whose hash equals `presented`.
///
/// What: compares every entry in constant time and never stops early, so
/// the time taken does not depend on where, or whether, a match sits.
fn find(grants: &[Grant], presented: &TokenHash) -> Option<usize> {
    let mut found = None;
    for (index, grant) in grants.iter().enumerate() {
        if constant_time_eq(&grant.token_hash, presented) {
            found = Some(index);
        }
    }
    found
}

/// Equality of two hashes without a data-dependent branch.
///
/// Test: `constant_time_eq_matches_only_identical_hashes`.
fn constant_time_eq(a: &TokenHash, b: &TokenHash) -> bool {
    let diff = a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y));
    std::hint::black_box(diff) == 0
}

#[cfg(test)]
#[path = "grant_tests.rs"]
mod tests;
