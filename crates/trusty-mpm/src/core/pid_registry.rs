//! On-disk PID-file registry for daemon-owned `claude` child processes.
//!
//! Why: a tmux session can vanish (or a daemon can crash with `kill -9`) while
//! the `claude` process it launched keeps running, reparented to PID 1. The
//! name-scanning orphan-GC (`crate::daemon::orphan_gc`) reconciles *tmux
//! sessions* against the registries, but it cannot see a `claude` process whose
//! tmux pane is already gone — that process is invisible to `tmux ls`. The spec
//! (DOC-26 §10.3) closes that gap with a PID-file registry: at spawn the daemon
//! writes the child PID to `~/.trusty-mpm/pids/<session-id>.pid`; a restarted
//! daemon reads every `.pid` file, cross-references the live session registry,
//! and SIGTERMs any PID whose session-id is no longer tracked. Tying each PID to
//! a specific session-id (rather than scanning `/proc/*/comm` for "claude") is
//! unambiguous: it never matches an unrelated user-launched `claude`, and it
//! survives PID-1 reparenting.
//!
//! What: [`PidRegistry`] is a thin handle over the `pids/` directory.
//! [`register`](PidRegistry::register) writes (atomically, before `spawn`
//! returns — the §10.3 invariant) a `<session-id>.pid` file;
//! [`unregister`](PidRegistry::unregister) removes it on normal termination;
//! [`entries`](PidRegistry::entries) enumerates `(session_id, pid)` pairs; and
//! [`sweep_orphans`](PidRegistry::sweep_orphans) is the GC pass — for every PID
//! file whose session-id is NOT in the live set it verifies (via the injected
//! [`ProcessProbe`]) that the process is still alive AND still named `claude`
//! before sending SIGTERM, then removes the file. Mismatches and dead PIDs are
//! removed WITHOUT signalling (stale or PID-reused — see §13.5).
//!
//! Safety: the name-verification gate is the alpha-1 mitigation for the PID-reuse
//! race (§13.5, accepted limitation). It reduces — but does not eliminate — the
//! chance of SIGTERMing a recycled PID. Every decision is a pure function of the
//! probe's answer, so the full matrix is unit-testable with a fake probe and a
//! `tempfile` directory; no real process is ever signalled in tests.
//!
//! Test: the `tests` submodule exercises register/unregister/entries round-trips
//! and the full [`classify_pidfile`] decision matrix against a fake probe and a
//! temp directory.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use tracing::{debug, info, warn};

/// File-extension marker for PID files in the registry directory.
const PID_FILE_EXT: &str = "pid";

/// A liveness + identity probe over an OS process, abstracted for testing.
///
/// Why: the orphan sweep must answer two questions about a recorded PID — "is it
/// still alive?" and "is it still a `claude` process?" — both of which poke the
/// real OS. Abstracting them behind a trait keeps [`classify_pidfile`] a pure
/// decision function and lets tests inject deterministic answers instead of
/// spawning (and signalling) real processes.
/// What: two methods — [`is_alive`](ProcessProbe::is_alive) and
/// [`is_claude`](ProcessProbe::is_claude) — plus a [`terminate`](ProcessProbe::terminate)
/// action seam so the sweep's SIGTERM is also test-observable. The production
/// impl ([`OsProcessProbe`]) delegates to `crate::core::process`.
/// Test: [`FakeProbe`] in the `tests` submodule drives the decision matrix.
pub trait ProcessProbe {
    /// True if a process with this PID currently exists.
    ///
    /// Why: a dead PID's file is stale and must be removed without signalling.
    /// What: existence check (e.g. `kill(pid, 0)`).
    /// Test: `FakeProbe` returns a canned answer.
    fn is_alive(&self, pid: u32) -> bool;

    /// True if the process at this PID is (still) named `claude`.
    ///
    /// Why: the alpha-1 PID-reuse mitigation (§13.5) — before SIGTERMing an
    /// orphan we confirm the PID still refers to a `claude` process, so a
    /// recycled PID assigned to an unrelated process is spared.
    /// What: command-name check (e.g. `/proc/<pid>/comm` or `ps -o comm=`).
    /// Test: `FakeProbe` returns a canned answer.
    fn is_claude(&self, pid: u32) -> bool;

    /// Send SIGTERM to the process at this PID; returns whether it was delivered.
    ///
    /// Why: making the kill an explicit trait method keeps the sweep pure-ish and
    /// lets tests assert exactly which PIDs were signalled without a real kill.
    /// What: best-effort SIGTERM; `false` if the signal could not be delivered
    /// (e.g. the process exited between the check and the signal — a benign race).
    /// Test: `FakeProbe` records the PID and reports success.
    fn terminate(&self, pid: u32) -> bool;
}

/// The verdict for a single `.pid` file during an orphan sweep.
///
/// Why: separating the decision from the file-system action makes the
/// safety-critical SIGTERM rule unit-testable and gives the sweep a clear,
/// auditable per-file outcome to log.
/// What: one variant per outcome — keep (session still tracked), terminate
/// (untracked, alive, still `claude` → SIGTERM + remove the file), or remove the
/// stale file without signalling (untracked but dead, or the PID was reused by a
/// non-`claude` process).
/// Test: `classify_*` unit tests assert the variant for each input class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PidDecision {
    /// The session-id is still in the live registry — leave the PID file alone.
    Keep,
    /// Untracked, alive, and still a `claude` process → SIGTERM, then remove.
    Terminate,
    /// Untracked but dead, or alive under a reused (non-`claude`) PID → remove
    /// the stale file without signalling. Carries why, for the audit log.
    RemoveStale(StaleReason),
}

/// Why a `.pid` file was removed without sending a signal.
///
/// Why: a single `RemoveStale` would lose the audit trail; recording the reason
/// lets the sweep log explain itself and lets tests assert the precise gate.
/// What: the mutually-exclusive reasons a stale file is dropped silently.
/// Test: asserted by `classify_removes_dead`, `classify_removes_reused_pid`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StaleReason {
    /// No process exists at the recorded PID — the child already exited.
    ProcessDead,
    /// A process exists at the PID but is no longer named `claude` (PID reused).
    PidReused,
}

/// One parsed `.pid` file: a session-id and the PID it recorded.
///
/// Why: the sweep reasons about plain data — the file's stem (session-id) and
/// its contents (PID) — so parsing is separated from the decision logic.
/// What: the `<session-id>` parsed from the file name and the `pid` read from
/// its body.
/// Test: built throughout the `tests` submodule and by [`PidRegistry::entries`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PidEntry {
    /// The session-id (the `.pid` file's stem).
    pub session_id: String,
    /// The recorded OS process id.
    pub pid: u32,
}

/// Decide what to do with one PID-file entry — the heart of the sweep (pure).
///
/// Why: this is the one place the PID-reuse mitigation (§13.5) lives, so it can
/// be audited and unit-tested exhaustively. A bug here risks SIGTERMing the wrong
/// process, so the rule is conservative: signal ONLY a still-alive, still-named-
/// `claude`, genuinely-untracked PID.
/// What: a [`PidDecision`] computed as —
/// 1. session-id present in `live_session_ids` → [`PidDecision::Keep`]; else
/// 2. process not alive → [`PidDecision::RemoveStale`]`(ProcessDead)`; else
/// 3. process alive but not named `claude` → [`PidDecision::RemoveStale`]`(PidReused)`; else
/// 4. untracked, alive, still `claude` → [`PidDecision::Terminate`].
/// Test: `classify_keeps_tracked`, `classify_removes_dead`,
/// `classify_removes_reused_pid`, `classify_terminates_untracked_claude`.
pub fn classify_pidfile(
    entry: &PidEntry,
    live_session_ids: &HashSet<String>,
    probe: &dyn ProcessProbe,
) -> PidDecision {
    // Gate 1: still tracked → never touch (the common, safe case).
    if live_session_ids.contains(&entry.session_id) {
        return PidDecision::Keep;
    }
    // Gate 2: dead PID → the file is stale; drop it without signalling.
    if !probe.is_alive(entry.pid) {
        return PidDecision::RemoveStale(StaleReason::ProcessDead);
    }
    // Gate 3: alive but not `claude` → PID was reused; spare it (§13.5).
    if !probe.is_claude(entry.pid) {
        return PidDecision::RemoveStale(StaleReason::PidReused);
    }
    // All gates passed: an untracked, live, genuine `claude` orphan.
    PidDecision::Terminate
}

/// Outcome counts from one [`PidRegistry::sweep_orphans`] pass.
///
/// Why: the daemon's GC loop logs a per-pass summary and `GET /health` may
/// surface orphan-GC activity (§10.3); returning structured counts keeps the
/// caller free of any file-system detail.
/// What: how many PID files were scanned, terminated (SIGTERM sent), and removed
/// as stale (no signal).
/// Test: asserted by `sweep_terminates_only_untracked_claude` and siblings.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SweepOutcome {
    /// Total `.pid` files examined this pass.
    pub scanned: usize,
    /// PIDs that were SIGTERMed (untracked, alive, still `claude`).
    pub terminated: usize,
    /// Stale PID files removed without signalling (dead PID or reused PID).
    pub removed_stale: usize,
}

/// A handle over the `~/.trusty-mpm/pids/` PID-file directory.
///
/// Why: every caller (the spawn path, the session-drop path, the GC loop) needs
/// a single, consistent answer for "where do PID files live and how are they
/// named?". Wrapping the directory in one type keeps that convention in one place
/// and makes the whole registry hermetically testable by pointing `dir` at a
/// `tempfile::TempDir`.
/// What: holds the absolute `pids/` directory path. The directory is created
/// lazily on first [`register`](Self::register). File names are
/// `<session-id>.pid`; bodies are the decimal PID.
/// Test: the `tests` submodule constructs a registry under a temp dir.
#[derive(Debug, Clone)]
pub struct PidRegistry {
    /// Absolute path of the `pids/` directory.
    dir: PathBuf,
}

impl PidRegistry {
    /// Build a registry rooted at an explicit `pids/` directory.
    ///
    /// Why: tests must point the registry at a temp dir, and the daemon resolves
    /// the directory from its framework root — both want to supply the path
    /// directly rather than re-derive the home directory.
    /// What: stores `dir` verbatim (created lazily on first write).
    /// Test: used throughout the `tests` submodule.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// Build the registry under a framework base directory (`<base>/pids`).
    ///
    /// Why: production callers hold a framework root (`~/.trusty-mpm`) and want
    /// the canonical `pids/` subdirectory without hard-coding the join at each
    /// call site (the same drift the `paths` module guards against).
    /// What: joins `<base>/pids` and delegates to [`new`](Self::new).
    /// Test: `under_nests_pids_subdir`.
    pub fn under(base: impl AsRef<Path>) -> Self {
        Self::new(base.as_ref().join("pids"))
    }

    /// The `pids/` directory this registry manages.
    ///
    /// Why: callers (and tests) occasionally need to inspect the directory path.
    /// What: returns the stored directory.
    /// Test: `under_nests_pids_subdir`.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Path of the `.pid` file for a given session-id.
    ///
    /// Why: register / unregister / classify all need the same file name; one
    /// helper keeps the `<session-id>.pid` convention authoritative.
    /// What: `<dir>/<session_id>.pid`.
    /// Test: exercised by `register_then_entries_round_trip`.
    fn pid_path(&self, session_id: &str) -> PathBuf {
        self.dir.join(format!("{session_id}.{PID_FILE_EXT}"))
    }

    /// Record `pid` for `session_id`, writing the PID file atomically.
    ///
    /// Why: §10.3 requires the PID file to exist BEFORE `spawn()` returns, so a
    /// crash immediately after spawn never leaves an orphan with no PID file.
    /// Writing it here (rather than letting the child write its own) keeps the
    /// daemon the single authority. The write is atomic (temp file + rename) so a
    /// concurrent sweep never reads a half-written PID.
    /// What: creates the `pids/` directory if needed, writes the decimal PID to a
    /// sibling `.<session_id>.pid.tmp` file, then renames it over the final path.
    /// Returns an `io::Error` if the directory or file cannot be written.
    /// Test: `register_then_entries_round_trip`, `register_is_idempotent`.
    pub fn register(&self, session_id: &str, pid: u32) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let final_path = self.pid_path(session_id);
        // Atomic write: stage in a temp sibling, then rename over the target.
        let tmp_path = self
            .dir
            .join(format!(".{session_id}.{PID_FILE_EXT}.tmp"));
        std::fs::write(&tmp_path, pid.to_string())?;
        std::fs::rename(&tmp_path, &final_path)?;
        debug!(session_id, pid, "pid-registry: registered");
        Ok(())
    }

    /// Remove the PID file for `session_id` on normal termination.
    ///
    /// Why: the Drop / decommission path must clear the PID file so a future
    /// sweep does not treat a cleanly-stopped session as an orphan. A missing
    /// file is not an error — the session may never have had a PID recorded.
    /// What: deletes `<session_id>.pid`, treating `NotFound` as success.
    /// Test: `unregister_removes_file`, `unregister_missing_is_ok`.
    pub fn unregister(&self, session_id: &str) -> std::io::Result<()> {
        let path = self.pid_path(session_id);
        match std::fs::remove_file(&path) {
            Ok(()) => {
                debug!(session_id, "pid-registry: unregistered");
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Enumerate every `(session_id, pid)` pair currently on disk.
    ///
    /// Why: the sweep needs the full set of recorded PIDs to reconcile against
    /// the live session registry. A missing directory means nothing was ever
    /// registered → an empty list (not an error).
    /// What: reads `<dir>/*.pid`, parsing each file's stem as the session-id and
    /// its body as a `u32` PID. Files that do not end in `.pid`, lack a stem, or
    /// hold an unparsable PID are skipped (logged at `warn!`) — a corrupt entry
    /// must never abort the sweep.
    /// Test: `register_then_entries_round_trip`, `entries_skips_unparsable`.
    pub fn entries(&self) -> std::io::Result<Vec<PidEntry>> {
        let read_dir = match std::fs::read_dir(&self.dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut out = Vec::new();
        for dent in read_dir.flatten() {
            let path = dent.path();
            if path.extension().and_then(|e| e.to_str()) != Some(PID_FILE_EXT) {
                continue;
            }
            let Some(session_id) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let body = match std::fs::read_to_string(&path) {
                Ok(b) => b,
                Err(e) => {
                    warn!(path = %path.display(), "pid-registry: read failed: {e}");
                    continue;
                }
            };
            match body.trim().parse::<u32>() {
                Ok(pid) => out.push(PidEntry {
                    session_id: session_id.to_string(),
                    pid,
                }),
                Err(_) => {
                    warn!(path = %path.display(), "pid-registry: unparsable PID; skipping");
                }
            }
        }
        Ok(out)
    }

    /// Reap orphaned `claude` processes recorded in the registry.
    ///
    /// Why: the §10.3 PID-file orphan-GC pass. A restarted (or long-running)
    /// daemon reconciles every recorded PID against the set of still-live
    /// session-ids; any recorded PID whose session is gone is a candidate
    /// orphan. The name-verification gate (§13.5) means we SIGTERM only a PID
    /// that is still alive AND still a `claude` process — a recycled PID that now
    /// belongs to an unrelated process is spared (its stale file is removed
    /// silently). Every gate fails toward NOT signalling.
    /// What: lists [`entries`](Self::entries), classifies each via
    /// [`classify_pidfile`] against `live_session_ids` and `probe`, then for each
    /// non-`Keep` decision performs the action (terminate → SIGTERM then remove;
    /// remove-stale → remove only) and removes the PID file. A removal failure is
    /// logged and does not abort the pass. Returns a [`SweepOutcome`] summary.
    /// Test: `sweep_terminates_only_untracked_claude`,
    /// `sweep_removes_dead_without_signal`, `sweep_spares_reused_pid`,
    /// `sweep_keeps_tracked`.
    pub fn sweep_orphans(
        &self,
        live_session_ids: &HashSet<String>,
        probe: &dyn ProcessProbe,
    ) -> SweepOutcome {
        let entries = match self.entries() {
            Ok(e) => e,
            Err(e) => {
                warn!("pid-registry: entries() failed; skipping sweep: {e}");
                return SweepOutcome::default();
            }
        };
        let mut outcome = SweepOutcome {
            scanned: entries.len(),
            ..SweepOutcome::default()
        };
        for entry in &entries {
            match classify_pidfile(entry, live_session_ids, probe) {
                PidDecision::Keep => {}
                PidDecision::Terminate => {
                    let delivered = probe.terminate(entry.pid);
                    info!(
                        session_id = %entry.session_id,
                        pid = entry.pid,
                        delivered,
                        "pid-registry: SIGTERM orphaned claude process"
                    );
                    outcome.terminated += 1;
                    self.remove_pidfile_logged(&entry.session_id);
                }
                PidDecision::RemoveStale(reason) => {
                    debug!(
                        session_id = %entry.session_id,
                        pid = entry.pid,
                        ?reason,
                        "pid-registry: removing stale PID file (no signal)"
                    );
                    outcome.removed_stale += 1;
                    self.remove_pidfile_logged(&entry.session_id);
                }
            }
        }
        debug!(
            scanned = outcome.scanned,
            terminated = outcome.terminated,
            removed_stale = outcome.removed_stale,
            "pid-registry: orphan sweep complete"
        );
        outcome
    }

    /// Remove a PID file during a sweep, logging (not propagating) any error.
    ///
    /// Why: a sweep must process every entry; one removal failing (e.g. a
    /// concurrent unregister already deleted it) must not abort the rest.
    /// What: delegates to [`unregister`](Self::unregister), logging a warning on
    /// error.
    /// Test: exercised by every `sweep_*` test (the files are gone afterwards).
    fn remove_pidfile_logged(&self, session_id: &str) {
        if let Err(e) = self.unregister(session_id) {
            warn!(session_id, "pid-registry: failed to remove PID file: {e}");
        }
    }
}

/// Production [`ProcessProbe`] backed by `crate::core::process` and `nix`.
///
/// Why: the registry's safety gates are only real if they inspect the live OS;
/// this impl wires [`is_alive`](ProcessProbe::is_alive) to the existing
/// `is_process_alive` (a `kill(pid, 0)` existence check), the name check to the
/// same `claude`-substring `comm` lookup the rest of the crate uses, and
/// [`terminate`](ProcessProbe::terminate) to a real SIGTERM.
/// What: a unit struct whose methods delegate to OS calls. Construct with
/// [`OsProcessProbe::default`].
/// Test: only the `terminate` seam touches the OS destructively, so it is not
/// unit-tested against a live process; the decision logic that drives it is
/// tested via [`classify_pidfile`] with a fake probe.
#[derive(Debug, Default, Clone, Copy)]
pub struct OsProcessProbe;

impl ProcessProbe for OsProcessProbe {
    fn is_alive(&self, pid: u32) -> bool {
        crate::core::process::is_process_alive(pid)
    }

    fn is_claude(&self, pid: u32) -> bool {
        crate::core::process::process_name_is_claude(pid)
    }

    fn terminate(&self, pid: u32) -> bool {
        #[cfg(unix)]
        {
            use nix::sys::signal::{Signal, kill};
            use nix::unistd::Pid;
            let Ok(raw) = i32::try_from(pid) else {
                return false;
            };
            if raw <= 0 {
                return false;
            }
            kill(Pid::from_raw(raw), Signal::SIGTERM).is_ok()
        }
        #[cfg(not(unix))]
        {
            let _ = pid;
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fully-scripted probe: liveness and `claude`-name answers are looked up
    /// per-PID from sets; `terminate` records every PID it was asked to signal.
    struct FakeProbe {
        alive: HashSet<u32>,
        claude: HashSet<u32>,
        terminated: std::cell::RefCell<Vec<u32>>,
    }

    impl FakeProbe {
        fn new(alive: &[u32], claude: &[u32]) -> Self {
            Self {
                alive: alive.iter().copied().collect(),
                claude: claude.iter().copied().collect(),
                terminated: std::cell::RefCell::new(Vec::new()),
            }
        }
    }

    impl ProcessProbe for FakeProbe {
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

    fn entry(id: &str, pid: u32) -> PidEntry {
        PidEntry {
            session_id: id.to_string(),
            pid,
        }
    }

    #[test]
    fn under_nests_pids_subdir() {
        let reg = PidRegistry::under("/base/.trusty-mpm");
        assert_eq!(reg.dir(), Path::new("/base/.trusty-mpm/pids"));
    }

    #[test]
    fn register_then_entries_round_trip() {
        let tmp = tempfile::TempDir::new().unwrap();
        let reg = PidRegistry::new(tmp.path());
        reg.register("sess-a", 1234).unwrap();
        reg.register("sess-b", 5678).unwrap();

        let mut got = reg.entries().unwrap();
        got.sort_by(|a, b| a.session_id.cmp(&b.session_id));
        assert_eq!(got, vec![entry("sess-a", 1234), entry("sess-b", 5678)]);
    }

    #[test]
    fn register_is_idempotent() {
        // Re-registering the same session overwrites the PID (atomic rename), so
        // exactly one entry remains with the latest value.
        let tmp = tempfile::TempDir::new().unwrap();
        let reg = PidRegistry::new(tmp.path());
        reg.register("sess", 11).unwrap();
        reg.register("sess", 22).unwrap();
        assert_eq!(reg.entries().unwrap(), vec![entry("sess", 22)]);
    }

    #[test]
    fn unregister_removes_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let reg = PidRegistry::new(tmp.path());
        reg.register("gone", 99).unwrap();
        reg.unregister("gone").unwrap();
        assert!(reg.entries().unwrap().is_empty());
    }

    #[test]
    fn unregister_missing_is_ok() {
        // Removing a PID file that was never written is a no-op success — a
        // cleanly-stopped session that never had a PID recorded must not error.
        let tmp = tempfile::TempDir::new().unwrap();
        let reg = PidRegistry::new(tmp.path());
        reg.unregister("never").unwrap();
    }

    #[test]
    fn entries_on_missing_dir_is_empty() {
        // No `pids/` directory yet (nothing ever registered) → empty, not error.
        let reg = PidRegistry::new("/nonexistent/path/that/does/not/exist/pids");
        assert!(reg.entries().unwrap().is_empty());
    }

    #[test]
    fn entries_skips_unparsable() {
        // A corrupt PID body and a non-`.pid` file must be skipped, never abort
        // the enumeration of the valid entries.
        let tmp = tempfile::TempDir::new().unwrap();
        let reg = PidRegistry::new(tmp.path());
        reg.register("good", 7).unwrap();
        std::fs::write(tmp.path().join("bad.pid"), "not-a-number").unwrap();
        std::fs::write(tmp.path().join("ignore.txt"), "100").unwrap();
        assert_eq!(reg.entries().unwrap(), vec![entry("good", 7)]);
    }

    #[test]
    fn classify_keeps_tracked() {
        // A session still in the live registry is never touched, even if the
        // probe would say it is a live claude.
        let probe = FakeProbe::new(&[10], &[10]);
        let d = classify_pidfile(&entry("alive-sess", 10), &live(&["alive-sess"]), &probe);
        assert_eq!(d, PidDecision::Keep);
    }

    #[test]
    fn classify_removes_dead() {
        // Untracked + dead PID → remove the stale file, no signal.
        let probe = FakeProbe::new(&[], &[]); // not alive
        let d = classify_pidfile(&entry("dead", 20), &live(&[]), &probe);
        assert_eq!(d, PidDecision::RemoveStale(StaleReason::ProcessDead));
    }

    #[test]
    fn classify_removes_reused_pid() {
        // Untracked + alive but NOT named claude → PID was reused; spare it.
        let probe = FakeProbe::new(&[30], &[]); // alive but not claude
        let d = classify_pidfile(&entry("reused", 30), &live(&[]), &probe);
        assert_eq!(d, PidDecision::RemoveStale(StaleReason::PidReused));
    }

    #[test]
    fn classify_terminates_untracked_claude() {
        // The ONLY SIGTERM case: untracked + alive + still claude.
        let probe = FakeProbe::new(&[40], &[40]);
        let d = classify_pidfile(&entry("orphan", 40), &live(&[]), &probe);
        assert_eq!(d, PidDecision::Terminate);
    }

    #[test]
    fn sweep_terminates_only_untracked_claude() {
        // Full matrix in one sweep: only the untracked-alive-claude PID is
        // SIGTERMed; its file (and the stale ones) are removed; the tracked
        // session's file survives.
        let tmp = tempfile::TempDir::new().unwrap();
        let reg = PidRegistry::new(tmp.path());
        reg.register("tracked", 100).unwrap(); // live + claude, but tracked → KEEP
        reg.register("orphan", 200).unwrap(); // untracked + live + claude → KILL
        reg.register("dead", 300).unwrap(); // untracked + dead → remove stale
        reg.register("reused", 400).unwrap(); // untracked + live, not claude → remove stale

        let probe = FakeProbe::new(&[100, 200, 400], &[100, 200]);
        let outcome = reg.sweep_orphans(&live(&["tracked"]), &probe);

        assert_eq!(outcome.scanned, 4);
        assert_eq!(outcome.terminated, 1);
        assert_eq!(outcome.removed_stale, 2);
        assert_eq!(*probe.terminated.borrow(), vec![200]);

        // Only the tracked session's PID file remains.
        assert_eq!(reg.entries().unwrap(), vec![entry("tracked", 100)]);
    }

    #[test]
    fn sweep_removes_dead_without_signal() {
        let tmp = tempfile::TempDir::new().unwrap();
        let reg = PidRegistry::new(tmp.path());
        reg.register("dead", 7).unwrap();
        let probe = FakeProbe::new(&[], &[]);
        let outcome = reg.sweep_orphans(&live(&[]), &probe);
        assert_eq!(outcome.removed_stale, 1);
        assert_eq!(outcome.terminated, 0);
        assert!(probe.terminated.borrow().is_empty());
        assert!(reg.entries().unwrap().is_empty());
    }

    #[test]
    fn sweep_spares_reused_pid() {
        // A recycled PID now owned by a non-claude process must NEVER be SIGTERMed.
        let tmp = tempfile::TempDir::new().unwrap();
        let reg = PidRegistry::new(tmp.path());
        reg.register("reused", 8).unwrap();
        let probe = FakeProbe::new(&[8], &[]); // alive, not claude
        let outcome = reg.sweep_orphans(&live(&[]), &probe);
        assert_eq!(outcome.terminated, 0);
        assert_eq!(outcome.removed_stale, 1);
        assert!(
            probe.terminated.borrow().is_empty(),
            "a reused PID must never be signalled"
        );
    }

    #[test]
    fn sweep_keeps_tracked() {
        let tmp = tempfile::TempDir::new().unwrap();
        let reg = PidRegistry::new(tmp.path());
        reg.register("tracked", 9).unwrap();
        let probe = FakeProbe::new(&[9], &[9]);
        let outcome = reg.sweep_orphans(&live(&["tracked"]), &probe);
        assert_eq!(outcome, SweepOutcome { scanned: 1, terminated: 0, removed_stale: 0 });
        assert_eq!(reg.entries().unwrap(), vec![entry("tracked", 9)]);
    }
}
