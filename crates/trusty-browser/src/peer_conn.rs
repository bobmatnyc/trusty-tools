//! One paired extension worker on `service.sock` (`protocol.py` `Peer`).
//!
//! Why: broker-to-worker commands share one stream with the worker's replies,
//! matched by id; a lost reply must map to `OutcomeUnknown` for an op that may
//! have run and `BrowserDisconnected` for a read.
//! What: [`Peer::page`] draws the id, writes `{"id","op","email","grant",...}`,
//! waits 8 s and applies Python's reply rules; [`Peer::kill`] is
//! `writer.close()`, which also ends the browser read loop.
//! Test: `captured_exchanges_replay_byte_identically` (worker commands 0-2).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::net::unix::OwnedWriteHalf;
use tokio::sync::{Notify, oneshot};

use crate::entropy::{Entropy, token_urlsafe};
use crate::pyjson::Json;
use crate::refusal::{MUTATING, PAGE_ERRORS, REFUSAL_FIELDS, Refusal, UNFINISHED};
use crate::wire::write_frame;

// `Peer.command`: the route-checked command behind the lease indicator (port phase 3).
mod command;

/// Seconds the broker waits for a worker reply (`PAGE_WAIT`).
pub const PAGE_WAIT: f64 = 8.0;

/// A connected, authenticated worker.
pub struct Peer {
    /// Profile id.
    pub profile: String,
    /// Case-folded email the worker must echo.
    pub email: String,
    /// Sealed browser key at handshake time.
    pub browser_key: String,
    /// Browser incarnation from `hello`.
    pub incarnation: String,
    /// This connection's generation uuid.
    pub generation: String,
    /// The pairing row at handshake time (`Peer.row`).
    pub route: crate::pairing::Route,
    writer: tokio::sync::Mutex<OwnedWriteHalf>,
    pending: Mutex<HashMap<String, oneshot::Sender<Json>>>,
    killed: Notify,
}

impl std::fmt::Debug for Peer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Peer")
            .field("profile", &self.profile)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

fn lost(op: &str) -> Refusal {
    Refusal::new(if MUTATING.contains(&op) {
        "OutcomeUnknown"
    } else {
        "BrowserDisconnected"
    })
}

impl Peer {
    /// A peer for a finished handshake.
    pub fn new(
        writer: OwnedWriteHalf,
        route: &crate::pairing::Route,
        incarnation: String,
        generation: String,
    ) -> Arc<Peer> {
        Arc::new(Peer {
            profile: route.profile.clone(),
            email: route.email.clone(),
            browser_key: route.browser_key.clone(),
            incarnation,
            generation,
            route: route.clone(),
            writer: tokio::sync::Mutex::new(writer),
            pending: Mutex::new(HashMap::new()),
            killed: Notify::new(),
        })
    }

    /// Close the connection: fail every pending command and stop the read loop.
    pub fn kill(&self) {
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
        self.killed.notify_one();
    }

    /// Resolves once [`Peer::kill`] ran.
    pub async fn killed(&self) {
        self.killed.notified().await;
    }

    /// Deliver a worker reply; `false` when no command waits for that id.
    pub fn deliver(&self, id: &str, reply: Json) -> bool {
        let sender = self
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id);
        sender.is_some_and(|s| s.send(reply).is_ok())
    }

    /// One page command; returns the worker's `result` (or `null`).
    ///
    /// Why: the contract's reply rules in one place (`Peer.page`).
    /// What: a lost reply, or the worker's own `OutcomeUnknown` /
    /// `BrowserDisconnected`, is `OutcomeUnknown` for a mutating op else
    /// `BrowserDisconnected`; an unknown error name is `BrowserRefused`; a
    /// known one keeps only `REFUSAL_FIELDS`; a wrong email is
    /// `IdentityMismatch`, a wrong tab `TabMismatch`.
    /// Test: `captured_exchanges_replay_byte_identically`.
    pub async fn page(
        &self,
        entropy: &dyn Entropy,
        op: &str,
        grant: &str,
        args: Vec<(&str, Json)>,
    ) -> Result<Json, Refusal> {
        let ident = token_urlsafe(entropy, 18);
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(ident.clone(), tx);
        let mut frame = Json::object([
            ("id", Json::from(ident.as_str())),
            ("op", Json::from(op)),
            ("email", Json::from(self.email.as_str())),
            ("grant", Json::from(grant)),
        ]);
        let want_tab = args
            .iter()
            .find(|(k, _)| *k == "tab")
            .map(|(_, v)| v.clone());
        for (k, v) in args {
            frame.insert(k, v);
        }
        let sent = {
            let mut w = self.writer.lock().await;
            write_frame(&mut *w, &frame).await
        };
        let observed = match sent {
            Ok(()) => tokio::time::timeout(Duration::from_secs_f64(PAGE_WAIT), rx).await,
            Err(_) => {
                self.forget_id(&ident);
                self.kill();
                return Err(lost(op));
            }
        };
        self.forget_id(&ident);
        let observed = match observed {
            Ok(Ok(reply)) => reply,
            _ => {
                self.kill();
                return Err(lost(op));
            }
        };
        if observed.get("ok") != Some(&Json::Bool(true)) {
            let code = observed.get("error").and_then(Json::as_str);
            let Some(code) = code.filter(|c| PAGE_ERRORS.contains(c)) else {
                return Err(Refusal::new("BrowserRefused"));
            };
            if UNFINISHED.contains(&code) {
                return Err(lost(op));
            }
            let mut refusal = Refusal::new(code);
            for k in REFUSAL_FIELDS {
                if let Some(v) = observed.get(k) {
                    refusal = refusal.with(*k, v.clone());
                }
            }
            return Err(refusal);
        }
        if observed.get("email").and_then(Json::as_str) != Some(self.email.as_str()) {
            return Err(Refusal::new("IdentityMismatch"));
        }
        if want_tab.is_some() && observed.get("tab") != want_tab.as_ref() {
            return Err(Refusal::new("TabMismatch"));
        }
        Ok(observed.get("result").cloned().unwrap_or(Json::Null))
    }

    fn forget_id(&self, ident: &str) {
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(ident);
    }

    /// Write a frame that is not a command (the hello acknowledgement).
    pub async fn send(&self, frame: &Json) -> Result<(), crate::wire::WireError> {
        let mut w = self.writer.lock().await;
        write_frame(&mut *w, frame).await
    }
}
