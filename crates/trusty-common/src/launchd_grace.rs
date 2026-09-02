//! Pre-bootout verification of the ACTIVE launchd unit's grace window (#6590).
//!
//! Why: [`crate::launchd::LaunchdConfig::render_plist`] has emitted a 60 s
//! `ExitTimeOut` since #4393, and #4919 made activation label-correct — yet the
//! 2026-09-02 reinstall still lost a 55 s snapshot flush to a 5 s SIGKILL. The
//! two fixes cannot reach the shutdown that matters, because launchd reads a
//! unit's `ExitTimeOut` from the job it has LOADED IN MEMORY, and it re-reads
//! the plist file only at `bootstrap`. `install_and_activate` writes the
//! corrected plist and then calls `bootstrap`, which boots the old job out
//! FIRST — so the termination that races the flush is always governed by the
//! window the PREVIOUS unit declared, never by the one just written. On a host
//! whose installed plist predates #4393 that window is launchd's
//! [`LAUNCHD_DEFAULT_EXIT_TIMEOUT_SECS`](crate::launchd_grace::LAUNCHD_DEFAULT_EXIT_TIMEOUT_SECS)
//! default, and the daemon is killed
//! mid-write while `KeepAlive` respawns the old binary as an orphan holding the
//! port.
//!
//! What: pure readers for the grace a unit will actually be granted
//! ([`parse_launchctl_exit_timeout`](crate::launchd_grace::parse_launchctl_exit_timeout),
//! [`parse_plist_exit_timeout`](crate::launchd_grace::parse_plist_exit_timeout)),
//! a verdict over it ([`grace_verdict`](crate::launchd_grace::grace_verdict)),
//! and a bounded quiesce ([`quiesce_job`](crate::launchd_grace::quiesce_job)) that
//! stops the live process with SIGTERM and waits for it to exit BEFORE the
//! bootout, so launchd's short window has nothing left to cut off. The
//! `launchctl` and signal effects are injected, so every decision is
//! unit-tested without touching a real unit.
//!
//! Test: `cargo test -p trusty-common --features unconditional-only
//! launchd_grace`.
#![cfg(target_os = "macos")]

use std::time::Duration;

use crate::launchd::{LaunchdConfig, current_uid};

/// Grace launchd grants a unit that declares no `ExitTimeOut` of its own.
///
/// Why: this is the number the whole issue turns on. launchd documents the
/// value only as "system-defined"; 5 s is what #4393 measured on macOS and what
/// #6590 observed cutting a 55 s flush short. A unit whose plist omits the key
/// is therefore not "unknown" — it is known to be this, which is what lets
/// [`plist_grace_secs`] answer for a legacy plist instead of giving up.
/// What: `5`, in seconds.
/// Test: `plist_grace_falls_back_to_the_system_default`.
pub const LAUNCHD_DEFAULT_EXIT_TIMEOUT_SECS: u64 = 5;

/// Whether the ACTIVE unit's grace window covers the flush the daemon plans.
///
/// Why: the caller needs three outcomes, not two. "Long enough" and "too short"
/// demand opposite handling, and "could not read it" must not be silently
/// folded into either — quiescing a daemon nobody asked us to stop is as wrong
/// as SIGKILLing one mid-flush.
/// What: the two decided states carry the numbers so the log line can name
/// them; [`GraceVerdict::Unknown`] means launchd answered nothing and no plist
/// was readable.
/// Test: `grace_verdict_flags_the_measured_launchd_default`,
/// `grace_verdict_accepts_an_equal_window`,
/// `grace_verdict_is_unknown_when_unreadable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum GraceVerdict {
    /// The active unit grants at least the required window.
    Sufficient {
        /// Seconds launchd will actually wait.
        active_secs: u64,
    },
    /// The active unit grants less than the flush needs — a bootout now would
    /// SIGKILL the daemon mid-write.
    TooShort {
        /// Seconds launchd will actually wait.
        active_secs: u64,
        /// Seconds the daemon plans to spend flushing.
        required_secs: u64,
    },
    /// Neither `launchctl` nor an installed plist could answer.
    Unknown,
}

impl GraceVerdict {
    /// Whether a bootout issued now would cut a flush short.
    ///
    /// [`GraceVerdict::Unknown`] reads as `false`: an unreadable unit is not
    /// evidence of a short window, and acting on it would stop daemons on every
    /// host whose `launchctl print` output cannot be parsed.
    #[must_use]
    pub fn is_too_short(&self) -> bool {
        matches!(self, GraceVerdict::TooShort { .. })
    }
}

/// Compare the grace launchd will grant against the window the daemon needs.
///
/// Why: kept pure and separate from the `launchctl` read so the comparison —
/// the thing that decides whether a live daemon gets stopped — is testable
/// without a loaded unit.
/// What: `None` yields [`GraceVerdict::Unknown`]; otherwise the active value is
/// [`GraceVerdict::Sufficient`] when it is at least `required_secs` and
/// [`GraceVerdict::TooShort`] below that. Equality is sufficient: a unit that
/// grants exactly the planned window is the state the renderer aims for.
/// Test: `grace_verdict_flags_the_measured_launchd_default`,
/// `grace_verdict_accepts_an_equal_window`,
/// `grace_verdict_is_unknown_when_unreadable`.
#[must_use]
pub fn grace_verdict(active_secs: Option<u64>, required_secs: u64) -> GraceVerdict {
    match active_secs {
        None => GraceVerdict::Unknown,
        Some(active_secs) if active_secs >= required_secs => {
            GraceVerdict::Sufficient { active_secs }
        }
        Some(active_secs) => GraceVerdict::TooShort {
            active_secs,
            required_secs,
        },
    }
}

/// Read the `exit timeout` launchd reports for a loaded job.
///
/// Why: the LOADED job is the only authority on the window a bootout will
/// respect — the plist on disk may already have been overwritten with the
/// corrected one, which is exactly the state #6590 fails in.
/// What: scans `launchctl print gui/<uid>/<label>` output for a line whose key
/// is `exit timeout` and returns the integer after the `=`. Returns `None` when
/// no such line is present.
/// Test: `parse_launchctl_exit_timeout_reads_the_loaded_window`,
/// `parse_launchctl_exit_timeout_ignores_unrelated_lines`.
#[must_use]
pub fn parse_launchctl_exit_timeout(printed: &str) -> Option<u64> {
    keyed_value(printed, "exit timeout")?.parse().ok()
}

/// Read the pid launchd reports for a loaded job.
///
/// Why: the quiesce needs the process to signal, and the pid launchd holds is
/// the one it will SIGKILL at the timeout boundary — reading it anywhere else
/// risks signalling a different instance.
/// What: scans `launchctl print` output for a `pid = <n>` line. `None` when the
/// job is loaded but not currently running.
/// Test: `parse_launchctl_pid_reads_a_running_job`,
/// `parse_launchctl_pid_is_none_for_an_idle_job`.
#[must_use]
pub fn parse_launchctl_pid(printed: &str) -> Option<u32> {
    keyed_value(printed, "pid")?.parse().ok()
}

/// Value of the `<key> = <value>` line whose key is exactly `key`.
///
/// Matching the whole trimmed key is what keeps `pid` from also matching
/// launchd's `original pid` and `pid of last exit` lines.
fn keyed_value<'a>(printed: &'a str, key: &str) -> Option<&'a str> {
    printed.lines().find_map(|line| {
        let (found, value) = line.split_once('=')?;
        found.trim().eq_ignore_ascii_case(key).then(|| value.trim())
    })
}

/// Read an `ExitTimeOut` declaration out of plist XML.
///
/// Why: when `launchctl print` cannot answer (the label is not loaded, or
/// launchctl is unavailable) the installed plist still says what launchd would
/// grant on the next load.
/// What: finds `<key>ExitTimeOut</key>` and returns the integer in the
/// `<integer>` element that follows it. `None` when the key is absent — see
/// [`plist_grace_secs`] for why that is not the same as "unknown".
/// Test: `parse_plist_exit_timeout_reads_a_declared_window`,
/// `parse_plist_exit_timeout_is_none_when_undeclared`.
#[must_use]
pub fn parse_plist_exit_timeout(xml: &str) -> Option<u64> {
    let after_key = xml.split_once("<key>ExitTimeOut</key>")?.1;
    let open = after_key.find("<integer>")? + "<integer>".len();
    let close = after_key[open..].find("</integer>")? + open;
    after_key[open..close].trim().parse().ok()
}

/// Grace launchd will grant a unit whose plist reads `xml`.
///
/// Why: a plist with no `ExitTimeOut` is not an unreadable unit — it is a unit
/// that will be granted [`LAUNCHD_DEFAULT_EXIT_TIMEOUT_SECS`]. Reporting it as
/// unknown is what would let the #6590 host, whose installed plist predates
/// #4393 and declares no key at all, slip past the guard.
/// What: the declared value, or the system default when none is declared.
/// Test: `plist_grace_falls_back_to_the_system_default`,
/// `plist_grace_prefers_a_declared_window`.
#[must_use]
pub fn plist_grace_secs(xml: &str) -> u64 {
    parse_plist_exit_timeout(xml).unwrap_or(LAUNCHD_DEFAULT_EXIT_TIMEOUT_SECS)
}

/// What a pre-bootout quiesce achieved.
///
/// Why: the caller must distinguish "the bootout is now safe" from "it is still
/// not", because only the second is worth warning an operator about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Quiesce {
    /// No live process — the short window has nothing to cut off.
    NotRunning,
    /// The daemon exited on its own after SIGTERM, within the window.
    Exited {
        /// Seconds waited before the process was observed gone.
        waited_secs: u64,
    },
    /// The daemon was still running when the window ran out. The bootout that
    /// follows will be governed by launchd's short grace after all.
    StillRunning,
}

/// Stop a live job ourselves, so the bootout that follows finds nothing to kill.
///
/// Why: launchd's `ExitTimeOut` bounds only terminations launchd performs. A
/// SIGTERM delivered directly is bounded by nothing, so the daemon gets its full
/// flush — this is precisely the `kill -TERM` remedy the #6590 operator applied
/// by hand after the bootout had already truncated one. Doing it before the
/// bootout means the subsequent `launchctl bootout` unloads an already-exited
/// job, which is instant and cannot SIGKILL anything.
///
/// What: with a live pid, sends SIGTERM once and then polls liveness one tick
/// per second for at most `required_secs`. Effects are injected: `terminate`
/// reports whether the signal was delivered, `still_alive` probes the process,
/// and `tick` is the wait between probes. A signal that cannot be delivered
/// yields [`Quiesce::StillRunning`] rather than a spurious success.
///
/// Note the deliberate ordering against `KeepAlive`: a clean exit is followed
/// immediately by the caller's bootout, well inside the agent's
/// `ThrottleInterval`, so launchd does not get to respawn in the gap. Should it
/// respawn anyway, the fresh instance has nothing flushed to lose.
///
/// Test: `quiesce_reports_not_running_without_a_pid`,
/// `quiesce_reports_not_running_when_the_pid_is_already_gone`,
/// `quiesce_waits_for_a_clean_exit`, `quiesce_gives_up_at_the_window`,
/// `quiesce_reports_still_running_when_the_signal_fails`.
pub fn quiesce_job(
    pid: Option<u32>,
    required_secs: u64,
    terminate: impl FnOnce(u32) -> bool,
    mut still_alive: impl FnMut(u32) -> bool,
    mut tick: impl FnMut(),
) -> Quiesce {
    let Some(pid) = pid else {
        return Quiesce::NotRunning;
    };
    if !still_alive(pid) {
        return Quiesce::NotRunning;
    }
    if !terminate(pid) {
        return Quiesce::StillRunning;
    }
    for waited_secs in 1..=required_secs {
        tick();
        if !still_alive(pid) {
            return Quiesce::Exited { waited_secs };
        }
    }
    Quiesce::StillRunning
}

impl LaunchdConfig {
    /// Raw `launchctl print gui/<uid>/<label>` output, when the label is loaded.
    ///
    /// Why: both the active grace and the running pid come from this one read,
    /// and taking it once keeps them describing the same instant.
    /// What: `None` when launchctl cannot be spawned or exits non-zero (which is
    /// how it reports a label that is not loaded).
    /// Test: side-effecting; the parsing it feeds is covered by
    /// `parse_launchctl_*`.
    #[must_use]
    pub fn launchctl_print(&self) -> Option<String> {
        let target = format!("gui/{}/{}", current_uid(), self.label);
        let output = std::process::Command::new("launchctl")
            .args(["print", &target])
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Seconds launchd will grant this unit before SIGKILL.
    ///
    /// Why: the loaded job outranks the file, because `install()` may already
    /// have replaced the file with the corrected plist that is not yet loaded —
    /// trusting the file there is what makes the guard report a 60 s window
    /// while launchd is holding 5.
    /// What: the loaded job's `exit timeout` if launchctl answers; otherwise the
    /// installed plist's window per [`plist_grace_secs`]; otherwise `None`.
    /// Test: side-effecting; both readers are unit-tested individually.
    #[must_use]
    pub fn active_exit_timeout_secs(&self) -> Option<u64> {
        if let Some(printed) = self.launchctl_print()
            && let Some(secs) = parse_launchctl_exit_timeout(&printed)
        {
            return Some(secs);
        }
        let path = self.plist_path().ok()?;
        let xml = std::fs::read_to_string(path).ok()?;
        Some(plist_grace_secs(&xml))
    }

    /// Verdict on whether a bootout now would cut `required_secs` of flush short.
    #[must_use]
    pub fn active_grace_verdict(&self, required_secs: u64) -> GraceVerdict {
        grace_verdict(self.active_exit_timeout_secs(), required_secs)
    }

    /// SIGTERM the running job and wait for it, bounded by `required_secs`.
    ///
    /// Why/What: see [`quiesce_job`] — this binds it to the real pid, signal,
    /// and clock.
    /// Test: the decision is covered by `quiesce_*`; the signal itself is
    /// side-effecting and is exercised by `service install`.
    pub fn quiesce_before_bootout(&self, required_secs: u64) -> Quiesce {
        let pid = self
            .launchctl_print()
            .as_deref()
            .and_then(parse_launchctl_pid);
        quiesce_job(
            pid,
            required_secs,
            |pid| signal_process(pid, libc::SIGTERM),
            |pid| signal_process(pid, 0),
            || std::thread::sleep(Duration::from_secs(1)),
        )
    }
}

/// Send `sig` to `pid`, reporting whether it was delivered.
///
/// Signal `0` performs the permission and existence checks without delivering
/// anything, which is the POSIX liveness probe.
fn signal_process(pid: u32, sig: i32) -> bool {
    // SAFETY: `kill` takes two scalars and reports failure through its return
    // value; there are no pointers or lifetimes involved.
    unsafe { libc::kill(pid as libc::pid_t, sig) == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trimmed-down sample of real `launchctl print` output.
    const PRINTED: &str = "\
com.trusty.search = {
\tactive count = 1
\tpath = /Users/test/Library/LaunchAgents/com.trusty.search.plist
\tstate = running
\tpid = 23570
\toriginal pid = 86880
\tprogram = /Users/test/.cargo/bin/trusty-search
\truns = 18
\texit timeout = 5
\tlast exit code = 1
}";

    /// Why (#6590): this is the observed failure — launchd holding the 5 s
    /// system default while the daemon plans a 55 s flush. The guard exists only
    /// to catch this, so it is asserted on the real number.
    /// What: 5 s active against a 60 s requirement is `TooShort`, carrying both
    /// numbers.
    /// Test: itself.
    #[test]
    fn grace_verdict_flags_the_measured_launchd_default() {
        assert_eq!(
            grace_verdict(Some(LAUNCHD_DEFAULT_EXIT_TIMEOUT_SECS), 60),
            GraceVerdict::TooShort {
                active_secs: 5,
                required_secs: 60,
            },
            "launchd's 5 s default cannot cover a 55 s snapshot flush"
        );
        assert!(grace_verdict(Some(5), 60).is_too_short());
    }

    /// Why: the renderer aims for exactly `TERMINATION_GRACE_SECS`, so treating
    /// an equal window as short would make every correctly-installed host
    /// quiesce its daemon on every install.
    /// What: equal and larger windows are both `Sufficient`.
    /// Test: itself.
    #[test]
    fn grace_verdict_accepts_an_equal_window() {
        assert_eq!(
            grace_verdict(Some(60), 60),
            GraceVerdict::Sufficient { active_secs: 60 }
        );
        assert_eq!(
            grace_verdict(Some(120), 60),
            GraceVerdict::Sufficient { active_secs: 120 }
        );
        assert!(!grace_verdict(Some(60), 60).is_too_short());
    }

    /// Why: an unreadable unit must not be treated as a short one — the guard
    /// would then stop a healthy daemon on every host whose launchctl output
    /// cannot be parsed.
    /// What: `None` is `Unknown`, and `Unknown` is not too short.
    /// Test: itself.
    #[test]
    fn grace_verdict_is_unknown_when_unreadable() {
        assert_eq!(grace_verdict(None, 60), GraceVerdict::Unknown);
        assert!(!GraceVerdict::Unknown.is_too_short());
    }

    #[test]
    fn parse_launchctl_exit_timeout_reads_the_loaded_window() {
        assert_eq!(parse_launchctl_exit_timeout(PRINTED), Some(5));
    }

    #[test]
    fn parse_launchctl_exit_timeout_ignores_unrelated_lines() {
        assert_eq!(parse_launchctl_exit_timeout("\tstate = running\n"), None);
        assert_eq!(parse_launchctl_exit_timeout(""), None);
    }

    /// Why: launchd prints `original pid` and `pid of last exit` beside the live
    /// `pid`. A substring match would read one of those and signal a pid that is
    /// either stale or belongs to something else entirely.
    /// What: the live `pid` line wins over the `original pid` line below it in
    /// the sample.
    /// Test: itself.
    #[test]
    fn parse_launchctl_pid_reads_a_running_job() {
        assert_eq!(parse_launchctl_pid(PRINTED), Some(23570));
    }

    #[test]
    fn parse_launchctl_pid_is_none_for_an_idle_job() {
        assert_eq!(parse_launchctl_pid("\tstate = waiting\n\truns = 3\n"), None);
    }

    #[test]
    fn parse_plist_exit_timeout_reads_a_declared_window() {
        let xml = "  <key>ExitTimeOut</key>\n  <integer>60</integer>\n";
        assert_eq!(parse_plist_exit_timeout(xml), Some(60));
    }

    #[test]
    fn parse_plist_exit_timeout_is_none_when_undeclared() {
        let xml = "  <key>ThrottleInterval</key>\n  <integer>30</integer>\n";
        assert_eq!(parse_plist_exit_timeout(xml), None);
    }

    /// Why (#6590): the host that lost the flush had an installed plist with no
    /// `ExitTimeOut` key at all — a legacy unit predating #4393. Reading that as
    /// "unknown" is what would let it past the guard, so it must read as the 5 s
    /// launchd actually applies.
    /// What: an undeclared window resolves to the system default.
    /// Test: itself.
    #[test]
    fn plist_grace_falls_back_to_the_system_default() {
        let legacy = "  <key>KeepAlive</key>\n  <dict/>\n";
        assert_eq!(plist_grace_secs(legacy), LAUNCHD_DEFAULT_EXIT_TIMEOUT_SECS);
        assert!(grace_verdict(Some(plist_grace_secs(legacy)), 60).is_too_short());
    }

    #[test]
    fn plist_grace_prefers_a_declared_window() {
        let current = "  <key>ExitTimeOut</key>\n  <integer>60</integer>\n";
        assert_eq!(plist_grace_secs(current), 60);
    }

    #[test]
    fn quiesce_reports_not_running_without_a_pid() {
        let outcome = quiesce_job(
            None,
            60,
            |_| unreachable!("nothing to signal"),
            |_| unreachable!("nothing to probe"),
            || unreachable!("nothing to wait for"),
        );
        assert_eq!(outcome, Quiesce::NotRunning);
    }

    #[test]
    fn quiesce_reports_not_running_when_the_pid_is_already_gone() {
        let outcome = quiesce_job(
            Some(4242),
            60,
            |_| unreachable!("a dead process must not be signalled"),
            |_| false,
            || unreachable!("nothing to wait for"),
        );
        assert_eq!(outcome, Quiesce::NotRunning);
    }

    /// Why (#6590): the whole point of the quiesce is that a directly-delivered
    /// SIGTERM is bounded by nothing, so the daemon gets the full flush launchd
    /// would have cut off. The operator's manual remedy took 15 s; this asserts
    /// the wait outlives launchd's 5 s window.
    /// What: a process that is still alive on the first 16 probes and gone on
    /// the next is reported as `Exited` after 16 ticks, inside a 60 s budget.
    /// Test: itself.
    #[test]
    fn quiesce_waits_for_a_clean_exit() {
        let mut probes = 0_u64;
        let mut ticks = 0_u64;
        let outcome = quiesce_job(
            Some(4242),
            60,
            |_| true,
            |_| {
                probes += 1;
                // Probe 1 is the liveness pre-check; the process goes away on
                // the 16th post-signal probe, well past launchd's 5 s window.
                probes <= 16
            },
            || ticks += 1,
        );
        assert_eq!(
            outcome,
            Quiesce::Exited { waited_secs: 16 },
            "a direct SIGTERM must be allowed to outlive launchd's short window"
        );
        assert_eq!(ticks, 16, "one probe per second, no busy loop");
        assert!(
            ticks > LAUNCHD_DEFAULT_EXIT_TIMEOUT_SECS,
            "a wait that fits inside launchd's default would not have helped"
        );
    }

    #[test]
    fn quiesce_gives_up_at_the_window() {
        let mut ticks = 0_u64;
        let outcome = quiesce_job(Some(4242), 3, |_| true, |_| true, || ticks += 1);
        assert_eq!(outcome, Quiesce::StillRunning);
        assert_eq!(ticks, 3, "the wait is bounded by the required window");
    }

    /// Why: a SIGTERM that was never delivered leaves the daemon running, and
    /// reporting success would send the caller into a bootout believing the
    /// process had already exited.
    /// What: a failed signal yields `StillRunning` and skips the wait entirely.
    /// Test: itself.
    #[test]
    fn quiesce_reports_still_running_when_the_signal_fails() {
        let outcome = quiesce_job(
            Some(4242),
            60,
            |_| false,
            |_| true,
            || unreachable!("no wait after a signal that never landed"),
        );
        assert_eq!(outcome, Quiesce::StillRunning);
    }
}
