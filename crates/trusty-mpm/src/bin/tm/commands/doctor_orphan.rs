//! Client-side orphan-daemon check for `tm doctor` (issue #4230).
//!
//! Why: every existing daemon probe treats a 200 from `/health` as proof the
//! stack is fine. #4230 is the case where that is exactly wrong: an orphaned
//! 1.0.2 daemon (PID 98606, PPID 1, `cwd=$HOME`) held :7880 for two days while
//! launchd's `com.trusty.mpm` reported `state = not running`, answering every
//! request successfully. A fresh signed install plus behavioural verification
//! all passed against a binary the install had just replaced, and `launchctl
//! bootout` was a silent no-op because launchd did not own the listener. A check
//! that cannot fail when the thing it checks is broken has no value.
//!
//! What: [`orphan_daemon_check`] compares WHO ANSWERED against WHO LAUNCHD RUNS.
//! Those two facts come from independent sources — `/health`'s `pid` and
//! `launchctl list <label>` — so neither the daemon nor launchd alone can make
//! the check pass.
//!
//! The `supervised` self-report is a FALLBACK, not the primary signal (#4230
//! review). It was an environment-variable heuristic — an `XPC_SERVICE_NAME`
//! probe with a `getppid() == 1` fallback, both of which a non-launchd process
//! satisfies — until #4469 replaced it with an authoritative `launchctl` query.
//! It is STILL not the primary signal here, for two reasons that outlive that
//! fix: the flag is computed once at daemon startup, and the responding daemon
//! may be an older build whose flag is the old heuristic. A detector resting on
//! a self-report is blind to exactly the population it targets, however that
//! self-report is derived.
//!
//! This lives in the `tm` CLI binary rather than the daemon's server-side
//! `run_doctor` for the same reason as the #2332 stale-daemon check
//! (`doctor_stale`): the answer depends on state outside the responding
//! process.
//! Only a client can compare "who answered" against "who launchd was told to
//! run", and in the #4230 state the daemon that answers is precisely the one
//! whose self-report cannot be trusted.
//!
//! Cost: one `launchctl list` invocation plus a `read_dir`, on top of the
//! `/health` snapshot the caller already fetched and passes in. No spawn, no port
//! scan, no network.
//!
//! Test: `crates/trusty-mpm/src/bin/tm/commands/doctor_orphan_tests.rs`.

use trusty_mpm::client::HealthSnapshot;
use trusty_mpm::core::doctor::{CheckStatus, DoctorCheck};

use crate::commands::launchd_probe;
use crate::commands::launchd_probe::LaunchdOwnership as Ownership;

/// Name of this check as it appears in `tm doctor` output.
pub(crate) const CHECK_NAME: &str = "daemon_orphan";

/// Assemble the #4230 orphan check from the caller's `/health` snapshot.
///
/// Why: the snapshot is passed in rather than re-fetched (#4230 review, LOW-1) —
/// `commands::doctor_local` already needs it for the #2332 staleness check, and
/// issuing a second GET both contradicted this module's own doc and sampled the
/// daemon twice, which could straddle a restart.
/// What: resolves the registered daemon launchd label and, when there is one,
/// asks launchd which PID it currently runs for it; delegates the decision to the
/// pure [`verdict`]. `None` (the caller's fetch failed) yields `Unknown` — an
/// unreachable daemon is explained in detail by the report's own checks, and an
/// undetermined check must not read as a pass (#4005 precedent). This is also what
/// keeps the documented `bootout → install → bootstrap` window from producing a
/// spurious `Fail`.
/// Test: the decision table is covered by `doctor_orphan_tests.rs`; the two live
/// probes are thin wrappers tested in `launchd_probe`.
pub(crate) fn orphan_daemon_check(snapshot: Option<&HealthSnapshot>) -> DoctorCheck {
    let label = launchd_probe::daemon_launchd_label();
    let Some(snapshot) = snapshot else {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Unknown,
            "no /health response — cannot tell whether a supervised daemon or an \
             orphan is serving (issue #4230)",
        );
    };
    // #5623: an unreadable `~/Library/LaunchAgents` used to arrive here as "no
    // unit registered", which `verdict` short-circuits to `Ok` — the check would
    // clear a daemon it never examined. An undetermined check reports `Unknown`
    // (#4005 precedent), same as an absent `/health` above.
    if label == launchd_probe::DaemonLabelProbe::Undeterminable {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Unknown,
            "`~/Library/LaunchAgents` could not be read — cannot tell whether a launchd \
             unit owns this daemon, so orphan status is undetermined. Check the directory \
             with `ls -ld ~/Library/LaunchAgents` (issues #4230, #5623)",
        );
    }
    // Only ask launchd when a daemon unit is registered; without one there is
    // nothing to compare against and `verdict` short-circuits to `Ok` anyway.
    let ownership = label
        .label()
        .map(launchd_probe::launchd_ownership)
        .unwrap_or(launchd_probe::LaunchdOwnership::Unavailable);
    // #4469 note: `snapshot.launchd_supervision` carries the daemon's THREE-STATE
    // answer, but this check deliberately does not consume it — and that is not
    // an oversight. Whenever the daemon reports a pid (every daemon since #4230),
    // `verdict`'s authoritative tier decides from THIS process's own launchd
    // query, where `None` and `Some(false)` are already equivalent
    // (`supervised != Some(true)`); the client's independent answer is the whole
    // point of that tier and correctly outranks any self-report. The three-state
    // signal's real consumer is `/health` itself, which `CLAUDE.md`'s restart
    // convention tells operators to read directly.
    verdict(
        label.label(),
        ownership,
        snapshot.pid,
        snapshot.supervised,
        snapshot.unsupervised_forced,
        &snapshot.version,
    )
}

/// Pure verdict: is the daemon answering `/health` an unsupervised orphan?
///
/// Why: separated from [`orphan_daemon_check`] so every branch — above all the
/// FAIL branches — is directly testable. A test that could only call the wrapper
/// would depend on the host's real `~/Library/LaunchAgents` and its real daemon,
/// so it would assert the happy path and silently lose the regression guard: the
/// same shape of blind spot #4230 is about.
/// What: a three-tier decision, strongest evidence first.
///
/// 1. No daemon launchd unit registered → `Ok`. A bare dev daemon is the expected
///    arrangement, and the supervisor plist does not count (see
///    [`launchd_probe::daemon_launchd_label_in`]).
/// 2. AUTHORITATIVE — the responding daemon reports its pid, and launchd gave a
///    POSITIVE answer. Equal pids → `Ok`; different → orphan. `NotRunning` while
///    something answers is the #4230 state verbatim: the job is down, yet the
///    port is served.
/// 3. FALLBACK — the responding daemon predates `pid`. Then and only then does the
///    `supervised` self-report decide: `Some(false)` → orphan (the daemon agrees
///    it is hazardous); `Some(true)` or `None` → `Unknown`, never `Ok`. A daemon
///    old enough to omit `pid` also predates #4469, so its `true` came from the
///    `getppid() == 1` prong that any reparented orphan satisfies.
///
/// Two guards keep the hard `Fail` — whose remediation prescribes `kill -TERM` —
/// off a healthy daemon (#4230 review round 2):
///
/// * [`launchd_probe::LaunchdOwnership::Unavailable`] never produces `Fail`. "I
///   could not ask launchd" is an absence of evidence, and the round-1 error was
///   exactly this mistake in the other direction. Verified live: with `launchctl`
///   off `PATH`, the previous code condemned the daemon it had just called `Ok`.
/// * `NotRunning` escalates to `Fail` only when `supervised != Some(true)`. When
///   launchd says the job is down and the daemon says it is supervised, the two
///   authorities contradict and neither can be acted on destructively. This does
///   not weaken the incident detection: the actual #4230 payload reported
///   `supervised: false`, so it still lands on `Fail`, and a lying daemon can only
///   reach `Unknown` — never `Ok`.
///
/// Test: `authoritative_fail_when_launchd_runs_a_different_pid`,
/// `authoritative_fail_when_launchd_runs_nothing_but_a_daemon_answers`,
/// `authoritative_ok_when_launchd_owns_the_responding_pid`,
/// `ok_when_no_launchd_unit_is_registered`,
/// `fallback_fail_when_old_daemon_reports_unsupervised`,
/// `fallback_unknown_when_old_daemon_claims_supervised`,
/// `fallback_unknown_when_supervised_is_absent`,
/// `warns_not_fails_when_the_operator_forced_it`,
/// `authoritative_verdict_ignores_a_lying_self_report`,
/// `failure_names_the_pid_lookup_the_kill_and_the_launchctl_restart`,
/// `failure_names_the_force_opt_in`, `failure_names_the_serving_version`,
/// `failure_is_a_hard_fail_not_a_warn`, `unknown_version_is_labelled_not_blank`,
/// `remediation_names_the_resolved_label`,
/// `unavailable_launchctl_is_unknown_not_a_fail`,
/// `unavailable_launchctl_never_prescribes_a_kill`,
/// `unavailable_message_names_the_domain_cause`,
/// `not_running_but_supervised_is_unknown_not_a_kill_order`,
/// `not_running_with_supervised_false_is_still_a_hard_fail`,
/// `unavailable_with_an_old_daemon_reporting_unsupervised_still_fails`.
pub(crate) fn verdict(
    label: Option<&str>,
    ownership: launchd_probe::LaunchdOwnership,
    serving_pid: Option<u32>,
    supervised: Option<bool>,
    forced: bool,
    version: &str,
) -> DoctorCheck {
    let Some(label) = label else {
        return ok(
            "no trusty-mpm daemon launchd unit is registered — an unsupervised \
             daemon is the expected arrangement on this host"
                .to_string(),
        );
    };

    match serving_pid {
        // Tier 2 — authoritative. Neither side can fake this.
        Some(serving) => match ownership {
            Ownership::Running(owned) if owned == serving => ok(format!(
                "launchd unit `{label}` owns the daemon answering /health \
                 (pid {serving}, version {})",
                display_version(version)
            )),
            // Both sides answered positively and they disagree. No self-report can
            // soften this: it is the lying-daemon case the round-1 review named.
            Ownership::Running(owned) => orphan(
                label,
                forced,
                version,
                format!(
                    "launchd runs pid {owned} for `{label}`, but pid {serving} is the one \
                     answering /health"
                ),
            ),
            // launchd POSITIVELY reports the job down while something serves the
            // port — #4230 verbatim. Escalate only when the daemon does not also
            // claim supervision; a contradiction between two authorities is not a
            // licence to prescribe `kill -TERM`.
            Ownership::NotRunning if supervised != Some(true) => orphan(
                label,
                forced,
                version,
                format!(
                    "launchd runs NO process for `{label}` (the unit is registered but its \
                     job is down), yet pid {serving} is answering /health"
                ),
            ),
            Ownership::NotRunning => contradiction(label, version, serving),
            // Absence of evidence. Never a `Fail`, and never a kill instruction.
            Ownership::Unavailable => unaskable(label, version, serving),
        },
        // Tier 3 — fallback: the responding daemon is too old to identify itself.
        None => match supervised {
            Some(false) => orphan(
                label,
                forced,
                version,
                format!(
                    "the daemon answering /health does not report its pid, and reports \
                     `supervised: false` against the registered unit `{label}`"
                ),
            ),
            // Never `Ok`. A daemon old enough to omit `pid` predates #4469, so
            // its `supervised` flag is still the env-var heuristic whose
            // `getppid() == 1` prong any reparented orphan satisfies.
            Some(true) => unconfirmable(
                label,
                version,
                "Its own `supervised: true` is NOT conclusive — a daemon this old computes \
                 that flag from the pre-#4469 heuristic, whose `getppid() == 1` prong any \
                 reparented orphan satisfies",
            ),
            None => unconfirmable(label, version, "It reports no supervision flag either"),
        },
    }
}

/// Build the `Ok` variant of this check.
///
/// Why: the two `Ok` paths would otherwise repeat the name/status pair.
/// What: a [`CheckStatus::Ok`] [`DoctorCheck`] carrying `message`.
/// Test: covered via `ok_when_no_launchd_unit_is_registered` and
/// `authoritative_ok_when_launchd_owns_the_responding_pid`.
fn ok(message: String) -> DoctorCheck {
    DoctorCheck::new(CHECK_NAME, CheckStatus::Ok, message)
}

/// Build the `Unknown` verdict for a daemon too old to be matched against launchd.
///
/// Why: `Unknown` is the correct third state for "this daemon cannot tell me" —
/// the reason the client models `supervised` as `Option<bool>`. Reporting `Ok`
/// here is what would inherit the self-report weakness the docs half of this
/// change argues against. Both fallback branches share the remediation, so they
/// share one builder.
/// What: `Unknown` naming the version, the registered label, the branch-specific
/// `reason`, and the restart command that makes the daemon report its pid.
/// Test: `fallback_unknown_when_old_daemon_claims_supervised`,
/// `fallback_unknown_when_supervised_is_absent`.
fn unconfirmable(label: &str, version: &str, reason: &str) -> DoctorCheck {
    DoctorCheck::new(
        CHECK_NAME,
        CheckStatus::Unknown,
        format!(
            "the daemon answering /health (version {}) is too old to report its pid, so \
             supervision cannot be confirmed against launchd unit `{label}`. {reason} \
             (issue #4230). Restart the daemon with `{}` so it reports its pid, then \
             re-run `tm doctor`.",
            display_version(version),
            launchd_probe::daemon_restart_command_for(Some(label))
        ),
    )
}

/// Build the `Unknown` verdict for "launchd could not be asked" (#4230 review
/// round 2).
///
/// Why: this is the branch the round-2 review found prescribing a DESTRUCTIVE
/// remediation on a false reading. `launchctl` not answering is not evidence that
/// launchd runs nothing — it happens with a scrubbed `PATH`, in a sandbox, and
/// when the unit is not visible in the caller's bootstrap domain. Since
/// `KeepAlive {SuccessfulExit: false}` means the job will not come back on its
/// own, telling the operator to `kill -TERM` a correctly supervised daemon here
/// costs them the daemon. `--force` is no escape either: it would relabel a
/// supervised daemon as a deliberate unsupervised one.
/// What: `Unknown`, naming the pid that answered, the unit, why launchd may have
/// declined, and a MANUAL comparison the operator can make. Contains no
/// `kill`/`-TERM` instruction by construction — asserted by a test.
/// Test: `unavailable_launchctl_is_unknown_not_a_fail`,
/// `unavailable_launchctl_never_prescribes_a_kill`,
/// `unavailable_message_names_the_domain_cause`.
fn unaskable(label: &str, version: &str, serving: u32) -> DoctorCheck {
    DoctorCheck::new(
        CHECK_NAME,
        CheckStatus::Unknown,
        format!(
            "cannot determine whether pid {serving} (version {}) answering /health is the \
             process launchd runs for unit `{label}`: `launchctl` did not answer. That \
             happens when `launchctl` is not on `PATH`, in a sandbox, or when the unit is \
             not visible in this shell's bootstrap domain — `launchctl list` resolves \
             against the caller's domain and an `Aqua`-only agent is invisible from some \
             contexts. This is NOT evidence of an orphan (issue #4230), so no corrective \
             action is prescribed. To check by hand, compare `launchctl print \
             gui/$(id -u)/{label}` against `lsof -nP -iTCP -sTCP:LISTEN | grep tm`.",
            display_version(version)
        ),
    )
}

/// Build the `Unknown` verdict when launchd and the daemon flatly contradict each
/// other (#4230 review round 2).
///
/// Why: launchd positively reporting the job down while the responding daemon
/// positively claims supervision cannot both be true. It is a real hazard shape,
/// but the safe response is to surface the contradiction, not to prescribe a kill:
/// if the daemon is right, the kill destroys a healthy supervised process. The
/// round-1 closure is preserved because the actual #4230 payload reported
/// `supervised: false` and so never reaches here, and because this state can only
/// ever reach `Unknown` — never `Ok`.
/// What: `Unknown` naming both claims and the non-destructive way to settle them.
/// Test: `not_running_but_supervised_is_unknown_not_a_kill_order`.
fn contradiction(label: &str, version: &str, serving: u32) -> DoctorCheck {
    DoctorCheck::new(
        CHECK_NAME,
        CheckStatus::Unknown,
        format!(
            "launchd reports NO running process for unit `{label}`, yet pid {serving} \
             (version {}) is answering /health AND reports `supervised: true`. Those two \
             claims contradict, so this is not conclusive and no corrective action is \
             prescribed (issue #4230) — the daemon's flag is a self-report computed once \
             at ITS startup (and on a pre-#4469 build, from an env-var heuristic a \
             reparented orphan satisfies), but launchd may also be answering from a \
             domain that cannot see the unit. Settle it with `launchctl print \
             gui/$(id -u)/{label}` before acting.",
            display_version(version)
        ),
    )
}

/// Build the orphan verdict, respecting the `--force` opt-in.
///
/// Why: the three orphan paths differ only in the EVIDENCE sentence; the
/// consequence, the remediation, and the `forced` downgrade are identical, so they
/// get one source of truth rather than three near-copies.
/// What: `Warn` when `forced` (a deliberate unsupervised run), else `Fail`. The
/// failure text names the consequence, then the `lsof` PID lookup, the
/// `kill -TERM`, and the label-correct restart command, in the order an operator
/// must run them.
/// Test: `warns_not_fails_when_the_operator_forced_it`,
/// `failure_is_a_hard_fail_not_a_warn`,
/// `failure_names_the_pid_lookup_the_kill_and_the_launchctl_restart`,
/// `failure_names_the_force_opt_in`, `remediation_names_the_resolved_label`.
fn orphan(label: &str, forced: bool, version: &str, evidence: String) -> DoctorCheck {
    let version = display_version(version);
    if forced {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Warn,
            format!(
                "an unsupervised daemon (version {version}) is serving alongside registered \
                 launchd unit `{label}`: {evidence}. It was started deliberately with \
                 `tm daemon --force`, so this is a warning, not a failure — but while it \
                 holds the port, installing a new binary will not take effect and \
                 `launchctl bootout` is a no-op against it (issue #4230)."
            ),
        );
    }
    DoctorCheck::new(
        CHECK_NAME,
        CheckStatus::Fail,
        format!(
            "ORPHAN daemon serving: {evidence}. The orphan (version {version}) answers 200, \
             so a fresh install and its verification both pass against the STALE binary it \
             is still serving, and `launchctl bootout` is a no-op against a process launchd \
             does not own (issue #4230). Fix: `lsof -nP -iTCP -sTCP:LISTEN | grep tm` to \
             confirm the PID, `kill -TERM <pid>`, then `{}`. If this daemon IS intentional, \
             restart it with `tm daemon --force` so it is reported as deliberate.",
            launchd_probe::daemon_restart_command_for(Some(label))
        ),
    )
}

/// Render a `/health` version for an operator-facing message.
///
/// Why: a daemon predating #2332 omits `version`, which arrives here as an empty
/// string; printing `version ` with nothing after it reads as a formatting bug and
/// hides that the build is unidentified — the exact ambiguity #2332 fixed.
/// What: returns the version unchanged, or `"unknown"` when it is empty.
/// Test: `unknown_version_is_labelled_not_blank`.
fn display_version(version: &str) -> &str {
    if version.is_empty() {
        "unknown"
    } else {
        version
    }
}

#[cfg(test)]
#[path = "doctor_orphan_tests.rs"]
mod tests;
