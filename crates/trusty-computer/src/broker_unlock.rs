//! The persisted lockout marker and the operator unlock.
//!
//! Why: launchd restarts the broker (`KeepAlive`) right after a `lockout`
//! exits it, so an in-memory lockout lasted only until that restart
//! (#11460 code-critic finding 7).
//! What: `lockout` writes `<state>/lockout` (0600, fsynced) before anything
//! else; [`Broker::new`](crate::broker::Broker::new) starts locked out while
//! it exists. Only the operator clears it: `unlock_request` shows a
//! [`PromptKind::Unlock`] dialog with a six-digit code (no broker lock held),
//! and `unlock_confirm` takes the code from the CLI's `/dev/tty`, removes
//! the marker and lifts the lockout. Bad codes count as mint strikes.
//! Test: `lockout_survives_a_broker_restart`, `unlock_needs_the_dialog_and_its_code`,
//! `lockout_marker_is_written_when_revoke_all_fails`.

use std::io::{self, Write};
use std::path::Path;

use serde_json::{Map, Value, json};

use crate::broker::{Broker, Ctx, Inner, base};
use crate::consent::{self, DIALOG_CODE_TTL, DialogOutcome, Prompt, PromptKind, STRIKE_LIMIT};
use crate::launcher::Launcher;

/// The marker's file name in the state dir.
pub const LOCKOUT_MARKER: &str = "lockout";

/// A confirmed unlock dialog awaiting its code.
#[derive(Debug)]
pub struct UnlockChallenge {
    challenge: String,
    code: String,
    expires: f64,
    bad: usize,
}

/// Write the marker 0600 and fsync it; an existing marker is kept.
pub(crate) fn write_marker(path: &Path) -> io::Result<()> {
    let mut file = crate::private::open_nofollow(path, false)?;
    file.write_all(b"locked out by the operator\n")?;
    file.sync_all()
}

impl<L: Launcher> Broker<L> {
    /// Operator op `unlock_request`: show the unlock dialog, answer a challenge.
    pub(crate) fn unlock_request(&self, ctx: &Ctx) -> Value {
        let rec = base(ctx, "unlock_request");
        let code = {
            let now = self.now();
            let mut inner = self.lock();
            if !inner.consent.locked_down {
                return inner.refuse(rec, "not_locked", "the broker is not locked out");
            }
            if inner.consent.mint_strikes.locked(now) {
                return inner.refuse(rec, "locked_out", "too many bad codes; wait 60 s");
            }
            match consent::six_digits() {
                Ok(c) => c,
                Err(e) => return inner.refuse(rec, "random_failed", &e.to_string()),
            }
        };
        let prompt = Prompt {
            kind: PromptKind::Unlock,
            bundle_id: String::new(),
            app_name: String::new(),
            pid: 0,
            tools: Vec::new(),
            ttl_secs: 0,
            code: code.clone(),
        };
        let outcome = consent::ask(self.dialog(), prompt, self.dialog_timeout());
        let now = self.now();
        let mut inner = self.lock();
        let refused = match outcome {
            DialogOutcome::Confirmed => None,
            DialogOutcome::Denied => {
                Some(("dialog_denied", "the operator denied the unlock".into()))
            }
            DialogOutcome::TimedOut => Some(("dialog_timeout", "no answer in the dialog".into())),
            DialogOutcome::Failed(e) => Some(("dialog_failed", e)),
        };
        if let Some((code, detail)) = refused {
            return inner.refuse(rec, code, &detail);
        }
        let challenge = match consent::challenge_id() {
            Ok(c) => c,
            Err(e) => return inner.refuse(rec, "random_failed", &e.to_string()),
        };
        let reply = inner.audited(rec, json!({"ok": true, "result": {"challenge": challenge}}));
        if reply["ok"] == json!(true) {
            inner.consent.unlock = Some(UnlockChallenge {
                challenge,
                code,
                expires: now + DIALOG_CODE_TTL,
                bad: 0,
            });
        }
        reply
    }
}

impl<L: Launcher> Inner<L> {
    /// Operator op `unlock_confirm`: check the dialog code, remove the marker, lift the lockout.
    ///
    /// Postcondition on `ok`: the marker is gone and `locked_down` is false;
    /// on any refusal both are unchanged.
    pub(crate) fn unlock_confirm(
        &mut self,
        req: &Map<String, Value>,
        rec: Map<String, Value>,
        now: f64,
    ) -> Value {
        if self.consent.mint_strikes.locked(now) {
            return self.refuse(rec, "locked_out", "too many bad codes; wait 60 s");
        }
        let challenge = req.get("challenge").and_then(Value::as_str).unwrap_or("");
        let code = req.get("code").and_then(Value::as_str).unwrap_or("");
        let Some(pending) = self
            .consent
            .unlock
            .as_mut()
            .filter(|p| p.challenge == challenge)
        else {
            self.consent.mint_strikes.strike(now);
            return self.refuse(rec, "bad_challenge", "no such challenge");
        };
        if now >= pending.expires {
            self.consent.unlock = None;
            return self.refuse(rec, "code_expired", "the dialog code expired; ask again");
        }
        if !consent::same_code(code.trim(), &pending.code) {
            pending.bad += 1;
            if pending.bad >= STRIKE_LIMIT {
                self.consent.unlock = None;
            }
            self.consent.mint_strikes.strike(now);
            return self.refuse(rec, "bad_code", "the code does not match the dialog");
        }
        match std::fs::remove_file(&self.lockout_marker) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return self.refuse(rec, "unlock_failed", &e.to_string()),
        }
        self.consent.unlock = None;
        self.consent.locked_down = false;
        self.audited(rec, json!({"ok": true, "result": {"unlocked": true}}))
    }
}
