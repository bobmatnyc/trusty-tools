//! `tm doctor` is a standalone diagnostic that needs no daemon (#6336).
//!
//! Why: doctor used to fetch the whole report from `GET /api/v1/doctor`, so
//! with no daemon running it printed `doctor failed: daemon unreachable: …`
//! and ran none of the 27 purely local checks — it refused to diagnose at
//! exactly the moment an operator needs a diagnosis. The unit tests around the
//! new row cannot catch a regression here, because the defect was never in a
//! function: it was in which function the CLI called. Only the real binary,
//! run against an address nothing listens on, proves the command completes.
//! What: runs the built `tm` as `tm --url <dead> doctor` under a scratch HOME
//! and cwd, then asserts the local checks printed, that daemon reachability is
//! ONE row saying "not running", and that no output names a port.
//! Test: `cargo test -p trusty-mpm --test tm_doctor_standalone`.

use std::process::Command;

/// Run `tm doctor` against an address nothing listens on.
///
/// The scratch HOME keeps the run off the operator's real framework root, and
/// port 1 on loopback is the same never-listening address the `tm hook`
/// fail-open suite uses — the connect is refused rather than timing out.
fn run_doctor_with_no_daemon() -> (bool, String, String) {
    let home = tempfile::tempdir().expect("scratch home");
    let cwd = tempfile::tempdir().expect("scratch cwd");
    let output = Command::new(env!("CARGO_BIN_EXE_tm"))
        .args(["--url", "http://127.0.0.1:1", "doctor"])
        .current_dir(cwd.path())
        .env("HOME", home.path())
        .env_remove("TRUSTY_MPM_URL")
        .output()
        .expect("failed to spawn `tm doctor`");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// With no daemon reachable, doctor still reports every local check and says
/// so in one daemon row — it does not abort.
///
/// This is the regression the whole change exists to prevent. Before #6336 the
/// same invocation printed `doctor failed: daemon unreachable: …` and nothing
/// else, so every assertion below fails on the pre-fix binary.
#[test]
fn tm_doctor_reports_every_local_check_with_no_daemon() {
    let (ok, stdout, stderr) = run_doctor_with_no_daemon();

    assert!(ok, "`tm doctor` exited non-zero.\nstderr:\n{stderr}");
    assert!(
        !stdout.contains("doctor failed") && !stderr.contains("doctor failed"),
        "doctor aborted instead of reporting.\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        !stdout.contains("daemon unreachable") && !stderr.contains("daemon unreachable"),
        "the pre-#6336 abort message is still emitted.\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    assert!(
        stdout.contains("trusty-mpm doctor"),
        "no report header.\nstdout:\n{stdout}"
    );
    // A representative spread of the purely local battery: the instruction
    // pipeline, the deploy tiers, and a probe that shells out rather than
    // reading files. None of them needs a daemon, and none of them ran before.
    for check in [
        "instructions",
        "agents",
        "skills",
        "output_style",
        "deployment",
        "binary_provenance",
        "session_store",
    ] {
        assert!(
            stdout.contains(check),
            "local check {check:?} missing from the report.\nstdout:\n{stdout}"
        );
    }
    assert!(
        stdout.contains("overall:"),
        "no overall verdict.\nstdout:\n{stdout}"
    );
}

/// Daemon reachability is exactly ONE row, and with nothing listening it reads
/// "not running" — never an error, never a repeated complaint.
#[test]
fn tm_doctor_reports_the_absent_daemon_as_exactly_one_row() {
    let (ok, stdout, stderr) = run_doctor_with_no_daemon();
    assert!(ok, "`tm doctor` exited non-zero.\nstderr:\n{stderr}");

    let rows: Vec<&str> = stdout
        .lines()
        .filter(|line| line.contains("trusty-mpm daemon:"))
        .collect();
    assert_eq!(
        rows.len(),
        1,
        "expected exactly one daemon row, got {}.\nstdout:\n{stdout}",
        rows.len()
    );
    assert!(
        rows[0].contains("not running"),
        "daemon row does not say the daemon is not running: {}",
        rows[0]
    );

    // #2332 staleness and #4230 orphan detection are comparisons ABOUT a
    // daemon's answer. With no answer there is nothing to compare, so they are
    // skipped rather than restating the row above in worse words.
    for skipped in ["daemon_version", "daemon_orphan"] {
        assert!(
            !stdout.contains(skipped),
            "{skipped} reported without a daemon to reason about.\nstdout:\n{stdout}"
        );
    }
}

/// No line of doctor's output names a port.
///
/// The reported symptom was the literal string "port 7880 unreachable". The
/// row must survive #6288 moving the daemon to a Unix socket without being
/// reworded, which it can only do if it never names the transport.
#[test]
fn tm_doctor_output_never_names_a_daemon_port() {
    let (ok, stdout, stderr) = run_doctor_with_no_daemon();
    assert!(ok, "`tm doctor` exited non-zero.\nstderr:\n{stderr}");
    assert!(
        !stdout.contains("7880"),
        "doctor output names port 7880.\nstdout:\n{stdout}"
    );
}
