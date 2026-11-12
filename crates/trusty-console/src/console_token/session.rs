//! In-memory browser sessions for the console token gate (#11074, TOK-5).
//!
//! Why: a browser cannot attach `Authorization` to a page load or an
//! `EventSource`, and the cookie must never hold the token, so a login trades
//! the token for a random session id the server remembers.
//! What: [`SessionStore`] maps a 256-bit id from the OS CSPRNG to its expiry.
//! Ids are opaque and unguessable, so a map lookup is the check. Expired
//! entries are pruned on each mint, and the store holds at most
//! [`MAX_SESSIONS`], evicting the oldest. Nothing persists: a restart ends
//! every session.
//! Test: `console_token/tests.rs` (`login_cookie_*`, `expired_session_*`).

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use rand_core::{OsRng, RngCore};

/// The most sessions held at once; a login past it evicts the oldest.
pub(super) const MAX_SESSIONS: usize = 1024;

/// The OS random source failed, so no session id could be minted.
#[derive(Debug)]
pub(super) struct MintFailed;

/// Live sessions: id → expiry.
pub(super) struct SessionStore {
    ttl: Duration,
    live: Mutex<HashMap<String, Instant>>,
}

impl SessionStore {
    /// An empty store whose sessions last `ttl`.
    pub(super) fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            live: Mutex::new(HashMap::new()),
        }
    }

    /// Mint a session: 32 CSPRNG bytes as 64 lowercase hex digits.
    pub(super) fn mint(&self) -> Result<String, MintFailed> {
        let mut bytes = [0u8; 32];
        OsRng.try_fill_bytes(&mut bytes).map_err(|_| MintFailed)?;
        let id: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let now = Instant::now();
        let mut live = self.live.lock().unwrap_or_else(PoisonError::into_inner);
        live.retain(|_, expires| *expires > now);
        if live.len() >= MAX_SESSIONS
            && let Some(oldest) = live
                .iter()
                .min_by_key(|(_, expires)| **expires)
                .map(|(id, _)| id.clone())
        {
            live.remove(&oldest);
        }
        live.insert(id.clone(), now + self.ttl);
        Ok(id)
    }

    /// Whether `id` names a session that has not expired.
    pub(super) fn is_live(&self, id: &str) -> bool {
        let live = self.live.lock().unwrap_or_else(PoisonError::into_inner);
        live.get(id)
            .is_some_and(|expires| *expires > Instant::now())
    }

    /// How many sessions the store holds; tests read it.
    #[cfg(all(test, unix))]
    pub(super) fn len(&self) -> usize {
        self.live
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}
