//! Output of `trusty-installer version`, the capability-discovery entry point.
//!
//! Why: `version::run` prints straight to stdout, so a unit test can only call
//! it and check it returns. Running the real binary is the way to assert what
//! a caller actually reads (#11268).
//!
//! What: runs the binary in human and `--json` mode and asserts the version
//! line and the DOC-5 discovery object.
//!
//! Test: `version_human_line_names_tool_and_stack_version`,
//! `version_json_is_the_capability_discovery_object`.

use std::process::Command;

/// Run `trusty-installer <args>` and return its stdout, asserting exit 0.
fn run_version(args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_trusty-installer"))
        .args(args)
        .output()
        .expect("spawn trusty-installer");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{args:?}: stdout: {stdout} stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

/// Why: the human line is what an operator pastes into a bug report.
/// What: asserts it names the crate version and the Phase-0 stack version.
/// Test: This is the test.
#[test]
fn version_human_line_names_tool_and_stack_version() {
    let stdout = run_version(&["version"]);
    let want = format!(
        "trusty-installer v{}  stack_version: 0.0.0-scaffold",
        env!("CARGO_PKG_VERSION")
    );
    assert!(stdout.contains(&want), "{stdout}");
}

/// Why: `--json` is the machine contract (DOC-5 §4.2); callers parse it.
/// What: parses stdout as JSON and asserts every field of the object.
/// Test: This is the test.
#[test]
fn version_json_is_the_capability_discovery_object() {
    let stdout = run_version(&["--json", "version"]);
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("stdout is JSON");
    assert_eq!(v["tool"], "trusty-installer");
    assert_eq!(v["tool_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(v["stack_version"], "0.0.0-scaffold");
    assert_eq!(v["contract_floor"], 1);
    assert_eq!(v["contract_target"], 1);
}
