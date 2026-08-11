//! `trusty-audit` and `taudit` are the same program.
//!
//! Why: #5502 settles the binary name by shipping BOTH — `trusty-audit` in the
//! docs and the handoff README, `taudit` as the short form for repeat use. The
//! two Cargo `[[bin]]` targets share one `src/main.rs`, so today they cannot
//! diverge; what this file catches is the edit that stops that holding — a
//! second `path`, a `cfg` keyed on the target name, a per-target feature set.
//! What: runs both compiled binaries over one working directory and compares
//! stdout, stderr and exit status across the whole CLI surface.
//! Test: `every_capability_prints_the_same_under_both_names`,
//! `both_names_report_the_same_version`,
//! `help_differs_only_in_the_name_it_was_invoked_as`.

use std::path::Path;
use std::process::Command;

/// Cargo builds one `CARGO_BIN_EXE_<target>` per `[[bin]]`, so these two
/// constants failing to resolve IS the "both names exist" assertion.
const PRIMARY: &str = env!("CARGO_BIN_EXE_trusty-audit");
const ALIAS: &str = env!("CARGO_BIN_EXE_taudit");

/// Everything an operator would see from one invocation.
type Seen = (String, String, Option<i32>);

fn run(binary: &str, args: &[&str]) -> Seen {
    let output = Command::new(binary)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("{binary} did not start: {e}"));
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.code(),
    )
}

fn run_in(binary: &str, work: &Path, args: &[&str]) -> Seen {
    let mut argv = vec!["--work-dir", work.to_str().expect("utf-8 temp path")];
    argv.extend_from_slice(args);
    run(binary, &argv)
}

#[test]
fn every_capability_prints_the_same_under_both_names() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let work = tmp.path().join("work");

    // A bare invocation plus one per verb — the whole capability set.
    let cases: [&[&str]; 6] = [
        &[],
        &["guided"],
        &["workdir"],
        &["repos"],
        &["tools"],
        &["manifest"],
    ];
    for args in cases {
        assert_eq!(
            run_in(PRIMARY, &work, args),
            run_in(ALIAS, &work, args),
            "`{args:?}` behaves differently under the two binary names"
        );
    }
}

#[test]
fn both_names_report_the_same_version() {
    assert_eq!(run(PRIMARY, &["--version"]), run(ALIAS, &["--version"]));
}

#[test]
fn help_differs_only_in_the_name_it_was_invoked_as() {
    let (primary, _, _) = run(PRIMARY, &["--help"]);
    let (alias, _, _) = run(ALIAS, &["--help"]);

    // clap takes the usage line's program name from argv[0], so each binary
    // echoes the name that was typed. Nothing else may differ.
    assert!(primary.contains("Usage: trusty-audit ["), "{primary}");
    assert!(alias.contains("Usage: taudit ["), "{alias}");
    assert_eq!(
        primary.replace("Usage: trusty-audit", "Usage: taudit"),
        alias
    );
}
