//! A scriptable mock [`SessionControl`] for the SM-8 delegation-loop tests.
//!
//! Why: the delegation loop must be exercised HERMETICALLY — no tmux, no real
//! spawn, no network. The loop depends on `SessionControl` (Dependency Inversion)
//! exactly so a test can inject a deterministic mock that (a) records every
//! `launch`/`send` so the test can assert the loop launched + LINKED + DELIVERED
//! the task (#1299), and (b) returns a SCRIPTED `get` response so OBSERVE/VERIFY
//! see a controlled pane (with or without evidence). This is that mock.
//! What: [`MockSessionControl`] hands out incrementing session ids on `launch`,
//! records `(session_id, params)` launches and `(session_id, text)` sends in a
//! shared log the test reads, and returns a configurable `get` body (default: a
//! `running` pane with no evidence; settable to a pane carrying evidence). All
//! verbs are deterministic and panic-free.
//! Test: drives `delegate_tests.rs`.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::core::sm::control::{LaunchParams, SessionControl, SessionControlError};

/// A scriptable, recording mock [`SessionControl`] (test-only).
///
/// Why: see the module docs — records launches/sends and returns a scripted
/// `get` so the loop is fully assertable with no real session.
/// What: a launch counter (for deterministic ids), a shared launch log, a shared
/// send log, and the JSON the next `get` returns (the observed pane).
/// Test: `delegate_tests.rs`.
pub struct MockSessionControl {
    /// Monotonic counter minting `s-1`, `s-2`, … session ids on launch.
    next_id: AtomicUsize,
    /// Recorded launches: `(session_id, LaunchParams)`.
    launches: Mutex<Vec<(String, LaunchParams)>>,
    /// Recorded task deliveries: `(session_id, text)` from `send` (#1299).
    sends: Mutex<Vec<(String, String)>>,
    /// The pane/record JSON each `get` returns (the OBSERVE input). Shared so a
    /// test can script evidence (or none).
    get_body: Mutex<Value>,
}

impl Default for MockSessionControl {
    fn default() -> Self {
        Self {
            next_id: AtomicUsize::new(0),
            launches: Mutex::new(Vec::new()),
            sends: Mutex::new(Vec::new()),
            // Default observation: a running session with NO evidence — the gate
            // stays closed unless a test scripts evidence.
            get_body: Mutex::new(json!({ "session": { "state": "running" } })),
        }
    }
}

impl MockSessionControl {
    /// A mock whose `get` returns a pane CARRYING the given evidence text.
    ///
    /// Why: the verification-gate tests need a session that observes as `Verified`
    /// (a PR URL / test-pass output in the pane). This builds a mock that scripts
    /// that evidence so OBSERVE captures it.
    /// What: returns a mock whose `get` body embeds `evidence` in a pane field.
    /// Test: `delegate_tests.rs::delegate_verifies_and_closes_with_evidence`.
    pub fn with_evidence(evidence: &str) -> Self {
        let mock = Self::default();
        *mock.get_body.lock().expect("lock") =
            json!({ "session": { "state": "running", "pane": evidence } });
        mock
    }

    /// The recorded launches `(session_id, params)` (test read).
    pub fn launches(&self) -> Vec<(String, LaunchParams)> {
        self.launches.lock().expect("lock").clone()
    }

    /// The recorded task deliveries `(session_id, text)` (test read, #1299).
    pub fn sends(&self) -> Vec<(String, String)> {
        self.sends.lock().expect("lock").clone()
    }
}

#[async_trait]
impl SessionControl for MockSessionControl {
    async fn launch(&self, params: LaunchParams) -> Result<Value, SessionControlError> {
        let n = self.next_id.fetch_add(1, Ordering::SeqCst) + 1;
        let session_id = format!("s-{n}");
        self.launches
            .lock()
            .expect("lock")
            .push((session_id.clone(), params));
        Ok(json!({ "session_id": session_id }))
    }

    async fn list(&self) -> Result<Value, SessionControlError> {
        Ok(json!({ "sessions": [] }))
    }

    async fn get(&self, _session_id: &str) -> Result<Value, SessionControlError> {
        Ok(self.get_body.lock().expect("lock").clone())
    }

    async fn send(&self, session_id: &str, text: &str) -> Result<Value, SessionControlError> {
        self.sends
            .lock()
            .expect("lock")
            .push((session_id.to_string(), text.to_string()));
        Ok(json!({ "ok": true }))
    }

    async fn stop(&self, _session_id: &str) -> Result<Value, SessionControlError> {
        Ok(json!({ "ok": true }))
    }

    async fn resume(&self, _session_id: &str) -> Result<Value, SessionControlError> {
        Ok(json!({ "ok": true }))
    }

    async fn kill(&self, _session_id: &str) -> Result<Value, SessionControlError> {
        Ok(json!({ "ok": true }))
    }
}
