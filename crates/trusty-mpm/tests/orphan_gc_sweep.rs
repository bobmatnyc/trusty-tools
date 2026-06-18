//! End-to-end safety tests for the orphan-GC sweep.
//!
//! Why: the orphan-GC kills tmux sessions, and a false kill destroys a live
//! agent's work. These tests prove the conservative reaping rule against a fake
//! [`ManagedTmuxDriver`] that records exactly which sessions were killed: given a
//! realistic mix of tracked/untracked × active/idle × managed/foreign sessions,
//! ONLY the untracked-idle managed session is ever reaped — and only after the
//! two-pass debounce, never on first sight.
//! What: drives [`orphan_gc::run_sweep`] with a recording fake driver and asserts
//! the killed-name list across passes.
//! Test: this file IS the test; run with `cargo test -p trusty-mpm --test orphan_gc_sweep`.

use std::sync::Mutex;

use trusty_mpm::daemon::orphan_gc::{
    AlwaysIdleProbe, ChildLivenessProbe, OrphanGc, PaneInfo, TrackedNames, run_sweep,
};
use trusty_mpm::session_manager::{ManagedError, ManagedTmuxDriver};

/// A fake driver that records every `kill_session` call and nothing else.
struct RecordingDriver {
    killed: Mutex<Vec<String>>,
}

impl RecordingDriver {
    fn new() -> Self {
        Self {
            killed: Mutex::new(Vec::new()),
        }
    }
    fn killed(&self) -> Vec<String> {
        self.killed.lock().unwrap().clone()
    }
}

impl ManagedTmuxDriver for RecordingDriver {
    fn create_session(&self, _name: &str, _workdir: &str) -> Result<(), ManagedError> {
        Ok(())
    }
    fn kill_session(&self, name: &str) -> Result<(), ManagedError> {
        self.killed.lock().unwrap().push(name.to_owned());
        Ok(())
    }
    fn send_line(&self, _name: &str, _text: &str) -> Result<(), ManagedError> {
        Ok(())
    }
    fn capture(&self, _name: &str, _lines: u32) -> Result<String, ManagedError> {
        Ok(String::new())
    }
    fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
        Ok(Vec::new())
    }
}

fn pane(name: &str, cmd: &str) -> PaneInfo {
    PaneInfo {
        session_name: name.to_string(),
        pane_current_command: cmd.to_string(),
        pane_pid: Some(9999),
    }
}

/// The headline safety guarantee: across a realistic mix, only the untracked
/// idle managed session is reaped — and only on the second pass (debounce).
#[test]
fn sweep_reaps_only_untracked_idle_managed_session_after_debounce() {
    // (a) tracked active, (b) tracked idle, (c) untracked active (non-shell),
    // (d) untracked idle bare-shell  <-- the ONLY legitimate orphan,
    // (e) non-managed-prefix idle bare shell.
    let panes = vec![
        pane("tmpm-a-tracked-active", "claude"),
        pane("tmpm-b-tracked-idle", "zsh"),
        pane("tmpm-c-untracked-active", "node"),
        pane("tmpm-d-untracked-idle", "zsh"),
        pane("a-developers-own-shell", "zsh"),
    ];
    let tracked = TrackedNames {
        legacy: ["tmpm-a-tracked-active".to_string()].into_iter().collect(),
        managed: ["tmpm-b-tracked-idle".to_string()].into_iter().collect(),
    };

    let driver = RecordingDriver::new();
    let probe = AlwaysIdleProbe;
    let mut gc = OrphanGc::new();

    // Pass 1: (d) is a candidate but the debounce must spare it — nothing killed.
    let reaped1 = run_sweep(&mut gc, &panes, &tracked, &probe, &driver);
    assert_eq!(reaped1, 0, "no session may be reaped on first observation");
    assert!(
        driver.killed().is_empty(),
        "first pass must kill nothing, killed: {:?}",
        driver.killed()
    );

    // Pass 2: (d) seen orphaned twice in a row → reaped. NOTHING else, ever.
    let reaped2 = run_sweep(&mut gc, &panes, &tracked, &probe, &driver);
    assert_eq!(reaped2, 1);
    assert_eq!(
        driver.killed(),
        vec!["tmpm-d-untracked-idle".to_string()],
        "only the untracked-idle managed session may be killed"
    );
}

/// A managed session running an agent but unknown to the registries must be
/// warned-and-kept across any number of passes — never reaped.
#[test]
fn sweep_never_reaps_untracked_active_managed_session() {
    let panes = vec![pane("tmpm-rogue-but-busy", "claude")];
    let driver = RecordingDriver::new();
    let probe = AlwaysIdleProbe;
    let mut gc = OrphanGc::new();

    for _ in 0..5 {
        run_sweep(&mut gc, &panes, &TrackedNames::default(), &probe, &driver);
    }
    assert!(
        driver.killed().is_empty(),
        "an untracked ACTIVE managed session must never be reaped, killed: {:?}",
        driver.killed()
    );
}

/// A foreign (non-`tmpm-`) idle shell — e.g. a developer's own tmux session —
/// must never be touched, no matter how many passes run.
#[test]
fn sweep_never_reaps_foreign_session() {
    let panes = vec![pane("my-work", "zsh"), pane("0", "bash")];
    let driver = RecordingDriver::new();
    let probe = AlwaysIdleProbe;
    let mut gc = OrphanGc::new();

    for _ in 0..5 {
        run_sweep(&mut gc, &panes, &TrackedNames::default(), &probe, &driver);
    }
    assert!(
        driver.killed().is_empty(),
        "foreign sessions must never be reaped, killed: {:?}",
        driver.killed()
    );
}

/// A live-child probe spares an otherwise-orphan-looking session (mid-spawn).
#[test]
fn sweep_spares_session_with_live_child() {
    struct LiveProbe;
    impl ChildLivenessProbe for LiveProbe {
        fn has_live_child(&self, _pid: Option<u32>) -> bool {
            true
        }
    }
    let panes = vec![pane("tmpm-mid-spawn", "zsh")];
    let driver = RecordingDriver::new();
    let mut gc = OrphanGc::new();

    for _ in 0..5 {
        run_sweep(
            &mut gc,
            &panes,
            &TrackedNames::default(),
            &LiveProbe,
            &driver,
        );
    }
    assert!(
        driver.killed().is_empty(),
        "a session with a live agent child must be spared, killed: {:?}",
        driver.killed()
    );
}
