//! End-to-end safety tests for the PID-file orphan-GC sweep (#1595, §10.3).
//!
//! Why: the PID-file registry SIGTERMs orphaned `claude` child processes whose
//! tmux pane is already gone — a false kill of a recycled PID would terminate an
//! unrelated process (§13.5). These tests prove, against a real on-disk registry
//! (a `tempfile::TempDir`) and a recording probe, that the sweep signals ONLY a
//! still-alive, still-`claude`, genuinely-untracked PID and removes every other
//! stale file WITHOUT signalling.
//! What: drives [`PidRegistry::sweep_orphans`] with a recording [`ProcessProbe`]
//! and asserts the terminated-PID list and the surviving PID files.
//! Test: this file IS the test; run with
//! `cargo test -p trusty-mpm --test pid_registry_sweep`.

use std::cell::RefCell;
use std::collections::HashSet;

use trusty_mpm::core::pid_registry::{PidRegistry, ProcessProbe};

/// A scripted probe: per-PID liveness / `claude`-name answers; records SIGTERMs.
struct RecordingProbe {
    alive: HashSet<u32>,
    claude: HashSet<u32>,
    terminated: RefCell<Vec<u32>>,
}

impl RecordingProbe {
    fn new(alive: &[u32], claude: &[u32]) -> Self {
        Self {
            alive: alive.iter().copied().collect(),
            claude: claude.iter().copied().collect(),
            terminated: RefCell::new(Vec::new()),
        }
    }
}

impl ProcessProbe for RecordingProbe {
    fn is_alive(&self, pid: u32) -> bool {
        self.alive.contains(&pid)
    }
    fn is_claude(&self, pid: u32) -> bool {
        self.claude.contains(&pid)
    }
    fn terminate(&self, pid: u32) -> bool {
        self.terminated.borrow_mut().push(pid);
        true
    }
}

fn live(ids: &[&str]) -> HashSet<String> {
    ids.iter().map(|s| s.to_string()).collect()
}

/// The full safety matrix in one sweep: ONLY the untracked-alive-`claude` PID is
/// SIGTERMed; the tracked file survives; the dead and reused files are removed
/// without any signal.
#[test]
fn sweep_signals_only_untracked_live_claude() {
    let tmp = tempfile::TempDir::new().unwrap();
    let reg = PidRegistry::new(tmp.path());

    reg.register("tracked", 100).unwrap(); // live + claude but TRACKED -> keep
    reg.register("orphan", 200).unwrap(); // untracked + live + claude -> KILL
    reg.register("dead", 300).unwrap(); // untracked + dead         -> remove stale
    reg.register("reused", 400).unwrap(); // untracked + live, NOT claude -> remove stale

    let probe = RecordingProbe::new(&[100, 200, 400], &[100, 200]);
    let outcome = reg.sweep_orphans(&live(&["tracked"]), &probe);

    assert_eq!(outcome.scanned, 4);
    assert_eq!(outcome.terminated, 1);
    assert_eq!(outcome.removed_stale, 2);
    assert_eq!(
        *probe.terminated.borrow(),
        vec![200],
        "only the untracked, live, claude PID may be signalled"
    );

    // Only the tracked session's PID file remains on disk.
    let mut remaining: Vec<String> = reg
        .entries()
        .unwrap()
        .into_iter()
        .map(|e| e.session_id)
        .collect();
    remaining.sort();
    assert_eq!(remaining, vec!["tracked".to_string()]);
}

/// A recycled PID now owned by a non-`claude` process is NEVER signalled (§13.5).
#[test]
fn sweep_never_signals_reused_pid() {
    let tmp = tempfile::TempDir::new().unwrap();
    let reg = PidRegistry::new(tmp.path());
    reg.register("reused", 4242).unwrap();

    // Alive, but NOT a claude process — the PID was recycled.
    let probe = RecordingProbe::new(&[4242], &[]);
    let outcome = reg.sweep_orphans(&live(&[]), &probe);

    assert_eq!(outcome.terminated, 0);
    assert_eq!(outcome.removed_stale, 1);
    assert!(
        probe.terminated.borrow().is_empty(),
        "a recycled PID must never receive SIGTERM"
    );
    assert!(reg.entries().unwrap().is_empty());
}

/// A registered, still-tracked session's PID file is left completely untouched.
#[test]
fn sweep_keeps_tracked_session_pidfile() {
    let tmp = tempfile::TempDir::new().unwrap();
    let reg = PidRegistry::new(tmp.path());
    reg.register("alive-sess", 7).unwrap();

    let probe = RecordingProbe::new(&[7], &[7]);
    let outcome = reg.sweep_orphans(&live(&["alive-sess"]), &probe);

    assert_eq!(outcome.terminated, 0);
    assert_eq!(outcome.removed_stale, 0);
    assert!(probe.terminated.borrow().is_empty());
    assert_eq!(reg.entries().unwrap().len(), 1);
}
