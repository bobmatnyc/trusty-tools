//! Launchd-unit probing shared by daemon startup and the MCP stdio bridge (#2486).
//!
//! Why: during the documented `launchctl bootout → cargo install → launchctl
//! bootstrap` restart, trusty-console's auto-reconnect can race the restart —
//! it spawns a fresh `tm serve --stdio` bridge child whose startup auto-spawns
//! an ORPHAN daemon before launchd has a chance to relaunch the supervised one.
//! The orphan answers `/health` 200 (so a green health check is NOT sufficient
//! evidence launchd won) but runs without the plist's `EnvironmentVariables`
//! (`TELEGRAM_BOT_TOKEN`, `OPENROUTER_API_KEY`) and with `cwd=$HOME`, silently
//! breaking features like the Telegram poller. Both the daemon (to report
//! `supervised` on `/health`) and the stdio bridge (to decide whether
//! auto-spawn is safe) need the same "does a trusty-mpm launchd unit exist"
//! signal, so it is centralised here instead of duplicated.
//!
//! What: [`daemon_launchd_label_in`] resolves WHICH launchd label owns the
//! daemon on a given host (the #4230 review narrowed this from "any
//! `com.trusty.mpm*` plist" — the supervisor plist does not own a daemon), and
//! [`mpm_launchd_plist_exists`] reduces it to the boolean the older guards
//! consume. [`compute_supervised`], [`compute_no_spawn`] and
//! [`compute_daemon_refuse`] are pure functions over that boolean (plus, for
//! supervision, [`trusty_common::update::is_launchd_supervised`]).
//! [`launchd_ownership`] asks launchd which PID it actually runs, so the
//! `daemon_orphan` doctor check has an AUTHORITATIVE signal rather than only the
//! daemon's own heuristic self-report. It answers in THREE states
//! ([`LaunchdOwnership`]) because "launchd runs nothing" and "launchd could not
//! be asked" carry opposite consequences and the round-2 review found them
//! conflated.
//!
//! Test: `compute_supervised_*`, `compute_no_spawn_*` and
//! `compute_daemon_refuse_*` below exercise every combination of the pure
//! decision tables; `daemon_label_*` cover label resolution against temp homes;
//! `parse_launchctl_pid_*` cover every real `launchctl` output shape and
//! `ownership_from_output_*` the exit-0 mapping. The two live wrappers
//! ([`daemon_launchd_label`], [`launchd_ownership`]) are thin `$HOME` /
//! subprocess lookups over those tested cores.

/// The canonical launchd label for the trusty-mpm DAEMON.
///
/// Why: `#4230` review — this is the one label whose plist `ProgramArguments`
/// actually run `trusty-mpm daemon`. Kept as a shared constant so the probe,
/// the refusal hint, and the restart recipe cannot drift apart.
pub(crate) const DAEMON_LAUNCHD_LABEL: &str = "com.trusty.mpm";

/// Resolve the launchd label of a registered trusty-mpm DAEMON unit, given a
/// home directory.
///
/// Why (#4230 review): the previous probe also counted
/// `com.trusty.mpm.supervisor.plist`. That plist runs `[<tm>, "supervisor"]`
/// (`trusty-installer`'s `plist_bootstrap::PLIST_TEMPLATE`), and `tm supervisor`
/// is a PASSIVE fleet observer — it never starts, restarts, or owns a daemon
/// (the only two daemon spawn sites in this crate are `daemon::start` and
/// `guided_autostart::ensure_daemon_started`). Treating it as "launchd owns the
/// daemon" made every consumer wrong on a `tctl install` host that has only the
/// supervisor plist: the refusal fired with a false claim, and the remediation
/// named `com.trusty.mpm`, a label that does not exist there.
///
/// Returning the LABEL rather than a bool is what lets the remediation name a
/// unit that genuinely exists on this host.
/// What: mirrors `scripts/install-trusty-mpm-signed.sh`'s `resolve_mpm_plist`,
/// which is already the repo's convention for this question — prefer the
/// canonical `com.trusty.mpm.plist`; otherwise accept a drifted
/// `com.trusty.*mpm*.plist`, excluding `supervisor`/`gui` names, which that
/// script documents as "distinct services" — and, since the round-2 review, only
/// when that drifted plist's `ProgramArguments` actually run `daemon` (see
/// [`plist_runs_daemon`]). `None` when neither is present.
///
/// The canonical `com.trusty.mpm.plist` short-circuit is deliberately NOT gated on
/// [`plist_runs_daemon`]: that label is the daemon's by definition, and the
/// failure modes are asymmetric. A false negative there would make
/// [`mpm_launchd_plist_exists`] report `false` on a correctly installed host,
/// standing the #4230 spawn guard down and reintroducing the very orphan this
/// issue is about. The contents check exists to stop a SIBLING unit being
/// promoted on the strength of its filename, which is a drift-path-only risk.
/// Test: `daemon_label_prefers_the_canonical_plist`,
/// `daemon_label_ignores_a_supervisor_only_home`,
/// `daemon_label_none_for_an_empty_home`,
/// `daemon_label_accepts_a_drifted_label`,
/// `daemon_label_ignores_a_gui_plist`,
/// `daemon_label_ignores_a_decoy_sibling_unit`.
pub(crate) fn daemon_launchd_label_in(home: &std::path::Path) -> Option<String> {
    let agents = home.join("Library/LaunchAgents");
    if agents
        .join(format!("{DAEMON_LAUNCHD_LABEL}.plist"))
        .exists()
    {
        return Some(DAEMON_LAUNCHD_LABEL.to_string());
    }
    let entries = std::fs::read_dir(&agents).ok()?;
    let mut drifted: Vec<String> = entries
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|name| name.strip_suffix(".plist").map(str::to_owned))
        .filter(|label| {
            label.starts_with("com.trusty.")
                && label.contains("mpm")
                && !label.contains("supervisor")
                && !label.contains("gui")
        })
        // #4230 review round 2 (MEDIUM): the name filter alone accepted ANY
        // sibling unit as THE daemon — `com.trusty.mpm.logrotate` would have
        // become the label, and every consequence (the refusal, the restart
        // recipe, the PID comparison) followed from that wrong unit. This host
        // already carries a real `com.trusty.trusty-search.logrotate.plist`, so
        // the shape is not hypothetical. Require the plist to actually run
        // `daemon` before believing it owns one.
        .filter(|label| plist_runs_daemon(&agents.join(format!("{label}.plist"))))
        .collect();
    // Sort so a home with several drifted labels resolves deterministically
    // rather than depending on directory-iteration order.
    drifted.sort();
    drifted.into_iter().next()
}

/// Does this plist's `ProgramArguments` actually run a `daemon` subcommand?
///
/// Why (#4230 review round 2): the drifted-label search is a NAME heuristic, and
/// names lie. The authoritative statement of what a unit runs is its
/// `ProgramArguments`, which is also how this change already distinguishes the
/// supervisor plist (`[<tm>, "supervisor"]`). Reading it turns the heuristic into
/// a check.
/// What: `true` when any element after the program path is exactly `daemon`.
/// Deliberately tolerant of the surrounding XML rather than a full plist parse:
/// scans `<string>…</string>` values inside the `ProgramArguments` array, which
/// is the shape `trusty-installer`'s `PLIST_TEMPLATE` and the signed installer
/// both emit. An unreadable or unparseable plist yields `false` — a unit whose
/// contents cannot be confirmed is not promoted to "the daemon" on the strength
/// of its filename.
/// Test: `daemon_label_ignores_a_decoy_sibling_unit`,
/// `daemon_label_accepts_a_drifted_label`,
/// `plist_runs_daemon_false_for_a_missing_file`.
fn plist_runs_daemon(path: &std::path::Path) -> bool {
    let Ok(xml) = std::fs::read_to_string(path) else {
        return false;
    };
    // Narrow to the ProgramArguments array so a `daemon` string elsewhere in the
    // plist (a log path like `…/Logs/trusty-mpm/daemon.log`, an environment
    // variable) cannot be mistaken for an argument.
    let Some(after_key) = xml.split("<key>ProgramArguments</key>").nth(1) else {
        return false;
    };
    let array = after_key.split("</array>").next().unwrap_or_default();
    array
        .split("<string>")
        .skip(1)
        .filter_map(|s| s.split("</string>").next())
        .any(|arg| arg.trim() == "daemon")
}

/// Resolve the registered trusty-mpm daemon launchd label on this host.
///
/// Why: the live wrapper over [`daemon_launchd_label_in`]; separated so the
/// resolution rules are unit-testable against a temp home.
/// What: applies [`daemon_launchd_label_in`] to `$HOME`, or `None` when `$HOME`
/// cannot be resolved (e.g. a stripped-down CI sandbox) — treated the same as
/// "no unit registered" by callers.
/// Test: the pure resolver is tested below; this wrapper is a thin `$HOME` lookup.
pub(crate) fn daemon_launchd_label() -> Option<String> {
    daemon_launchd_label_in(&dirs::home_dir()?)
}

/// Returns `true` when a trusty-mpm DAEMON launchd unit is registered.
///
/// Why: the presence of a daemon plist is the operator's declared intent that
/// launchd should own the daemon's lifecycle; the health-supervision signal, the
/// bridge's no-spawn decision, and the `tm start`/`tm restart` refusal all key
/// off this one fact.
/// What: `true` when [`daemon_launchd_label`] resolves a label. Narrowed by the
/// #4230 review from "any `com.trusty.mpm*` plist" — see
/// [`daemon_launchd_label_in`] for why the supervisor plist does not count. This
/// also removes a latent false refusal in the #4397 child guard and a latent
/// false `no_spawn` in the #2486 bridge guard on supervisor-only hosts.
/// Test: exercised via [`daemon_launchd_label_in`]'s tests; the pure decisions
/// that consume this boolean are unit-tested below.
pub(crate) fn mpm_launchd_plist_exists() -> bool {
    daemon_launchd_label().is_some()
}

/// The command that correctly restarts the daemon on this host, given the
/// resolved launchd label.
///
/// Why (#4230 review): `tm restart` now refuses when launchd owns the daemon, so
/// every message that prescribed it unconditionally became self-contradictory —
/// most visibly `tm doctor`, which printed "run `tm restart`" two lines above the
/// new orphan check. One resolver keeps the remediation correct per host instead
/// of hardcoding a verb that is wrong half the time.
/// What: the `launchctl kickstart -k` recipe for `label` when a daemon unit is
/// registered; plain `tm restart` otherwise.
/// Test: `restart_command_names_kickstart_when_launchd_owns_the_daemon`,
/// `restart_command_names_tm_restart_without_a_unit`.
pub(crate) fn daemon_restart_command_for(label: Option<&str>) -> String {
    match label {
        Some(l) => format!("launchctl kickstart -k gui/$(id -u)/{l}"),
        None => "tm restart".to_string(),
    }
}

/// Resolve [`daemon_restart_command_for`] against this host.
///
/// Why: call sites want the correct verb without each re-deriving the label.
/// What: applies [`daemon_restart_command_for`] to [`daemon_launchd_label`].
/// Test: the pure resolver is tested below.
pub(crate) fn daemon_restart_command() -> String {
    daemon_restart_command_for(daemon_launchd_label().as_deref())
}

/// Pure decision: is this daemon process in a hazardous unsupervised state?
///
/// Why: factored out of any filesystem/env access so the four-quadrant truth
/// table is testable without launchd, a plist, or environment variables.
/// What: `true` (safe) when no launchd unit exists at all (a dev-run daemon
/// with no plist is fine) OR this process itself is launchd-supervised.
/// `false` (hazardous) exactly when a launchd unit exists but this process
/// was NOT launched by launchd — the #2486 orphan-daemon signature.
/// Test: `compute_supervised_true_when_no_plist`,
/// `compute_supervised_true_when_plist_and_launchd`,
/// `compute_supervised_false_when_plist_without_launchd`,
/// `compute_supervised_true_when_launchd_and_no_plist` (all four quadrants of
/// the (`is_launchd_supervised`, `plist_exists`) truth table are covered).
pub(crate) fn compute_supervised(is_launchd_supervised: bool, plist_exists: bool) -> bool {
    !plist_exists || is_launchd_supervised
}

/// Pure decision: should a CLIENT-side auto-spawn of the daemon be refused?
///
/// Why: when a launchd unit is registered, `launchctl bootstrap`/`kickstart` is
/// the correct (and eventually consistent) way to bring the daemon back after a
/// restart; a client-side auto-spawn during the restart gap creates the #2486
/// orphan. When no launchd unit exists (a dev machine), auto-spawn remains the
/// convenient default.
///
/// #4230: this decision is deliberately keyed on `plist_exists` ALONE — no
/// supervision heuristic. It is consumed by every client that would spawn a
/// daemon subprocess: the MCP stdio bridge (`serve_stdio`, the original #2486
/// call site) and `tm start`/`tm restart` (`commands::daemon::start`, the path
/// that actually produced the #4230 orphan — it spawned a detached bare
/// `tm daemon` with no launchd awareness whatsoever). Keeping it heuristic-free
/// matters: the child-side #4397 guard folds in
/// [`trusty_common::update::is_launchd_supervised`], which until #4469 could
/// report `true` for a non-launchd child purely from an inherited
/// `XPC_SERVICE_NAME`. That prong is gone, but a callee-side guard still cannot
/// be the last line of defence — it runs in the very process whose legitimacy is
/// in question, and older binaries still carry the heuristic.
/// What: `no_spawn` is exactly `plist_exists` — spawn is refused if and only
/// if a trusty-mpm launchd unit is present.
/// Test: `compute_no_spawn_true_when_plist_exists`,
/// `compute_no_spawn_false_when_no_plist`.
pub(crate) fn compute_no_spawn(plist_exists: bool) -> bool {
    plist_exists
}

/// Operator guidance printed when `tm start`/`tm restart` refuses to spawn a
/// daemon because launchd owns it (issue #4230).
///
/// Why: the pre-#4230 `commands::daemon::start` spawned a detached bare
/// `tm daemon` unconditionally, with no launchd check at all — that is the path
/// that produced the #4230 orphan (PID 98606, PPID 1, `cwd=$HOME`, serving a
/// stale 1.0.2 image on :7880 for two days while launchd's `com.trusty.mpm`
/// reported `not running`). Once the child-side #4397 guard landed, the same
/// invocation instead spawns a child that dies in `~/.trusty-mpm/daemon.log`
/// and `tm start` reports only "daemon did not become healthy within 5s" — the
/// operator is told the daemon is broken, not that they used the wrong verb.
/// This message names the right verb up front.
/// What: a string naming the launchd unit that is ACTUALLY registered
/// (`label`, resolved by [`daemon_launchd_label`]), its
/// [`daemon_restart_command_for`] recipe, and the `tm daemon --force` opt-in.
/// Taking the label as a parameter is the #4230-review fix for a hint that
/// previously hardcoded `com.trusty.mpm` and so prescribed a nonexistent unit on
/// a host whose label had drifted.
/// Test: `cli_spawn_refusal_names_kickstart_and_force`,
/// `cli_spawn_refusal_names_the_resolved_label`.
pub(crate) fn cli_spawn_refusal_hint(label: &str) -> String {
    format!(
        "refusing to spawn: the launchd unit `{label}` is registered, so launchd owns \
         the daemon's lifecycle — spawning one here would create a duplicate, \
         unsupervised daemon that can seize the port and serve a stale binary \
         indefinitely (issue #2486/#4230). Restart it with `{}`, then re-run \
         `tm status`. To start an unsupervised daemon on purpose, run \
         `tm daemon --force`.",
        daemon_restart_command_for(Some(label))
    )
}

/// What launchd will say about a daemon unit — three states, not two.
///
/// Why (#4230 review round 2): the previous probe returned `Option<u32>` and so
/// collapsed two opposite facts into one `None`. "launchd has this unit loaded
/// and its job is DOWN" is the #4230 incident verbatim and warrants a hard
/// `Fail`; "I could not ask launchd" is an absence of evidence and warrants
/// `Unknown`. Conflating them made `tm doctor` prescribe `kill -TERM` against a
/// correctly supervised daemon whenever `launchctl` could not answer — a
/// DESTRUCTIVE action on a false reading, and the round-1 error (absence of
/// evidence read as evidence of a state) inverted. `KeepAlive
/// {SuccessfulExit: false}` means the job does not come back on its own after
/// that kill, so the wrong answer here costs the operator their daemon.
/// What: `Running(pid)` and `NotRunning` are POSITIVE answers from launchd;
/// `Unavailable` means the question could not be put to launchd at all.
/// Test: `ownership_*` and the `verdict_*` cases in `doctor_orphan_tests.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LaunchdOwnership {
    /// launchd positively reports a live PID for the unit.
    Running(u32),
    /// launchd positively reports the unit but no running process — the job is
    /// down. This is the #4230 state when something else is serving the port.
    NotRunning,
    /// launchd could not be asked: `launchctl` missing from `PATH`, a spawn
    /// failure, or the unit not visible in the caller's bootstrap domain.
    Unavailable,
}

/// Ask launchd what it currently runs for `label`, distinguishing "nothing" from
/// "could not ask".
///
/// Why (#4230 review): the `daemon_orphan` doctor check must not rest on the
/// daemon's own `supervised` self-report.
/// [`trusty_common::update::is_launchd_supervised`] HAD two prongs — an
/// `XPC_SERVICE_NAME` env probe and a `getppid() == 1` fallback — and the second
/// made any orphan whose spawning parent exited self-report as supervised. #4469
/// replaced both with an authoritative launchd query, but the check still must
/// not lean on the self-report: it is computed once at startup, and the daemon
/// answering may be an older binary that still carries the heuristic. launchd's own answer is authoritative; a PID comparison against it
/// is exactly what the revised runbook tells a human to do.
///
/// Round 2 added the third state and a second probe. `launchctl list <label>`
/// resolves against the CALLER's bootstrap domain and exits 113
/// (`Could not find service "…" in domain for port`) when the unit is not
/// visible there — the same exit code and message it gives for a unit that was
/// never bootstrapped at all. Those two worlds are indistinguishable from that
/// answer alone, so a 113 is escalated to an explicit `gui/<uid>` query (the
/// domain `scripts/install-trusty-mpm-signed.sh` bootstraps into, and the one
/// this codebase's own restart recipe names) before giving up. Only when BOTH
/// probes decline does the answer become `Unavailable`.
/// What: `Running(pid)` / `NotRunning` on an exit-0 answer, according to whether
/// [`parse_launchctl_pid`] finds a PID; otherwise the `gui/<uid>` retry; else
/// `Unavailable`.
/// Test: the parser is unit-tested by `parse_launchctl_pid_*` and the
/// exit-status-to-state mapping by `ownership_from_output_*`; the shell-out
/// itself needs a live launchd and is exercised by running `tm doctor`.
pub(crate) fn launchd_ownership(label: &str) -> LaunchdOwnership {
    // Probe 1: the legacy, domain-agnostic API. Cheapest, and the only one that
    // answers on hosts where the unit lives outside `gui/<uid>`.
    if let Some(state) = ask_launchctl(&["list".to_string(), label.to_string()]) {
        return state;
    }
    // Probe 2: name the GUI domain explicitly. A `list` that exited non-zero may
    // simply have been asked from a domain that cannot see an `Aqua`-only agent.
    //
    // NOT `trusty_common::launchd::current_uid()`, which would be the natural
    // shared helper: that whole module is `#[cfg(target_os = "macos")]`, while this
    // one is not, so borrowing it breaks the Linux build. `libc::getuid()` is what
    // that helper wraps anyway.
    // SAFETY: getuid() is a POSIX call with no arguments and no failure mode.
    let uid = unsafe { libc::getuid() };
    if let Some(state) = ask_launchctl(&["print".to_string(), format!("gui/{uid}/{label}")]) {
        return state;
    }
    LaunchdOwnership::Unavailable
}

/// Run one `launchctl` query and map a SUCCESSFUL answer onto a positive state.
///
/// Why: both probes in [`launchd_ownership`] share the same
/// "did launchd actually answer?" test, and keeping it in one place is what stops
/// a future edit from reintroducing the round-2 conflation on one of the two
/// paths.
/// What: `None` when `launchctl` could not be spawned or exited non-zero — i.e.
/// no answer, try the next probe. `Some(Running | NotRunning)` only for exit 0,
/// where launchd's silence about a PID genuinely means the job is down.
/// Test: `ownership_from_output_running`, `ownership_from_output_not_running`
/// cover the mapping; the spawn failure is covered live by scenario C in the PR.
fn ask_launchctl(args: &[String]) -> Option<LaunchdOwnership> {
    let out = std::process::Command::new("launchctl")
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(ownership_from_output(&String::from_utf8_lossy(&out.stdout)))
}

/// Map an exit-0 `launchctl` payload onto a POSITIVE ownership state.
///
/// Why: separated from the shell-out so the "exit 0 but no PID means the job is
/// genuinely down" rule is testable without launchd. This is the one place
/// allowed to read a missing PID as `NotRunning`, and it is only ever reached
/// after `launchctl` exited 0 — which is what makes that reading safe.
/// What: `Running(pid)` when [`parse_launchctl_pid`] finds one, else
/// `NotRunning`.
/// Test: `ownership_from_output_running`, `ownership_from_output_not_running`.
fn ownership_from_output(stdout: &str) -> LaunchdOwnership {
    match parse_launchctl_pid(stdout) {
        Some(pid) => LaunchdOwnership::Running(pid),
        None => LaunchdOwnership::NotRunning,
    }
}

/// Pure parser for the PID in `launchctl list <label>` / `launchctl print`
/// output.
///
/// Why: separated from the shell-out so every real output shape is testable
/// without launchd. Both forms are handled because the two commands differ and
/// macOS has changed `list`'s format across releases. A parser that silently
/// returned `None` on an unrecognised shape fails CLOSED (#4230 review round 2,
/// LOW): its caller [`ownership_from_output`] reads a missing PID as
/// `NotRunning`, which is a hazard verdict, so an unparsed shape would condemn a
/// healthy daemon rather than wave it through. Every shape launchd actually
/// emits is therefore covered by a test.
/// What: recognises `"PID" = 12345;` (`launchctl list <label>`), `pid = 12345`
/// (`launchctl print`), and a leading `12345\t` (the tab-separated table form).
/// `None` when no PID is present — including launchd's `"PID"` absence for a
/// loaded-but-not-running unit and the `-` placeholder in the table form.
/// Test: `parse_launchctl_pid_reads_the_list_dict_form`,
/// `parse_launchctl_pid_reads_the_print_form`,
/// `parse_launchctl_pid_reads_the_table_form`,
/// `parse_launchctl_pid_none_when_not_running`,
/// `parse_launchctl_pid_none_for_the_table_dash`,
/// `parse_launchctl_pid_ignores_other_numeric_keys`.
pub(crate) fn parse_launchctl_pid(output: &str) -> Option<u32> {
    for raw in output.lines() {
        let line = raw.trim();
        // `launchctl list <label>` dict form: `"PID" = 12345;`
        if let Some(rest) = line.strip_prefix("\"PID\" = ") {
            return rest.trim_end_matches(';').trim().parse().ok();
        }
        // `launchctl print gui/<uid>/<label>` form: `pid = 12345`
        if let Some(rest) = line.strip_prefix("pid = ") {
            return rest.trim_end_matches(';').trim().parse().ok();
        }
        // Tab-separated table form: `12345\t0\tcom.trusty.mpm`
        if let Some((first, rest)) = raw.split_once('\t')
            && rest.contains('\t')
            && let Ok(pid) = first.trim().parse::<u32>()
        {
            return Some(pid);
        }
    }
    None
}

/// Pure decision: should the bare `tm daemon` CLI path refuse to start?
///
/// Why (issue #4397): the #2486 guard was wired only into the MCP-bridge's
/// `no_spawn` path (`compute_no_spawn`, consulted from `serve_stdio.rs`).
/// `run_daemon` (the bare-CLI path, invoked directly or by a launchd plist)
/// never consulted any guard, so `tm daemon` run by hand walked straight past
/// the same hazard: a launchd unit is registered, but THIS process was not
/// started by launchd — the orphan-daemon signature `compute_supervised`
/// already detects. `compute_no_spawn` itself is unsuitable here because it
/// only takes `plist_exists` — applied directly to `run_daemon` it would also
/// refuse the legitimate case where launchd itself is the one invoking
/// `tm daemon`. This function instead keys off `supervised` (which already
/// folds in `is_launchd_supervised`), plus an explicit opt-in.
/// What: `true` (refuse) exactly when `!supervised && !force` — i.e. a
/// launchd unit exists, this process isn't the one launchd started, and the
/// operator did not pass `--force`.
/// Test: `compute_daemon_refuse_true_when_unsupervised_and_not_forced`,
/// `compute_daemon_refuse_false_when_supervised`,
/// `compute_daemon_refuse_false_when_forced`.
pub(crate) fn compute_daemon_refuse(supervised: bool, force: bool) -> bool {
    !supervised && !force
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_supervised_true_when_no_plist() {
        assert!(compute_supervised(false, false));
    }

    #[test]
    fn compute_supervised_true_when_launchd_and_no_plist() {
        // Launchd-supervised with no plist registered (e.g. probed before the
        // plist path convention existed, or a unit under a third path) — still
        // safe: `is_launchd_supervised` alone is sufficient.
        assert!(compute_supervised(true, false));
    }

    #[test]
    fn compute_supervised_true_when_plist_and_launchd() {
        assert!(compute_supervised(true, true));
    }

    #[test]
    fn compute_supervised_false_when_plist_without_launchd() {
        // The hazardous #2486 state: a unit is registered but this process
        // was not launched by launchd.
        assert!(!compute_supervised(false, true));
    }

    #[test]
    fn compute_no_spawn_true_when_plist_exists() {
        assert!(compute_no_spawn(true));
    }

    #[test]
    fn compute_no_spawn_false_when_no_plist() {
        assert!(!compute_no_spawn(false));
    }

    /// #4230: the `tm start` refusal must name the launchctl verb that
    /// actually works and the `--force` opt-in, so an operator hitting it has
    /// an immediate next step either way.
    #[test]
    fn cli_spawn_refusal_names_kickstart_and_force() {
        let msg = cli_spawn_refusal_hint(DAEMON_LAUNCHD_LABEL);
        assert!(msg.contains("launchctl kickstart"), "message was: {msg}");
        assert!(msg.contains("com.trusty.mpm"), "message was: {msg}");
        assert!(msg.contains("tm daemon --force"), "message was: {msg}");
        assert!(msg.contains("#4230"), "message was: {msg}");
    }

    /// #4230 review: the hint must name the label actually registered on this
    /// host. Hardcoding `com.trusty.mpm` prescribed a nonexistent unit —
    /// `launchctl kickstart` on it fails with "Could not find service".
    #[test]
    fn cli_spawn_refusal_names_the_resolved_label() {
        let msg = cli_spawn_refusal_hint("com.trusty.mpm.dogfood");
        assert!(msg.contains("com.trusty.mpm.dogfood"), "message was: {msg}");
        assert!(
            msg.contains("gui/$(id -u)/com.trusty.mpm.dogfood"),
            "message was: {msg}"
        );
    }

    /// A plist body whose `ProgramArguments` run the given subcommand.
    ///
    /// Why: since the round-2 review the drifted-label search reads
    /// `ProgramArguments`, so a test fixture has to carry a realistic body — a bare
    /// `<plist/>` would be rejected and the tests would pass for the wrong reason.
    fn plist_running(subcommand: &str) -> String {
        format!(
            "<plist version=\"1.0\"><dict>\
             <key>Label</key><string>x</string>\
             <key>ProgramArguments</key><array>\
             <string>/Users/x/.cargo/bin/trusty-mpm</string>\
             <string>{subcommand}</string>\
             </array>\
             <key>StandardOutPath</key><string>/Users/x/Logs/daemon.log</string>\
             </dict></plist>"
        )
    }

    /// Build a temp home containing `LaunchAgents/<name>` for each entry, each a
    /// plist that genuinely runs `daemon`.
    fn home_with(plists: &[&str]) -> tempfile::TempDir {
        home_with_bodies(
            &plists
                .iter()
                .map(|n| (*n, plist_running("daemon")))
                .collect::<Vec<_>>(),
        )
    }

    /// Build a temp home with explicit plist bodies, for fixtures that must differ.
    fn home_with_bodies(plists: &[(&str, String)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let agents = dir.path().join("Library/LaunchAgents");
        std::fs::create_dir_all(&agents).expect("create LaunchAgents");
        for (name, body) in plists {
            std::fs::write(agents.join(name), body).expect("write plist");
        }
        dir
    }

    /// Round-2 MEDIUM: the name filter alone would promote ANY `com.trusty.*mpm*`
    /// sibling to "the daemon unit", and every consequence — the spawn refusal, the
    /// restart recipe, the PID compared against `/health` — would key off the wrong
    /// unit. A `logrotate` sibling is the concrete shape; this host already carries a
    /// real `com.trusty.trusty-search.logrotate.plist`.
    #[test]
    fn daemon_label_ignores_a_decoy_sibling_unit() {
        let home =
            home_with_bodies(&[("com.trusty.mpm.logrotate.plist", plist_running("logrotate"))]);
        assert_eq!(
            daemon_launchd_label_in(home.path()),
            None,
            "a sibling unit that does not run `daemon` must not become the daemon label"
        );
    }

    /// The decoy must lose even when it sorts BEFORE the genuine drifted unit — the
    /// resolver sorts for determinism, so a contents-blind filter would have picked
    /// the decoy here.
    #[test]
    fn daemon_label_picks_the_daemon_unit_over_an_alphabetically_earlier_decoy() {
        let home = home_with_bodies(&[
            ("com.trusty.aaa-mpm.plist", plist_running("logrotate")),
            ("com.trusty.zzz-mpm.plist", plist_running("daemon")),
        ]);
        assert_eq!(
            daemon_launchd_label_in(home.path()).as_deref(),
            Some("com.trusty.zzz-mpm")
        );
    }

    /// A plist that cannot be read is not promoted on the strength of its filename.
    #[test]
    fn plist_runs_daemon_false_for_a_missing_file() {
        assert!(!plist_runs_daemon(std::path::Path::new(
            "/nonexistent/com.trusty.mpm.plist"
        )));
    }

    /// `daemon` appearing OUTSIDE `ProgramArguments` — a log path is the common case,
    /// and the real installed plist has exactly that — must not count as an argument.
    #[test]
    fn plist_runs_daemon_ignores_daemon_outside_program_arguments() {
        let home =
            home_with_bodies(&[("com.trusty.mpm.logrotate.plist", plist_running("logrotate"))]);
        let path = home
            .path()
            .join("Library/LaunchAgents/com.trusty.mpm.logrotate.plist");
        assert!(
            std::fs::read_to_string(&path)
                .expect("read fixture")
                .contains("daemon.log"),
            "fixture must contain the decoy substring for this test to mean anything"
        );
        assert!(!plist_runs_daemon(&path));
    }

    /// An exit-0 `launchctl` answer carrying a PID is a POSITIVE "launchd runs this".
    #[test]
    fn ownership_from_output_running() {
        assert_eq!(
            ownership_from_output("{\n\t\"PID\" = 73599;\n};"),
            LaunchdOwnership::Running(73599)
        );
    }

    /// An exit-0 answer with NO PID is the positive "the job is down" state — the
    /// #4230 incident. It is only reachable after `launchctl` exited 0, which is what
    /// separates it from `Unavailable`.
    #[test]
    fn ownership_from_output_not_running() {
        assert_eq!(
            ownership_from_output("{\n\t\"LastExitStatus\" = 0;\n};"),
            LaunchdOwnership::NotRunning
        );
    }

    #[test]
    fn daemon_label_prefers_the_canonical_plist() {
        let home = home_with(&["com.trusty.mpm.plist", "com.trusty.mpm.dogfood.plist"]);
        assert_eq!(
            daemon_launchd_label_in(home.path()).as_deref(),
            Some("com.trusty.mpm")
        );
    }

    /// #4230 review, HIGH-2: `com.trusty.mpm.supervisor.plist` runs
    /// `tm supervisor`, a passive fleet observer that never starts a daemon. On a
    /// `tctl install` host that plist is the ONLY one present, and counting it
    /// made `tm start`/`tm restart` refuse with a false claim and prescribe a
    /// `com.trusty.mpm` unit that does not exist there.
    #[test]
    fn daemon_label_ignores_a_supervisor_only_home() {
        let home = home_with(&["com.trusty.mpm.supervisor.plist"]);
        assert_eq!(daemon_launchd_label_in(home.path()), None);
        // And therefore the whole guard chain stands down on such a host.
        assert!(!compute_no_spawn(
            daemon_launchd_label_in(home.path()).is_some()
        ));
    }

    #[test]
    fn daemon_label_none_for_an_empty_home() {
        let home = home_with(&[]);
        assert_eq!(daemon_launchd_label_in(home.path()), None);
        // A home with no LaunchAgents directory at all must not panic either.
        let bare = tempfile::tempdir().expect("tempdir");
        assert_eq!(daemon_launchd_label_in(bare.path()), None);
    }

    /// Mirrors `install-trusty-mpm-signed.sh`'s drift glob: a non-canonical
    /// daemon label still resolves, so the remediation names a real unit.
    #[test]
    fn daemon_label_accepts_a_drifted_label() {
        let home = home_with(&["com.trusty.dogfood-mpm.plist"]);
        assert_eq!(
            daemon_launchd_label_in(home.path()).as_deref(),
            Some("com.trusty.dogfood-mpm")
        );
    }

    #[test]
    fn daemon_label_ignores_a_gui_plist() {
        let home = home_with(&["com.trusty.mpm.gui.plist"]);
        assert_eq!(daemon_launchd_label_in(home.path()), None);
    }

    #[test]
    fn restart_command_names_kickstart_when_launchd_owns_the_daemon() {
        assert_eq!(
            daemon_restart_command_for(Some("com.trusty.mpm")),
            "launchctl kickstart -k gui/$(id -u)/com.trusty.mpm"
        );
    }

    #[test]
    fn restart_command_names_tm_restart_without_a_unit() {
        assert_eq!(daemon_restart_command_for(None), "tm restart");
    }

    /// Real output captured from `launchctl list com.trusty.mpm` on macOS 15.
    #[test]
    fn parse_launchctl_pid_reads_the_list_dict_form() {
        let out = "{\n\t\"LimitLoadToSessionType\" = \"Aqua\";\n\t\"Label\" = \
                   \"com.trusty.mpm\";\n\t\"OnDemand\" = true;\n\t\"LastExitStatus\" = 0;\n\t\
                   \"PID\" = 73599;\n\t\"Program\" = \"/Users/masa/.cargo/bin/trusty-mpm\";\n};";
        assert_eq!(parse_launchctl_pid(out), Some(73599));
    }

    /// Real output captured from `launchctl print gui/<uid>/com.trusty.mpm`.
    #[test]
    fn parse_launchctl_pid_reads_the_print_form() {
        let out = "\tstate = running\n\tpid = 73599\n\t\tstate = active\n";
        assert_eq!(parse_launchctl_pid(out), Some(73599));
    }

    #[test]
    fn parse_launchctl_pid_reads_the_table_form() {
        assert_eq!(
            parse_launchctl_pid("12345\t0\tcom.trusty.mpm\n"),
            Some(12345)
        );
    }

    /// The #4230 state itself: the unit is loaded but launchd is not running it,
    /// so it reports no PID. Must be `None`, which the verdict reads as "launchd
    /// owns nothing" — hazardous when something else is serving.
    #[test]
    fn parse_launchctl_pid_none_when_not_running() {
        let out = "{\n\t\"Label\" = \"com.trusty.mpm\";\n\t\"LastExitStatus\" = 0;\n};";
        assert_eq!(parse_launchctl_pid(out), None);
    }

    #[test]
    fn parse_launchctl_pid_none_for_the_table_dash() {
        assert_eq!(parse_launchctl_pid("-\t0\tcom.trusty.mpm\n"), None);
    }

    /// `LastExitStatus`/`OnDemand` must never be mistaken for a PID — a parser
    /// that grabbed the first integer would report a bogus PID and, worse, make
    /// the comparison pass by accident.
    #[test]
    fn parse_launchctl_pid_ignores_other_numeric_keys() {
        let out = "{\n\t\"LastExitStatus\" = 0;\n\t\"TimeOut\" = 30;\n};";
        assert_eq!(parse_launchctl_pid(out), None);
    }

    #[test]
    fn compute_daemon_refuse_true_when_unsupervised_and_not_forced() {
        assert!(compute_daemon_refuse(false, false));
    }

    #[test]
    fn compute_daemon_refuse_false_when_supervised() {
        // Supervised (either no plist, or launchd-supervised) always proceeds,
        // regardless of `force`.
        assert!(!compute_daemon_refuse(true, false));
        assert!(!compute_daemon_refuse(true, true));
    }

    #[test]
    fn compute_daemon_refuse_false_when_forced() {
        // The hazardous state, but the operator explicitly opted in.
        assert!(!compute_daemon_refuse(false, true));
    }
}
