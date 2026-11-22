//! The `tagent` alias kept for one release after the rename to `ta` (#11632).
//!
//! Why: MB1516 §11 keeps the old binary name working for one release. A
//! caller of `tagent` must see a deprecation notice and otherwise get exactly
//! what `ta` gives it, or scripts that parse the output break.
//! What: runs `--version` through both binaries from an isolated `$HOME` and
//! compares stdout, exit status and the stderr notice.
//! Test: this file IS the test.

use std::process::Output;

mod common;

fn version(mut cmd: std::process::Command) -> Output {
    cmd.arg("--version").output().expect("spawn --version")
}

/// Why: the alias is a pass-through with one added stderr line.
/// Test: itself.
#[test]
fn tagent_warns_once_then_behaves_like_ta() {
    // #11632: both spawns run under the test's temporary `$HOME`.
    let tmp = tempfile::TempDir::new().unwrap();
    let ta = version(common::ta(&tmp));
    let tagent = version(common::tagent(&tmp));

    assert!(ta.status.success() && tagent.status.success());
    assert_eq!(tagent.stdout, ta.stdout, "same output as `ta --version`");
    assert!(
        String::from_utf8_lossy(&ta.stdout).starts_with("trusty-assistants v"),
        "{}",
        String::from_utf8_lossy(&ta.stdout)
    );
    let stderr = String::from_utf8_lossy(&tagent.stderr);
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(lines.len(), 1, "one notice line: {stderr}");
    assert!(
        lines[0].starts_with("tagent: deprecated") && lines[0].contains("`ta`"),
        "{stderr}"
    );
    assert!(ta.stderr.is_empty(), "`ta` itself prints no notice");
}
