//! Regression tests for the #4395 ownership-aware orphan reaper.
//!
//! Why: the defect was that a healthy production daemon could be SIGKILLed
//! because it shared an executable name. Every test here hands the policy a
//! candidate that a name match would have condemned and asserts it survives.
//! Reverting [`super::plan`] to "confirm every candidate" — the pre-#4395
//! behaviour, where `find_daemon_pids()`'s bare pid list WAS the kill list —
//! fails `plan_confirms_only_our_own_data_dir` and
//! `plan_spares_an_unidentifiable_candidate`.
//!
//! Test: this IS the test module.

use super::*;

fn words(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

/// The argv of a daemon started with no explicit `--data-dir`.
fn plain_argv() -> Vec<String> {
    words(&["trusty-search", "start", "--foreground", "--port", "7878"])
}

/// A minimal but non-empty environment, so "declares no data dir" is
/// distinguishable from "environment unreadable".
fn plain_environ() -> Vec<String> {
    words(&["PATH=/usr/bin", "HOME=/Users/test"])
}

/// Why (#4395): the reaper still has to work — a daemon holding our own data dir
/// but absent from our lockfile is the orphan issue #81 added it for. Sparing
/// everything would be as wrong as killing everything.
/// What: a candidate declaring our exact data dir via `--data-dir` is claimed.
/// Test: this IS the test.
#[test]
fn identify_claims_a_daemon_sharing_our_data_dir() {
    let argv = words(&["trusty-search", "start", "--data-dir", "/tmp/ts-shared"]);
    assert_eq!(
        identify(
            &argv,
            &plain_environ(),
            Path::new("/tmp/ts-shared"),
            Path::new("/platform/default")
        ),
        DaemonIdentity::OwnInstance
    );
}

/// Why (#4395 — THE defect): this is the healthy production daemon. It has the
/// same executable name, the same `start` in argv, and is serving real indexes
/// out of a different data directory. The pre-fix reaper SIGTERMed it and
/// SIGKILLed it 3 seconds later.
/// What: a candidate under a different `--data-dir` is `ForeignInstance`, and the
/// variant carries that directory so the log can name it.
/// Test: this IS the test.
#[test]
fn identify_spares_a_daemon_with_a_different_data_dir() {
    let argv = words(&["trusty-search", "start", "--data-dir", "/tmp/ts-theirs"]);
    assert_eq!(
        identify(
            &argv,
            &plain_environ(),
            Path::new("/tmp/ts-ours"),
            Path::new("/platform/default")
        ),
        DaemonIdentity::ForeignInstance(PathBuf::from("/tmp/ts-theirs"))
    );
}

/// Why (#4395, fail-closed): an unreadable environment is indistinguishable from
/// an empty one. Folding the two together would make every process we cannot
/// inspect resolve to the platform default — a name match by another route, with
/// the same fatal consequence.
/// What: a candidate with no `--data-dir` and an EMPTY environ slice is
/// `Unidentified`, never `OwnInstance`, even when our data dir IS the platform
/// default (the case where the fold would have been invisible).
/// Test: this IS the test.
#[test]
fn identify_spares_a_daemon_whose_environment_is_unreadable() {
    let identity = identify(
        &plain_argv(),
        &[],
        Path::new("/platform/default"),
        Path::new("/platform/default"),
    );
    assert!(
        matches!(identity, DaemonIdentity::Unidentified(_)),
        "an unreadable environment must never resolve to our data dir; got {identity:?}"
    );
}

/// Why (#4395, fail-closed): same argument one level up — argv we cannot read
/// tells us nothing at all.
/// What: an empty argv is `Unidentified`.
/// Test: this IS the test.
#[test]
fn identify_spares_a_daemon_whose_argv_is_unreadable() {
    let identity = identify(
        &[],
        &plain_environ(),
        Path::new("/platform/default"),
        Path::new("/platform/default"),
    );
    assert!(
        matches!(identity, DaemonIdentity::Unidentified(_)),
        "an unreadable argv must not be resolved to any data dir; got {identity:?}"
    );
}

/// Why (#4395, mirroring issue #1182): the self-spawn passes `--data-dir`
/// explicitly precisely so the flag beats a stale inherited `TRUSTY_DATA_DIR`.
/// Reading the environment first would identify such a daemon by the directory
/// it is NOT using.
/// What: with the flag and the env var disagreeing, the flag decides.
/// Test: this IS the test.
#[test]
fn identify_prefers_the_flag_over_the_environment() {
    let argv = words(&["trusty-search", "start", "--data-dir", "/tmp/ts-flag"]);
    let environ = words(&["TRUSTY_DATA_DIR=/tmp/ts-env", "PATH=/usr/bin"]);
    assert_eq!(
        identify(
            &argv,
            &environ,
            Path::new("/tmp/ts-flag"),
            Path::new("/platform/default")
        ),
        DaemonIdentity::OwnInstance,
        "the explicit --data-dir flag must decide (#1182)"
    );
}

/// Why: `--data-dir=/path` is as valid as `--data-dir /path` on the command
/// line, and missing the joined form would leave that daemon `Unidentified` at
/// best and misattributed at worst.
/// What: the `=` form resolves identically.
/// Test: this IS the test.
#[test]
fn identify_reads_the_equals_form_of_the_flag() {
    let argv = words(&["trusty-search", "start", "--data-dir=/tmp/ts-eq"]);
    assert_eq!(
        identify(
            &argv,
            &plain_environ(),
            Path::new("/tmp/ts-eq"),
            Path::new("/platform/default")
        ),
        DaemonIdentity::OwnInstance
    );
}

/// Why (#4395): the common path — neither side sets an override, both use the
/// platform default, so they genuinely do contend and the reap is correct.
/// Sparing here would leave issue #81's orphan accumulation unfixed.
/// What: with a readable environment declaring nothing, the candidate resolves
/// to the platform default and matches an owner on the same default.
/// Test: this IS the test.
#[test]
fn identify_falls_back_to_the_platform_default() {
    assert_eq!(
        identify(
            &plain_argv(),
            &plain_environ(),
            Path::new("/platform/default"),
            Path::new("/platform/default")
        ),
        DaemonIdentity::OwnInstance
    );
    assert_eq!(
        identify(
            &plain_argv(),
            &plain_environ(),
            Path::new("/tmp/ts-isolated"),
            Path::new("/platform/default")
        ),
        DaemonIdentity::ForeignInstance(PathBuf::from("/platform/default")),
        "an isolated owner must not claim the default-dir daemon"
    );
}

/// Why: a trailing separator is the same directory, and a spurious mismatch
/// there would leak orphans (the #81 regression) even though it errs safe.
/// What: `/tmp/ts-x/` and `/tmp/ts-x` compare equal.
/// Test: this IS the test.
#[test]
fn identify_treats_a_trailing_slash_as_the_same_dir() {
    let argv = words(&["trusty-search", "start", "--data-dir", "/tmp/ts-x/"]);
    assert_eq!(
        identify(
            &argv,
            &plain_environ(),
            Path::new("/tmp/ts-x"),
            Path::new("/platform/default")
        ),
        DaemonIdentity::OwnInstance
    );
}

/// Why (#4395 — the regression this whole module exists for): the pre-fix reaper
/// took `find_daemon_pids()`'s bare pid list and signalled all of it. This test
/// hands `plan` a mixed set — one of ours, one production daemon on another data
/// dir, one uninspectable — and asserts only ours is confirmed. Reverting `plan`
/// to confirm every candidate fails it.
/// What: asserts the confirmed set is exactly `[10]` and both others are spared
/// with a stated reason.
/// Test: this IS the test.
#[test]
fn plan_confirms_only_our_own_data_dir() {
    let candidates = vec![
        Candidate {
            pid: 10,
            argv: words(&["trusty-search", "start", "--data-dir", "/tmp/ts-ours"]),
            environ: plain_environ(),
        },
        Candidate {
            pid: 20,
            argv: words(&["trusty-search", "start", "--data-dir", "/tmp/ts-production"]),
            environ: plain_environ(),
        },
        Candidate {
            pid: 30,
            argv: plain_argv(),
            environ: vec![],
        },
    ];
    let plan = plan(
        &candidates,
        Path::new("/tmp/ts-ours"),
        Path::new("/platform/default"),
    );

    let confirmed: Vec<u32> = plan.orphans.iter().map(ConfirmedOrphan::pid).collect();
    assert_eq!(
        confirmed,
        vec![10],
        "only the daemon on our own data dir may be reaped — pid 20 is a healthy \
         production daemon and pid 30 could not be identified (#4395)"
    );
    let spared: Vec<u32> = plan.spared.iter().map(|(pid, _)| *pid).collect();
    assert_eq!(spared, vec![20, 30], "both non-matches must be reported");
    assert!(
        plan.spared.iter().all(|(_, why)| !why.is_empty()),
        "every spared process must carry a stated reason for the operator"
    );
}

/// Why (#4395, fail-closed): "we could not tell" must never advance to a kill.
/// This is the branch a `bool`-shaped check silently folds into "proceed" — the
/// same fail-open shape #4470's port guard was written to avoid.
/// What: a single uninspectable candidate produces an empty orphan list.
/// Test: this IS the test.
#[test]
fn plan_spares_an_unidentifiable_candidate() {
    let candidates = vec![Candidate {
        pid: 99,
        argv: plain_argv(),
        environ: vec![],
    }];
    let plan = plan(
        &candidates,
        Path::new("/platform/default"),
        Path::new("/platform/default"),
    );
    assert!(
        plan.orphans.is_empty(),
        "an unidentifiable process must never be confirmed as an orphan"
    );
    assert_eq!(plan.spared.len(), 1);
}

/// Why (#4395 fix 3, the #4393 half of this issue): the reaper allowed 3 s —
/// roughly a tenth of the 30 s floor the daemon's own shutdown flush applies per
/// index — so even a correctly-targeted orphan was SIGKILLed mid-write. The
/// window `reap` uses must cover that floor.
/// What: asserts the shared termination grace covers
/// `shutdown_flush::MIN_FLUSH_TIMEOUT_SECS`.
/// Test: this IS the test.
#[test]
fn reap_window_covers_the_flush_floor() {
    let window = trusty_common::shutdown::termination_grace();
    let floor = std::time::Duration::from_secs(
        crate::service::shutdown_flush::MIN_FLUSH_TIMEOUT_SECS,
    );
    assert!(
        window >= floor,
        "the reaper's SIGKILL window ({window:?}) must cover a reaped daemon's own \
         per-index flush floor ({floor:?}) — 3 s against 30 s is the #4395 defect"
    );
}
