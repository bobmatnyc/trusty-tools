//! Bot pairing business logic.
//!
//! Why: the `POST /pair/*` handlers wrapped `DaemonState`'s pairing primitives
//! with the wire-shape concerns (the TTL constant, the success/failure JSON).
//! A service gives the pairing flow one home and a typed result so the handlers
//! become trivial.
//! What: [`PairingService`] borrows [`DaemonState`] and exposes request /
//! confirm / status, returning typed [`PairCode`] / [`PairStatus`] values.
//! Test: `cargo test -p trusty-mpm-daemon services::pairing` round-trips a
//! generated code through confirm and rejects a bad code.

use serde::Serialize;

use crate::daemon::error::DaemonError;
use crate::daemon::state::{DaemonState, PAIR_CODE_TTL};

/// A freshly-issued one-time pairing code and its lifetime.
#[derive(Debug, Serialize)]
pub struct PairCode {
    /// The six-character uppercase code the operator types into the bot.
    pub code: String,
    /// How long, in seconds, the code remains valid.
    pub expires_in_seconds: u64,
}

/// The daemon's current pairing status.
#[derive(Debug, Serialize)]
pub struct PairStatus {
    /// Whether a Telegram chat is currently paired.
    pub paired: bool,
    /// The paired chat id, when one exists.
    pub chat_id: Option<i64>,
}

/// Bot pairing operations over the shared daemon state.
///
/// Why: a borrowed facade so a handler constructs one per request and delegates.
/// What: holds a borrow of [`DaemonState`]; methods wrap its pairing primitives
/// with the wire shapes and the [`DaemonError`] failure type.
/// Test: the module's `#[cfg(test)]` suite.
pub struct PairingService<'s> {
    state: &'s DaemonState,
}

impl<'s> PairingService<'s> {
    /// Build a service bound to `state`.
    pub fn new(state: &'s DaemonState) -> Self {
        Self { state }
    }

    /// Generate a one-time pairing code with its TTL.
    ///
    /// Why: `tm pair` asks the daemon for a short code the operator types into
    /// the bot; the response must carry the TTL so the operator knows the
    /// window.
    /// What: stores a fresh code in state and returns it with [`PAIR_CODE_TTL`].
    /// Test: `request_code_has_ttl`.
    pub fn request_code(&self) -> PairCode {
        PairCode {
            code: self.state.generate_pair_code(),
            expires_in_seconds: PAIR_CODE_TTL.as_secs(),
        }
    }

    /// Validate a code and bind `chat_id` on success, consuming the code.
    ///
    /// Why: the bot's `/pair <code>` flow validates the operator's code so push
    /// alerts have an authenticated destination.
    /// What: returns `Ok(())` and stores `chat_id` when the code matches the
    /// outstanding code within its TTL; an invalid or expired code maps to
    /// [`DaemonError::InvalidPairCode`].
    /// Test: `confirm_round_trip`, `confirm_rejects_bad_code`.
    pub fn confirm(&self, code: &str, chat_id: i64) -> Result<(), DaemonError> {
        if self.state.confirm_pair_code(code, chat_id) {
            Ok(())
        } else {
            Err(DaemonError::InvalidPairCode)
        }
    }

    /// The daemon's current pairing status.
    pub fn status(&self) -> PairStatus {
        let chat_id = self.state.paired_chat_id();
        PairStatus {
            paired: chat_id.is_some(),
            chat_id,
        }
    }

    /// Clear the pairing, in memory and on disk.
    ///
    /// Why: `POST /pair/reset` lets the operator unpair; the persisted record
    /// must be removed so a restart does not restore the old binding.
    /// What: delegates to [`DaemonState::clear_pairing`].
    /// Test: `reset_clears_pairing`.
    pub fn reset(&self) {
        self.state.clear_pairing();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_code_has_ttl() {
        let state = DaemonState::new();
        let svc = PairingService::new(&state);
        let code = svc.request_code();
        assert_eq!(code.code.len(), 6);
        assert_eq!(code.expires_in_seconds, PAIR_CODE_TTL.as_secs());
    }

    #[test]
    fn confirm_round_trip() {
        // Rooted at a temp dir so the persisted pairing never touches HOME.
        let dir = tempfile::tempdir().expect("temp dir");
        let state = DaemonState::with_root(dir.path().to_path_buf());
        let svc = PairingService::new(&state);
        let code = svc.request_code().code;
        svc.confirm(&code, 4242).expect("valid code confirms");
        assert_eq!(svc.status().chat_id, Some(4242));
        assert!(svc.status().paired);
    }

    #[test]
    fn reset_clears_pairing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let state = DaemonState::with_root(dir.path().to_path_buf());
        let svc = PairingService::new(&state);
        let code = svc.request_code().code;
        svc.confirm(&code, 99).expect("valid code confirms");
        assert!(svc.status().paired);
        svc.reset();
        assert!(!svc.status().paired);
    }

    #[test]
    fn confirm_rejects_bad_code() {
        let dir = tempfile::tempdir().expect("temp dir");
        let state = DaemonState::with_root(dir.path().to_path_buf());
        let svc = PairingService::new(&state);
        let _ = svc.request_code();
        let err = svc.confirm("ZZZZZZ", 1).unwrap_err();
        assert!(matches!(err, DaemonError::InvalidPairCode));
        assert!(!svc.status().paired);
    }
}
