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
        identify(&argv, &plain_environ(), Path::new("/tmp/ts-shared")),
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
        identify(&argv, &plain_environ(), Path::new("/tmp/ts-ours")),
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
    let identity = identify(&plain_argv(), &[], Path::new("/platform/default"));
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
    let identity = identify(&[], &plain_environ(), Path::new("/platform/default"));
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
        identify(&argv, &environ, Path::new("/tmp/ts-flag")),
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
        identify(&argv, &plain_environ(), Path::new("/tmp/ts-eq")),
        DaemonIdentity::OwnInstance
    );
}

/// Why (#4395): the common path — neither side sets an override, both use the
/// platform default, so they genuinely do contend and the reap is correct.
/// Sparing here would leave issue #81's orphan accumulation unfixed.
/// What: with a readable environment declaring nothing, the candidate resolves
/// to the platform default under its own HOME (#9232) and matches an owner on
/// that same default.
/// Test: this IS the test.
#[test]
fn identify_falls_back_to_the_platform_default() {
    let default = default_under("/Users/test");
    assert_eq!(
        identify(&plain_argv(), &plain_environ(), &default),
        DaemonIdentity::OwnInstance
    );
    assert_eq!(
        identify(
            &plain_argv(),
            &plain_environ(),
            Path::new("/tmp/ts-isolated")
        ),
        DaemonIdentity::ForeignInstance(default),
        "an isolated owner must not claim the default-dir daemon"
    );
}

/// The platform default data dir a process with `HOME=<home>` and no
/// `XDG_DATA_HOME` resolves to — what `dirs::data_local_dir()` gives it.
fn default_under(home: &str) -> PathBuf {
    let under_home = if cfg!(target_os = "macos") {
        "Library/Application Support"
    } else {
        ".local/share"
    };
    Path::new(home).join(under_home).join("trusty-search")
}

/// The argv launchd runs the live `com.trusty.search` service with: no
/// `--data-dir`, so it serves the platform default under its own HOME.
fn launchd_argv() -> Vec<String> {
    words(&[
        "/Users/live/.cargo/bin/trusty-search",
        "start",
        "--foreground",
        "--no-auto-discover",
    ])
}

/// Why (#9232 — the defect): a sandbox `start` under another HOME with no
/// `--data-dir` resolved the LIVE service's default data dir from the sandbox's
/// own HOME, found it equal to its own, and confirmed the live daemon as an
/// orphan to SIGTERM. A daemon under the same HOME as ours must still be reaped.
/// What: the reaper's data dir is the sandbox HOME's default; pid 20 is the live
/// service under `/Users/live` and must be spared, pid 10 is a real orphan under
/// the sandbox HOME and must be confirmed.
/// Test: this IS the test.
#[test]
fn plan_spares_a_daemon_under_another_home() {
    let ours = default_under("/tmp/sandbox-home");
    let candidates = vec![
        Candidate {
            pid: 10,
            start_time: 0,
            argv: launchd_argv(),
            environ: words(&["PATH=/usr/bin", "HOME=/tmp/sandbox-home"]),
        },
        Candidate {
            pid: 20,
            start_time: 0,
            argv: launchd_argv(),
            environ: words(&["PATH=/usr/bin", "HOME=/Users/live"]),
        },
    ];
    let plan = plan(&candidates, &ours);
    let confirmed: Vec<u32> = plan.orphans.iter().map(ConfirmedOrphan::pid).collect();
    assert_eq!(
        confirmed,
        vec![10],
        "the live daemon under another HOME must never be an orphan (#9232)"
    );
    assert_eq!(plan.spared.len(), 1);
    assert!(
        plan.spared[0].1.contains("/Users/live"),
        "the spared reason must name the live daemon's own data dir: {:?}",
        plan.spared
    );
}

/// Why (#9232, fail-closed): a candidate that declares no data dir and whose
/// environment carries no usable HOME has an unknown default. Assuming ours is
/// the defect by another route.
/// What: a readable environment without HOME, with an empty HOME, or with a
/// relative HOME is spared with a reason naming HOME, even when our data dir is
/// the default a plausible HOME would give.
/// Test: this IS the test.
#[test]
fn plan_spares_a_daemon_whose_home_is_unreadable() {
    let ours = default_under("/Users/live");
    for environ in [
        &["PATH=/usr/bin"][..],
        &["PATH=/usr/bin", "HOME="][..],
        &["PATH=/usr/bin", "HOME=Users/live"][..],
        &["PATH=/usr/bin", "HOMEBREW_PREFIX=/Users/live"][..],
    ] {
        let candidates = vec![Candidate {
            pid: 30,
            start_time: 0,
            argv: launchd_argv(),
            environ: words(environ),
        }];
        let plan = plan(&candidates, &ours);
        assert!(plan.orphans.is_empty(), "{environ:?} must be spared");
        assert!(
            plan.spared[0].1.contains("HOME"),
            "{environ:?}: the reason must say HOME is unknown: {:?}",
            plan.spared
        );
    }
}

/// Why (#9232): the candidate-side default must be the dir the daemon itself
/// resolves, or a genuine orphan on the default dir is never reaped (#81).
/// What: feeds this process's own HOME (and XDG_DATA_HOME, when set) to
/// [`candidate_default_data_dir`] and compares it with `resolve_daemon_dir(None)`.
/// Test: this IS the test.
#[test]
#[serial_test::serial]
fn candidate_default_matches_the_daemons_own_resolution() {
    let mut environ = vec!["PATH=/usr/bin".to_string()];
    for key in ["HOME", "XDG_DATA_HOME"] {
        if let Ok(value) = std::env::var(key) {
            environ.push(format!("{key}={value}"));
        }
    }
    assert_eq!(
        candidate_default_data_dir(&environ).ok(),
        crate::service::daemon::resolve_daemon_dir(None),
        "environ {environ:?}"
    );
}

/// A confirmed orphan, minted the only way there is: through [`plan`].
fn confirmed(pid: u32, start_time: u64, data_dir: &str) -> (ConfirmedOrphan, Candidate) {
    let candidate = Candidate {
        pid,
        start_time,
        argv: words(&["trusty-search", "start", "--data-dir", data_dir]),
        environ: plain_environ(),
    };
    let plan = plan(std::slice::from_ref(&candidate), Path::new(data_dir));
    let orphan = plan.orphans.into_iter().next().expect("our own daemon");
    (orphan, candidate)
}

/// Why (#9232): the reaper still has to signal an orphan that is still there.
/// What: the same pid, start time and data dir in a fresh scan pass.
/// Test: this IS the test.
#[test]
fn recheck_confirms_the_same_process() {
    let (orphan, now) = confirmed(10, 1_700, "/tmp/ts-ours");
    assert_eq!(recheck(&orphan, &[now], Path::new("/tmp/ts-ours")), Ok(()));
}

/// Why (#9232, fail-closed): between the plan and a signal the orphan can exit
/// and its pid be reused, or its argv change or become unreadable. Signalling
/// by number then hits a stranger.
/// What: a pid missing from the fresh scan, a different start time, a different
/// data dir, and an unreadable argv each refuse, with a reason naming the pid.
/// Test: this IS the test.
#[test]
fn recheck_refuses_a_pid_that_is_gone_reused_or_foreign() {
    let ours = Path::new("/tmp/ts-ours");
    let (orphan, now) = confirmed(10, 1_700, "/tmp/ts-ours");
    let reused = Candidate {
        start_time: 1_900,
        ..now.clone()
    };
    let foreign = Candidate {
        argv: words(&["trusty-search", "start", "--data-dir", "/tmp/ts-live"]),
        ..now.clone()
    };
    let unreadable = Candidate {
        argv: vec![],
        ..now.clone()
    };
    for (case, fresh, expect) in [
        ("gone", vec![], "no longer a trusty-search daemon"),
        ("reused", vec![reused], "was reused"),
        ("foreign", vec![foreign], "different data dir"),
        ("unreadable", vec![unreadable], "no longer be identified"),
    ] {
        let err = recheck(&orphan, &fresh, ours).expect_err(case);
        assert!(err.contains(expect) && err.contains("10"), "{case}: {err}");
    }
}

/// Why: a trailing separator is the same directory, and a spurious mismatch
/// there would leak orphans (the #81 regression) even though it errs safe.
/// What: `/tmp/ts-x/` and `/tmp/ts-x` compare equal.
/// Test: this IS the test.
#[test]
fn identify_treats_a_trailing_slash_as_the_same_dir() {
    let argv = words(&["trusty-search", "start", "--data-dir", "/tmp/ts-x/"]);
    assert_eq!(
        identify(&argv, &plain_environ(), Path::new("/tmp/ts-x")),
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
            start_time: 0,
            argv: words(&["trusty-search", "start", "--data-dir", "/tmp/ts-ours"]),
            environ: plain_environ(),
        },
        Candidate {
            pid: 20,
            start_time: 0,
            argv: words(&["trusty-search", "start", "--data-dir", "/tmp/ts-production"]),
            environ: plain_environ(),
        },
        Candidate {
            pid: 30,
            start_time: 0,
            argv: plain_argv(),
            environ: vec![],
        },
    ];
    let plan = plan(&candidates, Path::new("/tmp/ts-ours"));

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
        start_time: 0,
        argv: plain_argv(),
        environ: vec![],
    }];
    let plan = plan(&candidates, Path::new("/platform/default"));
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
    let floor =
        std::time::Duration::from_secs(crate::service::shutdown_flush::MIN_FLUSH_TIMEOUT_SECS);
    assert!(
        window >= floor,
        "the reaper's SIGKILL window ({window:?}) must cover a reaped daemon's own \
         per-index flush floor ({floor:?}) — 3 s against 30 s is the #4395 defect"
    );
}
