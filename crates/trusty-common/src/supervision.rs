//! Authoritative, three-state launchd supervision detection (issue #4469).
//!
//! Why: the previous answer to "is this process supervised by launchd?" was an
//! ENVIRONMENT-VARIABLE HEURISTIC — `XPC_SERVICE_NAME` set and `TERM_PROGRAM`
//! absent, with a `getppid() == 1` fallback. Both prongs are satisfiable by a
//! process launchd has never heard of: env vars are inherited by every child,
//! so a `tm daemon` spawned from a launchd-managed context (or from any
//! non-terminal parent) inherits `XPC_SERVICE_NAME` and self-reports
//! `supervised: true`; and an orphan whose spawning parent exited is reparented
//! to PID 1, satisfying the fallback. That is strictly worse than the #4230
//! orphan it was meant to catch — that one at least reported
//! `supervised: false`. `CLAUDE.md`'s connection-safe restart convention tells
//! operators to verify a restart with `curl /health | jq '.supervised'`, so a
//! false positive makes the documented verification unsound in exactly the case
//! it exists to catch.
//!
//! What: [`launchd_supervision`] asks LAUNCHD — not the environment — whether
//! it runs this process, by matching `std::process::id()` against the PID column
//! of `launchctl list`. It answers in THREE states ([`LaunchdSupervision`])
//! because "launchd does not run this PID" and "launchd could not be asked" have
//! opposite consequences and must never be conflated: an unanswerable question
//! reports [`LaunchdSupervision::Unknown`], never a confident `Supervised`.
//! [`is_launchd_supervised`] is the boolean adapter kept for the callers that
//! genuinely need one; it maps `Unknown` to `false`, which is the conservative
//! direction for its only decision (whether a self-exit will be respawned).
//!
//! Deliberately an EXACT PID match, never an ancestor walk: `Terminal.app` is
//! itself a launchd job, so every process in an interactive shell has a
//! launchd-supervised ancestor. Walking the tree would reintroduce the very
//! false positive this module exists to remove.
//!
//! Test: the `tests` module below covers every parser shape and every state
//! transition; `supervision_rejects_a_child_that_inherited_xpc_service_name` is
//! the #4469 regression proof.
//!
//! [`launchd_supervision`]: crate::supervision::launchd_supervision
//! [`LaunchdSupervision`]: crate::supervision::LaunchdSupervision
//! [`LaunchdSupervision::Unknown`]: crate::supervision::LaunchdSupervision::Unknown
//! [`is_launchd_supervised`]: crate::update::is_launchd_supervised

/// What launchd says about THIS process — three states, not two.
///
/// Why: the boolean the heuristic returned could not distinguish "launchd does
/// not run this PID" from "launchd was not reachable to ask", and collapsed both
/// into whichever answer the env vars happened to imply. Those two facts warrant
/// opposite operator actions — the first is the orphan-daemon signature, the
/// second is an absence of evidence — so each gets its own state and the
/// unanswerable case can never render as supervised (issue #4469).
/// What: [`Supervised`](Self::Supervised) carries the launchd label that owns
/// this PID (so callers can name the unit in a message);
/// [`NotSupervised`](Self::NotSupervised) is a POSITIVE answer from launchd that
/// it does not run this PID; [`Unknown`](Self::Unknown) carries a human-readable
/// reason the question could not be put to launchd at all.
/// Test: `supervision_reads_the_pid_column`,
/// `supervision_not_supervised_when_pid_absent`,
/// `supervision_unknown_when_table_has_no_rows`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchdSupervision {
    /// launchd positively reports this PID under the carried label.
    Supervised(String),
    /// launchd answered and this PID is not one of its jobs.
    NotSupervised,
    /// launchd could not be asked; the string explains why.
    Unknown(String),
}

impl LaunchdSupervision {
    /// Is this a positive supervision answer?
    ///
    /// Why: the boolean callers (the post-upgrade self-restart decision, the
    /// daemon's `/health` flag) want one predicate, and it must treat
    /// [`Unknown`](Self::Unknown) as NOT supervised — an unverified claim of
    /// supervision is the #4469 defect.
    /// What: `true` only for [`Supervised`](Self::Supervised).
    /// Test: `is_supervised_is_false_for_unknown`.
    pub fn is_supervised(&self) -> bool {
        matches!(self, LaunchdSupervision::Supervised(_))
    }

    /// One-line description suitable for an operator-facing message.
    ///
    /// Why: `/health` and `tm doctor` both need to say WHICH of the three
    /// states was observed, and a shared renderer keeps them from drifting.
    /// What: names the owning label when supervised, states the negative
    /// plainly, or repeats the `Unknown` reason.
    /// Test: `describe_names_the_label`, `describe_repeats_the_unknown_reason`.
    pub fn describe(&self) -> String {
        match self {
            LaunchdSupervision::Supervised(label) => {
                format!("launchd runs this process under `{label}`")
            }
            LaunchdSupervision::NotSupervised => "launchd does not run this process".to_string(),
            LaunchdSupervision::Unknown(why) => {
                format!("launchd supervision UNKNOWN — {why}")
            }
        }
    }
}

/// Ask launchd whether it runs the current process.
///
/// Why: see the module doc — this replaces the `XPC_SERVICE_NAME`/`getppid()`
/// heuristic that let an unsupervised child self-report as supervised (#4469).
/// What: on macOS, runs `launchctl list` and matches `std::process::id()`
/// against its PID column via [`supervision_from_launchctl_list`]. A
/// `launchctl` that cannot be spawned, exits non-zero, or emits a table with no
/// parseable rows yields [`LaunchdSupervision::Unknown`]. On every non-macOS
/// platform launchd does not exist, which is a POSITIVE negative, so the answer
/// is [`LaunchdSupervision::NotSupervised`] rather than `Unknown`.
/// Test: the pure parser is covered exhaustively below; this wrapper is a
/// subprocess call over it.
pub fn launchd_supervision() -> LaunchdSupervision {
    #[cfg(target_os = "macos")]
    {
        match run_bounded("launchctl", &["list"], LAUNCHCTL_TIMEOUT) {
            Ok(stdout) => supervision_from_launchctl_list(&stdout, std::process::id()),
            Err(why) => LaunchdSupervision::Unknown(why),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        // launchd is macOS-only. This is a known fact about the platform, not
        // an unanswered question, so it is a positive negative.
        LaunchdSupervision::NotSupervised
    }
}

/// How long to wait for `launchctl list` before giving up.
///
/// Why (#4622 review): this runs on the DAEMON-START path. `launchctl` normally
/// answers in milliseconds, but it talks to launchd over IPC and a wedged or
/// saturated launchd would otherwise hang daemon startup indefinitely — an
/// availability bug introduced by a diagnostic. Timing out yields `Unknown`,
/// which is the honest answer and one every consumer already handles.
#[cfg(target_os = "macos")]
const LAUNCHCTL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Spawn `program` with `args`, enforcing `timeout`; return stdout on exit 0.
///
/// Why: `std::process::Command::output()` blocks forever — see
/// [`LAUNCHCTL_TIMEOUT`]. Spawn + poll rather than a detached thread so the
/// child is actually KILLED on expiry instead of leaked.
/// What: polls `try_wait` every 20 ms until the deadline, then kills and reaps.
/// `Err` carries an operator-readable reason for every failure mode — spawn
/// failure, timeout, or a non-zero exit — which the caller reports verbatim as
/// the `Unknown` reason. Taking the program and timeout as parameters keeps the
/// deadline testable without needing a launchd that hangs.
/// Test: `bounded_run_kills_a_command_that_outlives_its_deadline`,
/// `bounded_run_returns_stdout_on_success`,
/// `bounded_run_reports_a_nonzero_exit`.
#[cfg(target_os = "macos")]
fn run_bounded(
    program: &str,
    args: &[&str],
    timeout: std::time::Duration,
) -> Result<String, String> {
    use std::io::Read as _;

    let label = format!("`{program} {}`", args.join(" "));
    let mut child = std::process::Command::new(program)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("{label} could not run: {e}"))?;

    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut stdout = String::new();
                if let Some(mut pipe) = child.stdout.take() {
                    let _ = pipe.read_to_string(&mut stdout);
                }
                return if status.success() {
                    Ok(stdout)
                } else {
                    Err(format!("{label} exited with status {status}"))
                };
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("{label} did not answer within {timeout:?}"));
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(e) => return Err(format!("{label} could not be waited on: {e}")),
        }
    }
}

/// Pure decision: does `stdout` (a `launchctl list` table) claim `pid`?
///
/// Why: separated from the shell-out so every real output shape — including the
/// unparseable one that must become `Unknown` — is testable without launchd.
/// What: `launchctl list` prints a `PID\tStatus\tLabel` table whose PID column
/// is `-` for a loaded-but-not-running job. A row whose PID equals `pid` yields
/// [`LaunchdSupervision::Supervised`] with that row's label. When at least one
/// row parsed but none matched, launchd has positively answered "not mine" →
/// [`LaunchdSupervision::NotSupervised`]. When NO row parsed at all (empty
/// output, header only, or a format this parser does not recognise) the question
/// was effectively not answered → [`LaunchdSupervision::Unknown`]; failing
/// closed here is what keeps an unrecognised future format from silently
/// reading as "not supervised" or, worse, as supervised.
/// Test: `supervision_reads_the_pid_column`,
/// `supervision_not_supervised_when_pid_absent`,
/// `supervision_ignores_the_header_row`,
/// `supervision_ignores_dash_pids`,
/// `supervision_unknown_when_table_has_no_rows`,
/// `supervision_unknown_for_unrecognised_format`.
pub fn supervision_from_launchctl_list(stdout: &str, pid: u32) -> LaunchdSupervision {
    let mut parsed_any_row = false;
    for line in stdout.lines() {
        let mut fields = line.split('\t');
        let (Some(pid_field), Some(_status), Some(label)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let label = label.trim();
        if label.is_empty() {
            continue;
        }
        // The header row (`PID\tStatus\tLabel`) and a not-running job's `-`
        // placeholder both fail to parse as a number; neither is a real row for
        // matching purposes, but a `-` row DOES prove the table parsed.
        let pid_field = pid_field.trim();
        if pid_field == "PID" {
            continue;
        }
        parsed_any_row = true;
        if pid_field.parse::<u32>() == Ok(pid) {
            return LaunchdSupervision::Supervised(label.to_string());
        }
    }

    if parsed_any_row {
        LaunchdSupervision::NotSupervised
    } else {
        LaunchdSupervision::Unknown("`launchctl list` returned no parseable job rows".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A realistic `launchctl list` table.
    fn table() -> String {
        [
            "PID\tStatus\tLabel",
            "505\t0\tcom.apple.trustd.agent",
            "-\t0\tcom.apple.mdworker.mail",
            "98606\t0\tcom.trusty.mpm",
        ]
        .join("\n")
    }

    #[test]
    fn supervision_reads_the_pid_column() {
        // launchd naming our PID is the only thing that proves supervision.
        assert_eq!(
            supervision_from_launchctl_list(&table(), 98606),
            LaunchdSupervision::Supervised("com.trusty.mpm".to_string())
        );
    }

    #[test]
    fn supervision_not_supervised_when_pid_absent() {
        // launchd answered, and this PID is not one of its jobs. That is a
        // POSITIVE negative, not an unknown.
        assert_eq!(
            supervision_from_launchctl_list(&table(), 12345),
            LaunchdSupervision::NotSupervised
        );
    }

    #[test]
    fn supervision_ignores_the_header_row() {
        // The literal header must never be mistaken for a job row.
        let out = supervision_from_launchctl_list("PID\tStatus\tLabel\n", 1);
        assert!(
            matches!(out, LaunchdSupervision::Unknown(_)),
            "header-only output is not an answer, got {out:?}"
        );
    }

    #[test]
    fn supervision_ignores_dash_pids() {
        // A `-` PID means the job is loaded but not running; it can never match.
        let out = supervision_from_launchctl_list("PID\tStatus\tLabel\n-\t0\tcom.x\n", 0);
        assert_eq!(out, LaunchdSupervision::NotSupervised);
    }

    #[test]
    fn supervision_unknown_when_table_has_no_rows() {
        // Empty output is an absence of evidence, never evidence of absence.
        assert!(matches!(
            supervision_from_launchctl_list("", 1),
            LaunchdSupervision::Unknown(_)
        ));
    }

    #[test]
    fn supervision_unknown_for_unrecognised_format() {
        // A future launchctl that stops emitting the tab-separated table must
        // report UNKNOWN, not a confident answer in either direction.
        assert!(matches!(
            supervision_from_launchctl_list("{\"jobs\": []}\n", 1),
            LaunchdSupervision::Unknown(_)
        ));
    }

    /// #4469 regression proof: the defect the old heuristic had.
    ///
    /// Why: the old implementation returned `true` whenever `XPC_SERVICE_NAME`
    /// was set and `TERM_PROGRAM` was not — a condition ANY child inherits. The
    /// authoritative answer comes from launchd's PID table, so a PID launchd does
    /// not run is `NotSupervised` no matter what the environment says.
    /// What: deliberately takes NO environment input (#4622 review — the
    /// previous version mutated `XPC_SERVICE_NAME` unguarded, racing the
    /// `ENV_LOCK`-guarded tests in the same binary). That the environment is
    /// irrelevant is proven structurally instead: `supervision_from_launchctl_list`
    /// has no env parameter and reads none. The env-shape proof that DOES need a
    /// live process lives in `update::tests::is_launchd_supervised_is_not_fooled_by_inherited_env`,
    /// which holds `ENV_LOCK`.
    /// Test: this test.
    #[test]
    fn supervision_rejects_a_child_that_inherited_xpc_service_name() {
        // PID 424242 is not in the table — i.e. launchd does not run it. The
        // heuristic would have said `true` for this process regardless.
        let verdict = supervision_from_launchctl_list(&table(), 424_242);
        assert_eq!(
            verdict,
            LaunchdSupervision::NotSupervised,
            "a child that merely INHERITED XPC_SERVICE_NAME must not self-report supervised"
        );
        assert!(!verdict.is_supervised());
    }

    /// The `launchctl` call must not be able to hang daemon startup.
    ///
    /// Why (#4622 review): this runs on the daemon-start path, so an unbounded
    /// wait turns a diagnostic into an availability bug. Driven against `sleep`
    /// rather than a wedged launchd, which cannot be constructed on demand.
    /// Test: this test.
    #[cfg(target_os = "macos")]
    #[test]
    fn bounded_run_kills_a_command_that_outlives_its_deadline() {
        let started = std::time::Instant::now();
        let result = run_bounded("sleep", &["30"], std::time::Duration::from_millis(300));
        let elapsed = started.elapsed();

        let err = result.expect_err("a 30s command must not satisfy a 300ms deadline");
        assert!(err.contains("did not answer within"), "was: {err}");
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "the deadline was not enforced; waited {elapsed:?}"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn bounded_run_returns_stdout_on_success() {
        let out = run_bounded("echo", &["hello"], std::time::Duration::from_secs(5)).unwrap();
        assert_eq!(out.trim(), "hello");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn bounded_run_reports_a_nonzero_exit() {
        // `false` exits 1 — a non-zero exit is an absence of an answer, so it
        // must surface as an error the caller turns into `Unknown`.
        let err = run_bounded("false", &[], std::time::Duration::from_secs(5))
            .expect_err("a non-zero exit is not an answer");
        assert!(err.contains("exited with status"), "was: {err}");
    }

    #[test]
    fn is_supervised_is_false_for_unknown() {
        // Unknown must never be treated as a pass.
        assert!(!LaunchdSupervision::Unknown("no launchctl".into()).is_supervised());
        assert!(!LaunchdSupervision::NotSupervised.is_supervised());
        assert!(LaunchdSupervision::Supervised("com.trusty.mpm".into()).is_supervised());
    }

    #[test]
    fn describe_names_the_label() {
        let d = LaunchdSupervision::Supervised("com.trusty.mpm".into()).describe();
        assert!(d.contains("com.trusty.mpm"), "was: {d}");
    }

    #[test]
    fn describe_repeats_the_unknown_reason() {
        let d = LaunchdSupervision::Unknown("launchctl missing".into()).describe();
        assert!(d.contains("UNKNOWN"), "was: {d}");
        assert!(d.contains("launchctl missing"), "was: {d}");
    }
}
