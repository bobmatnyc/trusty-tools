//! Contract tests for the harness-agnostic session-capability API (#1461).
//!
//! Why: the `SessionControl` seam grows three standard verbs every harness uses
//! — `inject_text` (write, with a [`Submit`] mode), `observe` (LLM-free read),
//! and `summarize` (LLM-OPTIONAL inference). These tests prove the CONTRACT at
//! the substance behind the trait: `SessionManager::inject` dispatches the three
//! keystroke intents onto the driver; `SessionManager::observe` returns the raw
//! pane + liveness WITHOUT any LLM; and the activity monitor that backs
//! `summarize` degrades to `Unknown`/`None` without a key and serves a cached
//! verdict on identical pane content.
//! What: a recording `ManagedTmuxDriver` lets us assert the exact driver calls
//! each `Submit` mode makes; a fake `LlmClassifier` lets us assert the cache-hit
//! and degrade behavior offline. Back-compat for `send`/`send_input` is asserted
//! alongside `inject(.., Enter)` so the two paths cannot diverge.
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm
//! --test session_control_api`.

use std::sync::{Arc, Mutex};

use tempfile::TempDir;

use trusty_mpm::core::sm::control::Submit;
use trusty_mpm::session_manager::{
    ManagedError, ManagedSessionId, ManagedTmuxDriver, SessionManager,
};

/// A recording tmux driver that captures EVERY keystroke primitive (#1461).
///
/// Why: the inject-dispatch contract is exactly "which driver call does each
/// `Submit` mode make?"; recording `send_line` / `send_keys_literal` /
/// `send_interrupt` separately lets each test assert the precise dispatch
/// (literal+Enter vs literal-only vs Ctrl-C). It also serves a scriptable
/// capture body and a liveness flag so `observe` can be exercised.
/// What: per-primitive call logs behind mutexes; `capture` returns a seeded
/// pane; `session_exists` is configurable so `runtime_active` can be both true
/// and false.
/// Test: drives every test in this file.
struct RecordingDriver {
    /// `(name, text)` recorded by `send_line` (the Enter path).
    send_line_calls: Mutex<Vec<(String, String)>>,
    /// `(name, text)` recorded by `send_keys_literal` (the NoSubmit path).
    literal_calls: Mutex<Vec<(String, String)>>,
    /// `name` recorded by `send_interrupt` (the Interrupt path).
    interrupt_calls: Mutex<Vec<String>>,
    /// Pane text `capture` returns (the OBSERVE input).
    capture_body: Mutex<String>,
    /// Names that `session_exists` reports as live (also `create_session` adds).
    live: Mutex<Vec<String>>,
    /// What `session_exists` returns regardless of `live` (None ⇒ consult `live`).
    force_alive: Mutex<Option<bool>>,
}

impl RecordingDriver {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            send_line_calls: Mutex::new(Vec::new()),
            literal_calls: Mutex::new(Vec::new()),
            interrupt_calls: Mutex::new(Vec::new()),
            capture_body: Mutex::new(String::new()),
            live: Mutex::new(Vec::new()),
            force_alive: Mutex::new(None),
        })
    }
}

impl ManagedTmuxDriver for RecordingDriver {
    fn create_session(&self, name: &str, _workdir: &str) -> Result<(), ManagedError> {
        self.live.lock().unwrap().push(name.to_owned());
        Ok(())
    }
    fn kill_session(&self, _name: &str) -> Result<(), ManagedError> {
        Ok(())
    }
    fn send_line(&self, name: &str, text: &str) -> Result<(), ManagedError> {
        self.send_line_calls
            .lock()
            .unwrap()
            .push((name.to_owned(), text.to_owned()));
        Ok(())
    }
    fn send_keys_literal(&self, name: &str, text: &str) -> Result<(), ManagedError> {
        self.literal_calls
            .lock()
            .unwrap()
            .push((name.to_owned(), text.to_owned()));
        Ok(())
    }
    fn send_interrupt(&self, name: &str) -> Result<(), ManagedError> {
        self.interrupt_calls.lock().unwrap().push(name.to_owned());
        Ok(())
    }
    fn capture(&self, _name: &str, _lines: usize) -> Result<String, ManagedError> {
        Ok(self.capture_body.lock().unwrap().clone())
    }
    fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
        Ok(self.live.lock().unwrap().clone())
    }
    fn session_exists(&self, name: &str) -> bool {
        if let Some(f) = *self.force_alive.lock().unwrap() {
            return f;
        }
        self.live.lock().unwrap().iter().any(|n| n == name)
    }
}

/// Create a manager + a fresh active session, returning the id and the driver.
async fn make_session(dir: &TempDir) -> (SessionManager, Arc<RecordingDriver>, ManagedSessionId) {
    let driver = RecordingDriver::new();
    let mgr = SessionManager::new(dir.path(), driver.clone())
        .await
        .expect("manager");
    let record = mgr
        .create(
            "task".into(),
            Some(dir.path().to_path_buf()),
            Some("proj".into()),
            None,
            None,
            None,
        )
        .await
        .expect("create");
    (mgr, driver, record.id)
}

// ── inject_text dispatch (the three Submit modes) ────────────────────────────

#[tokio::test]
async fn inject_dispatch_enter_sends_literal_then_enter() {
    // Submit::Enter must route through the driver's send_line (literal + Enter),
    // NOT the literal-only or interrupt primitives.
    let dir = TempDir::new().unwrap();
    let (mgr, driver, id) = make_session(&dir).await;

    mgr.inject(&id, "do the thing", Submit::Enter)
        .await
        .expect("inject enter");

    let sends = driver.send_line_calls.lock().unwrap();
    assert_eq!(sends.len(), 1, "Enter must call send_line exactly once");
    assert_eq!(sends[0].1, "do the thing");
    assert!(
        driver.literal_calls.lock().unwrap().is_empty(),
        "Enter must NOT call send_keys_literal"
    );
    assert!(
        driver.interrupt_calls.lock().unwrap().is_empty(),
        "Enter must NOT send an interrupt"
    );
}

#[tokio::test]
async fn inject_dispatch_nosubmit_sends_literal_only() {
    // Submit::NoSubmit must type the literal text with NO Enter (literal path
    // only) and never touch send_line or interrupt.
    let dir = TempDir::new().unwrap();
    let (mgr, driver, id) = make_session(&dir).await;

    mgr.inject(&id, "partial cmd", Submit::NoSubmit)
        .await
        .expect("inject nosubmit");

    let literal = driver.literal_calls.lock().unwrap();
    assert_eq!(
        literal.len(),
        1,
        "NoSubmit must call send_keys_literal once"
    );
    assert_eq!(literal[0].1, "partial cmd");
    assert!(
        driver.send_line_calls.lock().unwrap().is_empty(),
        "NoSubmit must NOT call send_line (no Enter)"
    );
    assert!(
        driver.interrupt_calls.lock().unwrap().is_empty(),
        "NoSubmit must NOT send an interrupt"
    );
}

#[tokio::test]
async fn inject_dispatch_interrupt_sends_ctrl_c() {
    // Submit::Interrupt must send the interrupt (Ctrl-C) and ignore any text.
    let dir = TempDir::new().unwrap();
    let (mgr, driver, id) = make_session(&dir).await;

    mgr.inject(&id, "ignored", Submit::Interrupt)
        .await
        .expect("inject interrupt");

    let interrupts = driver.interrupt_calls.lock().unwrap();
    assert_eq!(interrupts.len(), 1, "Interrupt must send exactly one C-c");
    assert!(
        driver.send_line_calls.lock().unwrap().is_empty(),
        "Interrupt must NOT call send_line"
    );
    assert!(
        driver.literal_calls.lock().unwrap().is_empty(),
        "Interrupt must NOT call send_keys_literal"
    );
}

#[tokio::test]
async fn inject_activity_bump_only_for_enter_and_nosubmit_not_interrupt() {
    // Advisory follow-up (#1461): an Interrupt (Ctrl-C) is a STOP signal, not
    // forward progress. A freshly-created record has `last_activity_at == None`;
    // Enter/NoSubmit must set it, Interrupt must leave it untouched so the
    // idle/orphan-GC reconciliation is not misled into thinking a stalled
    // session is still working.
    let dir = TempDir::new().unwrap();

    // Interrupt: last_activity_at stays None.
    {
        let (mgr, _driver, id) = make_session(&dir).await;
        assert!(
            mgr.get(&id).await.unwrap().last_activity_at.is_none(),
            "precondition: a fresh record has no activity timestamp"
        );
        mgr.inject(&id, "ignored", Submit::Interrupt)
            .await
            .expect("inject interrupt");
        assert!(
            mgr.get(&id).await.unwrap().last_activity_at.is_none(),
            "Interrupt is a STOP signal and must NOT bump last_activity_at"
        );
    }

    // Enter: last_activity_at becomes Some.
    {
        let (mgr, _driver, id) = make_session(&dir).await;
        mgr.inject(&id, "go", Submit::Enter)
            .await
            .expect("inject enter");
        assert!(
            mgr.get(&id).await.unwrap().last_activity_at.is_some(),
            "Enter is forward progress and MUST bump last_activity_at"
        );
    }

    // NoSubmit: last_activity_at becomes Some.
    {
        let (mgr, _driver, id) = make_session(&dir).await;
        mgr.inject(&id, "partial", Submit::NoSubmit)
            .await
            .expect("inject nosubmit");
        assert!(
            mgr.get(&id).await.unwrap().last_activity_at.is_some(),
            "NoSubmit is forward progress and MUST bump last_activity_at"
        );
    }
}

// ── observe (LLM-free) ───────────────────────────────────────────────────────

#[tokio::test]
async fn observe_returns_raw_pane_without_llm() {
    // observe is ALWAYS available with no LLM configured: it returns whatever the
    // driver captured verbatim. `observe` is LLM-FREE — it never touches the
    // classifier — so no env manipulation is needed (and `std::env::remove_var`
    // is UB under the multi-threaded test runner). The raw pane + runtime_active
    // are returned regardless of any key.
    let dir = TempDir::new().unwrap();
    let (mgr, driver, id) = make_session(&dir).await;
    *driver.capture_body.lock().unwrap() = "line 1\nline 2\nPR: https://x/1".into();

    let obs = mgr.observe(&id, 60).await.expect("observe");
    assert_eq!(obs.raw_pane, "line 1\nline 2\nPR: https://x/1");
    assert_eq!(obs.pending_decision, None);
    assert_eq!(obs.proposed_default, None);
}

#[tokio::test]
async fn observe_reports_runtime_active() {
    // runtime_active reflects tmux liveness: true while the session exists,
    // false once it is gone — all without any LLM.
    let dir = TempDir::new().unwrap();
    let (mgr, driver, id) = make_session(&dir).await;

    *driver.force_alive.lock().unwrap() = Some(true);
    assert!(
        mgr.observe(&id, 10)
            .await
            .expect("observe alive")
            .runtime_active
    );

    *driver.force_alive.lock().unwrap() = Some(false);
    assert!(
        !mgr.observe(&id, 10)
            .await
            .expect("observe dead")
            .runtime_active
    );
}

// ── back-compat: send_input (the legacy `send` path) still works ─────────────

#[tokio::test]
async fn send_input_back_compat_still_uses_send_line() {
    // The pre-#1461 send_input path (what SessionControl::send wraps) must remain
    // a literal+Enter send — equivalent to inject(.., Submit::Enter).
    let dir = TempDir::new().unwrap();
    let (mgr, driver, id) = make_session(&dir).await;

    mgr.send_input(&id, "legacy text")
        .await
        .expect("send_input");

    let sends = driver.send_line_calls.lock().unwrap();
    assert_eq!(sends.len(), 1);
    assert_eq!(sends[0].1, "legacy text");
}

// ── summarize: LLM-OPTIONAL degrade + cache (via the activity monitor) ────────

mod summarize {
    use trusty_mpm::activity::cache::{ActivityState, ActivityVerdict};
    use trusty_mpm::activity::monitor::{ActivityError, ActivityMonitor, LlmClassifier};

    use std::sync::Mutex;

    /// A classifier that always reports `MissingApiKey` — models an unconfigured
    /// host so we can prove `summarize` degrades to Unknown/None, never errors.
    struct NoKeyClassifier;
    impl LlmClassifier for NoKeyClassifier {
        async fn classify(
            &self,
            _pane: &str,
        ) -> Result<(ActivityVerdict, u32, u32), ActivityError> {
            Err(ActivityError::MissingApiKey)
        }
    }

    use std::sync::Arc;

    /// A classifier returning a fixed verdict and counting calls, so we can prove
    /// a content-hash cache hit serves the cached verdict without re-classifying.
    /// The call counter is an `Arc<Mutex>` the test keeps a handle to so it can
    /// read the count even after the classifier is moved into the monitor.
    struct CountingClassifier {
        verdict: ActivityVerdict,
        calls: Arc<Mutex<u32>>,
    }
    impl LlmClassifier for CountingClassifier {
        async fn classify(
            &self,
            _pane: &str,
        ) -> Result<(ActivityVerdict, u32, u32), ActivityError> {
            *self.calls.lock().unwrap() += 1;
            Ok((self.verdict.clone(), 10, 5))
        }
    }

    /// Map a verdict the way `summarize` does: drop the placeholder summary to
    /// None whenever the state is Unknown (the LLM-optional contract).
    fn summary_text(v: &ActivityVerdict) -> Option<String> {
        if v.state == ActivityState::Unknown {
            None
        } else {
            Some(v.summary.clone())
        }
    }

    #[tokio::test]
    async fn summarize_degrades_to_unknown_without_key() {
        // No key ⇒ the monitor yields an Unknown verdict (NOT an error), and the
        // summarize mapping drops the placeholder text to None.
        let monitor = ActivityMonitor::new(NoKeyClassifier, "test-model");
        let result = monitor
            .check("s1", "pane content")
            .await
            .expect("check must not error on missing key");
        assert_eq!(result.verdict.state, ActivityState::Unknown);
        assert_eq!(summary_text(&result.verdict), None);
    }

    #[tokio::test]
    async fn summarize_returns_fake_verdict() {
        // A configured (fake) classifier ⇒ the verdict flows through with a real
        // summary string.
        let classifier = CountingClassifier {
            verdict: ActivityVerdict {
                state: ActivityState::Working,
                summary: "writing code".into(),
                confidence: 0.9,
            },
            calls: Arc::new(Mutex::new(0)),
        };
        let monitor = ActivityMonitor::new(classifier, "test-model");
        let result = monitor.check("s1", "pane A").await.expect("check");
        assert_eq!(result.verdict.state, ActivityState::Working);
        assert_eq!(
            summary_text(&result.verdict).as_deref(),
            Some("writing code")
        );
        assert!(!result.cache_hit);
    }

    #[tokio::test]
    async fn summarize_serves_cached_verdict() {
        // Identical pane content ⇒ the second check is a content-hash cache hit,
        // so the classifier is called exactly once and the verdict is reused.
        let calls = Arc::new(Mutex::new(0));
        let classifier = CountingClassifier {
            verdict: ActivityVerdict {
                state: ActivityState::Working,
                summary: "same".into(),
                confidence: 1.0,
            },
            calls: calls.clone(),
        };
        let monitor = ActivityMonitor::new(classifier, "test-model");

        let r1 = monitor.check("s1", "unchanged pane").await.expect("first");
        assert!(!r1.cache_hit);
        let r2 = monitor.check("s1", "unchanged pane").await.expect("second");
        assert!(r2.cache_hit, "identical pane must be a cache hit");
        assert_eq!(*calls.lock().unwrap(), 1, "classifier called once only");
        assert_eq!(r2.verdict.summary, "same");
    }
}
