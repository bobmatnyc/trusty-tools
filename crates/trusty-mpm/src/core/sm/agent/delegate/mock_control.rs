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

use crate::activity::cache::ActivityState;
use crate::core::sm::control::{
    LaunchParams, RawObservation, SessionControl, SessionControlError, Submit, Summary,
};

/// One recorded adoption: `(tmux_name, cwd, task, runtime)` (#1433).
///
/// Why: a named alias keeps the `adopts` log's type readable and satisfies the
/// `clippy::type_complexity` lint (a 4-tuple of optionals is "very complex").
/// What: the four `adopt` arguments the mock records for test assertions.
/// Test: `chat_action_tests.rs::execute_adopt_reads_args`.
type AdoptRecord = (String, String, Option<String>, Option<String>);

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
    /// Recorded adoptions from `adopt` (#1433).
    adopts: Mutex<Vec<AdoptRecord>>,
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
            adopts: Mutex::new(Vec::new()),
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

    /// The recorded adoptions `(tmux_name, cwd, task, runtime)` (test read, #1433).
    pub fn adopts(&self) -> Vec<AdoptRecord> {
        self.adopts.lock().expect("lock").clone()
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

    /// Record an injected text as a `(session_id, text)` send, regardless of the
    /// [`Submit`] mode, so the delegation tests can assert delivery (#1461).
    async fn inject_text(
        &self,
        session_id: &str,
        text: &str,
        _submit: Submit,
    ) -> Result<(), SessionControlError> {
        self.sends
            .lock()
            .expect("lock")
            .push((session_id.to_string(), text.to_string()));
        Ok(())
    }

    /// Return a deterministic raw observation derived from the scripted `get`
    /// body (the same OBSERVE input), with no LLM (#1461).
    async fn observe(
        &self,
        _session_id: &str,
        _lines: usize,
    ) -> Result<RawObservation, SessionControlError> {
        let body = self.get_body.lock().expect("lock").clone();
        let raw_pane = body
            .get("session")
            .and_then(|s| s.get("pane"))
            .and_then(|p| p.as_str())
            .unwrap_or_default()
            .to_string();
        Ok(RawObservation {
            raw_pane,
            runtime_active: true,
            pending_decision: None,
            proposed_default: None,
        })
    }

    /// Return a degraded `Unknown` summary (the mock has no LLM) so callers see
    /// the LLM-optional contract honored (#1461).
    async fn summarize(&self, _session_id: &str) -> Result<Summary, SessionControlError> {
        Ok(Summary {
            state: ActivityState::Unknown,
            summary: None,
            confidence: 0.0,
        })
    }

    /// Record an adoption and return a deterministic session id (#1433).
    ///
    /// Why: the action-loop dispatch test must assert that `sessions.adopt` parsed
    /// `tmux_name`/`cwd`/`task`/`runtime` correctly and reached `adopt` — the
    /// trait's default impl errors, so the mock overrides it to record + succeed.
    /// What: records `(tmux_name, cwd, task, runtime)` and returns `{ session_id }`
    /// minted from the same monotonic counter `launch` uses.
    /// Test: `chat_action_tests.rs::execute_adopt_reads_args`.
    async fn adopt(
        &self,
        tmux_name: &str,
        cwd: &str,
        task: Option<&str>,
        runtime: Option<&str>,
    ) -> Result<Value, SessionControlError> {
        let n = self.next_id.fetch_add(1, Ordering::SeqCst) + 1;
        self.adopts.lock().expect("lock").push((
            tmux_name.to_string(),
            cwd.to_string(),
            task.map(str::to_string),
            runtime.map(str::to_string),
        ));
        Ok(json!({ "session_id": format!("s-{n}") }))
    }
}
