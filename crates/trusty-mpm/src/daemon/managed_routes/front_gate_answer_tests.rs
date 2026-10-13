//! The front gate's withheld launch and its answer classifier (#9843).
//!
//! Why: a launch whose runtime never came up settled as delivered, a launch
//! went ahead on a record whose state write failed, and the answer text was
//! never read, so "no" launched exactly as "yes" did.
//! What: drives [`launch_withheld`] with a [`CountingAdapter`] over an
//! [`AnswerDriver`]-backed manager, and tabulates [`front_gate_verdict`].
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use std::sync::atomic::{AtomicUsize, Ordering};

use tempfile::TempDir;

use super::{FrontGateVerdict, front_gate_verdict, launch_withheld};
use crate::runtime::{RuntimeAdapter, RuntimeError};
use crate::session_manager::decision_tests::{AnswerDriver, seed_answerable, set_mode};
use crate::session_manager::{ManagedSessionState, SessionManager};

/// A runtime adapter that counts its launches and starts nothing.
#[derive(Default)]
pub(crate) struct CountingAdapter {
    spawns: AtomicUsize,
}

impl CountingAdapter {
    /// How many times `spawn` was called.
    pub(crate) fn spawns(&self) -> usize {
        self.spawns.load(Ordering::SeqCst)
    }
}

impl RuntimeAdapter for CountingAdapter {
    fn spawn(
        &self,
        _tmux_name: &str,
        _cwd: &std::path::Path,
        _task: &str,
        _session_id: &str,
        _gh_env: &[(String, String)],
    ) -> Result<(), RuntimeError> {
        self.spawns.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn identify(&self) -> &str {
        "counting"
    }
}

/// #9843 (AC-F): keystrokes that tmux accepted, behind which no runtime ever
/// came up, are a failed launch, not a delivered one. Takes the production
/// verify budget (~5 s).
#[tokio::test]
async fn spawn_runtime_errs_when_the_runtime_never_came_up() {
    let dir = TempDir::new().unwrap();
    let spec = TempDir::new().unwrap();
    let driver = AnswerDriver::new();
    driver.hooks.lock().unwrap().runtime_never_starts = true;
    let mgr = SessionManager::new(dir.path(), driver.clone())
        .await
        .expect("manager");
    let id = seed_answerable(&mgr, &dir, ManagedSessionState::Provisioning).await;
    let record = mgr.get(&id).await.expect("record");
    let adapter = CountingAdapter::default();

    let launched = launch_withheld(&mgr, &record, &adapter, &[], Some(spec.path())).await;

    let msg = launched.expect_err("a runtime that never came up must fail the launch");
    assert!(msg.contains("#8233"), "{msg}");
    assert_eq!(adapter.spawns(), 1);
    let after = mgr.get(&id).await.expect("record");
    assert_eq!(after.state, ManagedSessionState::Errored);
}

/// #9843 (FO-2): a record whose `Active` write failed is not launched; the
/// launch fails instead of running behind a record that still reads
/// `Provisioning`.
#[tokio::test]
async fn spawn_runtime_refuses_to_launch_when_the_state_write_fails() {
    let dir = TempDir::new().unwrap();
    let spec = TempDir::new().unwrap();
    let driver = AnswerDriver::new();
    let mgr = SessionManager::new(dir.path(), driver.clone())
        .await
        .expect("manager");
    let id = seed_answerable(&mgr, &dir, ManagedSessionState::Provisioning).await;
    let record = mgr.get(&id).await.expect("record");
    let adapter = CountingAdapter::default();

    set_mode(dir.path(), 0o555);
    let launched = launch_withheld(&mgr, &record, &adapter, &[], Some(spec.path())).await;
    set_mode(dir.path(), 0o755);

    assert_eq!(
        adapter.spawns(),
        0,
        "the runtime launched behind an unpersisted state: {launched:?}"
    );
    assert!(launched.is_err(), "{launched:?}");
}

/// #9843 (Q2): deny words win over approve words and over a proposed default
/// that is itself a deny word; anything else is unclear.
#[test]
fn front_gate_verdict_matrix() {
    use FrontGateVerdict::{Approve, Deny, Unclear};
    let cases: [(&str, Option<&str>, FrontGateVerdict); 14] = [
        ("yes", None, Approve),
        ("  Y ", None, Approve),
        ("approve", None, Approve),
        ("/approve", None, Approve),
        ("Proceed", None, Approve),
        ("use cursor", Some("use cursor"), Approve),
        ("no", None, Deny),
        ("no", Some("no"), Deny),
        (" NO\n", Some("yes"), Deny),
        ("/deny", None, Deny),
        ("reject", Some("reject"), Deny),
        ("abort", None, Deny),
        ("maybe later", Some("use cursor"), Unclear),
        ("", None, Unclear),
    ];
    for (answer, default, want) in cases {
        assert_eq!(
            front_gate_verdict(answer, default),
            want,
            "answer {answer:?}, proposed default {default:?}"
        );
    }
}
