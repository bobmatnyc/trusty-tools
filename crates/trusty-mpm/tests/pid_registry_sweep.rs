//! End-to-end safety tests for the PID-file orphan-GC sweep (#1595, §10.3, #1918).
//!
//! Why: the PID-file registry SIGTERMs orphaned `claude` child processes whose
//! tmux pane is already gone — a false kill of a recycled PID would terminate an
//! unrelated process (§13.5), and a false kill right after a daemon restart would
//! terminate a perfectly legitimate, actively-used legacy session (#1918, since
//! the in-memory session registry is never persisted and is always empty
//! immediately after restart). These tests prove, against a real on-disk registry
//! (a `tempfile::TempDir`) and a recording probe, that the sweep signals ONLY a
//! still-alive, still-`claude`, genuinely-untracked PID that has ALSO survived
//! the two-pass restart-safety debounce, and removes every other stale file
//! WITHOUT signalling.
//! What: drives [`PidRegistry::sweep_orphans`] with a recording [`ProcessProbe`]
//! and a [`PidOrphanGc`] debounce, asserting the terminated-PID list and the
//! surviving PID files across one or more sweeps.
//! Test: this file IS the test; run with
//! `cargo test -p trusty-mpm --test integration pid_registry_sweep::`.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use trusty_mpm::core::pid_registry::{
    HostEvidence, PidOrphanGc, PidRegistry, ProcessProbe, SweepOutcome,
};

/// A scripted probe: per-PID liveness / `claude`-name / tmux-host answers;
/// records SIGTERMs. A PID with no host answer reads `Unhosted` — its tmux
/// pane is gone, the true-orphan case (#10753).
struct RecordingProbe {
    alive: HashSet<u32>,
    claude: HashSet<u32>,
    hosts: RefCell<HashMap<u32, HostEvidence>>,
    terminated: RefCell<Vec<u32>>,
}

impl RecordingProbe {
    fn new(alive: &[u32], claude: &[u32]) -> Self {
        Self {
            alive: alive.iter().copied().collect(),
            claude: claude.iter().copied().collect(),
            hosts: RefCell::new(HashMap::new()),
            terminated: RefCell::new(Vec::new()),
        }
    }

    /// Script the tmux-host answer for `pid` (changeable between sweeps).
    fn set_host(&self, pid: u32, evidence: HostEvidence) {
        self.hosts.borrow_mut().insert(pid, evidence);
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
    fn host_evidence(&self, pid: u32) -> HostEvidence {
        self.hosts
            .borrow()
            .get(&pid)
            .copied()
            .unwrap_or(HostEvidence::Unhosted)
    }
}

fn live(ids: &[&str]) -> HashSet<String> {
    ids.iter().map(|s| s.to_string()).collect()
}

/// The full safety matrix across two sweeps: the untracked-alive-`claude` PID is
/// deferred on the first sweep (restart-safety debounce, #1918) and SIGTERMed
/// only once confirmed on the second; the tracked file survives both passes; the
/// dead and reused files are removed immediately, without any signal or debounce.
#[test]
fn sweep_signals_only_untracked_live_claude_after_two_sweeps() {
    let tmp = tempfile::TempDir::new().unwrap();
    let reg = PidRegistry::new(tmp.path());

    reg.register("tracked", 100).unwrap(); // live + claude but TRACKED -> keep
    reg.register("orphan", 200).unwrap(); // untracked + live + claude -> KILL (2nd sweep)
    reg.register("dead", 300).unwrap(); // untracked + dead         -> remove stale
    reg.register("reused", 400).unwrap(); // untracked + live, NOT claude -> remove stale

    let probe = RecordingProbe::new(&[100, 200, 400], &[100, 200]);
    let mut gc = PidOrphanGc::new();

    // First sweep: dead/reused are removed immediately; the genuine orphan is
    // only a first sighting, so it is deferred rather than signalled.
    let outcome1 = reg.sweep_orphans(&live(&["tracked"]), &probe, &mut gc);
    assert_eq!(outcome1.scanned, 4);
    assert_eq!(outcome1.terminated, 0);
    assert_eq!(outcome1.deferred, 1);
    assert_eq!(outcome1.removed_stale, 2);
    assert!(probe.terminated.borrow().is_empty());

    // Second sweep: the orphan is confirmed on two consecutive sweeps -> killed.
    let outcome2 = reg.sweep_orphans(&live(&["tracked"]), &probe, &mut gc);
    assert_eq!(outcome2.terminated, 1);
    assert_eq!(outcome2.deferred, 0);
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

/// A recycled PID now owned by a non-`claude` process is NEVER signalled (§13.5),
/// removed immediately on the first sweep — stale-file removal is not debounced.
#[test]
fn sweep_never_signals_reused_pid() {
    let tmp = tempfile::TempDir::new().unwrap();
    let reg = PidRegistry::new(tmp.path());
    reg.register("reused", 4242).unwrap();

    // Alive, but NOT a claude process — the PID was recycled.
    let probe = RecordingProbe::new(&[4242], &[]);
    let outcome = reg.sweep_orphans(&live(&[]), &probe, &mut PidOrphanGc::new());

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
    let outcome = reg.sweep_orphans(&live(&["alive-sess"]), &probe, &mut PidOrphanGc::new());

    assert_eq!(outcome.terminated, 0);
    assert_eq!(outcome.removed_stale, 0);
    assert!(probe.terminated.borrow().is_empty());
    assert_eq!(reg.entries().unwrap().len(), 1);
}

/// The #1918 regression test: simulates a daemon restart across which a legacy
/// session's pidfile persists on disk while its `claude` process stays alive.
///
/// Why: `DaemonState::with_root`/`::new` always start with an EMPTY in-memory
/// `sessions` registry — there is no on-disk persistence for it — so
/// `gather_live_session_ids()` is empty immediately after every restart. Before
/// #1918 this meant `classify_pidfile` treated any pre-existing pidfile as an
/// orphan on the very first post-restart sweep and SIGTERMed a live, legitimate
/// session. This test drives that exact scenario end-to-end against the real
/// on-disk [`PidRegistry`] and proves the FIRST sweep after "restart" does NOT
/// terminate the still-alive process, only a confirmed SECOND sweep does.
#[test]
fn restart_does_not_kill_still_alive_legacy_session_on_first_sweep() {
    let tmp = tempfile::TempDir::new().unwrap();
    // Simulate the PRE-restart daemon: a legacy session registers its PID.
    let reg = PidRegistry::new(tmp.path());
    reg.register("legacy-session-still-alive", 9001).unwrap();

    // Simulate the POST-restart daemon: a brand-new `PidOrphanGc` (the daemon
    // rebuilds this on every process start) and an EMPTY live-session-id set
    // (the in-memory `DaemonState.sessions` registry is never persisted, per
    // `crates/trusty-mpm/src/daemon/state/core.rs`'s constructors).
    let live_ids: HashSet<String> = HashSet::new();
    let probe = RecordingProbe::new(&[9001], &[9001]); // still alive, still claude
    let mut gc = PidOrphanGc::new();

    // The very first sweep after restart MUST NOT signal the still-alive process.
    let first_sweep = reg.sweep_orphans(&live_ids, &probe, &mut gc);
    assert_eq!(
        first_sweep.terminated, 0,
        "a daemon restart must never SIGTERM a legacy session on the first sweep"
    );
    assert_eq!(first_sweep.deferred, 1);
    assert!(
        probe.terminated.borrow().is_empty(),
        "no signal may be sent on the first post-restart sweep"
    );
    assert_eq!(
        reg.entries().unwrap().len(),
        1,
        "the pidfile must still be on disk after the first sweep"
    );

    // A second sweep with the session STILL untracked (e.g. it never got
    // re-registered and is genuinely gone) is what finally reaps it — proving
    // the debounce is a grace period, not a permanent exemption. The probe
    // reports no tmux host for 9001, so this is a true orphan (#10753).
    let second_sweep = reg.sweep_orphans(&live_ids, &probe, &mut gc);
    assert_eq!(second_sweep.terminated, 1);
    assert_eq!(*probe.terminated.borrow(), vec![9001]);
    assert!(reg.entries().unwrap().is_empty());
}

/// The #10753 regression: after a daemon restart, with an EMPTY live set and
/// no hook event, an idle `claude` whose tmux pane still exists is never
/// signalled — not on the first sweep and not on the second.
///
/// Why: the #1918 debounce is time only; before #10753 the second sweep
/// SIGTERMed this process (the same inputs as
/// `restart_does_not_kill_still_alive_legacy_session_on_first_sweep`, which
/// reaps on sweep two). The live tmux pane is the evidence that spares it.
#[test]
fn restart_with_empty_live_set_spares_tmux_hosted_claude_across_two_sweeps() {
    let tmp = tempfile::TempDir::new().unwrap();
    let reg = PidRegistry::new(tmp.path());
    reg.register("legacy-idle-in-tmux", 9002).unwrap();

    // Post-restart: fresh debounce, empty live set, pid alive + claude, and a
    // tmux pane still hosts it.
    let live_ids: HashSet<String> = HashSet::new();
    let probe = RecordingProbe::new(&[9002], &[9002]);
    probe.set_host(9002, HostEvidence::Hosted);
    let mut gc = PidOrphanGc::new();

    let spared = SweepOutcome {
        scanned: 1,
        ..SweepOutcome::default()
    };
    assert_eq!(reg.sweep_orphans(&live_ids, &probe, &mut gc), spared);
    assert_eq!(reg.sweep_orphans(&live_ids, &probe, &mut gc), spared);
    assert!(
        probe.terminated.borrow().is_empty(),
        "a claude whose tmux pane still exists must never receive SIGTERM"
    );
    assert_eq!(
        reg.entries().unwrap().len(),
        1,
        "the hosted session's pidfile must survive both sweeps"
    );
}

/// The #10753 pair: same restart shape, but the tmux pane is gone (the
/// `claude` was reparented away from tmux) — the true orphan is still reaped
/// on the second sweep.
#[test]
fn restart_with_empty_live_set_reaps_claude_whose_tmux_is_gone() {
    let tmp = tempfile::TempDir::new().unwrap();
    let reg = PidRegistry::new(tmp.path());
    reg.register("legacy-orphaned", 9003).unwrap();

    let live_ids: HashSet<String> = HashSet::new();
    let probe = RecordingProbe::new(&[9003], &[9003]);
    probe.set_host(9003, HostEvidence::Unhosted);
    let mut gc = PidOrphanGc::new();

    let first = reg.sweep_orphans(&live_ids, &probe, &mut gc);
    assert_eq!(first.terminated, 0);
    assert_eq!(first.deferred, 1);
    let second = reg.sweep_orphans(&live_ids, &probe, &mut gc);
    assert_eq!(second.terminated, 1);
    assert_eq!(*probe.terminated.borrow(), vec![9003]);
    assert!(reg.entries().unwrap().is_empty());
}

/// The #10753 error arm: when the process table cannot say whether a tmux
/// pane hosts the PID, the sweep fails closed — no SIGTERM on any number of
/// sweeps, the file is kept, and once the answer returns as `Unhosted` the
/// kill still needs two fresh confirmed sightings.
#[test]
fn sweep_never_signals_when_host_cannot_be_determined() {
    let tmp = tempfile::TempDir::new().unwrap();
    let reg = PidRegistry::new(tmp.path());
    reg.register("host-unknown", 9004).unwrap();

    let live_ids: HashSet<String> = HashSet::new();
    let probe = RecordingProbe::new(&[9004], &[9004]);
    probe.set_host(9004, HostEvidence::Unknown);
    let mut gc = PidOrphanGc::new();

    for _ in 0..3 {
        let outcome = reg.sweep_orphans(&live_ids, &probe, &mut gc);
        assert_eq!(outcome.terminated, 0);
        assert_eq!(outcome.deferred, 0);
        assert_eq!(outcome.unverifiable, 1);
    }
    assert!(probe.terminated.borrow().is_empty());
    assert_eq!(reg.entries().unwrap().len(), 1);

    // The check answers again and the pane is gone: a fresh first sighting.
    probe.set_host(9004, HostEvidence::Unhosted);
    let first = reg.sweep_orphans(&live_ids, &probe, &mut gc);
    assert_eq!(first.terminated, 0);
    assert_eq!(first.deferred, 1);
    let second = reg.sweep_orphans(&live_ids, &probe, &mut gc);
    assert_eq!(second.terminated, 1);
    assert_eq!(*probe.terminated.borrow(), vec![9004]);
}
