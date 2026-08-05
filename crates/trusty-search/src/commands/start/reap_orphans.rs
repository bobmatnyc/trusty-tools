//! Ownership-aware orphan reaping for `trusty-search start` (#4395).
//!
//! Why: the reaper this replaces SIGTERMed **every process on the machine named
//! `trusty-search` with `start` in its argv**, SIGKILLed the survivors 3 seconds
//! later, and deleted their lock and port files. Process name is not an identity
//! — it says nothing about whose data a daemon is serving. Any asymmetry between
//! the starter's lock path and a running daemon's (a second instance under
//! `--data-dir` / `TRUSTY_DATA_DIR`, a daemon started with the override and
//! restarted without it, a deleted lockfile) turned a routine `start` into the
//! destruction of a healthy production daemon — with one tenth of that daemon's
//! own flush budget to shut down. The lockfile fast-path that would have caught
//! it (`service::running_daemon_pid`) is data-dir-local while the reaper was
//! machine-global, so the guard and the hazard never met.
//!
//! Which identity signal, and why this one. A pid file only names the daemon we
//! already track, which is by definition not the orphan we are hunting. A
//! launchd label does not exist for the CLI-detached daemons this reaper
//! actually finds (`PPID 1`, no plist). A port-ownership probe (the #4470 shape,
//! `trusty-installer::commands::port_guard`) answers "is someone on my port?",
//! which is the right question for a bootstrap and the wrong one here — the
//! daemon walks its port on collision, so a foreign holder is a condition it
//! survives, and a *different* port proves nothing about whose data dir a
//! process has open. A `/health` handshake would identify a daemon that is
//! *answering*, which excludes exactly the wedged instance the reaper exists to
//! clear.
//!
//! The signal that matches the actual hazard is the **data directory**: a
//! trusty-search daemon fights us for precisely one thing, the lock and index
//! data under its data dir, and it declares that directory in its own argv
//! (`--data-dir`, always passed explicitly by the self-spawn — issue #1182) or
//! its own environment (`TRUSTY_DATA_DIR`), falling back to the platform default
//! when it declares neither. Two daemons on disjoint data dirs are not in
//! conflict and never were.
//!
//! FAIL CLOSED, structurally. [`ConfirmedOrphan`] wraps a pid behind a private
//! field with no constructor from a bare `u32`, and [`reap`] accepts nothing
//! else. There is no way to write "signal this pid" for a pid that did not come
//! out of [`plan`] having been positively matched to our own data dir — a
//! process whose argv or environment could not be read is
//! [`DaemonIdentity::Unidentified`] and is spared, not reaped. "I could not tell"
//! is never "kill it".
//!
//! Scope: this is the IMPLICIT reaper that runs inside `start`. `trusty-search
//! stop` deliberately still terminates every daemon it can find (issue #81) —
//! that is an explicit operator command whose documented contract is "stop
//! everything", and narrowing it is a separate decision.
//!
//! Test: `reap_orphans_tests.rs`.

use std::path::{Path, PathBuf};

/// What we could establish about one candidate `trusty-search` process (#4395).
///
/// Why: three genuinely different answers, and the third is the one a `bool`
/// silently folds into "kill it" — which is how a name match became a death
/// sentence. Making "could not tell" its own variant is what forces the caller
/// to spare it.
/// What: `OwnInstance` — the candidate resolves to the same data directory we
/// do, so it is contending for our lock and our index files. `ForeignInstance`
/// — it resolves to a different one, carrying that directory for the log.
/// `Unidentified` — its argv or environment could not be read, carrying why.
/// Test: the `identify_*` tests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DaemonIdentity {
    /// Shares our data directory: our orphan, safe to reap.
    OwnInstance,
    /// Serves a different data directory: never ours to kill.
    ForeignInstance(PathBuf),
    /// Could not be established; the payload says why.
    Unidentified(String),
}

/// A pid the reaper has POSITIVELY identified as its own orphan (#4395).
///
/// Why: the invariant "we only ever signal a process we identified" is enforced
/// by the type system rather than by remembering to check. The field is private
/// and this module exposes no way to build one from a `u32`, so [`reap`] cannot
/// be handed an unvetted pid even by a future call site that has forgotten the
/// rule. The pre-#4395 reaper's whole defect was a `Vec<u32>` that carried no
/// evidence at all.
/// What: a newtype over the pid, minted only inside [`plan`].
/// Test: `plan_confirms_only_our_own_data_dir`; the unforgeability itself is a
/// compile-time property, not a runtime assertion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfirmedOrphan(u32);

impl ConfirmedOrphan {
    /// The identified pid.
    pub fn pid(&self) -> u32 {
        self.0
    }
}

/// One live `trusty-search` process as observed from the process table.
///
/// Why: separating the observation from the decision is what makes the decision
/// testable without spawning daemons — the same seam
/// `trusty-installer::commands::port_guard` uses for its `lsof` probe.
/// What: the pid plus the two things identity is read from. Both are `Vec<String>`
/// in the process table's own form: argv words, and `KEY=VALUE` environment
/// entries.
#[derive(Clone, Debug)]
pub struct Candidate {
    pub pid: u32,
    pub argv: Vec<String>,
    pub environ: Vec<String>,
}

/// The reaper's decision over a whole candidate set (#4395).
///
/// Why: the spared processes are as load-bearing as the doomed ones — an
/// operator who runs `start` against a machine with a production daemon needs to
/// see that it was recognised and left alone, not silent success. Carrying both
/// lists is what lets the caller say so.
/// What: `orphans` are the only pids [`reap`] will signal; `spared` pairs each
/// untouched pid with the reason.
pub struct ReapPlan {
    pub orphans: Vec<ConfirmedOrphan>,
    pub spared: Vec<(u32, String)>,
}

/// Which data directory a candidate declares, or why we cannot tell (#4395).
///
/// Why: the fallback to "platform default" is only sound when we have positively
/// read an environment that does NOT set `TRUSTY_DATA_DIR`. An UNREADABLE
/// environment looks identical to an empty one, and treating the two the same
/// would classify every process we cannot inspect as sharing our default data
/// dir — a name match by another route, and the same fatal outcome.
/// What: `Ok(Some(dir))` for an explicit declaration, `Ok(None)` for "positively
/// declares nothing, so it uses the platform default", `Err(why)` when neither
/// argv nor environ could be read.
/// Test: `declared_data_dir_*`.
fn declared_data_dir(argv: &[String], environ: &[String]) -> Result<Option<PathBuf>, String> {
    if argv.is_empty() {
        return Err("process argv is unreadable".to_string());
    }
    // `--data-dir` wins over the environment, matching issue #1182: the
    // self-spawn passes the flag explicitly precisely so it beats a stale
    // inherited `TRUSTY_DATA_DIR`.
    let mut words = argv.iter();
    while let Some(word) = words.next() {
        if let Some(value) = word.strip_prefix("--data-dir=") {
            return Ok(Some(PathBuf::from(value)));
        }
        if word == "--data-dir" {
            return match words.next() {
                Some(value) => Ok(Some(PathBuf::from(value))),
                None => Err("`--data-dir` present in argv with no value".to_string()),
            };
        }
    }
    for entry in environ {
        if let Some(value) = entry.strip_prefix("TRUSTY_DATA_DIR=") {
            return Ok(Some(PathBuf::from(value)));
        }
    }
    if environ.is_empty() {
        // Cannot distinguish "does not set the var" from "we may not read this
        // process's environment". Both look like an empty slice, so neither may
        // be treated as the default data dir.
        return Err("process environment is unreadable".to_string());
    }
    Ok(None)
}

/// Normalise a data-dir path for comparison.
///
/// Why: `/tmp/ts` and `/tmp/ts/` and a symlinked `/var/…` vs `/private/var/…`
/// (macOS) are the same directory, and a spurious mismatch here is the SAFE
/// direction (spare a process that was ours) while a spurious match is the fatal
/// one (kill a stranger). Canonicalising when the path exists gets both right.
/// What: `canonicalize` when it succeeds, otherwise the lexically re-joined
/// components, which drops trailing separators and `.` segments.
/// Test: exercised through `identify_treats_a_trailing_slash_as_the_same_dir`.
fn normalise(dir: &Path) -> PathBuf {
    std::fs::canonicalize(dir).unwrap_or_else(|_| dir.components().collect())
}

/// Decide whether one candidate is our orphan (#4395).
///
/// Why: the whole policy, pure, so the truth table is testable without a live
/// process table or a real daemon. `platform_default` is a parameter rather than
/// a call to `dirs::data_local_dir()` for the same reason.
/// What: resolves the candidate's data dir per [`declared_data_dir`] (falling
/// back to `platform_default` when it positively declares none), and compares it
/// with `our_data_dir` under [`normalise`]. An unreadable observation is
/// [`DaemonIdentity::Unidentified`] and never a match.
/// Test: `identify_claims_a_daemon_sharing_our_data_dir`,
/// `identify_spares_a_daemon_with_a_different_data_dir`,
/// `identify_spares_a_daemon_whose_environment_is_unreadable`,
/// `identify_prefers_the_flag_over_the_environment`,
/// `identify_reads_the_equals_form_of_the_flag`,
/// `identify_falls_back_to_the_platform_default`,
/// `identify_treats_a_trailing_slash_as_the_same_dir`.
pub fn identify(
    argv: &[String],
    environ: &[String],
    our_data_dir: &Path,
    platform_default: &Path,
) -> DaemonIdentity {
    let theirs = match declared_data_dir(argv, environ) {
        Ok(Some(dir)) => dir,
        Ok(None) => platform_default.to_path_buf(),
        Err(why) => return DaemonIdentity::Unidentified(why),
    };
    if normalise(&theirs) == normalise(our_data_dir) {
        DaemonIdentity::OwnInstance
    } else {
        DaemonIdentity::ForeignInstance(theirs)
    }
}

/// Turn observed candidates into a reap plan (#4395).
///
/// Why: the only place a [`ConfirmedOrphan`] comes into existence, so every pid
/// the reaper can signal has passed [`identify`] on its way here. A candidate
/// list is data; a `ConfirmedOrphan` list is a conclusion.
/// What: applies [`identify`] to each candidate; `OwnInstance` becomes a
/// confirmed orphan, everything else is spared with its reason recorded.
/// Test: `plan_confirms_only_our_own_data_dir`,
/// `plan_spares_an_unidentifiable_candidate`.
pub fn plan(candidates: &[Candidate], our_data_dir: &Path, platform_default: &Path) -> ReapPlan {
    let mut orphans = Vec::new();
    let mut spared = Vec::new();
    for candidate in candidates {
        match identify(
            &candidate.argv,
            &candidate.environ,
            our_data_dir,
            platform_default,
        ) {
            DaemonIdentity::OwnInstance => orphans.push(ConfirmedOrphan(candidate.pid)),
            DaemonIdentity::ForeignInstance(dir) => spared.push((
                candidate.pid,
                format!("serves a different data dir ({})", dir.display()),
            )),
            DaemonIdentity::Unidentified(why) => spared.push((
                candidate.pid,
                format!("could not be identified as ours ({why})"),
            )),
        }
    }
    ReapPlan { orphans, spared }
}

/// Snapshot every live `trusty-search` daemon process with its argv and environ.
///
/// Why: `commands::stop::find_daemon_pids` finds the candidates but returns bare
/// pids, which is exactly the evidence-free list #4395 is about. This gathers the
/// two fields identity is read from at the same time, from the same snapshot, so
/// a process cannot change identity between being listed and being judged.
/// What: one `sysinfo` refresh with `cmd` and `environ` requested, filtered the
/// same way `find_daemon_pids` filters — executable basename `trusty-search`,
/// `start` in argv, never our own pid.
/// Test: side-effecting (reads the live process table); the decision it feeds is
/// covered by the `plan_*` and `identify_*` tests.
fn observe_candidates() -> Vec<Candidate> {
    use sysinfo::{ProcessRefreshKind, RefreshKind, System, UpdateKind};
    let mut sys = System::new_with_specifics(
        RefreshKind::nothing().with_processes(
            ProcessRefreshKind::nothing()
                .with_cmd(UpdateKind::Always)
                .with_environ(UpdateKind::Always),
        ),
    );
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    let me = std::process::id();
    let mut out = Vec::new();
    for (pid, proc_) in sys.processes() {
        let raw = pid.as_u32();
        if raw == me || proc_.name().to_string_lossy() != "trusty-search" {
            continue;
        }
        let argv: Vec<String> = proc_
            .cmd()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        if !argv.iter().any(|a| a == "start") {
            continue;
        }
        let environ: Vec<String> = proc_
            .environ()
            .iter()
            .map(|e| e.to_string_lossy().into_owned())
            .collect();
        out.push(Candidate {
            pid: raw,
            argv,
            environ,
        });
    }
    out
}

/// SIGTERM the confirmed orphans, then SIGKILL whichever survive the window.
///
/// Why: the `&[ConfirmedOrphan]` parameter is the fix. There is no overload
/// taking pids, so a future edit cannot reintroduce "signal everything that
/// looked like us" without first inventing a way to mint the token.
///
/// The window is [`trusty_common::shutdown::termination_grace`], the same one
/// launchd's `ExitTimeOut` and `trusty-search stop` now use (#4393). The old 3 s
/// was a tenth of the flushing daemon's own 30 s per-index floor, so even a
/// correctly-targeted orphan was SIGKILLed mid-write.
///
/// What: SIGTERM all, poll every 100 ms until all are gone or the window
/// closes, then SIGKILL the remainder. Returns the pids that had to be killed.
/// Test: side-effecting; the window it uses is pinned by
/// `reap_window_covers_the_flush_floor`.
#[cfg(unix)]
fn reap(orphans: &[ConfirmedOrphan]) -> Vec<u32> {
    use nix::sys::signal::{kill, Signal};
    use nix::unistd::Pid;

    let alive = |pid: u32| kill(Pid::from_raw(pid as i32), None).is_ok();

    for orphan in orphans {
        let _ = kill(Pid::from_raw(orphan.pid() as i32), Signal::SIGTERM);
    }
    let deadline = std::time::Instant::now() + trusty_common::shutdown::termination_grace();
    loop {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if !orphans.iter().any(|o| alive(o.pid())) || std::time::Instant::now() >= deadline {
            break;
        }
    }
    let mut killed = Vec::new();
    for orphan in orphans {
        if alive(orphan.pid()) {
            tracing::warn!(
                "orphan pid {} ignored SIGTERM for {}s — sending SIGKILL",
                orphan.pid(),
                trusty_common::shutdown::termination_grace().as_secs(),
            );
            let _ = kill(Pid::from_raw(orphan.pid() as i32), Signal::SIGKILL);
            killed.push(orphan.pid());
        }
    }
    killed
}

#[cfg(not(unix))]
fn reap(_orphans: &[ConfirmedOrphan]) -> Vec<u32> {
    Vec::new()
}

/// Reap this instance's orphaned daemons before starting (#81, rescoped by #4395).
///
/// Why: `start` still has to clear daemons that hold our data dir but are not in
/// our lockfile, or two of them end up fighting over `bind_with_auto_port` and
/// the older one keeps consuming memory forever. What it must NOT do is decide
/// that by name — see the module doc.
///
/// What: observes the live candidates, plans against our own resolved data dir,
/// reports every spared process and why, reaps the confirmed orphans, and clears
/// the lock and port files only when something was actually reaped. Called from
/// `handle_start` after the lockfile fast-path has already ruled out a tracked
/// live daemon.
///
/// Test: side-effecting; the policy is `plan_*` / `identify_*`.
pub fn reap_orphans_before_start() {
    let Some(our_data_dir) =
        crate::service::daemon::resolve_daemon_dir(std::env::var_os("TRUSTY_DATA_DIR").as_deref())
    else {
        tracing::warn!(
            "orphan reaper: own data dir is unresolvable — skipping the sweep entirely \
             (#4395: an unidentifiable owner cannot identify anyone else's)"
        );
        return;
    };
    let Some(platform_default) = crate::service::daemon::resolve_daemon_dir(None) else {
        tracing::warn!(
            "orphan reaper: platform default data dir is unresolvable — skipping the \
             sweep entirely (#4395)"
        );
        return;
    };

    let candidates = observe_candidates();
    if candidates.is_empty() {
        return;
    }
    let plan = plan(&candidates, &our_data_dir, &platform_default);

    for (pid, reason) in &plan.spared {
        tracing::info!("orphan reaper: leaving pid {pid} alone — {reason} (#4395)");
    }
    if !plan.spared.is_empty() {
        eprintln!(
            "{} {} other trusty-search daemon(s) are running but serve different data — \
             leaving them alone",
            "·".dimmed(),
            plan.spared.len(),
        );
    }
    if plan.orphans.is_empty() {
        return;
    }

    let pids: Vec<u32> = plan.orphans.iter().map(ConfirmedOrphan::pid).collect();
    tracing::warn!(
        "found {} trusty-search daemon(s) on our own data dir ({}) not tracked by the \
         lockfile: {pids:?} — terminating before start",
        pids.len(),
        our_data_dir.display(),
    );
    eprintln!(
        "{} found {} orphaned trusty-search daemon(s) on this data dir — stopping them first",
        "⚠".yellow(),
        pids.len(),
    );

    reap(&plan.orphans);

    // Clear the lock/port files the reaped orphans left behind. Only reachable
    // when we actually reaped something on OUR data dir, so this can no longer
    // delete a foreign daemon's files.
    if let Ok(lock) = crate::service::daemon_lock_path() {
        let _ = std::fs::remove_file(&lock);
    }
    if let Some(port) = crate::commands::daemon_utils::daemon_port_path() {
        let _ = std::fs::remove_file(&port);
    }
}

use colored::Colorize;

#[cfg(test)]
#[path = "reap_orphans_tests.rs"]
mod tests;
